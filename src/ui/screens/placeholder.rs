//! Placeholder renderer for screens whose milestone has not landed yet.
//!
//! Each arm names the milestone that will replace it, so running the M1
//! binary shows the routing working end to end.

use ratatui::layout::Alignment;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, Screen};
use crate::ui::theme::Theme;

/// Render a "coming in milestone N" card for not-yet-implemented screens.
pub fn render(frame: &mut Frame, app: &mut App, theme: &Theme) {
    let (title, body) = match app.screen {
        Screen::FileBrowser => (
            "File Browser — M2",
            format!(
                "Here you will pick input file(s) for “{}”.\n\nDirectory navigation with vim keys and arrows, media-file filtering, multi-select with Space, and a lazily-loaded probe summary.\n\nEsc goes back.",
                app.selected_operation_meta().name
            ),
        ),
        Screen::ParameterForm => (
            "Parameter Form — M3",
            "Here you will tweak dropdowns, sliders, and toggles while the real ffmpeg command assembles itself live below.\n\nEsc goes back.".to_string(),
        ),
        Screen::Running => (
            "Running — M4",
            "Here you will watch progress bars, ETA, and speed while ffmpeg runs.\n\nEsc goes back.".to_string(),
        ),
        Screen::Queue { .. } => (
            "Queue — M6",
            "Here you will see pending / running / finished batch jobs.\n\nEsc goes back.".to_string(),
        ),
        Screen::OperationPicker
        | Screen::Help { .. }
        | Screen::Startup
        | Screen::Trim
        | Screen::MissingFfmpeg => ("ffkit", "Unexpected screen.".to_string()),
    };
    let card = Paragraph::new(body).alignment(Alignment::Left).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(theme.title()),
    );
    frame.render_widget(card, frame.area());
}
