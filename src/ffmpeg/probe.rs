//! Media inspection via ffprobe (spec section 8).
//!
//! Runs `ffprobe -v quiet -print_format json -show_format -show_streams`
//! asynchronously and deserializes into defensive typed structs — everything
//! is optional because real-world files are full of surprises.
//! Pure parsing (`from_json`, [`parse_rational`], …) is separated from I/O
//! ([`probe_file`]) so the parser is unit-testable against fixtures.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

/// How long [`probe_file`] waits for ffprobe before giving up.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Failures when probing a file. Kept distinct from ffmpeg *run* errors so
/// the UI can tell "can't read this file" apart from "encode failed".
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// No ffprobe binary (neither configured path nor `PATH`).
    #[error("ffprobe not found; install FFmpeg or set a custom path")]
    BinaryNotFound,
    /// ffprobe did not answer in time.
    #[error("ffprobe timed out after {0:?} on {1}")]
    Timeout(Duration, PathBuf),
    /// ffprobe exited non-zero (corrupt file, unknown format, …).
    #[error("ffprobe could not read {path}: {message}")]
    Failed { path: PathBuf, message: String },
    /// ffprobe output was not valid JSON we understand.
    #[error("could not parse ffprobe output for {path}: {message}")]
    Parse { path: PathBuf, message: String },
}

/// What kind of media a stream carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamType {
    Video,
    Audio,
    Subtitle,
    Attachment,
    Data,
    /// Any `codec_type` we did not anticipate; the raw string is kept.
    Other(String),
}

impl StreamType {
    /// Map ffprobe's `codec_type` string. Unknown values become `Other`
    /// rather than an error — see module docs.
    pub fn from_codec_type(s: &str) -> Self {
        match s {
            "video" => Self::Video,
            "audio" => Self::Audio,
            "subtitle" => Self::Subtitle,
            "attachment" => Self::Attachment,
            "data" => Self::Data,
            other => Self::Other(other.to_string()),
        }
    }
}

/// One stream in a probed file. All fields optional; `None` means ffprobe
/// did not report it (or reported `N/A`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamInfo {
    /// Stream index within the file.
    pub index: u32,
    /// Video / audio / subtitle / …
    pub codec_type: Option<StreamType>,
    /// e.g. `"h264"`, `"aac"`. Used for stream-copy decisions.
    pub codec_name: Option<String>,
    /// Pixel dimensions (video only).
    pub width: Option<u32>,
    /// Pixel dimensions (video only).
    pub height: Option<u32>,
    /// Frames per second, parsed from `avg_frame_rate` (falling back to
    /// `r_frame_rate`). Rational strings like `"30000/1001"` are parsed as
    /// fractions, never floats.
    pub fps: Option<f64>,
    /// Audio sample rate in Hz.
    pub sample_rate: Option<u32>,
    /// Audio channel count.
    pub channels: Option<u32>,
    /// Pixel format, e.g. `"yuv420p"`.
    pub pix_fmt: Option<String>,
    /// Stream bit rate in bits per second.
    pub bit_rate_bps: Option<u64>,
    /// Stream duration, if reported per-stream.
    pub duration: Option<Duration>,
    /// Rotation in degrees from side-data or tags. Warn the user when this
    /// will affect output orientation.
    pub rotation: Option<f64>,
    /// Stream language, e.g. `"eng"`.
    pub language: Option<String>,
}

/// Typed result of probing one file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProbeResult {
    /// The probed file.
    pub path: PathBuf,
    /// Container duration; drives progress % and trim bounds.
    pub duration: Option<Duration>,
    /// File size in bytes; drives before/after comparisons.
    pub size_bytes: Option<u64>,
    /// Container names, e.g. `"mov,mp4,m4a,3gp,3g2,mj2"`.
    pub format_name: Option<String>,
    /// Overall bit rate in bits per second.
    pub bit_rate_bps: Option<u64>,
    /// Stream list; lets the user pick audio/subtitle tracks.
    pub streams: Vec<StreamInfo>,
}

