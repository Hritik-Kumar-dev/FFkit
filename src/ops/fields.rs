//! Parameter fields: the generic form model (M3).
//!
//! Each operation declares `Vec<Field>` (dropdowns, sliders, toggles, text).
//! The parameter form renders them generically, rebuilds the
//! [`CommandSpec`](crate::ffmpeg::builder::CommandSpec) on every change,
//! and reads each field's `explanation` for the teaching pane.

use std::collections::HashMap;
use std::path::PathBuf;

use tui_input::Input;

use crate::ffmpeg::capabilities::CapabilityReport;
use crate::ffmpeg::probe::ProbeResult;

/// One dropdown option.
#[derive(Debug, Clone)]
pub struct SelectOption {
    /// Shown in the form, e.g. `"H.264 (libx264)"`.
    pub label: String,
    /// The value passed to the builder, e.g. `"libx264"`.
    pub value: String,
    /// False when the user's FFmpeg build lacks this (greyed out with reason).
    pub enabled: bool,
    /// Why disabled, e.g. `"not available in your FFmpeg build"`.
    pub disabled_reason: Option<String>,
}

impl SelectOption {
    /// An available option with the same label and value.
    pub fn available(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            label: value.clone(),
            value,
            enabled: true,
            disabled_reason: None,
        }
    }

    /// An available option with a distinct label.
    pub fn labeled(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            enabled: true,
            disabled_reason: None,
        }
    }

    /// An option the user's build cannot use. Still shown (greyed out) so
    /// the user learns it exists — never offered-then-failing (spec §9).
    pub fn unavailable(
        label: impl Into<String>,
        value: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            enabled: false,
            disabled_reason: Some(reason.into()),
        }
    }
}

/// The control rendered for a field.
#[derive(Debug, Clone)]
pub enum FieldKind {
    /// Dropdown cycled with ←/→; skips disabled options.
    Select {
        /// All options; at least one must be enabled.
        options: Vec<SelectOption>,
        /// Index into `options` of the current choice.
        selected: usize,
    },
    /// Numeric range with labeled landmarks (e.g. the CRF scale 18/23/28).
    Slider {
        /// Minimum value.
        min: i64,
        /// Maximum value.
        max: i64,
        /// Current value, always clamped to `[min, max]`.
        value: i64,
        /// Step per ←/→ press.
        step: i64,
        /// `(value, label)` landmarks drawn under the bar.
        landmarks: Vec<(i64, String)>,
        /// Small caption under the bar, e.g. `"smaller file ◂──▸ better quality"`.
        caption: Option<String>,
    },
    /// Two-option switch (e.g. trim fast/accurate, hwaccel opt-in).
    Toggle {
        /// Exactly two choices.
        options: [String; 2],
        /// Which is active (0 or 1).
        selected: usize,
    },
    /// Single-line text backed by `tui-input`.
    Text {
        /// Current contents.
        value: Input,
    },
}

/// One row of the parameter form.
#[derive(Debug, Clone)]
pub struct Field {
    /// Stable id read by builders, e.g. `"crf"`.
    pub id: &'static str,
    /// Row label, e.g. `"Quality (CRF)"`.
    pub label: String,
    /// The control.
    pub kind: FieldKind,
    /// Plain-English teaching text for the explanation pane.
    pub explanation: String,
}

impl Field {
    /// Current value for the builder.
    pub fn value(&self) -> FieldValue {
        match &self.kind {
            FieldKind::Select { options, selected } => FieldValue::Text(
                options
                    .get(*selected)
                    .map(|o| o.value.clone())
                    .unwrap_or_default(),
            ),
            FieldKind::Slider { value, .. } => FieldValue::Int(*value),
            FieldKind::Toggle { selected, .. } => FieldValue::Toggle(*selected),
            FieldKind::Text { value } => FieldValue::Text(value.value().to_string()),
        }
    }

    /// Move a Select/Toggle/slider backward, skipping disabled options.
    pub fn adjust_backward(&mut self) {
        match &mut self.kind {
            FieldKind::Select { options, selected } => {
                *selected = step_option(options, *selected, -1);
            }
            FieldKind::Slider {
                min, value, step, ..
            } => {
                *value = (*value - *step).max(*min);
            }
            FieldKind::Toggle { selected, .. } => {
                *selected = 1 - *selected;
            }
            FieldKind::Text { .. } => {}
        }
    }

