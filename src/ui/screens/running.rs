//! Running screen: progress bar, ETA, speed, collapsible stderr log (M4).
//!
//! Shows the progress bar, percentage, elapsed, ETA, current speed
//! multiplier, output size so far, and frames processed/dropped — with a
//! scrollable stderr log collapsed by default (`l` expands). Cancellation,
//! overwrite confirmation, and partial-file cleanup are state transitions
//! here, not modal dialogs.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tokio::sync::oneshot;

use crate::ffmpeg::builder::CommandSpec;
use crate::ffmpeg::errors::{diagnose, Diagnosis};
use crate::ffmpeg::probe::{format_duration, format_size};
use crate::ffmpeg::progress::{eta_seconds, progress_ratio, SpeedSmoother};
use crate::ffmpeg::runner::JobResult;
use crate::ui::theme::Theme;
use crate::ui::widgets::log_pane::{push_log, render_log};
use crate::ui::widgets::progress_bar::render_progress;

/// Lifecycle of the running screen.
#[derive(Debug, PartialEq, Eq)]
pub enum RunPhase {
    /// Output exists; `y` overwrites (then `-y` is passed), `n` goes back.
    ConfirmOverwrite,
    /// Job running normally.
    Active,
    /// `SIGTERM` sent; waiting out the grace period (second Ctrl+C kills).
    Cancelling,
    /// Ended with a partial file on disk; `y` deletes it, `n` keeps it.
    ConfirmDelete,
    /// Ended; result card shown.
    Done,
}

/// All state for one running job. Owned by [`App`](crate::app::App).
pub struct RunState {
    /// Which operation launched this job (display only).
    pub op_name: String,
    /// Input being processed — matches probe arrivals for backfill.
    pub input: Option<PathBuf>,
    /// Shared job-id space with the queue (0 reserved for nothing — every
    /// spawn takes an id, so late messages can never hit the wrong job).
    pub job_id: u64,
    /// The built command, kept for spawning after the overwrite confirm.
    pub spec: CommandSpec,
    /// Shell-quoted command, for display and the error report.
    pub display_command: String,
    /// Expected output path (partial-file prompt + size display).
    pub output: PathBuf,
    /// Input size for the before/after comparison, when known.
    pub input_size: Option<u64>,
    /// Total duration for percentage; `None` → indeterminate spinner.
    pub total_duration: Option<Duration>,
    /// When the job (or confirm) started; drives elapsed.
    pub started: Instant,
    /// Current phase.
    pub phase: RunPhase,
    /// Latest progress values.
    pub frame: Option<u64>,
    /// Latest frames/sec.
    pub fps: Option<f64>,
    /// Latest kbit/s.
    pub bitrate_kbps: Option<f64>,
    /// Latest output size in bytes.
    pub total_size: Option<u64>,
    /// Latest media timestamp reached.
    pub out_time: Option<Duration>,
    /// Cumulative duplicated/dropped frames.
    pub dup_frames: u64,
    /// See above.
    pub drop_frames: u64,
    /// Smoothed speed for the ETA.
    pub smoother: SpeedSmoother,
    /// Latest raw speed multiplier.
    pub last_speed: Option<f64>,
    /// Live stderr ring for the log pane.
    pub stderr: VecDeque<String>,
    /// Log pane expanded (`l` toggles).
    pub log_expanded: bool,
    /// Scroll offset from the live tail when expanded.
    pub log_scroll: usize,
    /// Child pid for second-press force-kill.
    pub pid: Option<u32>,
    /// Cancel trigger; taken when the user presses Ctrl+C.
    pub cancel_tx: Option<oneshot::Sender<()>>,
    /// Final result, once finished.
    pub result: Option<JobResult>,
    /// When a successful job settled — drives the §2 auto-return pause.
    /// Failures and cancellations leave this `None` (they stay up until
    /// acknowledged).
    pub finished_at: Option<Instant>,
    /// Translated failure, once finished unsuccessfully.
    pub diagnosis: Option<Diagnosis>,
}