impl ProbeResult {
    /// Parse `ffprobe -print_format json` output. Never panics on surprising
    /// shapes — unknown fields are ignored, missing ones become `None`.
    pub fn from_json(path: PathBuf, json: &str) -> Result<Self, ProbeError> {
        let raw: RawProbe = serde_json::from_str(json).map_err(|e| ProbeError::Parse {
            path: path.clone(),
            message: e.to_string(),
        })?;
        Ok(Self {
            path,
            duration: raw
                .format
                .as_ref()
                .and_then(|f| f.duration.as_deref())
                .and_then(parse_ffprobe_secs),
            size_bytes: raw
                .format
                .as_ref()
                .and_then(|f| f.size.as_deref())
                .and_then(|s| s.parse::<u64>().ok()),
            format_name: raw.format.as_ref().and_then(|f| f.format_name.clone()),
            bit_rate_bps: raw
                .format
                .as_ref()
                .and_then(|f| f.bit_rate.as_deref())
                .and_then(|s| s.parse::<u64>().ok()),
            streams: raw.streams.iter().map(StreamInfo::from_raw).collect(),
        })
    }

    /// True when the file has at least one video stream.
    pub fn has_video(&self) -> bool {
        self.streams
            .iter()
            .any(|s| s.codec_type == Some(StreamType::Video))
    }

    /// True when the file has at least one audio stream.
    pub fn has_audio(&self) -> bool {
        self.streams
            .iter()
            .any(|s| s.codec_type == Some(StreamType::Audio))
    }

    /// First video stream, if any. Drives resize defaults and aspect handling.
    pub fn video_stream(&self) -> Option<&StreamInfo> {
        self.streams
            .iter()
            .find(|s| s.codec_type == Some(StreamType::Video))
    }

    /// Video codec name, e.g. `Some("h264")`.
    pub fn video_codec(&self) -> Option<&str> {
        self.video_stream().and_then(|s| s.codec_name.as_deref())
    }

    /// First audio stream's codec, e.g. `Some("aac")`.
    pub fn audio_codec(&self) -> Option<&str> {
        self.streams
            .iter()
            .find(|s| s.codec_type == Some(StreamType::Audio))
            .and_then(|s| s.codec_name.as_deref())
    }

    /// Compact one-liner for the media-info panel, e.g.
    /// `320×240 · 30.00 fps · h264/aac · 0:02 · 32 KB`.
    /// Unknown parts are simply omitted, never faked.
    pub fn summary_line(&self) -> String {
        let mut parts = Vec::new();
        if let Some(v) = self.video_stream() {
            if let (Some(w), Some(h)) = (v.width, v.height) {
                parts.push(format!("{w}×{h}"));
            }
            if let Some(fps) = v.fps {
                parts.push(format!("{fps:.2} fps"));
            }
        }
        let codecs = match (self.video_codec(), self.audio_codec()) {
            (Some(v), Some(a)) => format!("{v}/{a}"),
            (Some(v), None) => v.to_string(),
            (None, Some(a)) => a.to_string(),
            (None, None) => String::new(),
        };
        if !codecs.is_empty() {
            parts.push(codecs);
        }
        if let Some(d) = self.duration {
            parts.push(format_duration(d));
        }
        if let Some(b) = self.size_bytes {
            parts.push(format_size(b));
        }
        if parts.is_empty() {
            return "no media info".to_string();
        }
        parts.join(" · ")
    }
}

impl StreamInfo {
    fn from_raw(raw: &RawStream) -> Self {
        let fps = raw
            .avg_frame_rate
            .as_deref()
            .and_then(parse_rational)
            .filter(|f| *f > 0.0)
            .or_else(|| {
                raw.r_frame_rate
                    .as_deref()
                    .and_then(parse_rational)
                    .filter(|f| *f > 0.0)
            });
        Self {
            index: raw.index.unwrap_or(0),
            codec_type: raw.codec_type.as_deref().map(StreamType::from_codec_type),
            codec_name: raw.codec_name.clone(),
            width: raw.width,
            height: raw.height,
            fps,
            sample_rate: raw
                .sample_rate
                .as_deref()
                .and_then(|s| s.parse::<u32>().ok()),
            channels: raw.channels,
            pix_fmt: raw.pix_fmt.clone(),
            bit_rate_bps: raw.bit_rate.as_deref().and_then(|s| s.parse::<u64>().ok()),
            duration: raw.duration.as_deref().and_then(parse_ffprobe_secs),
            rotation: rotation_from(raw),
            language: raw.tags.as_ref().and_then(|t| t.get("language").cloned()),
        }
    }
}

