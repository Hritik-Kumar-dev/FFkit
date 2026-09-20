//! Scrollable stderr log pane (M4).
//!
//! Collapsed by default (last few lines, dimmed); `l` expands to a
//! scrollable full view. Backed by the UI's ring buffer — the runner keeps
//! the authoritative capped copy for the final report.

use std::collections::VecDeque;

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Maximum lines the UI keeps for display. The runner retains more for the
/// "copy error report" action; the pane is for glancing, not forensics.
pub const LOG_DISPLAY_CAP: usize = 300;

/// Push one line, evicting the oldest past the cap.
pub fn push_log(log: &mut VecDeque<String>, line: String) {
    if log.len() >= LOG_DISPLAY_CAP {
        log.pop_front();
    }
    log.push_back(line);
}

/// Render the log pane. Collapsed shows the tail; expanded shows a window
/// ending `scroll` lines above the bottom (0 = live tail).
pub fn render_log(
    frame: &mut Frame,
    area: Rect,
    lines: &VecDeque<String>,
    expanded: bool,
    scroll: usize,
    theme: &crate::ui::theme::Theme,
) {
    let visible = (area.height.saturating_sub(2)) as usize;
    let title = if expanded {
        format!("Log — {} lines (l collapses, ↑↓ scrolls)", lines.len())
    } else {
        format!("Log — {} lines (l expands)", lines.len())
    };
    let body: Vec<Line> = if lines.is_empty() {
        vec![Line::from(Span::styled(
            "No output yet.",
            theme.muted_style(),
        ))]
    } else if expanded {
        let end = lines.len().saturating_sub(scroll);
        let start = end.saturating_sub(visible.max(1));
        lines
            .iter()
            .skip(start)
            .take(visible.max(1))
            .map(|line| Line::from(Span::raw(line.clone())))
            .collect()
    } else {
        lines
            .iter()
            .rev()
            .take(visible.clamp(1, 3))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|line| Line::from(Span::styled(line.clone(), theme.muted_style())))
            .collect()
    };
    let pane = Paragraph::new(body).block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(pane, area);
}
