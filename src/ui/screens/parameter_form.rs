//! Parameter form with the live command preview (M3).
//!
//! Layout (spec section 6): parameters left, media info right, the command
//! pane below — updating on every keystroke — with the focused field's
//! plain-English flag explanation underneath. `c` copies the command;
//! clipboard failures degrade to a status message (headless/SSH).

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;

use crate::ffmpeg::builder::CommandSpec;
use crate::ops::fields::{collect_values, BuildContext, Field, FieldKind, FieldValue};
use crate::ui::theme::Theme;

/// How long a changed command token stays highlighted (spec: ~400ms).
const HIGHLIGHT_MS: u128 = 400;

/// What a form keypress means to the app.
#[derive(Debug, PartialEq, Eq)]
pub enum FormAction {
    /// Navigation or no-op; preview already rebuilt when needed.
    None,
    /// Leave the form (back to the file browser).
    Back,
    /// A field value changed — the preview was rebuilt.
    Changed,
    /// The user pressed Enter to run (execution lands in M4).
    Run,
}

/// Live form state for one operation. Owned by [`App`](crate::app::App).
pub struct ParamForm {
    /// Operation id, for rebuilds.
    pub op_id: String,
    /// Current fields in form order.
    pub fields: Vec<Field>,
    /// Focused field index.
    pub focus: usize,
    /// Latest successfully built spec (preview + clipboard source).
    pub preview: Option<CommandSpec>,
    /// Build failure text (e.g. M5 ops), shown in the command pane.
    pub build_error: Option<String>,
    /// Changed token + when, for the ~400ms highlight.
    pub changed: Option<(String, Instant)>,
    /// Tab focus: false = parameters, true = command pane (scroll with ↑↓).
    pub focus_cmd: bool,
    /// Scroll offset into the wrapped command lines.
    pub cmd_scroll: usize,
    /// A text field is being edited (global single-key bindings suspended).
    pub editing: bool,
    /// The output field was just entered and its default is select-all:
    /// the next printable char replaces it, Backspace clears it.
    pub edit_select_all: bool,
}

impl ParamForm {
    /// Build the form for `op` with fields defaulted from `probe`/`caps`,
    /// then run the first build immediately so the preview is never empty.
    pub fn new(
        op: &dyn crate::ops::Operation,
        inputs: &[PathBuf],
        probe: Option<&crate::ffmpeg::probe::ProbeResult>,
        input_probes: Vec<crate::ffmpeg::probe::ProbeResult>,
        caps: Option<&crate::ffmpeg::capabilities::CapabilityReport>,
    ) -> Self {
        let mut form = Self {
            op_id: op.id().to_string(),
            fields: op.fields(&crate::ops::FieldContext { probe, caps }),
            focus: 0,
            preview: None,
            build_error: None,
            changed: None,
            focus_cmd: false,
            cmd_scroll: 0,
            editing: false,
            edit_select_all: false,
        };
        form.rebuild(op, inputs, probe, input_probes, caps);
        form
    }

    /// Re-run the builder from current field values. On success the preview
    /// updates and the first changed token is highlighted for ~400ms.
    pub fn rebuild(
        &mut self,
        op: &dyn crate::ops::Operation,
        inputs: &[PathBuf],
        probe: Option<&crate::ffmpeg::probe::ProbeResult>,
        input_probes: Vec<crate::ffmpeg::probe::ProbeResult>,
        caps: Option<&crate::ffmpeg::capabilities::CapabilityReport>,
    ) {
        let output = self
            .fields
            .iter()
            .find(|f| f.id == "output")
            .map(|f| match f.value() {
                FieldValue::Text(text) => PathBuf::from(text),
                _ => PathBuf::from(""),
            });
        let ctx = BuildContext {
            inputs,
            output: output.as_ref(),
            probe,
            input_probes,
            // Generic forms never own clips; trim uses its own screen.
            clips: Vec::new(),
            caps,
            fields: collect_values(&self.fields),
        };
        match op.build(&ctx) {
            Ok(spec) => {
                if let Some(previous) = self.preview.as_ref() {
                    if let Some(token) = diff_token(&previous.args, &spec.args) {
                        self.changed = Some((token, Instant::now()));
                    }
                }
                self.preview = Some(spec);
                self.build_error = None;
                self.cmd_scroll = 0;
            }
            Err(e) => {
                self.preview = None;
                self.build_error = Some(format!("{e:#}"));
            }
        }
    }

