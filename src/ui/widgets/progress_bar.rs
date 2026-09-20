//! Progress bar widget (M4).
//!
//! Determinate bar from `out_time / total_duration`; when the total is
//! unknown the widget shows an indeterminate spinner with elapsed time and
//! speed instead of a fake percentage (spec §7: never invent progress).

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::ui::theme::Theme;

/// Spinner frames shared with the indeterminate bar.
pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Render a determinate (`Some(ratio)`) or indeterminate (`None`) bar.
pub fn render_progress(
    frame: &mut Frame,
    area: Rect,
    ratio: Option<f64>,
    tick: usize,
    theme: &Theme,
) {
    let line = match ratio {
        Some(ratio) => determinate_line(ratio, area.width, theme),
        None => indeterminate_line(tick, theme),
    };
    let bar = Paragraph::new(line).block(Block::default().borders(Borders::ALL).title("Progress"));
    frame.render_widget(bar, area);
}

/// One bar line: `██████░░░░  42%`. Pure — unit-tested.
pub fn determinate_line(ratio: f64, width: u16, theme: &Theme) -> Line<'static> {
    let ratio = ratio.clamp(0.0, 1.0);
    // Label takes " 100%" (5) + borders/padding (~4).
    let bar_width = (width as usize).saturating_sub(11).max(8);
    let filled = (ratio * bar_width as f64).round() as usize;
    let mut bar = String::with_capacity(bar_width);
    for i in 0..bar_width {
        bar.push(if i < filled { '█' } else { '░' });
    }
    Line::from(vec![
        Span::styled(bar, theme.command_style()),
        Span::styled(format!(" {:>3.0}%", ratio * 100.0), theme.title()),
    ])
}

/// Indeterminate line: spinner + "working…" (no percentage invented).
pub fn indeterminate_line(tick: usize, theme: &Theme) -> Line<'static> {
    let glyph = SPINNER_FRAMES[tick % SPINNER_FRAMES.len()];
    Line::from(vec![
        Span::styled(format!("{glyph} "), theme.title()),
        Span::styled("working — total duration unknown", theme.muted_style()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_fills_proportionally() {
        let theme = Theme::dark();
        let line = determinate_line(0.5, 30, &theme);
        let text: String = line.spans.iter().map(|s| s.content.clone()).collect();
        let filled = text.chars().filter(|c| *c == '█').count();
        let empty = text.chars().filter(|c| *c == '░').count();
        assert_eq!(filled + empty, 19, "bar width is width-11: {text}");
        assert!(
            filled.abs_diff(empty) <= 1,
            "half ratio must ~half-fill: {text}"
        );
        assert!(text.contains("50%"), "{text}");
    }

    #[test]
    fn bar_clamps_out_of_range_ratios() {
        let theme = Theme::dark();
        let over = determinate_line(2.0, 30, &theme);
        let text: String = over.spans.iter().map(|s| s.content.clone()).collect();
        assert!(text.contains("100%"), "{text}");
        assert!(!text.contains('░'), "{text}");
    }
}
