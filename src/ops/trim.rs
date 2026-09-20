//! Trim: cut timeline clips, fast or accurate (§10 timeline rebuild).
//!
//! Ranges come from the visual timeline as [`ClipRange`]s, not text fields:
//! `-ss` before `-i` is fast input seeking (keyframe-aligned with stream
//! copy) while `-ss` after is frame-accurate but slower (requires
//! re-encode). One clip trims directly to the output; several clips each
//! trim to an intermediate and join through the shared concat-demuxer code
//! (§11 reuse, not duplication). The preview shows the full sequence.

use anyhow::{Context, Result};
use tui_input::Input;

use crate::ffmpeg::builder::{
    default_output_name, even_dims_filter, input_extension, push_globals, resolve_output,
    safe_path_arg, CommandSpec, ConcatListFile,
};
use crate::ops::concat::{concat_list_path, demuxer_args, filter_args};
use crate::ops::fields::{
    default_selected, gate_by_capability, BuildContext, ClipRange, Field, FieldContext, FieldKind,
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
        description: "Cut timeline clips: fast keyframe cuts or accurate re-encode",
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
        "Cut timeline clips: fast keyframe cuts or accurate re-encode"
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
                id: "mode",
                label: "Seek mode".into(),
                kind: FieldKind::Toggle {
                    options: [
                        "Fast (keyframe-aligned)".to_string(),
                        "Accurate (re-encode)".to_string(),
                    ],
                    selected: MODE_FAST,
                },
                explanation: "Applies to every clip. Fast puts -ss before -i: instant seeking, but with stream copy cuts land on the nearest keyframe. Accurate puts -ss after -i: frame-exact, but the video is re-encoded. Watch the flag order change in the preview.".into(),
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

        if ctx.clips.is_empty() {
            return Err(anyhow::anyhow!(
                "define at least one clip on the timeline (n creates one)"
            ));
        }
        // Output order follows timeline position, not creation order.
        let mut clips = ctx.clips.clone();
        clips.sort_by(|a, b| {
            a.start
                .partial_cmp(&b.start)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for clip in &clips {
            if !(0.0 <= clip.start && clip.start < clip.end) {
                return Err(anyhow::anyhow!(
                    "clip {}–{} is invalid: start must be before end",
                    format_timestamp(clip.start),
                    format_timestamp(clip.end)
                ));
            }
        }

        let accurate = ctx.get_toggle("mode", MODE_FAST) == MODE_ACCURATE;
        let vcodec = ctx.get_str("video_codec", "libx264");
        let crf = ctx.get_int("crf", 23);
        let ext = input_extension(input);
        let ext = if ext.is_empty() { "mp4" } else { &ext };
        let output = resolve_output(ctx.output, default_output_name(input, "trimmed", ext));

        if clips.len() == 1 {
            // N=1 is the same code path, straight to the final output.
            return Ok(single_clip(
                &program, input, &clips[0], accurate, &vcodec, crf, ctx.probe, &output,
            ));
        }

        // N>1: one trim per clip into intermediates, then join.
        // Accurate segments re-encode to exact timestamps, so the shared
        // demuxer join is valid (verified: exact total). Fast segments are
        // stream copies with ragged source timestamps that the demuxer
        // stacks wrong (measured 7.4s for a 6s join), so they join through
        // the re-encoding concat filter instead — the per-clip cuts stay
        // keyframe-aligned, only the join re-encodes.
        let stem = output
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "trimmed".to_string());
        let clip_ext = output
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_else(|| ext.to_string());
        let mut intermediates = Vec::new();
        let mut spec = CommandSpec::new(program.clone());
        push_globals(&mut spec, true);
        for (i, clip) in clips.iter().enumerate() {
            let name = format!("{stem}_clip{i}.{clip_ext}", i = i + 1);
            let intermediate = match output.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
                _ => std::path::PathBuf::from(name),
            };
            spec.pre_commands.push(single_clip(
                &program,
                input,
                clip,
                accurate,
                &vcodec,
                crf,
                ctx.probe,
                &intermediate,
            ));
            intermediates.push(intermediate);
        }
        if accurate {
            let list_path = concat_list_path(&output);
            spec.concat_list = Some(ConcatListFile {
                path: list_path.clone(),
                inputs: intermediates,
            });
            demuxer_args(
                &mut spec,
                &list_path,
                &format!(
                    "Join {} timeline clips in order — re-encoded to identical settings, so stream copy applies.",
                    clips.len()
                ),
            );
        } else {
            filter_args(
                &mut spec,
                &intermediates,
                &vcodec,
                crf,
                &format!(
                    "Join {} timeline clips in order — the stream-copied segments keep ragged timestamps, so the join re-encodes to line them up.",
                    clips.len()
                ),
            );
        }
        spec.arg(safe_path_arg(&output));
        Ok(spec)
    }
}