    /// Move a Select/Toggle/slider forward, skipping disabled options.
    pub fn adjust_forward(&mut self) {
        match &mut self.kind {
            FieldKind::Select { options, selected } => {
                *selected = step_option(options, *selected, 1);
            }
            FieldKind::Slider {
                max, value, step, ..
            } => {
                *value = (*value + *step).min(*max);
            }
            FieldKind::Toggle { selected, .. } => {
                *selected = 1 - *selected;
            }
            FieldKind::Text { .. } => {}
        }
    }
}

/// Step through select options in `direction`, skipping disabled ones and
/// wrapping. When every option is disabled (should not happen — builders
/// guarantee one enabled choice), the selection does not move.
fn step_option(options: &[SelectOption], current: usize, direction: i32) -> usize {
    if options.is_empty() {
        return 0;
    }
    let n = options.len() as i32;
    let mut next = current as i32;
    for _ in 0..options.len() {
        next = (next + direction).rem_euclid(n);
        if options[next as usize].enabled {
            return next as usize;
        }
    }
    current
}

/// A field's current value, as seen by builders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    /// Select choice or text contents.
    Text(String),
    /// Slider position.
    Int(i64),
    /// Toggle index (0 or 1).
    Toggle(usize),
}

/// Read-only context for `Operation::fields`: what the input looks like and
/// what the user's build can do. No field values — those do not exist yet.
pub struct FieldContext<'a> {
    /// Probe of the (first) input, if ffprobe could read it.
    pub probe: Option<&'a ProbeResult>,
    /// Session capabilities, if detection resolved.
    pub caps: Option<&'a CapabilityReport>,
}

/// Read-only context for `Operation::build`: resolved inputs, output
/// override, probes, capabilities, and current field values.
pub struct BuildContext<'a> {
    /// Selected input files.
    pub inputs: &'a [PathBuf],
    /// User-typed output path. Builders fall back to
    /// [`default_output_name`](crate::ffmpeg::builder::default_output_name)
    /// when this is `None`.
    pub output: Option<&'a PathBuf>,
    /// Probe of the first input, if available.
    pub probe: Option<&'a ProbeResult>,
    /// Ready probes for all inputs in order (may be shorter than `inputs`
    /// when some probes are still pending or failed). Concat compares these
    /// to pick demuxer-vs-filter; other ops ignore the field.
    pub input_probes: Vec<ProbeResult>,
    /// Timeline clips in seconds for trim (§10). Sorted, non-overlapping,
    /// owned by the timeline screen. Only trim reads this; every other
    /// operation ignores it.
    pub clips: Vec<ClipRange>,
    /// Session capabilities, if available.
    pub caps: Option<&'a CapabilityReport>,
    /// Current field values by field id.
    pub fields: HashMap<&'static str, FieldValue>,
}

/// One timeline clip range in seconds: keep `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipRange {
    /// Keep-from timestamp in seconds.
    pub start: f64,
    /// Keep-until timestamp in seconds (exclusive).
    pub end: f64,
}

