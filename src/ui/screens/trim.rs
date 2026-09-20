//! Trim timeline screen: filmstrip, scrubbable multi-clip track (§10).
//!
//! A filmstrip, not playback: thumbnails extracted once per input render
//! along the track as the visual backdrop (Kitty/iTerm2 inline images, or
//! a colored-block approximation, or timestamp ticks). Handles move by
//! keyboard — `Tab` cycles handles (spec), `f` jumps to the fields zone,
//! `←→`/`h`/`l` move coarsely, `Shift`/`H`/`L` nudge finely, `n` adds a
//! clip where free space exists, `d` removes, `Enter` confirms.

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::ffmpeg::builder::CommandSpec;
use crate::ffmpeg::probe::format_duration;
use crate::ops::fields::{ClipRange, Field, FieldKind};
use crate::ui::images::{
    block_line, blocks_for, iterm2_payload, kitty_payload, ImageBackend, THUMB_COLS, THUMB_ROWS,
};
use crate::ui::theme::Theme;

use super::parameter_form::{field_lines, handle_text_edit, render_spec_preview};

/// Minimum clip length in seconds: shorter ranges are not seekable cuts,
/// just slivers that confuse the concat step.
pub const MIN_CLIP_LEN: f64 = 0.5;

/// Which pane owns the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimZone {
    /// The track: handles move here.
    Timeline,
    /// Mode/codec/crf/output fields.
    Fields,
}

/// Live trim-timeline state. Owned by [`App`](crate::app::App).
pub struct TrimState {
    /// Input being trimmed.
    pub input: PathBuf,
    /// Full duration in seconds; `None` until the probe lands.
    pub duration: Option<f64>,
    /// Source fps for frame-sized fine steps; `None` → 0.5s fallback.
    pub fps: Option<f64>,
    /// Clips in timeline order (output follows position, not creation).
    pub clips: Vec<ClipRange>,
    /// Focused handle index into `clips * 2` (even = start, odd = end).
    pub handle: usize,
    /// Active zone.
    pub zone: TrimZone,
    /// Focused field when the zone is Fields.
    pub field_focus: usize,
    /// Non-clip fields: mode, video codec, CRF, output.
    pub fields: Vec<Field>,
    /// A text field is being edited.
    pub editing: bool,
    /// Output field just entered (select-all behavior).
    pub edit_select_all: bool,
    /// Latest successfully built spec (preview + run source).
    pub preview: Option<CommandSpec>,
    /// Build failure text.
    pub build_error: Option<String>,
    /// Changed token + when, for the ~400ms highlight.
    pub changed: Option<(String, Instant)>,
    /// Scroll offset into the wrapped command.
    pub cmd_scroll: usize,
    /// Extracted thumbnails in timeline order.
    pub filmstrip: Vec<PathBuf>,
    /// Extraction running (spinner note instead of ticks).
    pub strip_pending: bool,
    /// Extraction failure reason (ticks fallback note).
    pub strip_error: Option<String>,
    /// Decoded block grids, computed once when frames arrive (Blocks mode).
    pub strip_blocks: Vec<Vec<Vec<ratatui::style::Color>>>,
    /// Cached inline-image payloads (Kitty/iTerm2), computed on arrival.
    pub strip_payloads: Vec<String>,
    /// Re-emit payloads after the next draw (enter/frames/resize).
    pub strip_dirty: bool,
    /// Strip rows currently painted by payloads (for cleanup on exit).
    pub strip_area: Option<Rect>,
}

