//! Named parameter presets, built-in and user-saved (spec section 11).
//!
//! A preset names an operation plus field values by field id:
//! selects match by option value, sliders take integers, toggles take
//! 0/1 (or booleans), text takes strings. Unknown ids and unparseable
//! values are ignored — presets from newer versions never break older
//! forms. Parameter values are [`toml::Value`] so any TOML scalar parses;
//! floats round, everything else falls back gracefully.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tui_input::Input;

use crate::ops::fields::{Field, FieldKind};

/// One preset: an operation plus field values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Preset {
    /// Display name, e.g. `"Web MP4"`.
    pub name: String,
    /// Operation id this preset applies to, e.g. `"convert"`.
    pub operation: String,
    /// One-liner shown in the picker popup.
    #[serde(default)]
    pub description: String,
    /// Field id → value.
    #[serde(default)]
    pub params: HashMap<String, toml::Value>,
}

/// The five built-in presets from the spec: web-optimized MP4, archival
/// quality, small-for-messaging, audio-only extraction, social-media
/// square crop.
pub fn builtin_presets() -> Vec<Preset> {
    vec![
        Preset {
            name: "Web MP4".to_string(),
            operation: "convert".to_string(),
            description: "H.264 + faststart — plays everywhere".to_string(),
            params: params([
                ("format", text("mp4")),
                ("video_codec", text("libx264")),
                ("audio_codec", text("aac")),
                ("crf", int(23)),
                ("faststart", boolean(true)),
            ]),
        },
        Preset {
            name: "Archival Quality".to_string(),
            operation: "compress".to_string(),
            description: "CRF 18, slow — big files, no regrets".to_string(),
            params: params([
                ("video_codec", text("libx264")),
                ("crf", int(18)),
                ("preset", text("slow")),
                ("audio_bitrate", text("192k")),
            ]),
        },
        Preset {
            name: "Small for Messaging".to_string(),
            operation: "compress".to_string(),
            description: "CRF 28, 720p, 96k audio — tiny files".to_string(),
            params: params([
                ("video_codec", text("libx264")),
                ("crf", int(28)),
                ("preset", text("veryfast")),
                ("resolution", text("1280:-2")),
                ("audio_bitrate", text("96k")),
            ]),
        },
        Preset {
            name: "Audio Only".to_string(),
            operation: "extract-audio".to_string(),
            description: "MP3 at 192k".to_string(),
            params: params([("format", text("mp3")), ("bitrate", text("192k"))]),
        },
        Preset {
            name: "Square Social Clip".to_string(),
            operation: "resize".to_string(),
            description: "Center square crop at 720p".to_string(),
            params: params([
                ("preset", text("1280:-2")),
                ("crop", text("square")),
                ("video_codec", text("libx264")),
                ("crf", int(23)),
            ]),
        },
    ]
}

/// Snapshot the current form fields into a preset (the "save preset"
/// action). Selects store option values, sliders integers, toggles 0/1,
/// text the raw string — the same shapes [`apply_preset`] reads back.
pub fn preset_from_fields(name: String, operation: String, fields: &[Field]) -> Preset {
    let mut params = HashMap::new();
    for field in fields {
        let value = match &field.kind {
            FieldKind::Select { options, selected } => options
                .get(*selected)
                .map(|o| toml::Value::String(o.value.clone())),
            FieldKind::Slider { value, .. } => Some(toml::Value::Integer(*value)),
            FieldKind::Toggle { selected, .. } => Some(toml::Value::Integer(*selected as i64)),
            FieldKind::Text { value } => Some(toml::Value::String(value.value().to_string())),
        };
        if let Some(value) = value {
            params.insert(field.id.to_string(), value);
        }
    }
    Preset {
        name,
        operation,
        description: "Saved from the form".to_string(),
        params,
    }
}

/// Apply a preset to live fields with per-kind coercion. Never fails —
/// anything inapplicable is skipped so the form stays usable.
pub fn apply_preset(fields: &mut [Field], preset: &Preset) {
    for field in fields.iter_mut() {
        let Some(value) = preset.params.get(field.id) else {
            continue;
        };
        match &mut field.kind {
            FieldKind::Select { options, selected } => {
                if let Some(target) = as_string(value) {
                    if let Some(index) = options.iter().position(|o| o.enabled && o.value == target)
                    {
                        *selected = index;
                    }
                }
            }
            FieldKind::Slider {
                min,
                max,
                value: current,
                ..
            } => {
                if let Some(number) = as_int(value) {
                    *current = number.clamp(*min, *max);
                }
            }
            FieldKind::Toggle { selected, .. } => {
                if let Some(index) = as_toggle(value) {
                    *selected = index;
                }
            }
            FieldKind::Text { value: input } => {
                if let Some(text) = as_string(value) {
                    *input = Input::new(text);
                }
            }
        }
    }
}