    /// Handle one keypress. Returns [`FormAction::Changed`] when the caller
    /// must rebuild the preview (done by the caller so it can supply the
    /// operation + context).
    pub fn on_key(&mut self, key: KeyEvent) -> FormAction {
        if self.editing {
            return self.on_key_editing(key);
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.focus_cmd {
                    self.cmd_scroll = self.cmd_scroll.saturating_sub(1);
                } else {
                    self.focus = self.focus.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.focus_cmd {
                    self.cmd_scroll = self.cmd_scroll.saturating_add(1);
                } else {
                    self.focus = self
                        .focus
                        .saturating_add(1)
                        .min(self.fields.len().saturating_sub(1));
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(field) = self.fields.get_mut(self.focus) {
                    field.adjust_backward();
                    return FormAction::Changed;
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if let Some(field) = self.fields.get_mut(self.focus) {
                    field.adjust_forward();
                    return FormAction::Changed;
                }
            }
            KeyCode::Tab => {
                self.focus_cmd = !self.focus_cmd;
            }
            KeyCode::BackTab => {
                self.focus_cmd = !self.focus_cmd;
            }
            KeyCode::Enter => {
                if let Some(field) = self.fields.get(self.focus) {
                    if matches!(field.kind, FieldKind::Text { .. }) {
                        self.editing = true;
                        // The output default is select-all: typing replaces
                        // it instead of appending to a long path.
                        self.edit_select_all = field.id == "output";
                    } else {
                        return FormAction::Run;
                    }
                } else {
                    return FormAction::Run;
                }
            }
            KeyCode::Esc => return FormAction::Back,
            _ => {}
        }
        FormAction::None
    }

    /// Keys while a text field is being edited. Enter/Esc leave edit mode
    /// (Esc does not go back — it only cancels editing). While select-all
    /// is active (output field just entered), the first printable char
    /// replaces the whole value and Backspace/Delete clears it; any other
    /// key drops the selection and behaves normally.
    fn on_key_editing(&mut self, key: KeyEvent) -> FormAction {
        let Some(field) = self.fields.get_mut(self.focus) else {
            self.editing = false;
            self.edit_select_all = false;
            return FormAction::None;
        };
        if handle_text_edit(field, &mut self.editing, &mut self.edit_select_all, key) {
            FormAction::Changed
        } else {
            FormAction::None
        }
    }

    /// Currently focused field, if any.
    pub fn focused(&self) -> Option<&Field> {
        self.fields.get(self.focus)
    }
}

/// Shared text-editing keys for form text fields (parameter form and trim
/// timeline). Enter/Esc leave edit mode; select-all replaces on first
/// type. Returns true when the value changed (caller rebuilds the preview).
pub(crate) fn handle_text_edit(
    field: &mut Field,
    editing: &mut bool,
    select_all: &mut bool,
    key: KeyEvent,
) -> bool {
    match key.code {
        KeyCode::Enter | KeyCode::Esc => {
            *editing = false;
            *select_all = false;
            false
        }
        KeyCode::Char(c) if *select_all && key.modifiers.is_empty() => {
            if let FieldKind::Text { value } = &mut field.kind {
                *value = tui_input::Input::new(c.to_string());
                *select_all = false;
                return true;
            }
            *select_all = false;
            false
        }
        KeyCode::Backspace | KeyCode::Delete if *select_all => {
            if let FieldKind::Text { value } = &mut field.kind {
                *value = tui_input::Input::default();
                *select_all = false;
                return true;
            }
            *select_all = false;
            false
        }
        _ => {
            *select_all = false;
            if let FieldKind::Text { value } = &mut field.kind {
                value.handle_event(&crossterm::event::Event::Key(key));
                return true;
            }
            false
        }
    }
}

/// Find the first changed token between two argv vectors for highlighting.
/// When a value changed, the flag and value highlight together (`-crf 20`)
/// so the control→flag connection is unmistakable. Shared with the trim
/// timeline preview.
pub(crate) fn diff_token(old: &[String], new: &[String]) -> Option<String> {
    let len = old.len().min(new.len());
    let mut index = old.len().min(new.len());
    for (i, (a, b)) in old.iter().zip(new.iter()).take(len).enumerate() {
        if a != b {
            index = i;
            break;
        }
    }
    if index == len && old.len() == new.len() {
        return None;
    }
    let token = new.get(index)?.clone();
    if !token.starts_with('-') && index > 0 {
        let flag = &new[index - 1];
        if flag.starts_with('-') {
            return Some(format!("{flag} {token}"));
        }
    }
    Some(token)
}

/// Copy `text` to the system clipboard. Degrades gracefully when unavailable
/// (headless, SSH): the caller shows the returned message as a status.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|e| format!("clipboard copy failed: {e}"))
}

/// Render the full form: header, parameters + media info, command preview
/// with explanation, footer hints.
pub fn render(frame: &mut Frame, app: &crate::app::App, form: &ParamForm, theme: &Theme) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(10),
        Constraint::Length(3),
    ])
    .split(area);

    let op_name = crate::ops::operation_for(&form.op_id)
        .map(|op| op.name().to_string())
        .unwrap_or_else(|| form.op_id.clone());
    let title = Paragraph::new(Line::from(vec![
        Span::styled(op_name, theme.title()),
        Span::styled(" — tweak, watch the command", theme.muted_style()),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, chunks[0]);

    let body = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(chunks[1]);
    render_fields(frame, body[0], form, theme);
    render_info(frame, body[1], app, theme);

    render_command(frame, chunks[2], form, theme);

    let hints = if form.editing {
        "typing… Enter done · Esc cancel editing"
    } else if form.focus_cmd {
        "↑↓ scroll command · Tab back to fields · c copy · Esc back"
    } else {
        "↑↓ field · ←→ adjust · Enter edit/run · Tab command · c copy · s preset · ? help · esc back"
    };
    let footer = Paragraph::new(Line::from(Span::styled(hints, theme.footer())))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, chunks[3]);

    // Text cursor while editing.
    if form.editing {
        if let Some(cursor) = edit_cursor(form, body[0]) {
            frame.set_cursor_position(cursor);
        }
    }
}

