//! Convert: change container and/or codec (spec section 4).

use anyhow::{Context, Result};
use tui_input::Input;

use crate::ffmpeg::builder::{default_output_name, push_globals, safe_path_arg, CommandSpec};
use crate::ops::fields::{
    default_selected, gate_by_capability, BuildContext, Field, FieldContext, FieldKind,
    SelectOption,
};
use crate::ops::{InputKind, Operation};

/// Convert operation.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConvertOp;

impl ConvertOp {
    /// Static metadata for the operation picker registry.
    pub const META: crate::ops::OperationMeta = crate::ops::OperationMeta {
        id: "convert",
        name: "Convert",
        description: "Change container or codec (e.g. MKV to MP4)",
        accepts: InputKind::AudioOrVideo,
    };
}

impl Operation for ConvertOp {
    fn id(&self) -> &'static str {
        "convert"
    }

    fn name(&self) -> &'static str {
        "Convert"
    }

    fn description(&self) -> &'static str {
        "Change container or codec (e.g. MKV to MP4)"
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
            SelectOption::labeled("Copy video stream", "copy"),
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

        let input = ctx
            .probe
            .map(|p| p.path.clone())
            .unwrap_or_else(|| "input.mkv".into());

        vec![
            Field {
                id: "format",
                label: "Target format".into(),
                kind: FieldKind::Select {
                    options: vec![
                        SelectOption::labeled("MP4", "mp4"),
                        SelectOption::labeled("Matroska (MKV)", "mkv"),
                        SelectOption::labeled("WebM", "webm"),
                        SelectOption::labeled("QuickTime (MOV)", "mov"),
                    ],
                    selected: 0,
                },
                explanation: "Output container. MP4 plays everywhere; MKV holds anything; WebM is the web streaming default.".into(),
            },
            Field {
                id: "video_codec",
                label: "Video codec".into(),
                kind: FieldKind::Select {
                    selected: default_selected(&video_codecs),
                    options: video_codecs,
                },
                explanation: "Video encoder, or stream copy when the codec is already compatible with the new container.".into(),
            },
            Field {
                id: "audio_codec",
                label: "Audio codec".into(),
                kind: FieldKind::Select {
                    selected: default_selected(&audio_codecs),
                    options: audio_codecs,
                },
                explanation: "Audio encoder, or stream copy to avoid a needless re-encode.".into(),
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
                explanation: "Quality for the re-encoded streams. Ignored for any stream set to copy.".into(),
            },
            Field {
                id: "faststart",
                label: "Web faststart".into(),
                kind: FieldKind::Toggle {
                    options: ["On".to_string(), "Off".to_string()],
                    selected: 0,
                },
                explanation: "Moves the MP4/MOV index (moov atom) to the front so playback starts before the download finishes. Only applies to MP4 and MOV.".into(),
            },
            Field {
                id: "output",
                label: "Output".into(),
                kind: FieldKind::Text {
                    value: Input::new(
                        default_output_name(&input, "converted", "mp4")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                },
                explanation: "Output path; the extension follows the target format.".into(),
            },
        ]
    }

    fn build(&self, ctx: &BuildContext) -> Result<CommandSpec> {
        let input = ctx.first_input().context("convert needs an input file")?;
        let program = ctx
            .caps
            .and_then(|c| c.ffmpeg_path.clone())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "ffmpeg".to_string());

        let format = ctx.get_str("format", "mp4");
        let mut spec = CommandSpec::new(program);
        push_globals(&mut spec, true);
        spec.arg("-i");
        spec.arg(safe_path_arg(input));

        match ctx.get_str("video_codec", "libx264").as_str() {
            "copy" => {
                spec.flag_value("-c:v", "copy", "Video stream copy into the new container.");
            }
            vcodec => {
                spec.flag_value("-c:v", vcodec, format!("Re-encode video with {vcodec}."));
                spec.flag_value(
                    "-crf",
                    ctx.get_int("crf", 23).to_string(),
                    "Quality level for the re-encoded video.",
                );
            }
        }
        match ctx.get_str("audio_codec", "aac").as_str() {
            "copy" => {
                spec.flag_value("-c:a", "copy", "Audio stream copy into the new container.");
            }
            acodec => {
                spec.flag_value("-c:a", acodec, format!("Re-encode audio with {acodec}."));
            }
        }
        if (format == "mp4" || format == "mov") && ctx.get_toggle("faststart", 0) == 0 {
            spec.flag_value(
                "-movflags",
                "+faststart",
                "Move the index to the front so web playback starts immediately.",
            );
        }

        let output = match ctx.output {
            Some(path) => path.clone(),
            None => default_output_name(input, "converted", &format),
        };
        spec.arg(safe_path_arg(&output));
        Ok(spec)
    }
}