/// One clip trim to `output`. The single shared implementation behind both
/// the N=1 direct trim and every N>1 intermediate.
#[allow(clippy::too_many_arguments)]
fn single_clip(
    program: &str,
    input: &std::path::Path,
    clip: &ClipRange,
    accurate: bool,
    vcodec: &str,
    crf: i64,
    probe: Option<&crate::ffmpeg::probe::ProbeResult>,
    output: &std::path::Path,
) -> CommandSpec {
    let mut spec = CommandSpec::new(program);
    push_globals(&mut spec, true);
    let start = format_timestamp(clip.start);
    let end = format_timestamp(clip.end);
    if !accurate {
        // Fast: -ss before -i seeks at the input (keyframe-aligned);
        // `-t` takes the clip LENGTH — `-to` would count on the shifted
        // output timeline and overshoot. -c copy keeps it instant.
        if clip.start > 0.0 {
            spec.flag_value(
                "-ss",
                start,
                "Seek before the input: fast, jumps to the nearest keyframe.",
            );
        }
        spec.arg("-i");
        spec.arg(safe_path_arg(input));
        spec.flag_value(
            "-t",
            format_timestamp(clip.len()),
            "Keep this many seconds from the seek point (a duration, not a timestamp).",
        );
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
        if clip.start > 0.0 {
            spec.flag_value(
                "-ss",
                start,
                "Seek after the input: decodes everything up to the timestamp, so the cut is frame-accurate.",
            );
        }
        spec.flag_value(
            "-to",
            end,
            "Stop writing when the output reaches this timestamp.",
        );
        if let Some(even) = even_dims_filter(probe) {
            spec.flag_value(
                "-vf",
                format!("scale={even}"),
                "Odd-sized source, which these encoders refuse — shave one pixel edge to even dimensions.",
            );
        }
        spec.flag_value(
            "-c:v",
            vcodec,
            format!("Re-encode the cut with {vcodec} for frame accuracy."),
        );
        spec.flag_value(
            "-crf",
            crf.to_string(),
            "Quality level for the re-encoded cut.",
        );
        spec.flag_value("-c:a", "aac", "Re-encode the audio to stay in sync.");
    }
    spec.arg(safe_path_arg(output));
    spec
}

/// Format seconds for ffmpeg: whole seconds bare (`90`), fractions with
/// milliseconds (`41.234`). Unambiguous, locale-independent.
fn format_timestamp(secs: f64) -> String {
    if secs.fract() == 0.0 {
        format!("{}", secs as u64)
    } else {
        format!("{secs:.3}")
    }
}

#[cfg(test)]
mod tests {
    use super::format_timestamp;

    #[test]
    fn timestamps_format_unambiguously() {
        assert_eq!(format_timestamp(90.0), "90");
        assert_eq!(format_timestamp(0.0), "0");
        assert_eq!(format_timestamp(41.234567), "41.235");
    }
}
