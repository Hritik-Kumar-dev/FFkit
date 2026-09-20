//! All colors and styles centralized here (spec section 3).
//!
//! Every screen and widget imports from this module; no inline `Color`
//! literals elsewhere, so theming (M7) is a single-file change.

use ratatui::style::{Color, Modifier, Style};

/// The application theme, selected by name from config (`theme = "dark"`).
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// App title and focused borders.
    pub accent: Color,
    /// Currently selected / highlighted row.
    pub selection_bg: Color,
    /// Muted text: hints, descriptions, footer keys.
    pub muted: Color,
    /// Warnings and destructive hints.
    pub warning: Color,
    /// The live command preview text.
    pub command: Color,
    /// Plain-English flag explanations.
    pub explanation: Color,
}

impl Theme {
    /// Default dark theme.
    pub fn dark() -> Self {
        Self {
            accent: Color::Cyan,
            selection_bg: Color::DarkGray,
            muted: Color::Gray,
            warning: Color::Yellow,
            command: Color::Green,
            explanation: Color::DarkGray,
        }
    }

    /// Light-background variant. Same roles, contrast-checked hues —
    /// selection stays a background fill so it reads on white.
    pub fn light() -> Self {
        Self {
            accent: Color::Blue,
            selection_bg: Color::LightYellow,
            muted: Color::DarkGray,
            warning: Color::Red,
            command: Color::Green,
            explanation: Color::Gray,
        }
    }

    /// Resolve a config theme name. Unknown names fall back to dark (the
    /// caller logs the mismatch once at startup rather than refusing).
    pub fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "light" => Self::light(),
            _ => Self::dark(),
        }
    }

    /// Title bar style.
    pub fn title(&self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    /// Highlight style for the selected list row / focused field.
    pub fn selected(&self) -> Style {
        Style::default()
            .bg(self.selection_bg)
            .add_modifier(Modifier::BOLD)
    }

    /// Muted descriptive text.
    pub fn muted_style(&self) -> Style {
        Style::default().fg(self.muted)
    }

    /// Live command preview text.
    pub fn command_style(&self) -> Style {
        Style::default().fg(self.command)
    }

    /// Warnings and errors.
    pub fn warning_style(&self) -> Style {
        Style::default()
            .fg(self.warning)
            .add_modifier(Modifier::BOLD)
    }

    /// Plain-English flag explanations.
    pub fn explanation_style(&self) -> Style {
        Style::default().fg(self.explanation)
    }

    /// Footer key hints.
    pub fn footer(&self) -> Style {
        Style::default().fg(self.muted)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_names_resolve_with_dark_fallback() {
        assert_eq!(Theme::from_name("light").accent, Color::Blue);
        assert_eq!(Theme::from_name("dark").accent, Color::Cyan);
        assert_eq!(Theme::from_name("  LIGHT  ").accent, Color::Blue);
        assert_eq!(Theme::from_name("solarized").accent, Color::Cyan);
        assert_eq!(Theme::from_name("").accent, Color::Cyan);
    }
}
