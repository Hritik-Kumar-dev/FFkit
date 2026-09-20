//! Trim: cut a time range, fast or accurate (spec section 4).
//!
//! Exposes the `-ss` before/after `-i` distinction as a two-option toggle:
//! before is fast input seeking (keyframe-aligned with stream copy) while
//! after is frame-accurate but slower (requires re-encode). The preview
//! shows exactly which flag ordering each choice produces — the kind of trap
//! this tool exists to defuse.

use anyhow::{Context, Result};
use tui_input::Input;

use crate::ffmpeg::builder::{
    default_output_name, even_dims_filter, input_extension, push_globals, resolve_output,
    safe_path_arg, CommandSpec,
};
use crate::ops::fields::{
    default_selected, gate_by_capability, BuildContext, Field, FieldContext, FieldKind,
    SelectOption,
};
use crate::ops::{InputKind, Operation};

/// Trim operation.
#[derive(Debug, Default, Clone, Copy)]
pub struct TrimOp;

impl TrimOp {
    /// Static metadata for the operation picker registry.
    pub const META: crate::ops::OperationMeta = crate::ops::OperationMeta {
        id: "trim",
        name: "Trim",
        description: "Cut a time range: fast keyframe cut or accurate re-encode",
        accepts: InputKind::AudioOrVideo,
    };
}

/// Toggle index for fast, keyframe-aligned cutting.
const MODE_FAST: usize = 0;
/// Toggle index for accurate, re-encoded cutting.
const MODE_ACCURATE: usize = 1;