/// Render the parameter rows with focus highlight.
fn render_fields(frame: &mut Frame, area: Rect, form: &ParamForm, theme: &Theme) {
    let mut lines: Vec<Line> = Vec::new();
    if form.fields.is_empty() {
        lines.push(Line::from(Span::styled(
            "No parameters yet — this operation lands in M5.",
            theme.warning_style(),
        )));
    }
    for (i, field) in form.fields.iter().enumerate() {
        let focused = i == form.focus && !form.focus_cmd;
        let select_all = focused && form.editing && form.edit_select_all;
        lines.extend(field_lines(field, focused, select_all, theme));
    }
    let block = Block::default().borders(Borders::ALL).title("Parameters");
    let block = if form.focus_cmd {
        block
    } else {
        block.border_style(theme.title())
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// One field's rendered lines: label + control row, plus slider caption
/// and landmarks under a focused slider. Shared with the trim timeline
/// screen so both forms paint identically.
pub(crate) fn field_lines(
    field: &Field,
    focused: bool,
    select_all: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let marker = if focused { "▸ " } else { "  " };
    let label = format!("{marker}{}", field.label);
    let control = render_control(field, focused, select_all, theme);
    let mut spans = vec![Span::styled(label, label_style(focused, theme))];
    spans.push(Span::raw("  "));
    spans.extend(control);
    lines.push(Line::from(spans));
    // Slider caption + landmarks under the focused slider.
    if let FieldKind::Slider {
        caption, landmarks, ..
    } = &field.kind
    {
        if focused {
            if let Some(caption) = caption {
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(caption.clone(), theme.footer()),
                ]));
            }
            if !landmarks.is_empty() {
                let marks: Vec<String> = landmarks
                    .iter()
                    .map(|(v, label)| format!("{v} {label}"))
                    .collect();
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(marks.join(" · "), theme.footer()),
                ]));
            }
        }
    }
    lines
}
/// warnings, and the multi-file note. Carries the M2 panel into the M3 form.
/// Media-info pane for the confirmed input(s): probe summaries, rotation
/// warnings, and the multi-file note. Carries the M2 panel into the M3 form.
fn render_info(frame: &mut Frame, area: Rect, app: &crate::app::App, theme: &Theme) {
    use crate::app::ProbeState;

    let mut lines = vec![Line::from(Span::styled("Input", theme.title()))];
    if app.form_inputs.is_empty() {
        lines.push(Line::from(Span::styled(
            "No inputs confirmed (this should not happen — please report it).",
            theme.warning_style(),
        )));
    }
    for input in app.form_inputs.iter().take(1) {
        lines.push(Line::from(Span::raw(format!("  {}", input.display()))));
        match app.probes.get(input) {
            None => lines.push(Line::from(Span::styled("    …", theme.footer()))),
            Some(ProbeState::Pending) => {
                lines.push(Line::from(Span::styled("    Probing…", theme.footer())));
            }
            Some(ProbeState::Ready(result)) => {
                lines.push(Line::from(Span::styled(
                    format!("    {}", result.summary_line()),
                    theme.command_style(),
                )));
                for stream in &result.streams {
                    if let Some(rot) = stream.rotation {
                        if rot != 0.0 {
                            lines.push(Line::from(Span::styled(
                                format!(
                                    "    Stream {} rotated {rot}° — affects orientation",
                                    stream.index
                                ),
                                theme.warning_style(),
                            )));
                        }
                    }
                }
            }
            Some(ProbeState::Failed(message)) => {
                lines.push(Line::from(Span::styled(
                    format!("    Could not read: {message}"),
                    theme.warning_style(),
                )));
            }
        }
    }
    if app.form_inputs.len() > 1 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "+ {} more file(s) — same settings apply to all",
                app.form_inputs.len() - 1
            ),
            theme.footer(),
        )));
    }
    let panel =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Media info"));
    frame.render_widget(panel, area);
}