impl TrimState {
    /// Fresh timeline: one full-duration clip when the duration is known,
    /// empty (with a probing note) otherwise.
    pub fn new(
        input: PathBuf,
        duration: Option<f64>,
        fps: Option<f64>,
        fields: Vec<Field>,
    ) -> Self {
        let clips = duration
            .filter(|d| *d > 0.0)
            .map(|d| vec![ClipRange { start: 0.0, end: d }])
            .unwrap_or_default();
        Self {
            input,
            duration,
            fps,
            clips,
            handle: 0,
            zone: TrimZone::Timeline,
            field_focus: 0,
            fields,
            editing: false,
            edit_select_all: false,
            preview: None,
            build_error: None,
            changed: None,
            cmd_scroll: 0,
            filmstrip: Vec::new(),
            strip_pending: false,
            strip_error: None,
            strip_blocks: Vec::new(),
            strip_payloads: Vec::new(),
            strip_dirty: true,
            strip_area: None,
        }
    }

    /// Coarse handle step: 1% of duration, at least a second.
    pub fn coarse_step(&self) -> f64 {
        self.duration.map(|d| (d / 100.0).max(1.0)).unwrap_or(1.0)
    }

    /// Fine handle step: one frame when fps is known, else half a second.
    pub fn fine_step(&self) -> f64 {
        self.fps
            .filter(|f| *f > 0.0)
            .map(|f| 1.0 / f)
            .unwrap_or(0.5)
    }

    /// Number of handles (two per clip).
    pub fn handle_count(&self) -> usize {
        self.clips.len() * 2
    }

    /// Decode blocks + payloads once when frames arrive (not per frame).
    pub fn set_filmstrip(&mut self, frames: Vec<PathBuf>, backend: ImageBackend) {
        self.filmstrip = frames;
        self.strip_pending = false;
        self.strip_error = None;
        match backend {
            ImageBackend::Blocks => {
                self.strip_blocks = self.filmstrip.iter().map(|p| blocks_for(p)).collect();
                self.strip_payloads.clear();
            }
            ImageBackend::Kitty => {
                self.strip_payloads = self
                    .filmstrip
                    .iter()
                    .filter_map(|p| std::fs::read(p).ok())
                    .map(|bytes| kitty_payload(&bytes, THUMB_COLS as u32, THUMB_ROWS as u32))
                    .collect();
                self.strip_blocks.clear();
            }
            ImageBackend::ITerm2 => {
                self.strip_payloads = self
                    .filmstrip
                    .iter()
                    .filter_map(|p| std::fs::read(p).ok())
                    .map(|bytes| iterm2_payload(&bytes, THUMB_COLS as u32))
                    .collect();
                self.strip_blocks.clear();
            }
        }
        self.strip_dirty = true;
    }

    /// Move the focused handle by `delta` seconds, clamped to the duration,
    /// the minimum length, and neighbors (overlaps are rejected, never
    /// emitted as invalid ranges).
    pub fn move_handle(&mut self, delta: f64) {
        let Some(duration) = self.duration else {
            return;
        };
        let index = self.handle / 2;
        if index >= self.clips.len() {
            return;
        }
        if self.handle.is_multiple_of(2) {
            let prev_end = if index >= 1 {
                self.clips[index - 1].end
            } else {
                0.0
            };
            let end = self.clips[index].end;
            let start = (self.clips[index].start + delta)
                .clamp(prev_end, (end - MIN_CLIP_LEN).max(prev_end))
                .clamp(0.0, duration);
            self.clips[index].start = start;
        } else {
            let next_start = self
                .clips
                .get(index + 1)
                .map(|c| c.start)
                .unwrap_or(duration);
            let start = self.clips[index].start;
            let end = (self.clips[index].end + delta)
                .clamp((start + MIN_CLIP_LEN).max(0.0), next_start)
                .clamp(0.0, duration.max(start + MIN_CLIP_LEN));
            self.clips[index].end = end;
        }
        self.sort_clips();
    }

