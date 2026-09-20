//! M5 operation snapshot tests: exact argv for the eight operations that
//! landed in M5 (extract-audio, resize, gif, thumbnail, frames, subtitles,
//! image-convert, concat). Compress/convert/trim live in
//! `tests/builder_snapshots.rs`.

use std::collections::HashMap;
use std::path::PathBuf;

use ffkit::ops::fields::{BuildContext, FieldValue};
use ffkit::ops::{operation_for, OPERATIONS};

/// Owned test inputs that can lend a borrowed [`BuildContext`] view.
struct OwnedCtx {
    inputs: Vec<PathBuf>,
    output: Option<PathBuf>,
    fields: HashMap<&'static str, FieldValue>,
    probes: Vec<ffkit::ffmpeg::probe::ProbeResult>,
}

impl OwnedCtx {
    fn new(inputs: &[&str], output: Option<&str>, fields: &[(&'static str, FieldValue)]) -> Self {
        Self {
            inputs: inputs.iter().map(PathBuf::from).collect(),
            output: output.map(PathBuf::from),
            fields: fields.iter().cloned().collect(),
            probes: Vec::new(),
        }
    }

    /// Attach ready probes (in input order) for ops that compare inputs.
    fn with_probes(mut self, probes: Vec<ffkit::ffmpeg::probe::ProbeResult>) -> Self {
        self.probes = probes;
        self
    }

    /// Context with the first probe attached (drives copy decisions).
    fn view_full(&self) -> BuildContext<'_> {
        BuildContext {
            inputs: &self.inputs,
            output: self.output.as_ref(),
            probe: self.probes.first(),
            input_probes: self.probes.clone(),
            caps: None,
            fields: self.fields.clone(),
        }
    }

    fn view(&self) -> BuildContext<'_> {
        self.view_full()
    }
}

fn text(value: &str) -> FieldValue {
    FieldValue::Text(value.to_string())
}

/// Minimal video+audio probe for matching tests.
fn probe_video_audio(path: &str, vcodec: &str, acodec: &str) -> ffkit::ffmpeg::probe::ProbeResult {
    probe_video_audio_dims(path, vcodec, acodec, 1280, 720)
}

/// Minimal probe with explicit dimensions (odd sizes reproduce §4).
fn probe_video_audio_dims(
    path: &str,
    vcodec: &str,
    acodec: &str,
    width: u32,
    height: u32,
) -> ffkit::ffmpeg::probe::ProbeResult {
    use ffkit::ffmpeg::probe::{ProbeResult, StreamInfo, StreamType};
    ProbeResult {
        path: PathBuf::from(path),
        duration: None,
        size_bytes: None,
        format_name: Some("mp4".to_string()),
        bit_rate_bps: None,
        streams: vec![
            StreamInfo {
                codec_type: Some(StreamType::Video),
                codec_name: Some(vcodec.to_string()),
                width: Some(width),
                height: Some(height),
                ..StreamInfo::default()
            },
            StreamInfo {
                codec_type: Some(StreamType::Audio),
                codec_name: Some(acodec.to_string()),
                ..StreamInfo::default()
            },
        ],
    }
}

#[test]
fn extract_audio_mp3_reencodes() {
    let op = operation_for("extract-audio").expect("op exists");
    let owned = OwnedCtx::new(
        &["clip.mp4"],
        Some("clip_audio.mp3"),
        &[
            ("format", text("mp3")),
            ("track", text("0")),
            ("bitrate", text("192k")),
        ],
    );
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert_eq!(
        spec.args,
        vec![
            "-hide_banner",
            "-y",
            "-progress",
            "pipe:1",
            "-nostats",
            "-i",
            "clip.mp4",
            "-map",
            "0:a:0",
            "-c:a",
            "libmp3lame",
            "-b:a",
            "192k",
            "clip_audio.mp3",
        ]
    );
}

#[test]
fn extract_audio_copies_matching_source() {
    let op = operation_for("extract-audio").expect("op exists");
    let owned = OwnedCtx::new(
        &["clip.mp4"],
        Some("clip_audio.m4a"),
        &[
            ("format", text("m4a")),
            ("track", text("0")),
            ("bitrate", text("128k")),
        ],
    )
    .with_probes(vec![probe_video_audio("clip.mp4", "h264", "aac")]);
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert!(spec.args.contains(&"-c:a".to_string()));
    assert!(spec.args.contains(&"copy".to_string()));
    assert!(
        !spec.args.iter().any(|a| a == "-b:a"),
        "copy needs no bitrate, got {:?}",
        spec.args
    );
}

