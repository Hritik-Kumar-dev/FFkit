//! Thumbnail: grab a single frame (M5).
//!
//! Fast input seeking (`-ss` before `-i`) is fine here — thumbnails do not
//! need frame accuracy — so the grab is instant even on long files.

use anyhow::{Context, Result};
use tui_input::Input;

use crate::ffmpeg::builder::{
    default_output_name, push_globals, resolve_output, safe_path_arg, CommandSpec,
};
use crate::ops::fields::{BuildContext, Field, FieldContext, FieldKind, SelectOption};
use crate::ops::{simple_op, InputKind, Operation};

simple_op!(
    ThumbnailOp,
    "thumbnail",
    "Thumbnail",
    "Grab a single frame at a timestamp",
    InputKind::Video
);

impl Operation for ThumbnailOp {
    fn id(&self) -> &'static str {
        "thumbnail"
    }

    fn name(&self) -> &'static str {
        "Thumbnail"
    }

    fn description(&self) -> &'static str {
        "Grab a single frame at a timestamp"
    }

    fn accepts(&self) -> InputKind {
        InputKind::Video
    }

    fn fields(&self, ctx: &FieldContext) -> Vec<Field> {
        let input = ctx
            .probe
            .map(|p| p.path.clone())
            .unwrap_or_else(|| "input.mp4".into());
        vec![
            Field {
                id: "timestamp",
                label: "Timestamp".into(),
                kind: FieldKind::Text {
                    value: Input::new("00:00:01".to_string()),
                },
                explanation: "Which frame to grab (HH:MM:SS or seconds). Seeks before the input — instant, keyframe-adjacent, plenty for a thumbnail.".into(),
            },
            Field {
                id: "format",
                label: "Format".into(),
                kind: FieldKind::Select {
                    options: vec![
                        SelectOption::labeled("PNG", "png"),
                        SelectOption::labeled("JPEG", "jpg"),
                        SelectOption::labeled("WebP", "webp"),
                    ],
                    selected: 0,
                },
                explanation: "Output image format.".into(),
            },
            Field {
                id: "size",
                label: "Size".into(),
                kind: FieldKind::Select {
                    options: vec![
                        SelectOption::labeled("Original", ""),
                        SelectOption::labeled("640 wide", "640:-2"),
                        SelectOption::labeled("1280 wide", "1280:-2"),
                    ],
                    selected: 0,
                },
                explanation: "Downscale the grabbed frame, preserving aspect with even dimensions.".into(),
            },
            Field {
                id: "output",
                label: "Output".into(),
                kind: FieldKind::Text {
                    value: Input::new(
                        default_output_name(&input, "thumb", "png")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                },
                explanation: "Output path.".into(),
            },
        ]
    }

    fn build(&self, ctx: &BuildContext) -> Result<CommandSpec> {
        let input = ctx.first_input().context("thumbnail needs an input file")?;
        let program = ctx
            .caps
            .and_then(|c| c.ffmpeg_path.clone())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "ffmpeg".to_string());

        let format = ctx.get_str("format", "png");
        let timestamp = ctx.get_str("timestamp", "00:00:01");

        let mut spec = CommandSpec::new(program);
        push_globals(&mut spec, true);
        spec.flag_value(
            "-ss",
            timestamp.trim(),
            "Seek before the input: instant grab, no decoding of the whole file.",
        );
        spec.arg("-i");
        spec.arg(safe_path_arg(input));
        spec.flag_value(
            "-frames:v",
            "1",
            "Write exactly one video frame, then stop.",
        );
        let size = ctx.get_str("size", "");
        if !size.is_empty() {
            spec.flag_value(
                "-vf",
                format!("scale={size}"),
                "Downscale the grabbed frame.",
            );
        }

        let output = resolve_output(ctx.output, default_output_name(input, "thumb", &format));
        spec.arg(safe_path_arg(&output));
        Ok(spec)
    }
}
