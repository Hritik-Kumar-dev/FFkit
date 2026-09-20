//! Command-builder snapshot tests (spec sections 5 and 14).
//!
//! The highest-value suite in the project: every operation × several
//! parameter combinations, asserting the exact argv vector. A regression
//! here produces silently wrong commands, so these tests are exact.
//!
//! M3 covers compress/convert/trim; M5 covered the rest (thumbnail since removed).

use std::collections::HashMap;
use std::path::PathBuf;

use ffkit::ops::fields::{BuildContext, FieldValue};
use ffkit::ops::{find_by_id, operation_for, OPERATIONS};

/// Owned test inputs that can lend a borrowed [`BuildContext`] view.
struct OwnedCtx {
    inputs: Vec<PathBuf>,
    output: Option<PathBuf>,
    fields: HashMap<&'static str, FieldValue>,
    probes: Vec<ffkit::ffmpeg::probe::ProbeResult>,
    clips: Vec<ffkit::ops::fields::ClipRange>,
}

impl OwnedCtx {
    fn new(inputs: &[&str], output: Option<&str>, fields: &[(&'static str, FieldValue)]) -> Self {
        Self {
            inputs: inputs.iter().map(PathBuf::from).collect(),
            output: output.map(PathBuf::from),
            fields: fields.iter().cloned().collect(),
            probes: Vec::new(),
            clips: Vec::new(),
        }
    }

    /// Attach timeline clips for trim builds.
    fn with_clips(mut self, clips: Vec<(f64, f64)>) -> Self {
        self.clips = clips
            .into_iter()
            .map(|(start, end)| ffkit::ops::fields::ClipRange { start, end })
            .collect();
        self
    }

    fn view(&self) -> BuildContext<'_> {
        BuildContext {
            inputs: &self.inputs,
            output: self.output.as_ref(),
            probe: None,
            input_probes: self.probes.clone(),
            clips: self.clips.clone(),
            caps: None,
            fields: self.fields.clone(),
        }
    }
}

fn text(value: &str) -> FieldValue {
    FieldValue::Text(value.to_string())
}

#[test]
fn registry_lists_ten_operations_in_menu_order() {
    let ids: Vec<&str> = OPERATIONS.iter().map(|op| op.id).collect();
    assert_eq!(
        ids,
        vec![
            "convert",
            "compress",
            "trim",
            "extract-audio",
            "resize",
            "gif",
            "frames",
            "concat",
            "subtitles",
            "image-convert",
        ]
    );
}

#[test]
fn every_registered_operation_resolves_by_id() {
    for op in OPERATIONS {
        assert_eq!(find_by_id(op.id), Some(op));
        assert!(
            operation_for(op.id).is_some(),
            "operation_for must instantiate {}",
            op.id
        );
    }
    assert!(operation_for("no-such-operation").is_none());
}

/// The spec's canonical example (§5): exact argv for default CRF compression.
#[test]
fn compress_crf_defaults() {
    let op = operation_for("compress").expect("compress exists");
    let owned = OwnedCtx::new(
        &["input.mp4"],
        Some("output.mp4"),
        &[
            ("video_codec", text("libx264")),
            ("video_mode", FieldValue::Toggle(0)),
            ("crf", FieldValue::Int(23)),
            ("preset", text("medium")),
            ("audio_codec", text("aac")),
            ("audio_bitrate", text("128k")),
            ("resolution", text("")),
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
            "input.mp4",
            "-c:v",
            "libx264",
            "-crf",
            "23",
            "-preset",
            "medium",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "output.mp4",
        ]
    );
}

#[test]
fn compress_stream_copy_skips_encode_flags() {
    let op = operation_for("compress").expect("compress exists");
    let owned = OwnedCtx::new(
        &["input.mp4"],
        Some("out.mp4"),
        &[
            ("video_codec", text("libx264")),
            ("video_mode", FieldValue::Toggle(1)),
            ("crf", FieldValue::Int(23)),
            ("preset", text("medium")),
            ("audio_codec", text("copy")),
            ("audio_bitrate", text("128k")),
            ("resolution", text("")),
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
            "input.mp4",
            "-c:v",
            "copy",
            "-c:a",
            "copy",
            "out.mp4",
        ]
    );
    assert!(
        spec.explanation.iter().any(|e| e.flag == "-c:v copy"),
        "stream copy must be explained, not silent"
    );
}

#[test]
fn compress_hostile_filenames_stay_single_argv_elements() {
    let op = operation_for("compress").expect("compress exists");
    let owned = OwnedCtx::new(
        &["my holiday -rf.mp4"],
        None,
        &[
            ("video_codec", text("libx264")),
            ("video_mode", FieldValue::Toggle(0)),
            ("crf", FieldValue::Int(23)),
            ("preset", text("medium")),
            ("audio_codec", text("aac")),
            ("audio_bitrate", text("128k")),
            ("resolution", text("")),
        ],
    );
    let spec = op.build(&owned.view()).expect("build succeeds");
    // Argv keeps hostile names whole; quoting is display-only.
    assert!(spec.args.contains(&"my holiday -rf.mp4".to_string()));
    assert!(
        spec.args
            .contains(&"my holiday -rf_compressed.mp4".to_string()),
        "default output follows the stem template, got {:?}",
        spec.args
    );
    // Display quotes them for the shell.
    let display = spec.to_display();
    assert!(display.contains("'my holiday -rf.mp4'"), "{display}");
}

/// Fast mode seeks before `-i` with stream copy; accurate mode seeks after
/// `-i` with a re-encode. The flag *ordering* is the feature under test.
/// Ranges arrive as timeline clips (§10).
#[test]
fn trim_fast_seeks_before_input() {
    let op = operation_for("trim").expect("trim exists");
    let owned = OwnedCtx::new(
        &["input.mp4"],
        Some("clip.mp4"),
        &[
            ("mode", FieldValue::Toggle(0)),
            ("video_codec", text("libx264")),
            ("crf", FieldValue::Int(23)),
        ],
    )
    .with_clips(vec![(60.0, 120.0)]);
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
            "60",
            "-i",
            "input.mp4",
            "-t",
            "60",
            "-c",
            "copy",
            "clip.mp4",
        ]
    );
}

