//! Compress: shrink file size, CRF-first (spec section 4).
//!
//! Defaults to CRF (quality-targeted) over bitrate, with the scale explained
//! inline: lower is better quality and larger file; ~18 visually lossless,
//! 23 the x264 default, 28 noticeably lossy. When the parameters would not
//! transform the video stream at all, the form offers stream copy explicitly
//! — never a silent re-encode, never a silent copy: the preview always shows
//! which one the command does.

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

/// Compress operation.
#[derive(Debug, Default, Clone, Copy)]
pub struct CompressOp;

impl CompressOp {
    /// Static metadata for the operation picker registry.
    pub const META: crate::ops::OperationMeta = crate::ops::OperationMeta {
        id: "compress",
        name: "Compress video",
        description: "Shrink file size with a quality slider (CRF)",
        accepts: InputKind::Video,
    };
}

impl Operation for CompressOp {
    fn id(&self) -> &'static str {
        "compress"
    }

    fn name(&self) -> &'static str {
        "Compress video"
    }

    fn description(&self) -> &'static str {
        "Shrink file size with a quality slider (CRF)"
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
            SelectOption::labeled("H.264 NVENC (GPU, faster but larger)", "h264_nvenc"),
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
        gate_by_capability(
            &mut video_codecs,
            "h264_nvenc",
            has_encoder("h264_nvenc"),
            caps_known,
        );

        let mut audio_codecs = vec![
            SelectOption::labeled("AAC", "aac"),
            SelectOption::labeled("Opus (libopus)", "libopus"),
            SelectOption::labeled("Copy audio stream", "copy"),
        ];
        gate_by_capability(&mut audio_codecs, "aac", has_encoder("aac"), caps_known);
        gate_by_capability(
            &mut audio_codecs,
            "libopus",
            has_encoder("libopus"),
            caps_known,
        );

        // Offer stream copy when it is genuinely available: same codec as the
        // probe and no scaling. The default is explicit in the form, and the
        // preview shows `-c:v copy` — never a silent decision.
        let copy_available = ctx.probe.is_some_and(|p| {
            p.video_codec() == Some("libx264")
                || p.video_codec() == Some("libx265")
                || p.video_codec() == Some("h264")
                || p.video_codec() == Some("hevc")
        });
        let video_mode = if copy_available { 1 } else { 0 };

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
                id: "video_codec",
                label: "Video codec".into(),
                kind: FieldKind::Select {
                    selected: default_selected(&video_codecs),
                    options: video_codecs,
                },
                explanation: "Encoder for the video stream. libx264 is the safe default; libx265 is ~25% smaller but slower; NVENC uses your GPU (much faster, larger files).".into(),
            },
            Field {
                id: "video_mode",
                label: "Video".into(),
                kind: FieldKind::Toggle {
                    options: [
                        "Re-encode".to_string(),
                        "Copy stream (faster, no quality loss)".to_string(),
                    ],
                    selected: video_mode,
                },
                explanation: "Copy passes the video through untouched — only possible when the codec already matches and no scaling is applied. Watch the preview switch between -c:v libx264 and -c:v copy.".into(),
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
                explanation: "Constant Rate Factor: quality-targeted encoding. Lower means better quality and a larger file. Only applies when re-encoding (ignored with stream copy).".into(),
            },
            Field {
                id: "preset",
                label: "Preset".into(),
                kind: FieldKind::Select {
                    options: ["ultrafast", "veryfast", "faster", "fast", "medium", "slow", "slower", "veryslow"]
                        .iter()
                        .map(|p| SelectOption::available(*p))
                        .collect(),
                    selected: 4,
                },
                explanation: "Speed vs compression tradeoff. Slower squeezes more quality per byte but takes longer. Ignored by NVENC (it has its own presets) and by stream copy.".into(),
            },
            Field {
                id: "audio_codec",
                label: "Audio".into(),
                kind: FieldKind::Select {
                    selected: default_selected(&audio_codecs),
                    options: audio_codecs,
                },
                explanation: "Audio encoder. Copy avoids re-encoding when the source audio is already fine.".into(),
            },
            Field {
                id: "audio_bitrate",
                label: "Audio bitrate".into(),
                kind: FieldKind::Select {
                    options: ["96k", "128k", "160k", "192k"]
                        .iter()
                        .map(|b| SelectOption::available(*b))
                        .collect(),
                    selected: 1,
                },
                explanation: "Audio bitrate. 128k AAC is transparent for most listening; ignored with audio stream copy.".into(),
            },
            Field {
                id: "resolution",
                label: "Resolution".into(),
                kind: FieldKind::Select {
                    options: vec![
                        SelectOption::labeled("Keep original", ""),
                        SelectOption::labeled("1080p", "1920:-2"),
                        SelectOption::labeled("720p", "1280:-2"),
                        SelectOption::labeled("480p", "854:-2"),
                    ],
                    selected: 0,
                },
                explanation: "Downscale with -vf scale. The -2 keeps the aspect ratio with even dimensions (this codec family refuses odd heights). Scaling forces a re-encode.".into(),
            },
            Field {
                id: "output",
                label: "Output".into(),
                kind: FieldKind::Text {
                    value: Input::new(
                        default_output_name(&input, "compressed", &ext)
                            .to_string_lossy()
                            .into_owned(),
                    ),
                },
                explanation: "Output path. ffkit checks existence and asks before overwriting, then passes -y so ffmpeg never blocks on its own prompt.".into(),
            },
        ]
    }

    fn build(&self, ctx: &BuildContext) -> Result<CommandSpec> {
        let input = ctx.first_input().context("compress needs an input file")?;
        let program = ctx
            .caps
            .and_then(|c| c.ffmpeg_path.clone())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "ffmpeg".to_string());

        let mut spec = CommandSpec::new(program);
        push_globals(&mut spec, true);
        spec.arg("-i");
        spec.arg(safe_path_arg(input));

        let vcodec = ctx.get_str("video_codec", "libx264");
        let scale = ctx.get_str("resolution", "");
        let copy_stream = ctx.get_toggle("video_mode", 0) == 1;

        if copy_stream {
            spec.flag_value(
                "-c:v",
                "copy",
                "Stream copy: the video is passed through without re-encoding — much faster, zero quality loss.",
            );
        } else {
            spec.flag_value(
                "-c:v",
                vcodec.clone(),
                format!("Re-encode the video with {vcodec}."),
            );
            if vcodec == "h264_nvenc" {
                // NVENC uses constant-quality mode, not CRF — same slider, honest flag.
                spec.flag_value(
                    "-cq",
                    ctx.get_int("crf", 23).to_string(),
                    "Constant-quality level for NVENC (its equivalent of CRF).",
                );
            } else {
                spec.flag_value(
                    "-crf",
                    ctx.get_int("crf", 23).to_string(),
                    "Quality level — lower means better quality and a larger file.",
                );
                spec.flag_value(
                    "-preset",
                    ctx.get_str("preset", "medium"),
                    "Speed vs compression tradeoff for the software encoder.",
                );
            }
            if !scale.is_empty() {
                spec.flag_value(
                    "-vf",
                    format!("scale={scale}"),
                    "Downscale the video, preserving aspect ratio with even dimensions.",
                );
            } else if let Some(even) = even_dims_filter(ctx.probe) {
                // Odd-sized sources (phone video, screencasts) would make
                // x264/x265 refuse the encode — shave the odd edge instead.
                let dims = ctx
                    .probe
                    .and_then(|p| p.video_stream())
                    .map(|v| format!("{}x{}", v.width.unwrap_or(0), v.height.unwrap_or(0)))
                    .unwrap_or_else(|| "odd-sized".to_string());
                spec.flag_value(
                    "-vf",
                    format!("scale={even}"),
                    format!(
                        "Source is {dims}, which these encoders refuse — shave one pixel edge to even dimensions."
                    ),
                );
            }
        }

        match ctx.get_str("audio_codec", "aac").as_str() {
            "copy" => {
                spec.flag_value(
                    "-c:a",
                    "copy",
                    "Audio stream copy: no re-encode, no quality loss.",
                );
            }
            acodec => {
                spec.flag_value(
                    "-c:a",
                    acodec,
                    format!("Re-encode the audio with {acodec}."),
                );
                spec.flag_value(
                    "-b:a",
                    ctx.get_str("audio_bitrate", "128k"),
                    "Audio bitrate.",
                );
            }
        }

        let ext = input_extension(input);
        let ext = if ext.is_empty() { "mp4" } else { &ext };
        let output = resolve_output(ctx.output, default_output_name(input, "compressed", ext));
        spec.arg(safe_path_arg(&output));
        Ok(spec)
    }
}