    /// Keep clips sorted (output follows timeline position).
    fn sort_clips(&mut self) {
        self.clips.sort_by(|a, b| {
            a.start
                .partial_cmp(&b.start)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    /// Add a clip in the first free gap of at least a second, centered on
    /// half the gap. Returns false when there is no free space.
    pub fn new_clip(&mut self) -> bool {
        let Some(duration) = self.duration else {
            return false;
        };
        let mut bounds = vec![0.0];
        for clip in &self.clips {
            bounds.push(clip.start);
            bounds.push(clip.end);
        }
        bounds.push(duration);
        bounds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for gap in bounds.chunks(2) {
            let (from, to) = (gap[0], gap[1]);
            if to - from >= MIN_CLIP_LEN * 2.0 {
                let len = ((to - from) / 2.0).max(MIN_CLIP_LEN);
                self.clips.push(ClipRange {
                    start: from,
                    end: (from + len).min(to),
                });
                self.sort_clips();
                // Focus the new clip's start handle.
                if let Some(index) = self
                    .clips
                    .iter()
                    .position(|c| (c.start - from).abs() < f64::EPSILON)
                {
                    self.handle = index * 2;
                }
                return true;
            }
        }
        false
    }

    /// Remove the focused clip. Returns false when there is nothing to remove.
    pub fn remove_clip(&mut self) -> bool {
        if self.clips.is_empty() {
            return false;
        }
        let index = (self.handle / 2).min(self.clips.len() - 1);
        self.clips.remove(index);
        self.handle = self.handle.min(self.handle_count().saturating_sub(1));
        true
    }

    /// Total kept duration across clips.
    pub fn total_kept(&self) -> f64 {
        self.clips.iter().map(ClipRange::len).sum()
    }
}

/// What a trim keypress means to the app.
#[derive(Debug, PartialEq, Eq)]
pub enum TrimAction {
    /// Nothing further needed.
    None,
    /// Leave the timeline (back to the file browser).
    Back,
    /// Clips or fields changed — the preview was dirtied.
    Changed,
    /// Confirm and build the command (run flow).
    Run,
    /// Show a status line (e.g. no free space for a new clip).
    Status(String),
}

/// Handle one keypress on the trim screen.
pub fn on_key(state: &mut TrimState, key: KeyEvent) -> TrimAction {
    if state.editing {
        return on_key_editing(state, key);
    }
    match state.zone {
        TrimZone::Timeline => on_key_timeline(state, key),
        TrimZone::Fields => on_key_fields(state, key),
    }
}

fn on_key_timeline(state: &mut TrimState, key: KeyEvent) -> TrimAction {
    let fine = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Esc => TrimAction::Back,
        KeyCode::Tab => {
            let count = state.handle_count();
            if count > 0 {
                state.handle = (state.handle + 1) % count;
            }
            TrimAction::None
        }
        KeyCode::BackTab => {
            let count = state.handle_count();
            if count > 0 {
                state.handle = (state.handle + count - 1) % count;
            }
            TrimAction::None
        }
        KeyCode::Char('f') => {
            state.zone = TrimZone::Fields;
            TrimAction::None
        }
        KeyCode::Char('n') => {
            if state.new_clip() {
                TrimAction::Changed
            } else {
                TrimAction::Status("No free space — shorten a clip first.".to_string())
            }
        }
        KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => {
            if state.remove_clip() {
                TrimAction::Changed
            } else {
                TrimAction::Status("No clips to remove.".to_string())
            }
        }
        KeyCode::Left | KeyCode::Char('h') => {
            let step = if fine {
                state.fine_step()
            } else {
                state.coarse_step()
            };
            state.move_handle(-step);
            TrimAction::Changed
        }
        KeyCode::Right | KeyCode::Char('l') => {
            let step = if fine {
                state.fine_step()
            } else {
                state.coarse_step()
            };
            state.move_handle(step);
            TrimAction::Changed
        }
        KeyCode::Char('H') => {
            state.move_handle(-state.fine_step());
            TrimAction::Changed
        }
        KeyCode::Char('L') => {
            state.move_handle(state.fine_step());
            TrimAction::Changed
        }
        KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
            // Vertical motion is reserved: k/j do nothing on the track so
            // they cannot nudge handles by accident.
            TrimAction::None
        }
        KeyCode::Enter => TrimAction::Run,
        _ => TrimAction::None,
    }
}

fn on_key_fields(state: &mut TrimState, key: KeyEvent) -> TrimAction {
    match key.code {
        KeyCode::Esc | KeyCode::Char('f') => {
            state.zone = TrimZone::Timeline;
            TrimAction::None
        }
        KeyCode::Tab => {
            state.zone = TrimZone::Timeline;
            let count = state.handle_count();
            if count > 0 {
                state.handle = (state.handle + 1) % count;
            }
            TrimAction::None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.field_focus = state.field_focus.saturating_sub(1);
            TrimAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.field_focus = state
                .field_focus
                .saturating_add(1)
                .min(state.fields.len().saturating_sub(1));
            TrimAction::None
        }
        KeyCode::Left | KeyCode::Char('h') => {
            if let Some(field) = state.fields.get_mut(state.field_focus) {
                field.adjust_backward();
                return TrimAction::Changed;
            }
            TrimAction::None
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if let Some(field) = state.fields.get_mut(state.field_focus) {
                field.adjust_forward();
                return TrimAction::Changed;
            }
            TrimAction::None
        }
        KeyCode::Enter => {
            if let Some(field) = state.fields.get(state.field_focus) {
                if matches!(field.kind, FieldKind::Text { .. }) {
                    state.editing = true;
                    state.edit_select_all = field.id == "output";
                    return TrimAction::None;
                }
            }
            TrimAction::Run
        }
        _ => TrimAction::None,
    }
}

/// Text editing inside trim fields (same select-all contract as the form).
fn on_key_editing(state: &mut TrimState, key: KeyEvent) -> TrimAction {
    let Some(field) = state.fields.get_mut(state.field_focus) else {
        state.editing = false;
        state.edit_select_all = false;
        return TrimAction::None;
    };
    if handle_text_edit(field, &mut state.editing, &mut state.edit_select_all, key) {
        TrimAction::Changed
    } else {
        TrimAction::None
    }
}

/// Render the trim screen: header, filmstrip, ruler + track, clips/fields,
/// live command preview, footer. Takes `&mut App`: overlay payloads and
/// dirty flags are recorded during render (inline images bypass ratatui —
/// see the images module docs).
pub fn render(frame: &mut Frame, app: &mut crate::app::App, theme: &Theme) {
    let backend = app.image_backend;
    let Some(state) = app.trim.as_mut() else {
        return;
    };
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(4),
        Constraint::Length(4),
        Constraint::Length(5),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .split(area);

    render_header(frame, chunks[0], state, theme);
    render_strip(frame, chunks[1], state, backend, theme);
    render_track(frame, chunks[2], state, theme);
    render_mid(frame, chunks[3], state, theme);
    let highlight = state
        .changed
        .as_ref()
        .filter(|(_, at)| at.elapsed().as_millis() < 400)
        .map(|(token, _)| token.as_str());
    let explanation = focused_explanation(state);
    render_spec_preview(
        frame,
        chunks[4],
        "Command — [c] copy",
        state.preview.as_ref(),
        state.build_error.as_deref(),
        highlight,
        false,
        state.cmd_scroll,
        explanation,
        theme,
    );

    let hints = match state.zone {
        TrimZone::Timeline if state.editing => "typing… Enter done · Esc cancel",
        TrimZone::Timeline => {
            "Tab handle · ←→/hl move · ⇧←→/HL fine · n clip · d delete · f fields · Enter run · esc back"
        }
        TrimZone::Fields if state.editing => "typing… Enter done · Esc cancel",
        TrimZone::Fields => "↑↓ field · ←→ adjust · Enter edit/run · Tab timeline · esc back",
    };
    let footer = Paragraph::new(Line::from(Span::styled(hints, theme.footer())))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, chunks[5]);