/// Render one control (the value half of a row). `select_all` paints a
/// text value fully selected (output field just entered — typing replaces).
fn render_control(
    field: &Field,
    focused: bool,
    select_all: bool,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let highlight = |text: String| {
        if focused {
            Span::styled(text, theme.selected())
        } else {
            Span::raw(text)
        }
    };
    match &field.kind {
        FieldKind::Select { options, selected } => {
            let label = options
                .get(*selected)
                .map(|o| o.label.clone())
                .unwrap_or_default();
            let mut spans = vec![highlight(format!("◂ {label} ▸"))];
            if let Some(reason) = options
                .get(*selected)
                .and_then(|o| o.disabled_reason.clone())
            {
                spans.push(Span::styled(format!(" ({reason})"), theme.warning_style()));
            }
            spans
        }
        FieldKind::Toggle { options, selected } => {
            let [a, b] = options;
            let (left, right) = if *selected == 0 { (a, b) } else { (b, a) };
            vec![
                highlight(format!("● {}", left.clone())),
                Span::raw("  "),
                Span::styled(format!("○ {}", right.clone()), theme.muted_style()),
            ]
        }
        FieldKind::Slider {
            min, max, value, ..
        } => {
            const BAR: usize = 18;
            let ratio = (*value - *min).max(0) as f64 / (*max - *min).max(1) as f64;
            let pos = (ratio * BAR as f64).round() as usize;
            let mut bar = String::new();
            for i in 0..=BAR {
                bar.push(if i == pos { '●' } else { '─' });
            }
            vec![
                Span::styled(format!("{min}"), theme.muted_style()),
                Span::raw(" "),
                highlight(bar),
                Span::raw(" "),
                Span::styled(format!("{max}"), theme.muted_style()),
                Span::raw("  "),
                highlight(format!("[{value}]")),
            ]
        }
        FieldKind::Text { value } => {
            let text = value.value().to_string();
            let shown = if focused && text.len() > 40 {
                format!("…{}", &text[text.len().saturating_sub(39)..])
            } else {
                text
            };
            if select_all {
                // Focus paints the control selected already; the underline
                // marks "typing replaces everything".
                vec![Span::styled(
                    format!("[{shown}]"),
                    theme.selected().add_modifier(Modifier::UNDERLINED),
                )]
            } else {
                vec![highlight(format!("[{shown}]"))]
            }
        }
    }
}