impl RunState {
    /// Fresh state behind the overwrite confirm. The confirm resolves
    /// before any process spawns, so `-y` is never a surprise.
    pub fn confirming(
        op_name: String,
        spec: &CommandSpec,
        output: PathBuf,
        total_duration: Option<Duration>,
        input_size: Option<u64>,
    ) -> Self {
        Self::new(
            op_name,
            spec,
            output,
            total_duration,
            input_size,
            RunPhase::ConfirmOverwrite,
        )
    }

    /// Fresh state for an imminent spawn (no confirm needed).
    pub fn starting(
        op_name: String,
        spec: &CommandSpec,
        output: PathBuf,
        total_duration: Option<Duration>,
        input_size: Option<u64>,
    ) -> Self {
        Self::new(
            op_name,
            spec,
            output,
            total_duration,
            input_size,
            RunPhase::Active,
        )
    }

    fn new(
        op_name: String,
        spec: &CommandSpec,
        output: PathBuf,
        total_duration: Option<Duration>,
        input_size: Option<u64>,
        phase: RunPhase,
    ) -> Self {
        Self {
            op_name,
            input: None,
            job_id: u64::MAX,
            spec: spec.clone(),
            display_command: spec.to_display(),
            output,
            input_size,
            total_duration,
            started: Instant::now(),
            phase,
            frame: None,
            fps: None,
            bitrate_kbps: None,
            total_size: None,
            out_time: None,
            dup_frames: 0,
            drop_frames: 0,
            smoother: SpeedSmoother::default(),
            last_speed: None,
            stderr: VecDeque::new(),
            log_expanded: false,
            log_scroll: 0,
            pid: None,
            cancel_tx: None,
            result: None,
            finished_at: None,
            diagnosis: None,
        }
    }

    /// Fold one progress update into the displayed state.
    pub fn apply_progress(&mut self, update: &crate::ffmpeg::progress::ProgressUpdate) {
        if update.frame.is_some() {
            self.frame = update.frame;
        }
        if update.fps.is_some() {
            self.fps = update.fps;
        }
        if update.bitrate_kbps.is_some() {
            self.bitrate_kbps = update.bitrate_kbps;
        }
        if update.total_size_bytes.is_some() {
            self.total_size = update.total_size_bytes;
        }
        if update.out_time.is_some() {
            self.out_time = update.out_time;
        }
        if let Some(dup) = update.dup_frames {
            self.dup_frames = dup;
        }
        if let Some(drop) = update.drop_frames {
            self.drop_frames = drop;
        }
        self.last_speed = update.speed;
        self.smoother.push(update.speed);
    }

    /// Push one stderr line into the live ring.
    pub fn push_stderr(&mut self, line: String) {
        push_log(&mut self.stderr, line);
        self.log_scroll = 0;
    }

    /// Fold the final result: compute the diagnosis for genuine failures
    /// and pick the next phase (delete prompt when a partial file exists).
    /// Success stamps `finished_at` for the auto-return pause (§2).
    pub fn apply_finished(&mut self, result: JobResult) {
        if !result.success && !result.cancelled {
            let joined = result.stderr.join("\n");
            self.diagnosis = diagnose(&joined);
        }
        if result.success {
            self.finished_at = Some(Instant::now());
        }
        self.result = Some(result);
        if self.output.exists() && !self.succeeded() {
            self.phase = RunPhase::ConfirmDelete;
        } else {
            self.phase = RunPhase::Done;
        }
    }

    /// True when the job exited 0 without cancellation.
    pub fn succeeded(&self) -> bool {
        self.result.as_ref().is_some_and(|r| r.success)
    }

    /// Progress ratio, or `None` for the indeterminate spinner.
    pub fn ratio(&self) -> Option<f64> {
        match (self.out_time, self.total_duration) {
            (Some(out), Some(total)) => progress_ratio(out, total),
            _ => None,
        }
    }

    /// ETA seconds from smoothed speed, or `None` when unknowable.
    pub fn eta(&self) -> Option<f64> {
        match (self.out_time, self.total_duration, self.smoother.smoothed()) {
            (Some(out), Some(total), Some(speed)) => eta_seconds(out, total, speed),
            _ => None,
        }
    }

