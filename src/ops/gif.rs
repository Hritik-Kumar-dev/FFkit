//! GIF: video to animated GIF via two-pass palette (M5).
//!
//! Naive single-pass GIF output looks terrible, so the builder emits two
//! commands: `palettegen` first (as a pre-command the runner executes), then
//! `paletteuse`. The preview shows both — the teaching moment is the
//! palette itself.

use anyhow::{Context, Result};
use tui_input::Input;

use crate::ffmpeg::builder::{
    default_output_name, push_globals, resolve_output, safe_path_arg, CommandSpec,
};
use crate::ops::fields::{BuildContext, Field, FieldContext, FieldKind, SelectOption};
use crate::ops::{simple_op, InputKind, Operation};

simple_op!(
    GifOp,
    "gif",
    "Video to GIF",
    "Animated GIF with palette generation (two-pass, high quality)",
    InputKind::Video
);

impl Operation for GifOp {
    fn id(&self) -> &'static str {
        "gif"
    }

    fn name(&self) -> &'static str {
        "Video to GIF"
    }

    fn description(&self) -> &'static str {
        "Animated GIF with palette generation (two-pass, high quality)"
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
                id: "fps",
                label: "FPS".into(),
                kind: FieldKind::Slider {
                    min: 1,
                    max: 30,
                    value: 12,
                    step: 1,
                    landmarks: vec![
                        (10, "small".into()),
                        (15, "smooth".into()),
                        (24, "large".into()),
                    ],
                    caption: Some("smaller file ◂──▸ smoother motion".into()),
                },
                explanation: "GIF frames per second. 10–15 is the sweet spot; GIF has no inter-frame compression, so every fps costs bytes.".into(),
            },
            Field {
                id: "width",
                label: "Width".into(),
                kind: FieldKind::Select {
                    options: ["320", "480", "640"]
                        .iter()
                        .map(|w| SelectOption::available(*w))
                        .collect(),
                    selected: 0,
                },
                explanation: "Output width in pixels; height follows the aspect ratio. GIFs are rarely shown large — 320–480 keeps files sane.".into(),
            },
            Field {
                id: "dither",
                label: "Dithering".into(),
                kind: FieldKind::Select {
                    options: vec![
                        SelectOption::labeled("Bayer (sharp, patterned)", "bayer:bayer_scale=5"),
                        SelectOption::labeled("Sierra (smooth gradients)", "sierra2_4a"),
                        SelectOption::labeled("None (flat, banding)", "none"),
                    ],
                    selected: 0,
                },
                explanation: "How paletteuse fakes colors outside the 256-color palette. Bayer is crisp; Sierra blends gradients better.".into(),
            },
            Field {
                id: "loop",
                label: "Loop".into(),
                kind: FieldKind::Toggle {
                    options: ["Forever".to_string(), "Once".to_string()],
                    selected: 0,
                },
                explanation: "GIF loop count: 0 loops forever, -1 plays once.".into(),
            },
            Field {
                id: "output",
                label: "Output".into(),
                kind: FieldKind::Text {
                    value: Input::new(
                        default_output_name(&input, "anim", "gif")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                },
                explanation: "Output path. A {stem}_palette.png appears next to it — the generated palette, kept so you can see what the second pass used.".into(),
            },
        ]
    }

    fn build(&self, ctx: &BuildContext) -> Result<CommandSpec> {
        let input = ctx.first_input().context("gif needs an input file")?;
        let program = ctx
            .caps
            .and_then(|c| c.ffmpeg_path.clone())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "ffmpeg".to_string());

        let fps = ctx.get_int("fps", 12).clamp(1, 30);
        let width = ctx.get_str("width", "320");
        let dither = ctx.get_str("dither", "bayer:bayer_scale=5");
        let loop_arg = if ctx.get_toggle("loop", 0) == 0 {
            "0"
        } else {
            "-1"
        };

        let output = resolve_output(ctx.output, default_output_name(input, "anim", "gif"));
        // Kept next to the output so the preview names a real file.
        let palette_name = format!(
            "{}_palette.png",
            output
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "palette".to_string())
        );
        let palette = match output.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.join(palette_name),
            _ => std::path::PathBuf::from(palette_name),
        };

        // Pass 1: learn the 256 colors that matter for this clip.
        let mut gen = CommandSpec::new(program.clone());
        push_globals(&mut gen, true);
        gen.arg("-i");
        gen.arg(safe_path_arg(input));
        gen.flag_value(
            "-vf",
            format!("fps={fps},scale={width}:-1:flags=lanczos,palettegen"),
            "First pass: analyze the clip and write the optimal 256-color palette.",
        );
        gen.arg(safe_path_arg(&palette));

        // Pass 2: render the GIF through that palette.
        let mut spec = CommandSpec::new(program);
        push_globals(&mut spec, true);
        spec.arg("-i");
        spec.arg(safe_path_arg(input));
        spec.arg("-i");
        spec.arg(safe_path_arg(&palette));
        spec.flag_value(
            "-filter_complex",
            format!(
                "fps={fps},scale={width}:-1:flags=lanczos[x];[x][1:v]paletteuse=dither={dither}"
            ),
            "Second pass: map every frame to the palette. Single-pass GIFs skip this and look posterized — this is the step that makes GIFs look good.",
        );
        spec.flag_value("-loop", loop_arg, "Loop forever (0) or play once (-1).");
        spec.arg(safe_path_arg(&output));
        spec.pre_commands.push(gen);
        Ok(spec)
    }
}
