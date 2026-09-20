//! Overlay popups: the preset picker and the preset-name prompt (M6).
//!
//! Both render centered over whatever screen is underneath and take over
//! input while visible. The app checks them before screen keys.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::config::presets::Preset;
use crate::ui::layout::centered_rect;
use crate::ui::theme::Theme;

/// Preset picker state: index into the combined (built-in + user) list.
#[derive(Debug, Default)]
pub struct PresetPopup {
    /// Highlighted preset.
    pub selected: usize,
}

/// All presets, built-ins first. Returned fresh each call — cheap and
/// always in sync with freshly saved user presets.
pub fn all_presets(settings: &crate::config::settings::Settings) -> Vec<Preset> {
    let mut presets = crate::config::presets::builtin_presets();
    presets.extend(settings.presets.iter().cloned());
    presets
}

/// Handle keys in the preset popup. Returns the chosen preset on Enter.
pub fn on_key_preset_popup(
    popup: &mut PresetPopup,
    key: KeyEvent,
    presets: &[Preset],
) -> PresetPopupAction {
    match key.code {
        KeyCode::Esc => PresetPopupAction::Close,
        KeyCode::Up | KeyCode::Char('k') => {
            popup.selected = popup.selected.saturating_sub(1);
            PresetPopupAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            popup.selected = popup
                .selected
                .saturating_add(1)
                .min(presets.len().saturating_sub(1));
            PresetPopupAction::None
        }
        KeyCode::Enter => match presets.get(popup.selected) {
            Some(preset) => PresetPopupAction::Apply(preset.clone()),
            None => PresetPopupAction::Close,
        },
        _ => PresetPopupAction::None,
    }
}

/// What a preset-popup keypress means to the app.
#[derive(Debug)]
pub enum PresetPopupAction {
    /// Navigation or no-op.
    None,
    /// Close without applying.
    Close,
    /// Jump to the preset's operation with these values pending.
    Apply(Preset),
}

/// Render the preset list centered over the current screen.
pub fn render_preset_popup(
    frame: &mut Frame,
    popup: &PresetPopup,
    presets: &[Preset],
    theme: &Theme,
) {
    // Two lines per preset (name + description) plus borders and margins.
    let height = (presets.len() * 2 + 4).min(26) as u16;
    let area = centered_rect(64, height, frame.area());
    let mut lines: Vec<Line> = Vec::new();
    for (i, preset) in presets.iter().enumerate() {
        let marker = if i == popup.selected { "▸ " } else { "  " };
        let style = if i == popup.selected {
            theme.selected()
        } else {
            theme.muted_style()
        };
        lines.push(Line::from(vec![
            Span::raw(marker.to_string()),
            Span::styled(format!("{} ({})", preset.name, preset.operation), style),
        ]));
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(preset.description.clone(), theme.footer()),
        ]));
    }
    if presets.is_empty() {
        lines.push(Line::from(Span::styled("No presets.", theme.muted_style())));
    }
    let popup_widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Presets — Enter applies (picks files next)")
            .border_style(theme.title()),
    );
    frame.render_widget(popup_widget, area);
}

/// Single-line name entry for "save preset".
#[derive(Debug)]
pub struct NamePrompt {
    /// Window title, e.g. `"Save preset"`.
    pub title: String,
    /// The name being typed.
    pub input: Input,
}

impl NamePrompt {
    /// Fresh prompt with an empty field.
    pub fn new(title: &str) -> Self {
        Self {
            title: title.to_string(),
            input: Input::default(),
        }
    }
}

/// Handle keys in the name prompt. Returns the typed name on Enter.
pub fn on_key_prompt(prompt: &mut NamePrompt, key: KeyEvent) -> PromptAction {
    match key.code {
        KeyCode::Esc => PromptAction::Close,
        KeyCode::Enter => PromptAction::Submit(prompt.input.value().trim().to_string()),
        _ => {
            prompt
                .input
                .handle_event(&crossterm::event::Event::Key(key));
            PromptAction::None
        }
    }
}

/// What a prompt keypress means to the app.
#[derive(Debug)]
pub enum PromptAction {
    /// Typing or no-op.
    None,
    /// Close without submitting.
    Close,
    /// Submitted text (may be empty — the caller validates).
    Submit(String),
}

/// Render the name prompt centered, with the terminal cursor in the field.
pub fn render_prompt(frame: &mut Frame, prompt: &NamePrompt, theme: &Theme) {
    let area = centered_rect(56, 5, frame.area());
    let field = Paragraph::new(format!("> {}", prompt.input.value())).block(
        Block::default()
            .borders(Borders::ALL)
            .title(prompt.title.clone())
            .border_style(theme.title()),
    );
    frame.render_widget(field, area);
    let cursor_x = area.x + 3 + prompt.input.visual_cursor() as u16;
    frame.set_cursor_position((cursor_x, area.y + 1));
}