    /// Full error report for the clipboard: command + complete stderr.
    pub fn error_report(&self) -> String {
        let mut report = format!("$ {}\n\n", self.display_command);
        match self.result.as_ref() {
            Some(result) => {
                report.push_str(&result.stderr.join("\n"));
                report.push('\n');
            }
            None => {
                for line in &self.stderr {
                    report.push_str(line);
                    report.push('\n');
                }
            }
        }
        report
    }
}

/// What a running-screen keypress means to the app.
#[derive(Debug)]
pub enum RunAction {
    /// Nothing further needed.
    None,
    /// Leave the running screen (back to the form).
    Back,
    /// User confirmed overwrite — spawn with this request.
    Spawn,
    /// Delete the partial output file, then settle into Done.
    DeletePartial,
    /// Copy the error report (command + stderr) to the clipboard.
    CopyReport,
}

/// Handle one keypress. Cancellation itself arrives globally (Ctrl+C in
/// [`App`](crate::app::App)) because it must work from anywhere on this
/// screen; keys here cover prompts, the log pane, and exit.
pub fn on_key(state: &mut RunState, key: KeyEvent) -> RunAction {
    match state.phase {
        RunPhase::ConfirmOverwrite => match key.code {
            // Enter confirms: it is the universal "go" key everywhere else
            // in ffkit, and ignoring it here reads as a freeze on re-runs.
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => RunAction::Spawn,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => RunAction::Back,
            _ => RunAction::None,
        },
        RunPhase::ConfirmDelete => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => RunAction::DeletePartial,
            // Enter keeps the file (the safe default); only an explicit y deletes.
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Enter => {
                state.phase = RunPhase::Done;
                RunAction::None
            }
            _ => RunAction::None,
        },
        RunPhase::Active | RunPhase::Cancelling => match key.code {
            KeyCode::Char('l') => {
                state.log_expanded = !state.log_expanded;
                state.log_scroll = 0;
                RunAction::None
            }
            KeyCode::Up => {
                state.log_scroll = state.log_scroll.saturating_add(1);
                RunAction::None
            }
            KeyCode::Down => {
                state.log_scroll = state.log_scroll.saturating_sub(1);
                RunAction::None
            }
            KeyCode::Char('c') => RunAction::CopyReport,
            _ => RunAction::None,
        },
        RunPhase::Done => match key.code {
            KeyCode::Enter | KeyCode::Esc => RunAction::Back,
            KeyCode::Char('c') => RunAction::CopyReport,
            KeyCode::Char('l') => {
                state.log_expanded = !state.log_expanded;
                state.log_scroll = 0;
                RunAction::None
            }
            _ => RunAction::None,
        },
    }
}

/// Render the running screen: header, progress + stats, log pane, footer.
pub fn render(frame: &mut Frame, app: &crate::app::App, theme: &Theme) {
    let Some(state) = app.run.as_ref() else {
        return;
    };
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(8),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .split(area);

    render_header(frame, chunks[0], state, theme);
    match state.phase {
        RunPhase::ConfirmOverwrite => render_overwrite(frame, chunks[1], state, theme),
        RunPhase::ConfirmDelete => render_delete(frame, chunks[1], state, theme),
        RunPhase::Active | RunPhase::Cancelling | RunPhase::Done => {
            render_progress_block(frame, chunks[1], state, app.spinner, theme);
        }
    }
    render_log(
        frame,
        chunks[2],
        &state.stderr,
        state.log_expanded,
        state.log_scroll.min(state.stderr.len()),
        theme,
    );

    let hints = match state.phase {
        RunPhase::ConfirmOverwrite => "Output exists — y/Enter overwrite · n/Esc back",
        RunPhase::ConfirmDelete => "Delete the partial file? y delete · n/Enter keep",
        RunPhase::Active => "Ctrl+C cancel · l log · c copy command · ↑↓ scroll log",
        RunPhase::Cancelling => "Terminating (SIGTERM)… second Ctrl+C force-kills · l log",
        RunPhase::Done => "Enter/Esc back · c copy error report · l log",
    };
    let footer = Paragraph::new(Line::from(Span::styled(hints, theme.footer())))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, chunks[3]);
}

