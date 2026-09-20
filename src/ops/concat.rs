//! Concat: join multiple files (M5).
//!
//! The method is auto-selected by comparing probes across inputs. The
//! `concat` demuxer with a list file serves identically-encoded inputs
//! (fast stream copy); the `concat` filter serves mismatched inputs
//! (requires re-encode). The builder cannot do I/O, so demuxer mode
//! attaches a [`ConcatListFile`](crate::ffmpeg::builder::ConcatListFile)
//! descriptor that the runner materializes before the main command runs.
//! The preview names which path was chosen and why; incomplete probes fall
//! back to the always-working re-encode.

use anyhow::Result;
use tui_input::Input;

use crate::ffmpeg::builder::{
    push_globals, resolve_output, safe_path_arg, CommandSpec, ConcatListFile,
};
use crate::ops::fields::{BuildContext, Field, FieldContext, FieldKind, SelectOption};
use crate::ops::{simple_op, InputKind, Operation};

simple_op!(
    ConcatOp,
    "concat",
    "Join videos",
    "Concatenate files (stream-copy when possible)",
    InputKind::Multiple
);

/// Comparable encoding signature of one input.
#[derive(Debug, PartialEq, Eq)]
struct Signature {
    video_codec: Option<String>,
    audio_codec: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

impl Signature {
    fn of(probe: &crate::ffmpeg::probe::ProbeResult) -> Self {
        let video = probe.video_stream();
        Self {
            video_codec: probe.video_codec().map(str::to_string),
            audio_codec: probe.audio_codec().map(str::to_string),
            width: video.and_then(|v| v.width),
            height: video.and_then(|v| v.height),
        }
    }

    fn describe(&self) -> String {
        format!(
            "{}/{} {}x{}",
            self.video_codec.as_deref().unwrap_or("?"),
            self.audio_codec.as_deref().unwrap_or("?"),
            self.width
                .map(|w| w.to_string())
                .unwrap_or_else(|| "?".into()),
            self.height
                .map(|h| h.to_string())
                .unwrap_or_else(|| "?".into()),
        )
    }
}

impl Operation for ConcatOp {
    fn id(&self) -> &'static str {
        "concat"
    }

    fn name(&self) -> &'static str {
        "Join videos"
    }

    fn description(&self) -> &'static str {
        "Concatenate files (stream-copy when possible)"
    }

    fn accepts(&self) -> InputKind {
        InputKind::Multiple
    }

    fn fields(&self, _ctx: &FieldContext) -> Vec<Field> {
        vec![
            Field {
                id: "method",
                label: "Method".into(),
                kind: FieldKind::Select {
                    options: vec![
                        SelectOption::labeled("Auto (recommended)", "auto"),
                        SelectOption::labeled("Stream copy (demuxer)", "demuxer"),
                        SelectOption::labeled("Re-encode (filter)", "filter"),
                    ],
                    selected: 0,
                },
                explanation: "Auto compares every input: identical codec, size, and audio → demuxer with stream copy (fast). Anything else → concat filter with re-encode (always works). Forcing demuxer on mismatched files fails — ffmpeg says so loudly.".into(),
            },
            Field {
                id: "output",
                label: "Output".into(),
                kind: FieldKind::Text {
                    value: Input::new("joined.mp4".to_string()),
                },
                explanation: "Joined output path.".into(),
            },
        ]
    }

    fn build(&self, ctx: &BuildContext) -> Result<CommandSpec> {
        if ctx.inputs.len() < 2 {
            return Err(anyhow::anyhow!(
                "concat needs at least two inputs — pick more files with Space"
            ));
        }
        let program = ctx
            .caps
            .and_then(|c| c.ffmpeg_path.clone())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "ffmpeg".to_string());

        let output = resolve_output(ctx.output, std::path::PathBuf::from("joined.mp4"));

        let method = ctx.get_str("method", "auto");
        let demuxer = match method.as_str() {
            "demuxer" => true,
            "filter" => false,
            _ => {
                // Auto: demuxer only when every input is probed AND all
                // signatures match. Anything unknown → safe re-encode.
                ctx.input_probes.len() == ctx.inputs.len() && signatures_match(&ctx.input_probes)
            }
        };

        let mut spec = CommandSpec::new(program);
        push_globals(&mut spec, true);
        if demuxer {
            let list_path = std::env::temp_dir().join(format!(
                "ffkit-concat-{}.txt",
                output
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "join".to_string())
            ));
            spec.concat_list = Some(ConcatListFile {
                path: list_path.clone(),
                inputs: ctx.inputs.to_vec(),
            });
            spec.arg("-f");
            spec.arg("concat");
            spec.arg("-safe");
            spec.arg("0");
            spec.arg("-i");
            spec.arg(safe_path_arg(&list_path));
            spec.flag_value(
                "-c",
                "copy",
                format!(
                    "Stream copy through the concat demuxer — {}.",
                    demuxer_reason(&ctx.input_probes, method == "demuxer")
                ),
            );
        } else {
            let (filter, has_video, has_audio) = concat_filter(ctx)?;
            for input in ctx.inputs {
                spec.arg("-i");
                spec.arg(safe_path_arg(input));
            }
            spec.flag_value(
                "-filter_complex",
                filter,
                format!(
                    "Concat filter: re-encode to normalize inputs. {}",
                    filter_reason(ctx)
                ),
            );
            if has_video {
                spec.arg("-map");
                spec.arg("[outv]");
                spec.flag_value("-c:v", "libx264", "Re-encoded join video.");
                spec.flag_value("-crf", "23", "Quality for the joined video.");
            }
            if has_audio {
                spec.arg("-map");
                spec.arg("[outa]");
                spec.flag_value("-c:a", "aac", "Re-encoded join audio.");
            }
        }
        spec.arg(safe_path_arg(&output));
        Ok(spec)
    }
}