impl Operation for TrimOp {
    fn id(&self) -> &'static str {
        "trim"
    }

    fn name(&self) -> &'static str {
        "Trim"
    }

    fn description(&self) -> &'static str {
        "Cut a time range: fast keyframe cut or accurate re-encode"
    }

    fn accepts(&self) -> InputKind {
        InputKind::AudioOrVideo
    }

    fn fields(&self, ctx: &FieldContext) -> Vec<Field> {
        let caps_known = ctx.caps.is_some();
        let has_encoder = |name: &str| ctx.caps.is_some_and(|c| c.has_encoder(name));
        let mut video_codecs = vec![
            SelectOption::labeled("H.264 (libx264)", "libx264"),
            SelectOption::labeled("H.265 (libx265)", "libx265"),
        ];
        gate_by_capability(
            &mut video_codecs,
            "libx264",
            has_encoder("libx264"),
            caps_known,
        );
        gate_by_capability(
            &mut video_codecs,
            "libx265",
            has_encoder("libx265"),
            caps_known,
        );

        let input = ctx
            .probe
            .map(|p| p.path.clone())
            .unwrap_or_else(|| "input.mp4".into());
        let ext = input_extension(&input);
        let ext = if ext.is_empty() {
            "mp4".to_string()
        } else {
            ext
        };

        vec![
            Field {
                id: "start",
                label: "Start".into(),
                kind: FieldKind::Text {
                    value: Input::new("00:00:00".to_string()),
                },
                explanation: "Where the cut begins (HH:MM:SS or seconds). Empty means the very start.".into(),
            },
            Field {
                id: "end",
                label: "End".into(),
                kind: FieldKind::Text {
                    value: Input::new(String::new()),
                },
                explanation: "Where the cut ends (HH:MM:SS or seconds). Empty means the end of the file.".into(),
            },
            Field {
                id: "mode",
                label: "Seek mode".into(),
                kind: FieldKind::Toggle {
                    options: [
                        "Fast (keyframe-aligned)".to_string(),
                        "Accurate (re-encode)".to_string(),
                    ],
                    selected: MODE_FAST,
                },
                explanation: "Fast puts -ss before -i: instant seeking, but with stream copy the cut lands on the nearest keyframe. Accurate puts -ss after -i: frame-exact, but the video is re-encoded. Watch the flag order change in the preview.".into(),
            },
            Field {
                id: "video_codec",
                label: "Video codec".into(),
                kind: FieldKind::Select {
                    selected: default_selected(&video_codecs),
                    options: video_codecs,
                },
                explanation: "Encoder used in Accurate mode. Ignored in Fast mode, which always stream-copies.".into(),
            },
            Field {
                id: "crf",
                label: "Quality (CRF)".into(),
                kind: FieldKind::Slider {
                    min: 0,
                    max: 51,
                    value: 23,
                    step: 1,
                    landmarks: vec![
                        (18, "visually lossless".into()),
                        (23, "x264 default".into()),
                        (28, "noticeably lossy".into()),
                    ],
                    caption: Some("smaller file ◂──▸ better quality".into()),
                },
                explanation: "Quality for the re-encoded cut in Accurate mode. Ignored in Fast mode.".into(),
            },
            Field {
                id: "output",
                label: "Output".into(),
                kind: FieldKind::Text {
                    value: Input::new(
                        default_output_name(&input, "trimmed", &ext)
                            .to_string_lossy()
                            .into_owned(),
                    ),
                },
                explanation: "Output path.".into(),
            },
        ]
    }

    fn build(&self, ctx: &BuildContext) -> Result<CommandSpec> {
        let input = ctx.first_input().context("trim needs an input file")?;
        let program = ctx
            .caps
            .and_then(|c| c.ffmpeg_path.clone())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "ffmpeg".to_string());

        let start = ctx.get_str("start", "");
        let start = normalize_timestamp(&start);
        let end = ctx.get_str("end", "");
        let end = normalize_timestamp(&end);
        let accurate = ctx.get_toggle("mode", MODE_FAST) == MODE_ACCURATE;

        let mut spec = CommandSpec::new(program);
        push_globals(&mut spec, true);

        if !accurate {
            // Fast: -ss before -i seeks at the input (keyframe-aligned);
            // -c copy keeps it instant and lossless.
            if let Some(start) = start.as_deref() {
                spec.flag_value(
                    "-ss",
                    start,
                    "Seek before the input: fast, jumps to the nearest keyframe.",
                );
            }
            spec.arg("-i");
            spec.arg(safe_path_arg(input));
            if let Some(end) = end.as_deref() {
                spec.flag_value(
                    "-to",
                    end,
                    "Stop writing when the output reaches this timestamp.",
                );
            }
            spec.flag_value(
                "-c",
                "copy",
                "Stream copy: no re-encode, so cuts stay keyframe-aligned.",
            );
        } else {
            // Accurate: -ss after -i decodes up to the timestamp (slow but
            // frame-exact) and the video is re-encoded.
            spec.arg("-i");
            spec.arg(safe_path_arg(input));
            if let Some(start) = start.as_deref() {
                spec.flag_value(
                    "-ss",
                    start,
                    "Seek after the input: decodes everything up to the timestamp, so the cut is frame-accurate.",
                );
            }
            if let Some(end) = end.as_deref() {
                spec.flag_value(
                    "-to",
                    end,
                    "Stop writing when the output reaches this timestamp.",
                );
            }
            let vcodec = ctx.get_str("video_codec", "libx264");
            if let Some(even) = even_dims_filter(ctx.probe) {
                spec.flag_value(
                    "-vf",
                    format!("scale={even}"),
                    "Odd-sized source, which these encoders refuse — shave one pixel edge to even dimensions.",
                );
            }
            spec.flag_value(
                "-c:v",
                vcodec.clone(),
                format!("Re-encode the cut with {vcodec} for frame accuracy."),
            );
            spec.flag_value(
                "-crf",
                ctx.get_int("crf", 23).to_string(),
                "Quality level for the re-encoded cut.",
            );
            spec.flag_value("-c:a", "aac", "Re-encode the audio to stay in sync.");
        }

        let ext = input_extension(input);
        let ext = if ext.is_empty() { "mp4" } else { &ext };
        let output = resolve_output(ctx.output, default_output_name(input, "trimmed", ext));
        spec.arg(safe_path_arg(&output));
        Ok(spec)
    }
}

/// Normalize a user-typed timestamp: empty or all-zero means "unset" (omit
/// the flag); anything else passes through for ffmpeg to parse.
fn normalize_timestamp(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let is_zero = trimmed.chars().all(|c| c == '0' || c == ':' || c == '.');
    if is_zero {
        return None;
    }
    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::normalize_timestamp;

    #[test]
    fn empty_and_zero_timestamps_are_unset() {
        assert_eq!(normalize_timestamp(""), None);
        assert_eq!(normalize_timestamp("  "), None);
        assert_eq!(normalize_timestamp("00:00:00"), None);
        assert_eq!(normalize_timestamp("0"), None);
        assert_eq!(
            normalize_timestamp("00:01:30"),
            Some("00:01:30".to_string())
        );
        assert_eq!(normalize_timestamp("90"), Some("90".to_string()));
    }
}