#[test]
fn resize_preset_scales_with_audio_copy() {
    let op = operation_for("resize").expect("op exists");
    let owned = OwnedCtx::new(
        &["clip.mp4"],
        Some("clip_720p.mp4"),
        &[
            ("preset", text("1280:-2")),
            ("custom_w", text("")),
            ("custom_h", text("")),
            ("aspect", FieldValue::Toggle(0)),
            ("video_codec", text("libx264")),
            ("crf", FieldValue::Int(23)),
        ],
    );
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert_eq!(
        spec.args,
        vec![
            "-hide_banner",
            "-y",
            "-progress",
            "pipe:1",
            "-nostats",
            "-i",
            "clip.mp4",
            "-vf",
            "scale=1280:-2",
            "-c:v",
            "libx264",
            "-crf",
            "23",
            "-c:a",
            "copy",
            "clip_720p.mp4",
        ]
    );
}

#[test]
fn resize_custom_needs_dimensions() {
    let op = operation_for("resize").expect("op exists");
    let owned = OwnedCtx::new(
        &["clip.mp4"],
        None,
        &[
            ("preset", text("custom")),
            ("custom_w", text("")),
            ("custom_h", text("")),
            ("aspect", FieldValue::Toggle(0)),
            ("video_codec", text("libx264")),
            ("crf", FieldValue::Int(23)),
        ],
    );
    op.build(&owned.view())
        .expect_err("empty custom size must fail loudly");
}

#[test]
fn gif_emits_palettegen_then_paletteuse() {
    let op = operation_for("gif").expect("op exists");
    let owned = OwnedCtx::new(
        &["clip.mp4"],
        Some("clip_anim.gif"),
        &[
            ("fps", FieldValue::Int(12)),
            ("width", text("320")),
            ("dither", text("bayer:bayer_scale=5")),
            ("loop", FieldValue::Toggle(0)),
        ],
    );
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert_eq!(spec.pre_commands.len(), 1, "palettegen pass first");
    assert!(
        spec.pre_commands[0]
            .args
            .iter()
            .any(|a| a.contains("palettegen")),
        "got {:?}",
        spec.pre_commands[0].args
    );
    let main: Vec<&str> = spec.args.iter().map(String::as_str).collect();
    assert!(main.contains(&"-filter_complex"));
    let complex = &spec.args[main.iter().position(|a| *a == "-filter_complex").unwrap() + 1];
    assert!(complex.contains("paletteuse"), "{complex}");
    assert!(spec.args.contains(&"-loop".to_string()));
    assert_eq!(spec.args.last().map(String::as_str), Some("clip_anim.gif"));
}

#[test]
fn thumbnail_seeks_before_input_for_speed() {
    let op = operation_for("thumbnail").expect("op exists");
    let owned = OwnedCtx::new(
        &["clip.mp4"],
        Some("thumb.png"),
        &[
            ("timestamp", text("00:00:05")),
            ("format", text("png")),
            ("size", text("")),
        ],
    );
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert_eq!(
        spec.args,
        vec![
            "-hide_banner",
            "-y",
            "-progress",
            "pipe:1",
            "-nostats",
            "-ss",
            "00:00:05",
            "-i",
            "clip.mp4",
            "-frames:v",
            "1",
            "thumb.png",
        ]
    );
}

#[test]
fn frames_fps_mode_and_naming_pattern() {
    let op = operation_for("frames").expect("op exists");
    let owned = OwnedCtx::new(
        &["clip.mp4"],
        None,
        &[
            ("mode", FieldValue::Toggle(0)),
            ("value", text("2")),
            ("format", text("png")),
            ("pattern", text("{stem}_frame_%04d")),
        ],
    );
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert!(spec.args.contains(&"fps=2".to_string()), "{:?}", spec.args);
    assert!(
        spec.args.contains(&"clip_frame_%04d.png".to_string()),
        "{:?}",
        spec.args
    );
}

#[test]
fn frames_nth_mode_uses_select_filter() {
    let op = operation_for("frames").expect("op exists");
    let owned = OwnedCtx::new(
        &["clip.mp4"],
        None,
        &[
            ("mode", FieldValue::Toggle(1)),
            ("value", text("25")),
            ("format", text("jpg")),
            ("pattern", text("{stem}_%04d")),
        ],
    );
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert!(
        spec.args
            .contains(&r"select=not(mod(n\,25)),setpts=N/FRAME_RATE/TB".to_string()),
        "{:?}",
        spec.args
    );
}