    if state.editing {
        if let Some(cursor) = edit_cursor(state, chunks[3]) {
            frame.set_cursor_position(cursor);
        }
    }

    // Inline-image overlay (Kitty/iTerm2 only): payloads print post-draw
    // from the main loop. Emission happens only when dirty (screen enter,
    // new frames, resize) — quiescent frames re-emit nothing and the image
    // persists, because ratatui's diff leaves untouched rows alone.
    //
    // Borrow discipline: `state` borrows `app.trim`; the push below touches
    // only `app.pending_images` / `app.trim_images_shown` (disjoint fields),
    // and NLL ends the `state` borrow at its last use above this point.
    // Payloads are cloned because dirty draws are rare (enter/frames/resize).
    let emit =
        backend != ImageBackend::Blocks && state.strip_dirty && !state.strip_payloads.is_empty();
    let payloads: Vec<String> = if emit {
        let inner = strip_inner(chunks[1]);
        let count = strip_count(inner.width as usize, state.strip_payloads.len());
        positioned_payloads(&state.strip_payloads, count, inner)
    } else {
        Vec::new()
    };
    if emit {
        state.strip_area = Some(strip_inner(chunks[1]));
        state.strip_dirty = false;
        app.pending_images.extend(payloads);
        app.trim_images_shown = true;
    }
}

