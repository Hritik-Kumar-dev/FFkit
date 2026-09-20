//! Frames: video to image sequence (M5).
//!
//! Two extraction modes: frames-per-second (`-vf fps=N`) or every-Nth-frame
//! (`select=not(mod(n\,N))` — the backslash-comma is a literal comma inside
//! the filter expression, not a filter separator).

use anyhow::{Context, Result};
use std::path::PathBuf;
use tui_input::Input;

use crate::ffmpeg::builder::{push_globals, resolve_output, safe_path_arg, CommandSpec};
use crate::ops::fields::{BuildContext, Field, FieldContext, FieldKind, SelectOption};
use crate::ops::{simple_op, InputKind, Operation};

simple_op!(
    FramesOp,
    "frames",
    "Extract frames",
    "Video to image sequence (FPS or every-Nth-frame)",
    InputKind::Video
);

/// Toggle index for fps mode.
const MODE_FPS: usize = 0;
/// Toggle index for every-Nth-frame mode.
const MODE_NTH: usize = 1;

impl Operation for FramesOp {
    fn id(&self) -> &'static str {
        "frames"
    }

    fn name(&self) -> &'static str {
        "Extract frames"
    }

    fn description(&self) -> &'static str {
        "Video to image sequence (FPS or every-Nth-frame)"
    }

    fn accepts(&self) -> InputKind {
        InputKind::Video
    }

    fn fields(&self, _ctx: &FieldContext) -> Vec<Field> {
        vec![
            Field {
                id: "mode",
                label: "Mode".into(),
                kind: FieldKind::Toggle {
                    options: ["Frames per second".to_string(), "Every Nth frame".to_string()],
                    selected: MODE_FPS,
                },
                explanation: "FPS extracts on a time grid; Nth-frame extracts on a frame grid (exact frames, uneven times for VFR).".into(),
            },
            Field {
                id: "value",
                label: "Rate / N".into(),
                kind: FieldKind::Text {
                    value: Input::new("1".to_string()),
                },
                explanation: "FPS value (e.g. 1 = one frame per second) or N for every-Nth-frame.".into(),
            },
            Field {
                id: "format",
                label: "Format".into(),
                kind: FieldKind::Select {
                    options: vec![
                        SelectOption::labeled("PNG", "png"),
                        SelectOption::labeled("JPEG", "jpg"),
                    ],
                    selected: 0,
                },
                explanation: "Image format per frame.".into(),
            },
            Field {
                id: "pattern",
                label: "Naming".into(),
                kind: FieldKind::Text {
                    value: Input::new("{stem}_frame_%04d".to_string()),
                },
                explanation: "Output name pattern. {stem} becomes the input name; %04d numbers the frames (required — one file per frame).".into(),
            },
            Field {
                id: "output",
                label: "Output".into(),
                kind: FieldKind::Text {
                    value: Input::new(String::new()),
                },
                explanation: "Optional: a full output pattern overriding Naming (same {stem} and %04d rules). Empty uses Naming next to the input.".into(),
            },
        ]
    }

    fn build(&self, ctx: &BuildContext) -> Result<CommandSpec> {
        let input = ctx.first_input().context("frames needs an input file")?;
        let program = ctx
            .caps
            .and_then(|c| c.ffmpeg_path.clone())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "ffmpeg".to_string());

        let format = ctx.get_str("format", "png");
        let value = ctx.get_str("value", "1").trim().to_string();
        if value.is_empty() || value.parse::<f64>().is_err() {
            return Err(anyhow::anyhow!("rate/N must be a number, got {value:?}"));
        }
        let filter = if ctx.get_toggle("mode", MODE_FPS) == MODE_NTH {
            let n = value.parse::<u64>().map_err(|_| {
                anyhow::anyhow!("every-Nth-frame mode needs an integer N, got {value:?}")
            })?;
            if n == 0 {
                return Err(anyhow::anyhow!("N must be at least 1"));
            }
            format!("select=not(mod(n\\,{n})),setpts=N/FRAME_RATE/TB")
        } else {
            format!("fps={value}")
        };

        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "frames".to_string());
        let pattern = ctx.get_str("pattern", "{stem}_frame_%04d");
        let pattern = pattern.replace("{stem}", &stem);
        if !pattern.contains("%0") && !pattern.contains('%') {
            return Err(anyhow::anyhow!(
                "naming pattern needs a %04d-style number — otherwise every frame overwrites the same file"
            ));
        }
        let file_name = format!("{pattern}.{format}");
        let default_out = match input.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.join(&file_name),
            _ => PathBuf::from(&file_name),
        };
        let output = match ctx.output {
            Some(path) if !path.as_os_str().is_empty() => {
                let text = path.to_string_lossy().replace("{stem}", &stem);
                // Extensionless overrides gain the format extension —
                // same rule as every other operation.
                resolve_output(Some(&PathBuf::from(text)), default_out)
            }
            _ => default_out,
        };
        // The output Text field doubles as the pattern override; an explicit
        // ctx.output from snapshots wins over the pattern field.

        let mut spec = CommandSpec::new(program);
        push_globals(&mut spec, true);
        spec.arg("-i");
        spec.arg(safe_path_arg(input));
        spec.flag_value(
            "-vf",
            filter,
            "Frame selection: time grid (fps) or frame grid (select).",
        );
        spec.arg(safe_path_arg(&output));
        Ok(spec)
    }
}
