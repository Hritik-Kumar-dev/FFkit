//! Image convert: batch image format change (M5).
//!
//! Quality slider maps to each encoder's native scale: WebP takes it
//! directly (`-quality`), JPEG takes inverted `-q:v` (lower is better —
//! the explanation says so), PNG ignores it (lossless).

use anyhow::{Context, Result};
use tui_input::Input;

use crate::ffmpeg::builder::{
    default_output_name, push_globals, resolve_output, safe_path_arg, CommandSpec,
};
use crate::ops::fields::{
    default_selected, gate_by_capability, BuildContext, Field, FieldContext, FieldKind,
    SelectOption,
};
use crate::ops::{simple_op, InputKind, Operation};

simple_op!(
    ImageConvertOp,
    "image-convert",
    "Convert images",
    "Batch image format change with quality and resize",
    InputKind::Image
);

/// (Label, extension); encoder mapping lives in [`quality_args`].
const FORMATS: &[(&str, &str)] = &[("PNG", "png"), ("JPEG", "jpg"), ("WebP", "webp")];

/// Explicit encoder per format. Emitted as `-c:v` on every build so the
/// bytes on disk always match the selected format — never whatever encoder
/// the output file's extension happens to imply.
fn encoder_for(format: &str) -> &'static str {
    match format {
        "jpg" => "mjpeg",
        "webp" => "libwebp",
        _ => "png",
    }
}

/// Quality flags for `quality` in 1..=100, or `None` when the format is
/// lossless and the slider does not apply.
fn quality_args(format: &str, quality: i64) -> Option<(String, String)> {
    let quality = quality.clamp(1, 100);
    match format {
        // -q:v is inverted: 2 is best, 31 worst.
        "jpg" => Some((
            "-q:v".to_string(),
            (31.0 - (quality as f64 / 100.0) * 29.0).round().to_string(),
        )),
        "webp" => Some(("-quality".to_string(), quality.to_string())),
        _ => None,
    }
}

impl Operation for ImageConvertOp {
    fn id(&self) -> &'static str {
        "image-convert"
    }

    fn name(&self) -> &'static str {
        "Convert images"
    }

    fn description(&self) -> &'static str {
        "Batch image format change with quality and resize"
    }

    fn accepts(&self) -> InputKind {
        InputKind::Image
    }

    fn fields(&self, ctx: &FieldContext) -> Vec<Field> {
        let caps_known = ctx.caps.is_some();
        let has_encoder = |name: &str| ctx.caps.is_some_and(|c| c.has_encoder(name));
        let mut formats: Vec<SelectOption> = FORMATS
            .iter()
            .map(|(label, value)| SelectOption::labeled(*label, *value))
            .collect();
        // JPEG/MJPEG, PNG, and WebP encoders are native; gate on the off
        // chance of a minimal build.
        gate_by_capability(&mut formats, "jpg", has_encoder("mjpeg"), caps_known);
        gate_by_capability(&mut formats, "png", has_encoder("png"), caps_known);
        gate_by_capability(&mut formats, "webp", has_encoder("libwebp"), caps_known);

        let input = ctx
            .probe
            .map(|p| p.path.clone())
            .unwrap_or_else(|| "photo.png".into());
        // The output default follows the default-selected format so the
        // two agree out of the box (a stale pairing here is exactly how
        // "the format didn't change" reports happen).
        let default_format = formats
            .get(default_selected(&formats))
            .map(|o| o.value.clone())
            .unwrap_or_else(|| "jpg".to_string());

        vec![
            Field {
                id: "format",
                label: "Target format".into(),
                kind: FieldKind::Select {
                    selected: default_selected(&formats),
                    options: formats,
                },
                explanation: "Output image format. Keep the Output extension in sync with it — the bytes always follow this choice (-c:v), but a mismatched extension lies about them.".into(),
            },
            Field {
                id: "quality",
                label: "Quality".into(),
                kind: FieldKind::Slider {
                    min: 1,
                    max: 100,
                    value: 85,
                    step: 1,
                    landmarks: vec![
                        (60, "small".into()),
                        (85, "balanced".into()),
                        (100, "max".into()),
                    ],
                    caption: Some("smaller file ◂──▸ better quality".into()),
                },
                explanation: "Quality 1–100. WebP takes it directly; JPEG maps to inverted -q:v (lower is better); PNG ignores it (lossless).".into(),
            },
            Field {
                id: "resize",
                label: "Resize".into(),
                kind: FieldKind::Select {
                    options: vec![
                        SelectOption::labeled("Keep original", ""),
                        SelectOption::labeled("1920 wide", "1920:-2"),
                        SelectOption::labeled("1280 wide", "1280:-2"),
                        SelectOption::labeled("800 wide", "800:-2"),
                    ],
                    selected: 0,
                },
                explanation: "Downscale, preserving aspect with even dimensions.".into(),
            },
            Field {
                id: "output",
                label: "Output".into(),
                kind: FieldKind::Text {
                    value: Input::new(
                        default_output_name(&input, "converted", &default_format)
                            .to_string_lossy()
                            .into_owned(),
                    ),
                },
                explanation: "Output path for single files; batch naming uses the queue template in M6.".into(),
            },
        ]
    }

    fn build(&self, ctx: &BuildContext) -> Result<CommandSpec> {
        let input = ctx
            .first_input()
            .context("image-convert needs an input file")?;
        let program = ctx
            .caps
            .and_then(|c| c.ffmpeg_path.clone())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "ffmpeg".to_string());

        let format = ctx.get_str("format", "jpg");
        let mut spec = CommandSpec::new(program);
        push_globals(&mut spec, true);
        spec.arg("-i");
        spec.arg(safe_path_arg(input));

        let resize = ctx.get_str("resize", "");
        if !resize.is_empty() {
            spec.flag_value(
                "-vf",
                format!("scale={resize}"),
                "Downscale, preserving aspect with even dimensions.",
            );
        }
        let encoder = encoder_for(&format);
        spec.flag_value(
            "-c:v",
            encoder,
            format!("Image encoder for {format} — the bytes follow this choice, not the filename."),
        );
        match quality_args(&format, ctx.get_int("quality", 85)) {
            Some((flag, value)) => {
                spec.flag_value(
                    &flag,
                    value,
                    "Quality for the lossy encoder (JPEG -q:v is inverted: lower is better).",
                );
            }
            None => {
                spec.explanation
                    .push(crate::ffmpeg::builder::FlagExplanation {
                        flag: "(no quality flag)".to_string(),
                        plain_english: "PNG is lossless — the quality slider does not apply."
                            .to_string(),
                    });
            }
        }

        let output = resolve_output(ctx.output, default_output_name(input, "converted", &format));
        spec.arg(safe_path_arg(&output));
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::quality_args;

    #[test]
    fn jpeg_quality_maps_inverted() {
        // q=100 → best (2), q=1 → worst (31).
        assert_eq!(
            quality_args("jpg", 100),
            Some(("-q:v".to_string(), "2".to_string()))
        );
        assert_eq!(
            quality_args("jpg", 1),
            Some(("-q:v".to_string(), "31".to_string()))
        );
    }

    #[test]
    fn webp_takes_quality_directly_and_png_ignores_it() {
        assert_eq!(
            quality_args("webp", 80),
            Some(("-quality".to_string(), "80".to_string()))
        );
        assert_eq!(quality_args("png", 80), None);
    }
}