#[test]
fn frames_rejects_patterns_without_a_number() {
    let op = operation_for("frames").expect("op exists");
    let owned = OwnedCtx::new(
        &["clip.mp4"],
        None,
        &[
            ("mode", FieldValue::Toggle(0)),
            ("value", text("1")),
            ("format", text("png")),
            ("pattern", text("still")),
        ],
    );
    op.build(&owned.view())
        .expect_err("missing %04d must fail loudly");
}

#[test]
fn subtitles_burn_forces_reencode() {
    let op = operation_for("subtitles").expect("op exists");
    let owned = OwnedCtx::new(
        &["clip.mp4"],
        Some("sub.mp4"),
        &[
            ("subs", text("subs.srt")),
            ("mode", FieldValue::Toggle(0)),
            ("video_codec", text("libx264")),
            ("crf", FieldValue::Int(23)),
        ],
    );
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert!(spec.args.contains(&"-vf".to_string()));
    let vf = &spec.args[spec.args.iter().position(|a| a == "-vf").unwrap() + 1];
    assert!(vf.starts_with("subtitles="), "{vf}");
    assert!(spec.args.contains(&"-c:v".to_string()));
}

#[test]
fn subtitles_mux_copies_everything() {
    let op = operation_for("subtitles").expect("op exists");
    let owned = OwnedCtx::new(
        &["clip.mp4"],
        Some("sub.mp4"),
        &[
            ("subs", text("subs.srt")),
            ("mode", FieldValue::Toggle(1)),
            ("video_codec", text("libx264")),
            ("crf", FieldValue::Int(23)),
        ],
    );
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert!(spec.args.contains(&"-c:s".to_string()));
    assert!(spec.args.contains(&"mov_text".to_string()));
    assert!(
        !spec.args.contains(&"-c:v".to_string()),
        "mux must not re-encode video: {:?}",
        spec.args
    );
}

#[test]
fn subtitles_requires_a_subtitle_file() {
    let op = operation_for("subtitles").expect("op exists");
    let owned = OwnedCtx::new(
        &["clip.mp4"],
        None,
        &[("subs", text("")), ("mode", FieldValue::Toggle(0))],
    );
    op.build(&owned.view())
        .expect_err("empty subs must fail loudly");
}

#[test]
fn image_convert_jpeg_maps_quality() {
    let op = operation_for("image-convert").expect("op exists");
    let owned = OwnedCtx::new(
        &["photo.png"],
        Some("photo_converted.jpg"),
        &[
            ("format", text("jpg")),
            ("quality", FieldValue::Int(90)),
            ("resize", text("")),
        ],
    );
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert!(spec.args.contains(&"-q:v".to_string()), "{:?}", spec.args);
}

#[test]
fn concat_demuxer_for_matching_inputs() {
    let op = operation_for("concat").expect("op exists");
    let owned = OwnedCtx::new(
        &["a.mp4", "b.mp4"],
        Some("joined.mp4"),
        &[("method", text("auto"))],
    )
    .with_probes(vec![
        probe_video_audio("a.mp4", "h264", "aac"),
        probe_video_audio("b.mp4", "h264", "aac"),
    ]);
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert!(spec.args.contains(&"concat".to_string()));
    assert!(spec.args.contains(&"-c".to_string()));
    assert!(spec.args.contains(&"copy".to_string()));
    let list = spec.concat_list.expect("demuxer needs a list file");
    assert_eq!(list.inputs.len(), 2);
    assert!(
        spec.explanation.iter().any(|e| e.flag == "-c copy"),
        "method choice must be explained"
    );
}

#[test]
fn concat_filter_for_mismatched_inputs() {
    let op = operation_for("concat").expect("op exists");
    let owned = OwnedCtx::new(
        &["a.mp4", "b.mp4"],
        Some("joined.mp4"),
        &[("method", text("auto"))],
    )
    .with_probes(vec![
        probe_video_audio("a.mp4", "h264", "aac"),
        probe_video_audio("b.mp4", "hevc", "mp3"),
    ]);
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert!(spec.concat_list.is_none(), "filter needs no list file");
    let complex = spec
        .args
        .iter()
        .find(|a| a.contains("concat=n=2"))
        .expect("filter expr");
    assert!(complex.contains("v=1:a=1"), "{complex}");
    assert!(spec.args.contains(&"-c:v".to_string()));
}

