//! Job queue screen (M6).
//!
//! Per-job status rows (pending/running/done/failed/cancelled) with live
//! ratios for active jobs, the aggregate header, the overwrite-conflicts
//! banner for armed batches, and the end-of-run summary. A failed job never
//! halts the queue — the summary counts the damage at the end.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::queue::JobStatus;
use crate::ui::theme::Theme;

/// What a queue-screen keypress means to the app.
#[derive(Debug, PartialEq, Eq)]
pub enum QueueAction {
    /// Nothing further needed.
    None,
    /// Leave the queue (back to where it was opened from).
    Back,
    /// Start an armed batch (overwrite conflicts confirmed).
    StartArmed,
    /// Remove the selected pending job.
    RemoveSelected,
    /// Drop all settled jobs from the list.
    ClearFinished,
}

/// Handle one keypress. Cancellation arrives globally (Ctrl+C in App)
/// because it must work while rows scroll; `?` is handled by the app so
/// help returns here.
pub fn on_key(queue: &mut crate::queue::QueueState, key: KeyEvent) -> QueueAction {
    match key.code {
        KeyCode::Esc => QueueAction::Back,
        KeyCode::Up | KeyCode::Char('k') => {
            queue.selected = queue.selected.saturating_sub(1);
            QueueAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            queue.selected = queue
                .selected
                .saturating_add(1)
                .min(queue.jobs.len().saturating_sub(1));
            QueueAction::None
        }
        KeyCode::Char('x') => QueueAction::RemoveSelected,
        KeyCode::Char('c') => QueueAction::ClearFinished,
        KeyCode::Char('y') | KeyCode::Char('Y') => QueueAction::StartArmed,
        _ => QueueAction::None,
    }
}

/// Render the queue: aggregate header, job rows, banners, footer.
pub fn render(frame: &mut Frame, app: &crate::app::App, theme: &Theme) {
    let queue = &app.queue;
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .split(area);

    let header = Paragraph::new(Line::from(vec![
        Span::styled("Job queue", theme.title()),
        Span::styled(
            format!(" — {}", queue.aggregate_line()),
            theme.muted_style(),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, chunks[0]);

    let mut lines: Vec<Line> = Vec::new();
    if queue.jobs.is_empty() {
        lines.push(Line::from(Span::styled(
            "Empty. Select several files with Space in the browser, tune once, and run — one job per file lands here.",
            theme.muted_style(),
        )));
    }
    for (i, job) in queue.jobs.iter().enumerate() {
        let marker = if i == queue.selected { "▸ " } else { "  " };
        let (glyph, style) = match &job.status {
            JobStatus::Pending => ("○", theme.muted_style()),
            JobStatus::Running => ("●", theme.title()),
            JobStatus::Done => ("✓", theme.command_style()),
            JobStatus::Failed(_) => ("✗", theme.warning_style()),
            JobStatus::Cancelled => ("■", theme.muted_style()),
        };
        let mut spans = vec![
            Span::raw(marker.to_string()),
            Span::styled(format!("{glyph} "), style),
            Span::styled(format!("{}: ", job.op_name), style),
            Span::raw(format!(
                "{} → {}",
                job.input.display(),
                job.output.display()
            )),
            Span::raw("  "),
            Span::styled(job.status_text(), style),
        ];
        if job.status == JobStatus::Running {
            if let Some(speed) = job.speed {
                spans.push(Span::styled(format!(" · {speed:.2}x"), theme.muted_style()));
            }
        }
        lines.push(Line::from(spans));
    }
    if !queue.overwrite_conflicts.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "{} output(s) already exist: {}",
                queue.overwrite_conflicts.len(),
                queue
                    .overwrite_conflicts
                    .iter()
                    .take(3)
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            theme.warning_style(),
        )));
        lines.push(Line::from(Span::styled(
            "y runs the batch and overwrites · Esc goes back",
            theme.footer(),
        )));
    }
    if let Some(summary) = queue.summary.as_ref() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(summary.clone(), theme.title())));
    } else if let Some(status) = app.status_message.as_ref() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(status.clone(), theme.footer())));
    }
    let list = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Jobs"));
    frame.render_widget(list, chunks[1]);

    let footer = Paragraph::new(Line::from(Span::styled(
        "↑↓ select · x remove pending · c clear finished · Ctrl+C cancel running · Esc back",
        theme.footer(),
    )))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, chunks[2]);
}