#[test]
fn trim_accurate_seeks_after_input_with_reencode() {
    let op = operation_for("trim").expect("trim exists");
    let owned = OwnedCtx::new(
        &["input.mp4"],
        Some("clip.mp4"),
        &[
            ("mode", FieldValue::Toggle(1)),
            ("video_codec", text("libx264")),
            ("crf", FieldValue::Int(20)),
        ],
    )
    .with_clips(vec![(60.0, 120.0)]);
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
            "input.mp4",
            "-ss",
            "60",
            "-to",
            "120",
            "-c:v",
            "libx264",
            "-crf",
            "20",
            "-c:a",
            "aac",
            "clip.mp4",
        ]
    );
}

/// §10: two fast clips trim to intermediates and join through the
/// re-encoding filter (stream-copied segments keep ragged timestamps the
/// demuxer stacks wrong) — in timeline order.
#[test]
fn trim_two_clips_join_in_timeline_order() {
    let op = operation_for("trim").expect("trim exists");
    let owned = OwnedCtx::new(
        &["input.mp4"],
        Some("joined.mp4"),
        &[
            ("mode", FieldValue::Toggle(0)),
            ("video_codec", text("libx264")),
            ("crf", FieldValue::Int(23)),
        ],
    )
    // Created out of order on purpose: output follows timeline position.
    .with_clips(vec![(110.0, 135.0), (3.0, 41.0)]);
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert_eq!(spec.pre_commands.len(), 2);
    // First pre-command is the earliest clip (3–41), not creation order.
    assert!(spec.pre_commands[0].args.contains(&"3".to_string()));
    assert!(spec.pre_commands[1].args.contains(&"110".to_string()));
    // Fast join re-encodes (filter), it does not demuxer-copy.
    assert!(spec.concat_list.is_none());
    assert!(spec.args.iter().any(|a| a.contains("concat=n=2")));
    assert_eq!(spec.args.last().map(String::as_str), Some("joined.mp4"));
}

/// §10: two accurate clips join through the demuxer (re-encoded segments
/// carry exact timestamps, verified live at 6.02s for 3+3).
#[test]
fn trim_two_accurate_clips_join_via_demuxer() {
    let op = operation_for("trim").expect("trim exists");
    let owned = OwnedCtx::new(
        &["input.mp4"],
        Some("joined.mp4"),
        &[
            ("mode", FieldValue::Toggle(1)),
            ("video_codec", text("libx264")),
            ("crf", FieldValue::Int(23)),
        ],
    )
    .with_clips(vec![(3.0, 41.0), (110.0, 135.0)]);
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert_eq!(spec.pre_commands.len(), 2);
    assert!(spec.concat_list.is_some());
    assert!(spec.args.contains(&"-c".to_string()));
    assert!(spec.args.contains(&"copy".to_string()));
}

/// §10: no clips is a loud error, not an empty command.
#[test]
fn trim_without_clips_fails_loudly() {
    let op = operation_for("trim").expect("trim exists");
    let owned = OwnedCtx::new(
        &["input.mp4"],
        Some("clip.mp4"),
        &[("mode", FieldValue::Toggle(0))],
    );
    op.build(&owned.view())
        .expect_err("empty clips must fail loudly");
}

#[test]
fn convert_mp4_adds_faststart() {
    let op = operation_for("convert").expect("convert exists");
    let owned = OwnedCtx::new(
        &["film.mkv"],
        None,
        &[
            ("format", text("mp4")),
            ("video_codec", text("libx264")),
            ("audio_codec", text("aac")),
            ("crf", FieldValue::Int(23)),
            ("faststart", FieldValue::Toggle(0)),
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
            "film.mkv",
            "-c:v",
            "libx264",
            "-crf",
            "23",
            "-c:a",
            "aac",
            "-movflags",
            "+faststart",
            "film_converted.mp4",
        ]
    );
}

#[test]
fn all_operations_are_implemented() {
    // M5 implemented everything — this test asserts the factory covers the
    // registry instead of expecting failures.
    for op in OPERATIONS {
        assert!(
            operation_for(op.id).is_some(),
            "factory must cover {}",
            op.id
        );
    }
}

/// §3: a typed output without an extension gains the format's extension —
/// never an extensionless `.mp4`-encoded file.
#[test]
fn output_without_extension_gains_format_ext() {
    let op = operation_for("convert").expect("convert exists");
    let owned = OwnedCtx::new(
        &["film.mkv"],
        Some("myclip"),
        &[
            ("format", text("mp4")),
            ("video_codec", text("libx264")),
            ("audio_codec", text("aac")),
            ("crf", FieldValue::Int(23)),
            ("faststart", FieldValue::Toggle(0)),
        ],
    );
    let spec = op.build(&owned.view()).expect("build succeeds");
    assert_eq!(spec.args.last().map(String::as_str), Some("myclip.mp4"));
}