impl ClipRange {
    /// Length in seconds, never negative.
    pub fn len(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

impl<'a> BuildContext<'a> {
    /// Raw value for `id`, if the field exists.
    pub fn get(&self, id: &str) -> Option<&FieldValue> {
        self.fields.get(id)
    }

    /// Text value for `id`, or `default` when missing/wrong type.
    pub fn get_str(&self, id: &str, default: &str) -> String {
        match self.fields.get(id) {
            Some(FieldValue::Text(text)) => text.clone(),
            _ => default.to_string(),
        }
    }

    /// Integer value for `id`, or `default` when missing/wrong type.
    pub fn get_int(&self, id: &str, default: i64) -> i64 {
        match self.fields.get(id) {
            Some(FieldValue::Int(value)) => *value,
            _ => default,
        }
    }

    /// Toggle index for `id`, or `default` when missing/wrong type.
    pub fn get_toggle(&self, id: &str, default: usize) -> usize {
        match self.fields.get(id) {
            Some(FieldValue::Toggle(selected)) => *selected,
            _ => default,
        }
    }

    /// First input. Builders need at least one; the form guarantees it.
    pub fn first_input(&self) -> Option<&PathBuf> {
        self.inputs.first()
    }
}

/// Collect current values from a field list into a builder-ready map.
pub fn collect_values(fields: &[Field]) -> HashMap<&'static str, FieldValue> {
    fields.iter().map(|f| (f.id, f.value())).collect()
}

/// Index of the first enabled option — the default selection. Falls back to
/// 0 when everything is disabled (builders must guarantee one enabled
/// choice; the form still renders so the failure is visible, not a panic).
pub fn default_selected(options: &[SelectOption]) -> usize {
    options.iter().position(|o| o.enabled).unwrap_or(0)
}

/// Mark `value` unavailable in `options` when `available` is false, keeping
/// it visible-but-greyed with the standard reason (spec §9).
pub fn gate_by_capability(
    options: &mut [SelectOption],
    value: &str,
    available: bool,
    caps_known: bool,
) {
    if available || !caps_known {
        return;
    }
    if let Some(option) = options.iter_mut().find(|o| o.value == value) {
        option.enabled = false;
        option.disabled_reason = Some("not available in your FFmpeg build".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn select(values: &[&str]) -> Field {
        Field {
            id: "codec",
            label: "Codec".into(),
            kind: FieldKind::Select {
                options: values.iter().map(|v| SelectOption::available(*v)).collect(),
                selected: 0,
            },
            explanation: String::new(),
        }
    }

    #[test]
    fn select_cycles_and_wraps() {
        let mut field = select(&["a", "b", "c"]);
        field.adjust_forward();
        field.adjust_forward();
        assert_eq!(field.value(), FieldValue::Text("c".into()));
        field.adjust_forward();
        assert_eq!(field.value(), FieldValue::Text("a".into()));
        field.adjust_backward();
        assert_eq!(field.value(), FieldValue::Text("c".into()));
    }

    #[test]
    fn select_skips_disabled_options() {
        let mut field = select(&["a", "b", "c"]);
        if let FieldKind::Select { options, .. } = &mut field.kind {
            options[1].enabled = false;
        }
        field.adjust_forward();
        assert_eq!(field.value(), FieldValue::Text("c".into()));
    }

    #[test]
    fn select_stuck_when_all_disabled() {
        let mut field = select(&["a"]);
        if let FieldKind::Select { options, .. } = &mut field.kind {
            options[0].enabled = false;
        }
        field.adjust_forward();
        assert_eq!(field.value(), FieldValue::Text("a".into()));
    }

    #[test]
    fn slider_clamps_to_range() {
        let mut field = Field {
            id: "crf",
            label: "CRF".into(),
            kind: FieldKind::Slider {
                min: 0,
                max: 51,
                value: 23,
                step: 1,
                landmarks: Vec::new(),
                caption: None,
            },
            explanation: String::new(),
        };
        for _ in 0..100 {
            field.adjust_forward();
        }
        assert_eq!(field.value(), FieldValue::Int(51));
        for _ in 0..100 {
            field.adjust_backward();
        }
        assert_eq!(field.value(), FieldValue::Int(0));
    }

    #[test]
    fn toggle_flips_between_two() {
        let mut field = Field {
            id: "mode",
            label: "Mode".into(),
            kind: FieldKind::Toggle {
                options: ["Fast".into(), "Accurate".into()],
                selected: 0,
            },
            explanation: String::new(),
        };
        field.adjust_forward();
        assert_eq!(field.value(), FieldValue::Toggle(1));
        field.adjust_backward();
        assert_eq!(field.value(), FieldValue::Toggle(0));
    }

    #[test]
    fn context_getters_fall_back_cleanly() {
        let ctx = BuildContext {
            inputs: &[],
            output: None,
            probe: None,
            input_probes: Vec::new(),
            clips: Vec::new(),
            caps: None,
            fields: HashMap::new(),
        };
        assert_eq!(ctx.get_str("missing", "dflt"), "dflt");
        assert_eq!(ctx.get_int("missing", 23), 23);
        assert_eq!(ctx.get_toggle("missing", 1), 1);
    }
}