/// Rotation from display-matrix side data, falling back to the `rotate` tag.
/// Both are real-world shapes; absence means "no rotation metadata".
fn rotation_from(raw: &RawStream) -> Option<f64> {
    if let Some(list) = raw.side_data_list.as_ref() {
        for side in list {
            if let Some(rot) = side.rotation {
                return Some(rot);
            }
        }
    }
    raw.tags
        .as_ref()
        .and_then(|t| t.get("rotate"))
        .and_then(|s| s.parse::<f64>().ok())
}

/// Parse an ffprobe rational like `"30000/1001"` as a fraction.
/// Returns `None` for `"0/0"`, `"N/A"`, malformed input, or a zero divisor.
pub fn parse_rational(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("n/a") {
        return None;
    }
    let (num, den) = s.split_once('/')?;
    let num: f64 = num.trim().parse().ok()?;
    let den: f64 = den.trim().parse().ok()?;
    if !num.is_finite() || !den.is_finite() || den == 0.0 {
        return None;
    }
    let value = num / den;
    if value.is_finite() {
        Some(value)
    } else {
        None
    }
}

/// Parse an ffprobe duration in seconds like `"248.123456"`.
/// Returns `None` for `"N/A"` or malformed input.
pub fn parse_ffprobe_secs(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("n/a") {
        return None;
    }
    let secs: f64 = s.parse().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(secs))
}

