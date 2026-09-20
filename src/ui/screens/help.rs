//! Help overlay: the non-negotiable keybinding table (spec section 6).

use ratatui::layout::Alignment;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::ui::layout::centered_rect;
use crate::ui::theme::Theme;

/// Bindings shown in the overlay. Kept as data (not inline spans) so M7
/// theming and any `--help` text output can reuse it.
pub const BINDINGS: &[(&str, &str)] = &[
    ("↑ ↓ / k j", "Move between fields or list items"),
    (
        "← → / h l",
        "Adjust the focused value (h goes up in the file browser)",
    ),
    (
        "Enter",
        "Confirm / run (opens directories in the file browser)",
    ),
    ("Esc", "Back one screen (clears the browser filter first)"),
    ("Tab / Shift+Tab", "Cycle panes"),
    ("c", "Copy command to clipboard"),
    ("q", "Quit (confirm if jobs are running)"),
    ("Ctrl+C", "Cancel running job; second press quits"),
    ("?", "This help overlay"),
    ("/", "Jump to a path (file browser)"),
    ("~", "Home directory (file browser)"),
    ("a", "Toggle all files / media only (file browser)"),
    ("g G", "First / last entry (file browser)"),
    ("Space", "Toggle selection (file browser, queue)"),
    ("type to filter", "Type-ahead search within the directory"),
    ("l", "Toggle stderr log (running screen)"),
    ("y / n", "Answer prompts (overwrite, delete partial file)"),
    ("Q", "Job queue (from picker, browser, form)"),
    ("p", "Preset picker (operation picker)"),
    ("s", "Save form as preset (parameter form)"),
    ("x", "Remove pending job (queue screen)"),
];

/// Render the help overlay centered over a dimmed frame.
pub fn render(frame: &mut Frame, _app: &mut App, theme: &Theme) {
    let area = centered_rect(56, (BINDINGS.len() + 5) as u16, frame.area());

    let mut lines: Vec<Line> = BINDINGS
        .iter()
        .map(|(key, action)| {
            Line::from(vec![
                Span::styled(format!("{key:14}"), theme.title()),
                Span::styled(*action, theme.muted_style()),
            ])
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Press any key to close",
        theme.footer(),
    )));

    let popup = Paragraph::new(lines).alignment(Alignment::Left).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Help — keybindings"),
    );
    frame.render_widget(popup, area);
}

/// Re-export for tests that assert overlay geometry helpers.
pub fn popup_size() -> (u16, u16) {
    (56, (BINDINGS.len() + 5) as u16)
}
