//! Friendly missing-FFmpeg screen (spec section 9).
//!
//! Shown instead of a stack trace when no `ffmpeg` binary resolves.
//! Per-platform install instructions, plus a custom-path field whose value
//! is validated (a real `-version` run, not just file existence) and
//! persisted to config on success.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

/// State for the missing-FFmpeg screen. Owned by [`App`](crate::app::App).
#[derive(Debug, Default)]
pub struct MissingState {
    /// Custom binary path being typed.
    pub input: Input,
    /// A validation run is in flight; input is locked until it resolves.
    pub checking: bool,
    /// Why the last custom path failed (or why auto-discovery failed).
    pub error: Option<String>,
    /// The path currently being validated — needed to persist on success.
    pub pending_path: Option<std::path::PathBuf>,
}

/// Handle keys on the missing screen. Returns true when the app should spawn
/// a validation run for the typed path.
pub fn on_key(state: &mut MissingState, key: KeyEvent) -> bool {
    if state.checking {
        return false;
    }
    match key.code {
        KeyCode::Enter => {
            let raw = state.input.value().trim().to_string();
            if raw.is_empty() {
                state.error = Some("Type a path to your ffmpeg binary first.".to_string());
                return false;
            }
            state.pending_path = Some(std::path::PathBuf::from(&raw));
            state.checking = true;
            state.error = None;
            true
        }
        _ => {
            state.input.handle_event(&crossterm::event::Event::Key(key));
            state.error = None;
            false
        }
    }
}

/// Render install instructions + custom path field.
pub fn render(frame: &mut Frame, app: &crate::app::App, theme: &crate::ui::theme::Theme) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(5),
    ])
    .split(area);

    let title = Paragraph::new(Line::from(Span::styled(
        "ffkit needs FFmpeg, but none was found on PATH",
        theme.title(),
    )))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, chunks[0]);

    let body = Paragraph::new(vec![
        Line::from(Span::styled(
            "Install it for your platform:",
            theme.muted_style(),
        )),
        Line::from(""),
        Line::from(Span::raw("  Debian/Ubuntu   sudo apt install ffmpeg")),
        Line::from(Span::raw("  Fedora          sudo dnf install ffmpeg")),
        Line::from(Span::raw("  macOS           brew install ffmpeg")),
        Line::from(Span::raw("  Windows         winget install ffmpeg")),
        Line::from(""),
        Line::from(Span::styled(
            "Or download a static build from https://ffmpeg.org/download.html",
            theme.muted_style(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Already have it somewhere unusual? Type the full path below.",
            theme.muted_style(),
        )),
        Line::from(""),
        Line::from(match app.missing.checking {
            true => Span::styled("Checking that binary…", theme.footer()),
            false => match app.missing.error.as_ref() {
                Some(err) => Span::styled(err.clone(), theme.warning_style()),
                None => Span::raw(""),
            },
        }),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Install FFmpeg"),
    );
    frame.render_widget(body, chunks[1]);

    let field = Paragraph::new(format!("> {}", app.missing.input.value())).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Custom ffmpeg path (Enter checks, q quits)"),
    );
    frame.render_widget(field, chunks[2]);
    let cursor_x = chunks[2].x + 3 + app.missing.input.visual_cursor() as u16;
    frame.set_cursor_position((cursor_x, chunks[2].y + 1));
}