/// Command preview pane: wrapped shell-quoted command with the just-changed
/// token highlighted, plus the focused field's flag explanation.
fn render_command(frame: &mut Frame, area: Rect, form: &ParamForm, theme: &Theme) {
    let highlight = form
        .changed
        .as_ref()
        .filter(|(_, at)| at.elapsed().as_millis() < HIGHLIGHT_MS)
        .map(|(token, _)| token.as_str());
    let explanation = form
        .focused()
        .map(|field| (field.label.as_str(), field.explanation.as_str()));
    render_spec_preview(
        frame,
        area,
        "Command — [c] copy",
        form.preview.as_ref(),
        form.build_error.as_deref(),
        highlight,
        form.focus_cmd,
        form.cmd_scroll,
        explanation,
        theme,
    );
}

/// Shared live-preview pane: wrapped shell-quoted command with the
/// just-changed token highlighted, an optional scrolled view, and the
/// focused control's flag explanation. Used by the parameter form and the
/// trim timeline so both previews paint identically.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_spec_preview(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    preview: Option<&CommandSpec>,
    build_error: Option<&str>,
    highlight: Option<&str>,
    focused: bool,
    scroll: usize,
    explanation: Option<(&str, &str)>,
    theme: &Theme,
) {
    let mut lines: Vec<Line> = Vec::new();
    match preview {
        None => {
            let message = build_error.unwrap_or("No command built yet.");
            lines.push(Line::from(Span::styled(
                message.to_string(),
                theme.warning_style(),
            )));
        }
        Some(spec) => {
            let display = spec.to_display();
            let styled = styled_display(&display, highlight, theme);
            let total = styled.len();
            let visible = (area.height.saturating_sub(2)) as usize;
            let max_scroll = total.saturating_sub(visible.max(1));
            let start = scroll.min(max_scroll);
            lines.extend(styled.into_iter().skip(start).take(visible.max(1)));
            if total > visible.max(1) {
                lines.push(Line::from(Span::styled(
                    format!(
                        "… {} more lines (Tab focuses, ↑↓ scrolls)",
                        total - visible.max(1)
                    ),
                    theme.footer(),
                )));
            }
        }
    }
    // Contextual explanation for the focused control — the teaching mechanism.
    if let Some((label, text)) = explanation {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(format!("{label}  "), theme.title()),
            Span::styled(text.to_string(), theme.explanation_style()),
        ]));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string());
    let block = if focused {
        block.border_style(theme.title())
    } else {
        block
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(ratatui::widgets::Wrap { trim: true }),
        area,
    );
}

/// Style for a row label.
fn label_style(focused: bool, theme: &Theme) -> Style {
    if focused {
        theme.selected()
    } else {
        Style::default()
    }
}

/// Split display text into lines, highlighting the first occurrence of
/// `highlight` (the just-changed token) so the control→flag connection is
/// unmistakable. Returns owned lines — the display string is a temporary.
fn styled_display(display: &str, highlight: Option<&str>, theme: &Theme) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut pending = highlight;
    for line in display.lines() {
        match pending {
            Some(token) if line.contains(token) => {
                let (before, after) = line.split_once(token).unwrap_or((line, ""));
                let mut spans = Vec::new();
                if !before.is_empty() {
                    spans.push(Span::styled(before.to_string(), theme.command_style()));
                }
                spans.push(Span::styled(token.to_string(), theme.selected()));
                if !after.is_empty() {
                    spans.push(Span::styled(after.to_string(), theme.command_style()));
                }
                out.push(Line::from(spans));
                pending = None;
            }
            _ => out.push(Line::from(Span::styled(
                line.to_string(),
                theme.command_style(),
            ))),
        }
    }
    out
}