fn render_header(frame: &mut Frame, area: Rect, state: &TrimState, theme: &Theme) {
    let duration = state
        .duration
        .map(|d| format_duration(std::time::Duration::from_secs_f64(d)))
        .unwrap_or_else(|| "probing…".to_string());
    let title = Paragraph::new(Line::from(vec![
        Span::styled("Trim", theme.title()),
        Span::styled(
            format!(
                " — {} · {} · {} clip(s), {} kept",
                state.input.display(),
                duration,
                state.clips.len(),
                format_duration(std::time::Duration::from_secs_f64(state.total_kept()))
            ),
            theme.muted_style(),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, area);
}

/// Filmstrip pane. Blocks and ticks render natively; Kitty/iTerm2 frames
/// render as blank rows here and the real pixels overlay post-draw (see
/// `render` — payloads are positioned over exactly these rows).
fn render_strip(
    frame: &mut Frame,
    area: Rect,
    state: &TrimState,
    backend: ImageBackend,
    theme: &Theme,
) {
    let inner_width = (area.width.saturating_sub(2)) as usize;
    let mut lines: Vec<Line> = Vec::new();
    if backend == ImageBackend::Blocks && !state.filmstrip.is_empty() {
        let count = strip_count(inner_width, state.filmstrip.len());
        let shown = sample_every(state.filmstrip.len(), count);
        let thumbs: Vec<Vec<Vec<ratatui::style::Color>>> = shown
            .iter()
            .map(|i| {
                state.strip_blocks.get(*i).cloned().unwrap_or_else(|| {
                    vec![vec![ratatui::style::Color::Reset; THUMB_COLS]; THUMB_ROWS]
                })
            })
            .collect();
        for row in 0..THUMB_ROWS {
            lines.push(Line::from(block_line(&thumbs, row)));
        }
    } else if backend != ImageBackend::Blocks && !state.filmstrip.is_empty() {
        // Blank rows reserve the overlay region (images paint post-draw).
        for _ in 0..THUMB_ROWS {
            lines.push(Line::from(""));
        }
    } else if state.strip_pending {
        lines.push(Line::from(Span::styled(
            "Extracting thumbnails…",
            theme.footer(),
        )));
    } else {
        // Ticks fallback: evenly spaced marks with or without frames.
        lines.push(Line::from(Span::styled(
            tick_row(inner_width, state.filmstrip.len().max(8)),
            theme.muted_style(),
        )));
        if let Some(reason) = state.strip_error.as_ref() {
            lines.push(Line::from(Span::styled(
                format!("({reason} — showing ticks)"),
                theme.footer(),
            )));
        }
    }
    let strip = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Strip"));
    frame.render_widget(strip, area);
}

/// Inner rect of a bordered pane (content rows for overlays/cursors).
fn strip_inner(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

/// Cursor-addressed payloads for the sampled thumbnails: each thumb prints
/// at its slot column on the strip's first row (the image fills its own
/// cell rows from there).
fn positioned_payloads(payloads: &[String], count: usize, inner: Rect) -> Vec<String> {
    sample_every(payloads.len(), count)
        .into_iter()
        .filter_map(|i| payloads.get(i))
        .enumerate()
        .map(|(slot, payload)| {
            let col = inner.x + (slot as u16) * (THUMB_COLS as u16 + 1);
            format!("\x1b[{};{}H{payload}", inner.y + 1, col + 1)
        })
        .collect()
}

/// Cursor-addressed clear sequence for a strip rect: spaces over the rows
/// inline images painted. Emitted when leaving the trim screen so remnants
/// never linger (ratatui cannot clear what it never buffered).
pub(crate) fn clear_payload(area: Rect) -> String {
    let width = area.width as usize;
    let mut out = String::new();
    for row in 0..area.height {
        out.push_str(&format!(
            "\x1b[{};{}H{}",
            area.y + row + 1,
            area.x + 1,
            " ".repeat(width)
        ));
    }
    out
}

/// How many thumbnails fit: one slot per thumbnail plus a gap column.
fn strip_count(inner_width: usize, available: usize) -> usize {
    (inner_width / (THUMB_COLS + 1))
        .max(1)
        .min(available.max(1))
}

/// Evenly spaced indices into `total` thumbnails, taking `count`.
fn sample_every(total: usize, count: usize) -> Vec<usize> {
    if total == 0 || count == 0 {
        return Vec::new();
    }
    (0..count).map(|i| i * total / count).collect()
}

/// Timestamp tick row for the no-imagery fallback.
fn tick_row(width: usize, ticks: usize) -> String {
    let mut row = vec!['·'; width];
    for i in 0..ticks.max(1) {
        let col = i * width.saturating_sub(1) / ticks.max(1).saturating_sub(1).max(1);
        row[col.min(width.saturating_sub(1))] = '│';
    }
    row.into_iter().collect()
}

/// Ruler + track pane: time labels over a single track where kept ranges
/// render filled and the focused handle blinks as a solid block.
fn render_track(frame: &mut Frame, area: Rect, state: &TrimState, theme: &Theme) {
    let inner_width = (area.width.saturating_sub(2)) as usize;
    let mut lines = vec![Line::from(Span::styled(
        ruler_line(inner_width, state.duration),
        theme.muted_style(),
    ))];
    lines.push(Line::from(track_spans(
        inner_width,
        state.duration,
        &state.clips,
        state.handle,
        state.zone == TrimZone::Timeline,
        theme,
    )));
    let track =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Timeline"));
    frame.render_widget(track, area);
}

/// Time ruler: start, middle, and end labels spread across the width.
fn ruler_line(width: usize, duration: Option<f64>) -> String {
    let Some(duration) = duration.filter(|d| *d > 0.0) else {
        return "—:—".to_string();
    };
    let labels = [
        format_timestamp_short(0.0),
        format_timestamp_short(duration / 2.0),
        format_timestamp_short(duration),
    ];
    let mut row = vec![' '; width];
    let positions = [0, width / 2, width];
    for (label, pos) in labels.iter().zip(positions) {
        let start = pos
            .saturating_sub(label.len() / 2)
            .min(width.saturating_sub(label.len()));
        for (i, c) in label.chars().enumerate() {
            if start + i < width {
                row[start + i] = c;
            }
        }
    }
    row.into_iter().collect()
}

/// Short `M:SS` timestamp for ruler and clip rows.
fn format_timestamp_short(secs: f64) -> String {
    format_duration(std::time::Duration::from_secs_f64(secs.max(0.0)))
}

/// Track spans: `░` outside clips, `▓` inside, `█` on the focused handle.
fn track_spans(
    width: usize,
    duration: Option<f64>,
    clips: &[ClipRange],
    handle: usize,
    handles_live: bool,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let Some(duration) = duration.filter(|d| *d > 0.0) else {
        return vec![Span::styled("duration unknown".to_string(), theme.footer())];
    };
    let col_of = |t: f64| ((t / duration) * width as f64).round() as usize;
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut current_filled = false;
    let mut current_focus = false;
    let flush = |spans: &mut Vec<Span<'static>>, text: &mut String, filled: bool, focus: bool| {
        if text.is_empty() {
            return;
        }
        let style = if focus {
            theme.selected()
        } else if filled {
            theme.command_style()
        } else {
            theme.muted_style()
        };
        spans.push(Span::styled(std::mem::take(text), style));
    };
    for col in 0..width {
        let t = col as f64 / width as f64 * duration;
        let filled = clips.iter().any(|c| c.start <= t && t < c.end);
        let focus = handles_live
            && clips.iter().enumerate().any(|(i, c)| {
                (col_of(c.start) == col && handle == i * 2)
                    || (col_of(c.end) == col && handle == i * 2 + 1)
            });
        let ch = if focus {
            '█'
        } else if filled {
            '▓'
        } else {
            '░'
        };
        if filled != current_filled || focus != current_focus {
            let (f1, f2) = (current_filled, current_focus);
            flush(&mut spans, &mut current, f1, f2);
            current_filled = filled;
            current_focus = focus;
        }
        current.push(ch);
    }
    let (f1, f2) = (current_filled, current_focus);
    flush(&mut spans, &mut current, f1, f2);
    spans
}

/// Clips list or fields, depending on the zone.
fn render_mid(frame: &mut Frame, area: Rect, state: &TrimState, theme: &Theme) {
    let mut lines: Vec<Line> = Vec::new();
    match state.zone {
        TrimZone::Timeline => {
            if state.clips.is_empty() {
                lines.push(Line::from(Span::styled(
                    "No clips — press n to create one.",
                    theme.warning_style(),
                )));
            }
            for (i, clip) in state.clips.iter().enumerate().take(3) {
                let focused = state.handle / 2 == i;
                let marker = if focused { "▸ " } else { "  " };
                lines.push(Line::from(vec![Span::styled(
                    format!(
                        "{marker}{}. {}–{} ({} kept)",
                        i + 1,
                        format_timestamp_short(clip.start),
                        format_timestamp_short(clip.end),
                        format_duration(std::time::Duration::from_secs_f64(clip.len()))
                    ),
                    if focused {
                        theme.selected()
                    } else {
                        theme.muted_style()
                    },
                )]));
            }
            if state.clips.len() > 3 {
                lines.push(Line::from(Span::styled(
                    format!("+ {} more", state.clips.len() - 3),
                    theme.footer(),
                )));
            }
        }
        TrimZone::Fields => {
            // Four fields, three content rows: window around the focus
            // (the ▸ marker shows position; no indicator line needed).
            let start = state
                .field_focus
                .saturating_sub(1)
                .min(state.fields.len().saturating_sub(3));
            for (offset, field) in state.fields.iter().skip(start).take(3).enumerate() {
                let focused = start + offset == state.field_focus;
                let select_all = focused && state.editing && state.edit_select_all;
                lines.extend(field_lines(field, focused, select_all, theme));
            }
        }
    }
    let mid = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Clips / fields (f)"),
    );
    frame.render_widget(mid, area);
}

/// Focused control's explanation for the preview pane.
fn focused_explanation(state: &TrimState) -> Option<(&str, &str)> {
    match state.zone {
        TrimZone::Timeline => Some((
            "Timeline",
            "One track, full duration. Tab cycles handles, arrows move the focused one (Shift for fine steps), n adds a clip where space is free, d removes. Output follows timeline position.",
        )),
        TrimZone::Fields => state.fields.get(state.field_focus).map(|field| {
            (
                field.label.as_str(),
                field.explanation.as_str(),
            )
        }),
    }
}

/// Cursor position for trim text editing. Mirrors the render window.
fn edit_cursor(state: &TrimState, area: Rect) -> Option<(u16, u16)> {
    let field = state.fields.get(state.field_focus)?;
    let FieldKind::Text { value } = &field.kind else {
        return None;
    };
    let start = state
        .field_focus
        .saturating_sub(1)
        .min(state.fields.len().saturating_sub(3));
    let cursor = (value.visual_cursor() as u16).min(40);
    let x = area.x + 1 + 2 + field.label.len() as u16 + 2 + 1 + cursor;
    let y = area.y + 1 + (state.field_focus.saturating_sub(start)) as u16;
    if x >= area.x + area.width || y >= area.y + area.height {
        return None;
    }
    Some((x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_clips(clips: Vec<(f64, f64)>) -> TrimState {
        let mut state =
            TrimState::new(PathBuf::from("in.mp4"), Some(252.0), Some(30.0), Vec::new());
        state.clips = clips
            .into_iter()
            .map(|(start, end)| ClipRange { start, end })
            .collect();
        state
    }

    #[test]
    fn new_clip_fills_the_first_free_gap() {
        let mut state = state_with_clips(vec![(0.0, 10.0)]);
        assert!(state.new_clip());
        // Gap is [10, 252]: centered on half the gap.
        assert_eq!(state.clips.len(), 2);
        assert!((state.clips[1].start - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn new_clip_fails_when_full() {
        let mut state = state_with_clips(vec![(0.0, 252.0)]);
        assert!(!state.new_clip());
    }

    #[test]
    fn handles_clamp_against_neighbors_and_bounds() {
        // Start handle stops at the previous clip's end.
        let mut state = state_with_clips(vec![(10.0, 40.0), (50.0, 80.0)]);
        state.handle = 2;
        state.move_handle(-100.0);
        assert!((state.clips[1].start - 40.0).abs() < f64::EPSILON);
        // End handle stops at the next clip's start.
        let mut state = state_with_clips(vec![(10.0, 40.0), (50.0, 80.0)]);
        state.handle = 1;
        state.move_handle(100.0);
        assert!((state.clips[0].end - 50.0).abs() < f64::EPSILON);
        // First start never goes negative.
        let mut state = state_with_clips(vec![(10.0, 40.0), (50.0, 80.0)]);
        state.handle = 0;
        state.move_handle(-100.0);
        assert!((state.clips[0].start - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn minimum_length_holds_on_shrink() {
        let mut state = state_with_clips(vec![(10.0, 40.0)]);
        state.handle = 0;
        state.move_handle(100.0);
        assert!((state.clips[0].len() - MIN_CLIP_LEN).abs() < 0.01);
    }

    #[test]
    fn ruler_and_ticks_degrade_without_duration() {
        assert_eq!(ruler_line(40, None), "—:—");
        assert_eq!(tick_row(10, 3).chars().count(), 10);
    }

    #[test]
    fn track_marks_kept_ranges_and_focus() {
        let clips = vec![ClipRange {
            start: 0.0,
            end: 50.0,
        }];
        let spans = track_spans(10, Some(100.0), &clips, 0, true, &Theme::dark());
        let text: String = spans.iter().map(|s| s.content.clone()).collect();
        assert!(text.contains('█'), "focused handle renders solid: {text}");
        assert!(text.contains('▓'), "kept range renders filled: {text}");
        assert!(text.contains('░'), "outside renders empty: {text}");
    }
}