fn params<const N: usize>(entries: [(&str, toml::Value); N]) -> HashMap<String, toml::Value> {
    entries
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

fn text(value: &str) -> toml::Value {
    toml::Value::String(value.to_string())
}

fn int(value: i64) -> toml::Value {
    toml::Value::Integer(value)
}

fn boolean(value: bool) -> toml::Value {
    toml::Value::Boolean(value)
}

/// String coercion: strings verbatim, integers/bools rendered.
fn as_string(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(text) => Some(text.clone()),
        toml::Value::Integer(number) => Some(number.to_string()),
        toml::Value::Boolean(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Integer coercion: integers, numeric strings, floats (rounded).
/// Booleans are NOT integers here — a `true` CRF would be nonsense.
fn as_int(value: &toml::Value) -> Option<i64> {
    match value {
        toml::Value::Integer(number) => Some(*number),
        toml::Value::String(text) => text.trim().parse().ok(),
        toml::Value::Float(number) => Some(number.round() as i64),
        _ => None,
    }
}

/// Toggle coercion: 0/1 integers, booleans (true → first option),
/// and `"1"`/`"true"`/`"on"` strings. Anything else is ignored.
fn as_toggle(value: &toml::Value) -> Option<usize> {
    match value {
        toml::Value::Integer(0) => Some(0),
        toml::Value::Integer(1) => Some(1),
        toml::Value::Boolean(true) => Some(0),
        toml::Value::Boolean(false) => Some(1),
        toml::Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "off" => Some(1),
            "1" | "true" | "on" => Some(0),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::fields::{FieldKind, SelectOption};

    fn select_field() -> Field {
        Field {
            id: "video_codec",
            label: "Codec".into(),
            kind: FieldKind::Select {
                options: vec![
                    SelectOption::available("libx264"),
                    SelectOption::available("libx265"),
                ],
                selected: 0,
            },
            explanation: String::new(),
        }
    }

    #[test]
    fn select_applies_by_value_and_ignores_unknowns() {
        let mut fields = vec![select_field()];
        apply_preset(
            &mut fields,
            &Preset {
                name: "x".into(),
                operation: "compress".into(),
                description: String::new(),
                params: params([("video_codec", text("libx265")), ("nope", text("1"))]),
            },
        );
        assert_eq!(
            fields[0].value(),
            crate::ops::fields::FieldValue::Text("libx265".into())
        );
        // Unknown value keeps the current selection.
        apply_preset(
            &mut fields,
            &Preset {
                name: "x".into(),
                operation: "compress".into(),
                description: String::new(),
                params: params([("video_codec", text("av1"))]),
            },
        );
        assert_eq!(
            fields[0].value(),
            crate::ops::fields::FieldValue::Text("libx265".into())
        );
    }

    #[test]
    fn slider_clamps_and_parses_strings() {
        let mut fields = vec![Field {
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
        }];
        apply_preset(
            &mut fields,
            &Preset {
                name: "x".into(),
                operation: "compress".into(),
                description: String::new(),
                params: params([("crf", int(999))]),
            },
        );
        assert_eq!(fields[0].value(), crate::ops::fields::FieldValue::Int(51));
    }

    #[test]
    fn toggle_reads_bools_and_ints() {
        let mut fields = vec![Field {
            id: "mode",
            label: "Mode".into(),
            kind: FieldKind::Toggle {
                options: ["A".into(), "B".into()],
                selected: 0,
            },
            explanation: String::new(),
        }];
        let preset = Preset {
            name: "x".into(),
            operation: "y".into(),
            description: String::new(),
            params: params([("mode", boolean(false))]),
        };
        apply_preset(&mut fields, &preset);
        assert_eq!(fields[0].value(), crate::ops::fields::FieldValue::Toggle(1));
    }

    #[test]
    fn round_trip_through_preset_from_fields() {
        let fields = vec![select_field()];
        let preset = preset_from_fields("Mine".into(), "compress".into(), &fields);
        assert_eq!(preset.params.get("video_codec"), Some(&text("libx264")));
        let mut fresh = vec![select_field()];
        apply_preset(&mut fresh, &preset);
        assert_eq!(fresh[0].value(), fields[0].value());
    }

    #[test]
    fn builtin_presets_reference_real_operations() {
        for preset in builtin_presets() {
            assert!(
                crate::ops::find_by_id(&preset.operation).is_some(),
                "builtin {} references unknown op {}",
                preset.name,
                preset.operation
            );
            assert!(
                !preset.params.is_empty(),
                "builtin {} is empty",
                preset.name
            );
        }
    }
}
