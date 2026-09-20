//! Resize: scale resolution with presets or custom size (M5).
//!
//! Presets keep the aspect ratio with even dimensions (`-2`); custom sizes
//! offer keep-aspect (empty dimension becomes `-2`) or exact stretch. The
//! video is always re-encoded (scaling forbids stream copy); the audio is
//! copied untouched — the preview shows that split.

use anyhow::{Context, Result};
use tui_input::Input;

use crate::ffmpeg::builder::{
    default_output_name, input_extension, push_globals, safe_path_arg, CommandSpec,
};
use crate::ops::fields::{
    default_selected, gate_by_capability, BuildContext, Field, FieldContext, FieldKind,
    SelectOption,
};
use crate::ops::{simple_op, InputKind, Operation};

simple_op!(
    ResizeOp,
    "resize",
    "Resize",
    "Scale resolution with presets or custom dimensions",
    InputKind::Video
);

/// (Label, scale value); empty means "keep original" and is never emitted.
const PRESETS: &[(&str, &str)] = &[
    ("1080p", "1920:-2"),
    ("720p", "1280:-2"),
    ("480p", "854:-2"),
    ("Custom…", "custom"),
];

impl Operation for ResizeOp {
    fn id(&self) -> &'static str {
        "resize"
    }

    fn name(&self) -> &'static str {
        "Resize"
    }

    fn description(&self) -> &'static str {
        "Scale resolution with presets or custom dimensions"
    }

    fn accepts(&self) -> InputKind {
        InputKind::Video
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
                id: "preset",
                label: "Size".into(),
                kind: FieldKind::Select {
                    options: PRESETS
                        .iter()
                        .map(|(label, value)| SelectOption::labeled(*label, *value))
                        .collect(),
                    selected: 1,
                },
                explanation: "Downscale presets keep the aspect ratio with even dimensions. Custom uses the width/height below.".into(),
            },
            Field {
                id: "custom_w",
                label: "Custom width".into(),
                kind: FieldKind::Text {
                    value: Input::new("1280".to_string()),
                },
                explanation: "Custom width in pixels (used when Size is Custom). Empty means “scale from height”.".into(),
            },
            Field {
                id: "custom_h",
                label: "Custom height".into(),
                kind: FieldKind::Text {
                    value: Input::new(String::new()),
                },
                explanation: "Custom height in pixels (used when Size is Custom). Empty means “scale from width”.".into(),
            },
            Field {
                id: "aspect",
                label: "Aspect".into(),
                kind: FieldKind::Toggle {
                    options: ["Keep aspect ratio".to_string(), "Stretch to exact".to_string()],
                    selected: 0,
                },
                explanation: "Keep replaces an empty dimension with -2 (aspect-preserving, even). Stretch needs both dimensions and distorts. Watch -vf change.".into(),
            },
            Field {
                id: "crop",
                label: "Crop".into(),
                kind: FieldKind::Select {
                    options: vec![
                        SelectOption::labeled("No crop", ""),
                        SelectOption::labeled("Square (center)", "square"),
                    ],
                    selected: 0,
                },
                explanation: "Center square crop applied before scaling — for social-media squares. Uses crop=min(iw\\,ih):min(iw\\,ih) (the backslash-comma is a literal comma inside the filter).".into(),
            },
            Field {
                id: "video_codec",
                label: "Video codec".into(),
                kind: FieldKind::Select {
                    selected: default_selected(&video_codecs),
                    options: video_codecs,
                },
                explanation: "Scaling forces a re-encode, so stream copy is not offered here.".into(),
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
                explanation: "Quality for the re-encoded, resized video.".into(),
            },
            Field {
                id: "output",
                label: "Output".into(),
                kind: FieldKind::Text {
                    value: Input::new(
                        default_output_name(&input, "720p", &ext)
                            .to_string_lossy()
                            .into_owned(),
                    ),
                },
                explanation: "Output path.".into(),
            },
        ]
    }

    fn build(&self, ctx: &BuildContext) -> Result<CommandSpec> {
        let input = ctx.first_input().context("resize needs an input file")?;
        let program = ctx
            .caps
            .and_then(|c| c.ffmpeg_path.clone())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "ffmpeg".to_string());

        let scale = match ctx.get_str("preset", "1280:-2").as_str() {
            "custom" => {
                let w = ctx.get_str("custom_w", "").trim().to_string();
                let h = ctx.get_str("custom_h", "").trim().to_string();
                let stretch = ctx.get_toggle("aspect", 0) == 1;
                match (w.is_empty(), h.is_empty(), stretch) {
                    (true, true, _) => {
                        return Err(anyhow::anyhow!(
                            "custom size needs at least a width or a height"
                        ));
                    }
                    (_, _, false) => format!(
                        "{}:{}",
                        if w.is_empty() { "-2".to_string() } else { w },
                        if h.is_empty() { "-2".to_string() } else { h }
                    ),
                    (true, false, true) | (false, true, true) => {
                        return Err(anyhow::anyhow!(
                            "stretch needs both width and height — or keep the aspect ratio"
                        ));
                    }
                    (false, false, true) => format!("{w}:{h}"),
                }
            }
            preset => preset.to_string(),
        };

        let mut spec = CommandSpec::new(program);
        push_globals(&mut spec, true);
        spec.arg("-i");
        spec.arg(safe_path_arg(input));

        let vcodec = ctx.get_str("video_codec", "libx264");
        let vf = match ctx.get_str("crop", "").as_str() {
            "square" => format!("crop=min(iw\\,ih):min(iw\\,ih),scale={scale}"),
            _ => format!("scale={scale}"),
        };
        spec.flag_value(
            "-vf",
            vf,
            "Scale the video. -2 keeps the aspect ratio with even dimensions (odd heights are refused by these codecs).",
        );
        spec.flag_value("-c:v", vcodec.clone(), format!("Re-encode with {vcodec}."));
        spec.flag_value(
            "-crf",
            ctx.get_int("crf", 23).to_string(),
            "Quality level for the resized video.",
        );
        spec.flag_value(
            "-c:a",
            "copy",
            "Audio is untouched by a resize — copied, not re-encoded.",
        );

        let output = match ctx.output {
            Some(path) => path.clone(),
            None => {
                let ext = input_extension(input);
                let ext = if ext.is_empty() { "mp4" } else { &ext };
                default_output_name(input, "resized", ext)
            }
        };
        spec.arg(safe_path_arg(&output));
        Ok(spec)
    }
}