fn render_header(frame: &mut Frame, area: ratatui::layout::Rect, state: &RunState, theme: &Theme) {
    let phase = match state.phase {
        RunPhase::ConfirmOverwrite => "confirm overwrite",
        RunPhase::Active => "running",
        RunPhase::Cancelling => "cancelling…",
        RunPhase::ConfirmDelete => "partial file",
        RunPhase::Done => {
            if state.succeeded() {
                "done ✓"
            } else if state.result.as_ref().is_some_and(|r| r.cancelled) {
                "cancelled"
            } else {
                "failed"
            }
        }
    };
    let header = Paragraph::new(Line::from(vec![
        Span::styled(format!("{} — {}", state.op_name, phase), theme.title()),
        Span::styled(format!("  {}", state.output.display()), theme.muted_style()),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, area);
}

/// Progress bar + stats line: percentage, elapsed, ETA, speed, size, frames.
fn render_progress_block(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    state: &RunState,
    tick: usize,
    theme: &Theme,
) {
    let rows = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(area);
    render_progress(frame, rows[0], state.ratio(), tick, theme);

    let mut stats: Vec<Span> = Vec::new();
    let elapsed = format_duration(state.started.elapsed());
    stats.push(Span::styled(
        format!("elapsed {elapsed}"),
        theme.muted_style(),
    ));
    // The countdown always renders: ticking down when the estimate exists,
    // an explicit `--:--` while duration or speed is still unknown — never
    // absent (an absent field reads as "only elapsed exists") and never a
    // fake number before the data stabilizes (§12).
    stats.push(Span::raw(" · "));
    match state.eta() {
        Some(eta) => stats.push(Span::styled(
            format!(
                "remaining {}",
                format_duration(Duration::from_secs_f64(eta))
            ),
            theme.title(),
        )),
        None => stats.push(Span::styled("remaining --:--", theme.footer())),
    }
    if let Some(speed) = state.last_speed {
        stats.push(Span::raw(" · "));
        stats.push(Span::styled(format!("{speed:.2}x"), theme.muted_style()));
    }
    if let Some(size) = state.total_size {
        stats.push(Span::raw(" · "));
        stats.push(Span::styled(format_size(size), theme.muted_style()));
    }
    if let Some(frame) = state.frame {
        stats.push(Span::raw(" · "));
        let mut text = format!("frame {frame}");
        if state.drop_frames > 0 {
            text.push_str(&format!(" (drop {})", state.drop_frames));
        }
        stats.push(Span::styled(text, theme.muted_style()));
    }
    if let Some(bitrate) = state.bitrate_kbps {
        stats.push(Span::raw(" · "));
        stats.push(Span::styled(
            format!("{bitrate:.0} kbits/s"),
            theme.muted_style(),
        ));
    }
    let mut lines = vec![Line::from(stats)];

    match state.phase {
        RunPhase::Done if state.succeeded() => {
            lines.push(Line::from(""));
            lines.push(Line::from(success_line(state, theme)));
        }
        RunPhase::Done => {
            lines.push(Line::from(""));
            lines.extend(diagnosis_lines(state, theme));
        }
        RunPhase::Cancelling => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Sent SIGTERM — ffmpeg is flushing a playable file…",
                theme.warning_style(),
            )));
        }
        _ => {}
    }
    let panel = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Status"));
    frame.render_widget(panel, rows[1]);
}

/// Success line with the before/after size comparison.
fn success_line(state: &RunState, theme: &Theme) -> Vec<Span<'static>> {
    let output_size = std::fs::metadata(&state.output).ok().map(|m| m.len());
    let comparison = match (state.input_size, output_size) {
        (Some(before), Some(after)) => {
            format!(" ({} → {})", format_size(before), format_size(after))
        }
        _ => String::new(),
    };
    vec![
        Span::styled("✓ Completed successfully", theme.command_style()),
        Span::styled(
            format!(
                " in {}{comparison}",
                format_duration(state.started.elapsed())
            ),
            theme.muted_style(),
        ),
    ]
}