/// Format a duration compactly: `0:02`, `4:12`, `1:02:03`.
pub fn format_duration(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Format a byte count compactly: `32 KB`, `248 MB`.
pub fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// Probe one file with ffprobe. Async with a timeout — never blocks the
/// render thread; callers run this in a spawned task and report back over
/// the background-message channel.
pub async fn probe_file(
    ffprobe: &Path,
    path: &Path,
    timeout: Duration,
) -> Result<ProbeResult, ProbeError> {
    let output = tokio::time::timeout(
        timeout,
        tokio::process::Command::new(ffprobe)
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path)
            .output(),
    )
    .await
    .map_err(|_| ProbeError::Timeout(timeout, path.to_path_buf()))?
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ProbeError::BinaryNotFound
        } else {
            ProbeError::Failed {
                path: path.to_path_buf(),
                message: e.to_string(),
            }
        }
    })?;
    if !output.status.success() {
        return Err(ProbeError::Failed {
            path: path.to_path_buf(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let stdout = String::from_utf8(output.stdout).map_err(|e| ProbeError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    ProbeResult::from_json(path.to_path_buf(), &stdout)
}

// ---------------------------------------------------------------------------
// Raw ffprobe JSON shapes. Every field is optional with a default: ffprobe
// output varies across versions, containers, and levels of file corruption.
// ---------------------------------------------------------------------------

/// Top-level ffprobe JSON document.
#[derive(Debug, Deserialize, Default)]
struct RawProbe {
    #[serde(default)]
    streams: Vec<RawStream>,
    #[serde(default)]
    format: Option<RawFormat>,
}

/// One entry of `streams[]`.
#[derive(Debug, Deserialize, Default)]
struct RawStream {
    #[serde(default)]
    index: Option<u32>,
    #[serde(default)]
    codec_name: Option<String>,
    #[serde(default)]
    codec_type: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    r_frame_rate: Option<String>,
    #[serde(default)]
    avg_frame_rate: Option<String>,
    #[serde(default)]
    sample_rate: Option<String>,
    #[serde(default)]
    channels: Option<u32>,
    #[serde(default)]
    pix_fmt: Option<String>,
    #[serde(default)]
    bit_rate: Option<String>,
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    tags: Option<HashMap<String, String>>,
    #[serde(default)]
    side_data_list: Option<Vec<RawSideData>>,
}

/// One entry of a stream's `side_data_list`.
#[derive(Debug, Deserialize, Default)]
struct RawSideData {
    #[serde(default)]
    rotation: Option<f64>,
}

/// The `format` object.
#[derive(Debug, Deserialize, Default)]
struct RawFormat {
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    bit_rate: Option<String>,
    #[serde(default)]
    format_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `ffprobe` output for a 2s 320×240 h264/aac MP4, generated with
    /// `ffmpeg -f lavfi -i testsrc… -f lavfi -i sine…`. See
    /// `tests/fixtures/probe_sample.json`.
    fn sample_json() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/probe_sample.json"
        ))
        .expect("probe fixture must exist")
    }

    #[test]
    fn parses_real_probe_fixture() {
        let result = ProbeResult::from_json("/tmp/ffkit_sample.mp4".into(), &sample_json())
            .expect("fixture must parse");
        assert_eq!(result.streams.len(), 2);
        assert!(result.has_video() && result.has_audio());
        assert_eq!(result.video_codec(), Some("h264"));
        assert_eq!(result.audio_codec(), Some("aac"));

        let video = result.video_stream().expect("video stream");
        assert_eq!((video.width, video.height), (Some(320), Some(240)));
        assert!((video.fps.unwrap_or(0.0) - 30.0).abs() < 0.01);

        assert_eq!(result.duration, Some(Duration::from_secs(2)));
        assert_eq!(result.size_bytes, Some(32710));

        let summary = result.summary_line();
        assert!(summary.contains("320×240"), "summary: {summary}");
        assert!(summary.contains("h264/aac"), "summary: {summary}");
    }

    #[test]
    fn rational_parsing_handles_real_world_shapes() {
        assert!((parse_rational("30000/1001").unwrap_or(0.0) - 29.97).abs() < 0.01);
        assert_eq!(parse_rational("30/1"), Some(30.0));
        assert_eq!(parse_rational("0/0"), None);
        assert_eq!(parse_rational("N/A"), None);
        assert_eq!(parse_rational("n/a"), None);
        assert_eq!(parse_rational(""), None);
        assert_eq!(parse_rational("25"), None);
        assert_eq!(parse_rational("abc/def"), None);
        assert_eq!(parse_rational("1/0"), None);
    }

    #[test]
    fn duration_parsing_rejects_garbage() {
        assert_eq!(
            parse_ffprobe_secs("248.123456"),
            Some(Duration::from_secs_f64(248.123456))
        );
        assert_eq!(parse_ffprobe_secs("N/A"), None);
        assert_eq!(parse_ffprobe_secs(""), None);
        assert_eq!(parse_ffprobe_secs("-3"), None);
        assert_eq!(parse_ffprobe_secs("inf"), None);
    }

    #[test]
    fn corrupt_or_empty_input_is_an_error_not_a_panic() {
        let err =
            ProbeResult::from_json("x.mp4".into(), "not json").expect_err("garbage must fail");
        assert!(matches!(err, ProbeError::Parse { .. }));
        let empty = ProbeResult::from_json("x.mp4".into(), "{}").expect("empty doc");
        assert!(!empty.has_video());
        assert_eq!(empty.summary_line(), "no media info");
    }

    #[test]
    fn rotation_prefers_side_data_then_tag() {
        let with_side = serde_json::json!({
            "streams": [{
                "codec_type": "video",
                "side_data_list": [{"rotation": -90.0}],
                "tags": {"rotate": "180"}
            }]
        });
        let r = ProbeResult::from_json("x".into(), &with_side.to_string()).unwrap();
        assert_eq!(r.streams[0].rotation, Some(-90.0));

        let with_tag = serde_json::json!({
            "streams": [{"codec_type": "video", "tags": {"rotate": "90"}}]
        });
        let r = ProbeResult::from_json("x".into(), &with_tag.to_string()).unwrap();
        assert_eq!(r.streams[0].rotation, Some(90.0));
    }
}
