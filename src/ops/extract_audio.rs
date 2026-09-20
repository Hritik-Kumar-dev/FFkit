//! Extract audio: pull an audio track out of video (M5).
//!
//! Codec follows the target format; "Copy original audio" passes the stream
//! through untouched when the source codec already matches — the preview
//! shows which one the command does, never a silent choice.

use anyhow::{Context, Result};
use tui_input::Input;

use crate::ffmpeg::builder::{default_output_name, push_globals, safe_path_arg, CommandSpec};
use crate::ops::fields::{
    default_selected, gate_by_capability, BuildContext, Field, FieldContext, FieldKind,
    SelectOption,
};
use crate::ops::{simple_op, InputKind, Operation};

simple_op!(
    ExtractAudioOp,
    "extract-audio",
    "Extract audio",
    "Pull the audio track out (MP3, AAC, Opus, …)",
    InputKind::Video
);

/// (Target format id, extension, encoder, probe codec name for copy.)
const FORMATS: &[(&str, &str, &str, &str)] = &[
    ("MP3 (libmp3lame)", "mp3", "libmp3lame", "mp3"),
    ("M4A (AAC)", "m4a", "aac", "aac"),
    ("Opus", "opus", "libopus", "opus"),
    ("FLAC (lossless)", "flac", "flac", "flac"),
    ("WAV (uncompressed)", "wav", "pcm_s16le", "pcm_s16le"),
];

impl Operation for ExtractAudioOp {
    fn id(&self) -> &'static str {
        "extract-audio"
    }

    fn name(&self) -> &'static str {
        "Extract audio"
    }

    fn description(&self) -> &'static str {
        "Pull the audio track out (MP3, AAC, Opus, …)"
    }

    fn accepts(&self) -> InputKind {
        InputKind::Video
    }

    fn fields(&self, ctx: &FieldContext) -> Vec<Field> {
        let caps_known = ctx.caps.is_some();
        let has_encoder = |name: &str| ctx.caps.is_some_and(|c| c.has_encoder(name));

        let mut formats: Vec<SelectOption> = FORMATS
            .iter()
            .map(|(label, value, _, _)| SelectOption::labeled(*label, *value))
            .collect();
        for (_, value, encoder, _) in FORMATS {
            gate_by_capability(&mut formats, value, has_encoder(encoder), caps_known);
        }
        // Copy is only offered when the probe shows a matching source codec;
        // without a probe the option stays hidden rather than failing later.
        let copy_available = ctx.probe.is_some_and(|p| {
            p.audio_codec()
                .is_some_and(|codec| FORMATS.iter().any(|(_, _, _, probe)| *probe == codec))
        });
        if copy_available {
            formats.push(SelectOption::labeled(
                "Copy original audio (no re-encode)",
                "copy",
            ));
        }

        // One option per audio stream so the user picks the track, not an
        // index they have to look up.
        let mut tracks = match ctx.probe {
            Some(probe) => {
                let audio: Vec<_> = probe
                    .streams
                    .iter()
                    .filter(|s| s.codec_type == Some(crate::ffmpeg::probe::StreamType::Audio))
                    .collect();
                if audio.is_empty() {
                    vec![SelectOption::labeled("Default audio track", "0")]
                } else {
                    audio
                        .iter()
                        .enumerate()
                        .map(|(i, s)| {
                            let codec = s.codec_name.as_deref().unwrap_or("?");
                            let lang = s
                                .language
                                .as_deref()
                                .map(|l| format!(", {l}"))
                                .unwrap_or_default();
                            SelectOption::labeled(
                                format!("Track {} ({codec}{lang})", i + 1),
                                i.to_string(),
                            )
                        })
                        .collect()
                }
            }
            None => vec![SelectOption::labeled("Default audio track", "0")],
        };
        if tracks.is_empty() {
            tracks.push(SelectOption::labeled("Default audio track", "0"));
        }

        let input = ctx
            .probe
            .map(|p| p.path.clone())
            .unwrap_or_else(|| "input.mp4".into());

        vec![
            Field {
                id: "format",
                label: "Format".into(),
                kind: FieldKind::Select {
                    selected: default_selected(&formats),
                    options: formats,
                },
                explanation: "Output audio format; the encoder follows the container. Copy passes matching source audio through untouched.".into(),
            },
            Field {
                id: "track",
                label: "Audio track".into(),
                kind: FieldKind::Select {
                    selected: 0,
                    options: tracks,
                },
                explanation: "Which audio stream to extract, mapped with -map 0:a:N.".into(),
            },
            Field {
                id: "bitrate",
                label: "Bitrate".into(),
                kind: FieldKind::Select {
                    options: ["96k", "128k", "192k", "320k"]
                        .iter()
                        .map(|b| SelectOption::available(*b))
                        .collect(),
                    selected: 1,
                },
                explanation: "Audio bitrate. Ignored for FLAC/WAV (lossless) and for stream copy.".into(),
            },
            Field {
                id: "output",
                label: "Output".into(),
                kind: FieldKind::Text {
                    value: Input::new(
                        default_output_name(&input, "audio", "m4a")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                },
                explanation: "Output path.".into(),
            },
        ]
    }

    fn build(&self, ctx: &BuildContext) -> Result<CommandSpec> {
        let input = ctx
            .first_input()
            .context("extract-audio needs an input file")?;
        let program = ctx
            .caps
            .and_then(|c| c.ffmpeg_path.clone())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "ffmpeg".to_string());

        let format = ctx.get_str("format", "m4a");
        let (ext, encoder, probe_codec) = FORMATS
            .iter()
            .find(|(_, value, _, _)| *value == format)
            .map(|(_, ext, encoder, probe)| (*ext, *encoder, *probe))
            .unwrap_or(("m4a", "aac", "aac"));

        let mut spec = CommandSpec::new(program);
        push_globals(&mut spec, true);
        spec.arg("-i");
        spec.arg(safe_path_arg(input));
        spec.flag_value(
            "-map",
            format!("0:a:{}", ctx.get_str("track", "0")),
            "Select the audio stream to extract.",
        );

        // Copy when explicitly chosen (offered only when the probe matched)
        // or when the source codec genuinely matches the target — the
        // preview always shows which one the command does.
        let source_matches = ctx
            .probe
            .and_then(|p| p.audio_codec())
            .is_some_and(|codec| codec == probe_codec);
        if format == "copy" || source_matches {
            spec.flag_value(
                "-c:a",
                "copy",
                "Audio stream copy: the source codec already matches — no re-encode.",
            );
        } else {
            spec.flag_value("-c:a", encoder, format!("Encode audio with {encoder}."));
            if !matches!(encoder, "flac" | "pcm_s16le") {
                spec.flag_value("-b:a", ctx.get_str("bitrate", "128k"), "Audio bitrate.");
            }
        }

        let output = match ctx.output {
            Some(path) => path.clone(),
            None => default_output_name(input, "audio", ext),
        };
        spec.arg(safe_path_arg(&output));
        Ok(spec)
    }
}