/// True when every probed input shares codec, audio, and dimensions.
fn signatures_match(probes: &[crate::ffmpeg::probe::ProbeResult]) -> bool {
    let mut signatures = probes.iter().map(Signature::of);
    match signatures.next() {
        None => false,
        Some(first) => signatures.all(|s| s == first),
    }
}

/// Human reason for the demuxer path, shown in the preview explanation.
fn demuxer_reason(probes: &[crate::ffmpeg::probe::ProbeResult], forced: bool) -> String {
    if forced {
        return "forced demuxer mode — fails loudly on mismatched files".to_string();
    }
    match probes.first() {
        Some(first) => format!(
            "all {} inputs are {} — no re-encode needed",
            probes.len(),
            Signature::of(first).describe()
        ),
        None => "inputs unprobed but demuxer forced".to_string(),
    }
}

/// Human reason for the filter path.
fn filter_reason(ctx: &BuildContext) -> String {
    if ctx.get_str("method", "auto") == "filter" {
        return "Forced re-encode mode.".to_string();
    }
    if ctx.input_probes.len() != ctx.inputs.len() {
        return "Not all inputs are probed yet — re-encoding to be safe.".to_string();
    }
    "Inputs differ in codec, size, or audio — the demuxer would refuse them.".to_string()
}

/// Build the concat filter expression plus whether it carries video/audio.
/// Mixed stream types (some inputs video, some audio-only) are a loud error:
// the filter needs matching segments.
fn concat_filter(ctx: &BuildContext) -> Result<(String, bool, bool)> {
    let complete = ctx.input_probes.len() == ctx.inputs.len();
    let mut kinds = Vec::new();
    for i in 0..ctx.inputs.len() {
        let (has_video, has_audio) = if complete {
            let probe = &ctx.input_probes[i];
            (probe.has_video(), probe.has_audio())
        } else {
            // Unknown inputs are assumed to carry both; the explanation says so.
            (true, true)
        };
        kinds.push((has_video, has_audio));
    }
    let has_video = kinds.iter().any(|(v, _)| *v);
    let has_audio = kinds.iter().any(|(_, a)| *a);
    let mixed_video = kinds.iter().any(|(v, _)| *v) && kinds.iter().any(|(v, _)| !*v);
    let mixed_audio = kinds.iter().any(|(_, a)| *a) && kinds.iter().any(|(_, a)| !*a);
    if mixed_video || mixed_audio {
        return Err(anyhow::anyhow!(
            "inputs mix different stream types (some with video/audio, some without) — normalize them first (e.g. convert each to MP4), then join"
        ));
    }
    let mut filter = String::new();
    for (i, (has_video, has_audio)) in kinds.iter().enumerate() {
        if *has_video {
            filter.push_str(&format!("[{i}:v]"));
        }
        if *has_audio {
            filter.push_str(&format!("[{i}:a]"));
        }
    }
    filter.push_str(&format!(
        "concat=n={}:v={}:a={}[outv][outa]",
        ctx.inputs.len(),
        u8::from(has_video),
        u8::from(has_audio)
    ));
    Ok((filter, has_video, has_audio))
}