/// Failure card: translated diagnosis + last-20 tail pointer.
fn diagnosis_lines(state: &RunState, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match state.diagnosis.as_ref() {
        Some(diagnosis) => {
            lines.push(Line::from(Span::styled(
                diagnosis.title.clone(),
                theme.warning_style(),
            )));
            lines.push(Line::from(Span::styled(
                diagnosis.detail.clone(),
                theme.muted_style(),
            )));
            if let Some(suggestion) = diagnosis.suggestion.as_ref() {
                lines.push(Line::from(vec![
                    Span::styled("Try: ", theme.title()),
                    Span::styled(suggestion.clone(), theme.muted_style()),
                ]));
            }
        }
        None => {
            lines.push(Line::from(Span::styled(
                "Failed with unrecognized output — the raw log below is the full story.",
                theme.warning_style(),
            )));
        }
    }
    lines.push(Line::from(Span::styled(
        "c copies the full error report (command + stderr) for search or filing.",
        theme.footer(),
    )));
    lines
}

fn render_overwrite(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    state: &RunState,
    theme: &Theme,
) {
    let body = Paragraph::new(vec![
        Line::from(Span::styled(
            format!("{} already exists.", state.output.display()),
            theme.warning_style(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "ffkit asks here so ffmpeg never blocks on its own prompt. Overwriting passes -y.",
            theme.muted_style(),
        )),
    ])
    .block(Block::default().borders(Borders::ALL).title("Overwrite?"));
    frame.render_widget(body, area);
}

fn render_delete(frame: &mut Frame, area: ratatui::layout::Rect, state: &RunState, theme: &Theme) {
    let body = Paragraph::new(vec![
        Line::from(Span::styled(
            format!("Partial output remains at {}.", state.output.display()),
            theme.warning_style(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Cancelled/failed jobs often leave unplayable fragments.",
            theme.muted_style(),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Delete partial file?"),
    );
    frame.render_widget(body, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn confirming_state() -> RunState {
        let spec = CommandSpec::new("ffmpeg");
        RunState::confirming(
            "Convert".into(),
            &spec,
            PathBuf::from("out.mp4"),
            None,
            None,
        )
    }

    #[test]
    fn enter_confirms_overwrite_like_y() {
        let mut state = confirming_state();
        assert!(matches!(
            on_key(&mut state, key(KeyCode::Enter)),
            RunAction::Spawn
        ));
        let mut state = confirming_state();
        assert!(matches!(
            on_key(&mut state, key(KeyCode::Char('y'))),
            RunAction::Spawn
        ));
    }

    #[test]
    fn enter_on_delete_prompt_keeps_the_file() {
        let mut state = confirming_state();
        state.phase = RunPhase::ConfirmDelete;
        on_key(&mut state, key(KeyCode::Enter));
        assert_eq!(state.phase, RunPhase::Done);
    }

    /// §12: the countdown renders ticking down next to elapsed…
    #[test]
    fn remaining_counts_down_alongside_elapsed() {
        use crate::ffmpeg::progress::ProgressUpdate;
        use std::collections::HashMap;

        let channel = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(channel.0, channel.1, Default::default());
        let spec = crate::ffmpeg::builder::CommandSpec::new("ffmpeg");
        let mut run = RunState::starting(
            "Convert".into(),
            &spec,
            PathBuf::from("out.mp4"),
            Some(Duration::from_secs(80)),
            None,
        );
        run.apply_progress(&ProgressUpdate {
            out_time: Some(Duration::from_secs(20)),
            speed: Some(2.0),
            frame: None,
            fps: None,
            bitrate_kbps: None,
            total_size_bytes: None,
            dup_frames: None,
            drop_frames: None,
            stream_q: HashMap::new(),
            finished: false,
        });
        app.run = Some(run);
        app.screen = crate::app::Screen::Running;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| app.render(f)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(content.contains("elapsed"), "{content}");
        // (80 - 20) / 2 = 30s remaining.
        assert!(content.contains("remaining 0:30"), "{content}");
    }

    /// §12: …and shows an explicit indeterminate marker before the data
    /// exists instead of omitting the field.
    #[test]
    fn remaining_is_indeterminate_without_data() {
        let channel = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(channel.0, channel.1, Default::default());
        let spec = crate::ffmpeg::builder::CommandSpec::new("ffmpeg");
        let run = RunState::starting(
            "Convert".into(),
            &spec,
            PathBuf::from("out.mp4"),
            None,
            None,
        );
        app.run = Some(run);
        app.screen = crate::app::Screen::Running;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| app.render(f)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(content.contains("remaining --:--"), "{content}");
    }
}
