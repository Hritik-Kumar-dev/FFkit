//! Operation picker: the entry screen (M1).
//!
//! Renders the [`OPERATIONS`](crate::ops::OPERATIONS) registry as a
//! navigable list with a description pane and a footer of key hints.

use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::ops::OPERATIONS;
use crate::ui::theme::Theme;

/// Render the operation picker into the whole frame.
pub fn render(frame: &mut Frame, app: &mut App, theme: &Theme) {
    let area = frame.area();

    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .split(area);

    render_header(frame, chunks[0], theme);
    render_body(frame, chunks[1], app, theme);
    render_footer(frame, chunks[2], theme);
}

fn render_header(frame: &mut Frame, area: ratatui::layout::Rect, theme: &Theme) {
    let title = Paragraph::new(Line::from(vec![
        Span::styled("ffkit", theme.title()),
        Span::styled(
            " — pick an operation, watch the ffmpeg command build itself",
            theme.muted_style(),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, area);
}

fn render_body(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut App, theme: &Theme) {
    let columns =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(area);

    let items: Vec<ListItem> = OPERATIONS.iter().map(|op| ListItem::new(op.name)).collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Operations"))
        .highlight_style(theme.selected())
        .highlight_symbol("▸ ");

    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(app.selected_operation));
    frame.render_stateful_widget(list, columns[0], &mut state);

    let selected = app.selected_operation_meta();
    let detail = Paragraph::new(vec![
        Line::from(Span::styled(selected.name, theme.title())),
        Line::from(""),
        Line::from(Span::styled(selected.description, theme.muted_style())),
        Line::from(""),
        Line::from(Span::styled(
            format!("Accepts: {}", accepts_label(selected.accepts)),
            theme.muted_style(),
        )),
        Line::from(""),
        Line::from(Span::styled("Enter: choose · q: quit", theme.footer())),
    ])
    .block(Block::default().borders(Borders::ALL).title("About"));
    frame.render_widget(detail, columns[1]);
}

fn render_footer(frame: &mut Frame, area: ratatui::layout::Rect, theme: &Theme) {
    let footer = Paragraph::new(Line::from(Span::styled(
        "↑↓/k j move · Enter choose · p presets · Q queue · ? help · q quit",
        theme.footer(),
    )))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, area);
}

fn accepts_label(accepts: crate::ops::InputKind) -> &'static str {
    match accepts {
        crate::ops::InputKind::Video => "video files",
        crate::ops::InputKind::Audio => "audio files",
        crate::ops::InputKind::AudioOrVideo => "audio or video files",
        crate::ops::InputKind::Image => "image files",
        crate::ops::InputKind::Multiple => "multiple files",
    }
}
