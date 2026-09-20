//! Shared layout helpers: outer chrome, centered popups, minimum-size guard.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Render the "terminal too small" fallback instead of a garbled layout.
/// The spec requires degrading gracefully below 80x24.
pub fn render_too_small(frame: &mut Frame, area: Rect, min_width: u16, min_height: u16) {
    let message = Paragraph::new(format!(
        "Terminal too small: need at least {min_width}x{min_height}, got {}x{}.\nPlease resize your terminal.",
        area.width, area.height
    ))
    .alignment(Alignment::Center)
    .block(Block::default().borders(Borders::ALL).title("ffkit"));
    frame.render_widget(message, area);
}

/// Centered rectangle of `width` x `height` inside `area` (for dialogs/overlays).
pub fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Length(area.height.saturating_sub(height) / 2),
        Constraint::Length(height.min(area.height)),
        Constraint::Min(0),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Length(area.width.saturating_sub(width) / 2),
        Constraint::Length(width.min(area.width)),
        Constraint::Min(0),
    ])
    .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_rect_stays_inside_area() {
        let area = Rect::new(0, 0, 100, 40);
        let inner = centered_rect(60, 20, area);
        assert!(inner.x + inner.width <= area.width);
        assert!(inner.y + inner.height <= area.height);
    }

    #[test]
    fn centered_rect_clamps_to_tiny_areas() {
        let area = Rect::new(0, 0, 10, 5);
        let inner = centered_rect(60, 20, area);
        assert!(inner.width <= area.width && inner.height <= area.height);
    }
}