/// Cursor position for the text being edited.
fn edit_cursor(form: &ParamForm, area: Rect) -> Option<(u16, u16)> {
    let field = form.focused()?;
    let FieldKind::Text { value } = &field.kind else {
        return None;
    };
    // Rows before the focused one take exactly one line each (captions only
    // render under the focused slider, i.e. at/after this row).
    let row = form.focus;
    let text = value.value();
    let cursor = (value.visual_cursor() as u16).min(text.len().min(40) as u16);
    // "[text]" starts after "▸ label  " — marker(2) + label + 2 spaces + '['.
    let x = area.x + 1 + 2 + field.label.len() as u16 + 2 + 1 + cursor;
    let y = area.y + 1 + row as u16;
    if x >= area.x + area.width || y >= area.y + area.height {
        return None;
    }
    Some((x, y))
}

#[cfg(test)]
mod tests {
    use super::diff_token;
    use super::{FormAction, ParamForm};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    /// Build a one-field output form for editing tests.
    fn output_form() -> ParamForm {
        use crate::ops::fields::{Field, FieldKind};
        ParamForm {
            op_id: "convert".to_string(),
            fields: vec![Field {
                id: "output",
                label: "Output".to_string(),
                kind: FieldKind::Text {
                    value: tui_input::Input::new("clip_converted.mp4".to_string()),
                },
                explanation: String::new(),
            }],
            focus: 0,
            preview: None,
            build_error: None,
            changed: None,
            focus_cmd: false,
            cmd_scroll: 0,
            editing: false,
            edit_select_all: false,
        }
    }

    fn output_value(form: &ParamForm) -> String {
        use crate::ops::fields::FieldKind;
        match &form.fields[0].kind {
            FieldKind::Text { value } => value.value().to_string(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn diff_finds_value_change_with_flag() {
        let old = vec!["-crf".to_string(), "23".to_string()];
        let new = vec!["-crf".to_string(), "20".to_string()];
        assert_eq!(diff_token(&old, &new), Some("-crf 20".to_string()));
    }

    #[test]
    fn output_field_selects_all_on_enter() {
        let mut form = output_form();
        form.on_key(key(KeyCode::Enter));
        assert!(form.editing);
        assert!(form.edit_select_all);
    }

    #[test]
    fn typing_replaces_selected_output() {
        let mut form = output_form();
        form.on_key(key(KeyCode::Enter));
        let action = form.on_key(key(KeyCode::Char('m')));
        assert_eq!(action, FormAction::Changed);
        assert_eq!(output_value(&form), "m");
        assert!(!form.edit_select_all);
    }

    #[test]
    fn backspace_clears_selected_output() {
        let mut form = output_form();
        form.on_key(key(KeyCode::Enter));
        let action = form.on_key(key(KeyCode::Backspace));
        assert_eq!(action, FormAction::Changed);
        assert_eq!(output_value(&form), "");
    }

    #[test]
    fn arrows_drop_selection_without_editing() {
        let mut form = output_form();
        form.on_key(key(KeyCode::Enter));
        form.on_key(key(KeyCode::Left));
        assert!(!form.edit_select_all);
        assert_eq!(output_value(&form), "clip_converted.mp4");
    }

    #[test]
    fn diff_returns_none_when_identical() {
        let args = vec!["-y".to_string(), "-i".to_string()];
        assert_eq!(diff_token(&args, &args), None);
    }

    #[test]
    fn diff_handles_added_flags() {
        let old = vec!["-y".to_string()];
        let new = vec!["-y".to_string(), "-vf".to_string()];
        assert_eq!(diff_token(&old, &new), Some("-vf".to_string()));
    }
}