#[test]
fn concat_needs_two_inputs() {
    let op = operation_for("concat").expect("op exists");
    let owned = OwnedCtx::new(&["only.mp4"], None, &[("method", text("auto"))]);
    op.build(&owned.view())
        .expect_err("single input must fail loudly");
}

#[test]
fn every_operation_declares_fields() {
    use ffkit::ops::fields::FieldContext;
    let ctx = FieldContext {
        probe: None,
        caps: None,
    };
    for op in OPERATIONS {
        let op = operation_for(op.id).expect("factory covers registry");
        assert!(
            !op.fields(&ctx).is_empty(),
            "{id} must declare at least one field",
            id = op.id()
        );
    }
}

/// §4: odd-sized sources gain the even-preserving scale filter so
/// block encoders stop refusing the encode; even sources are untouched.
#[test]
fn compress_odd_dimensions_get_even_filter() {
    let op = operation_for("compress").expect("op exists");
    let owned = OwnedCtx::new(
        &["odd.mp4"],
        Some("odd_c.mp4"),
        &[
            ("video_codec", text("libx264")),
            ("video_mode", FieldValue::Toggle(0)),
            ("crf", FieldValue::Int(23)),
            ("preset", text("medium")),
            ("audio_codec", text("aac")),
            ("audio_bitrate", text("128k")),
            ("resolution", text("")),
        ],
    )
    .with_probes(vec![probe_video_audio_dims(
        "odd.mp4", "mpeg4", "aac", 1092, 863,
    )]);
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert!(
        spec.args
            .contains(&"scale=trunc(iw/2)*2:trunc(ih/2)*2".to_string()),
        "odd dims must gain the guard filter, got {:?}",
        spec.args
    );
}

#[test]
fn compress_even_dimensions_gain_no_filter() {
    let op = operation_for("compress").expect("op exists");
    let owned = OwnedCtx::new(
        &["even.mp4"],
        Some("even_c.mp4"),
        &[
            ("video_codec", text("libx264")),
            ("video_mode", FieldValue::Toggle(0)),
            ("crf", FieldValue::Int(23)),
            ("preset", text("medium")),
            ("audio_codec", text("aac")),
            ("audio_bitrate", text("128k")),
            ("resolution", text("")),
        ],
    )
    .with_probes(vec![probe_video_audio_dims(
        "even.mp4", "h264", "aac", 320, 240,
    )]);
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert!(
        !spec.args.iter().any(|a| a.contains("trunc(")),
        "even dims must not gain a filter, got {:?}",
        spec.args
    );
}

/// §5: a video-only input refuses loudly at build time instead of emitting
/// a doomed `-map 0:a:0` that ffmpeg rejects cryptically at runtime.
#[test]
fn extract_audio_refuses_video_only_input() {
    use ffkit::ffmpeg::probe::{ProbeResult, StreamInfo, StreamType};

    let op = operation_for("extract-audio").expect("op exists");
    let probe = ProbeResult {
        path: PathBuf::from("silent.mp4"),
        duration: None,
        size_bytes: None,
        format_name: Some("mp4".to_string()),
        bit_rate_bps: None,
        streams: vec![StreamInfo {
            codec_type: Some(StreamType::Video),
            codec_name: Some("h264".to_string()),
            width: Some(320),
            height: Some(240),
            ..StreamInfo::default()
        }],
    };
    let owned = OwnedCtx::new(
        &["silent.mp4"],
        Some("silent_audio.mp3"),
        &[
            ("format", text("mp3")),
            ("track", text("0")),
            ("bitrate", text("192k")),
        ],
    )
    .with_probes(vec![probe.clone()]);
    let err = op.build(&owned.view()).expect_err("must refuse loudly");
    assert!(err.to_string().contains("no audio streams"), "{err}");

    // The form offers no fake default track either.
    let fields = op.fields(&ffkit::ops::fields::FieldContext {
        probe: Some(&probe),
        caps: None,
    });
    let track = fields
        .iter()
        .find(|f| f.id == "track")
        .expect("track field");
    let selected = match track.value() {
        ffkit::ops::fields::FieldValue::Text(value) => value,
        other => panic!("track must be a select, got {other:?}"),
    };
    assert_ne!(
        selected, "0",
        "no selectable default track without audio (got a fake default)"
    );
}
