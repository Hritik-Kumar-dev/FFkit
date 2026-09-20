//! `-progress pipe:1` block parser (spec section 7).
//!
//! Field shapes verified against `print_report` in fftools/ffmpeg.c:
//! - keys: `frame`, `fps`, `stream_<file>_<stream>_q`, `bitrate`
//!   (`%6.1fkbits/s` or `N/A`), `total_size` (bytes or `N/A`),
//!   `out_time_us` + `out_time_ms` (**both print `pts` in microseconds** —
//!   the `_ms` name is a long-standing lie; prefer `out_time_us`),
//!   `out_time` (`HH:MM:SS.ffffff` or `N/A`), `dup_frames`, `drop_frames`,
//!   `speed` (`%4.3gx` or `N/A`), `progress` (`continue`/`end`).
//! - each update block ends with a `progress=` line; parse blocks, not lines.
//! - audio-only encodes emit no `frame`/`fps` keys at all — everything stays
//!   `Option`, never a panic.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

/// One parsed progress block.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProgressUpdate {
    /// Frames written (video only; absent for audio-only encodes).
    pub frame: Option<u64>,
    /// Current encoding speed in frames/sec.
    pub fps: Option<f64>,
    /// Output bitrate in kbit/s.
    pub bitrate_kbps: Option<f64>,
    /// Output file size so far in bytes.
    pub total_size_bytes: Option<u64>,
    /// Media timestamp reached.
    pub out_time: Option<Duration>,
    /// Duplicated frames (filter/graph stalls).
    pub dup_frames: Option<u64>,
    /// Dropped frames.
    pub drop_frames: Option<u64>,
    /// Speed multiplier (1.0 = realtime).
    pub speed: Option<f64>,
    /// Per-stream quality values keyed by (file, stream).
    pub stream_q: HashMap<(u32, u32), f64>,
    /// True when the block ended with `progress=end`.
    pub finished: bool,
}

/// Stateful block accumulator. Feed stdout lines; a completed
/// [`ProgressUpdate`] comes out each time a `progress=` line arrives.
#[derive(Debug, Default)]
pub struct ProgressParser {
    block: HashMap<String, String>,
}

impl ProgressParser {
    /// Fresh parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one stdout line. Returns a completed update when the line is a
    /// `progress=` terminator, otherwise `None`. Malformed lines (no `=`)
    /// are ignored — stderr noise must never break progress.
    pub fn feed_line(&mut self, line: &str) -> Option<ProgressUpdate> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let (key, value) = line.split_once('=')?;
        if key == "progress" {
            let finished = value.trim() == "end";
            let update = build_update(&self.block, finished);
            self.block.clear();
            Some(update)
        } else {
            self.block.insert(key.to_string(), value.to_string());
            None
        }
    }

    /// Feed a chunk that may contain many lines (convenience for tests and
    /// for readers that deliver partial buffers — callers splitting on
    /// newlines get the same result).
    pub fn feed_str(&mut self, chunk: &str) -> Vec<ProgressUpdate> {
        chunk
            .lines()
            .filter_map(|line| self.feed_line(line))
            .collect()
    }
}

/// Build an update from one accumulated block.
fn build_update(block: &HashMap<String, String>, finished: bool) -> ProgressUpdate {
    let get = |key: &str| block.get(key).map(String::as_str);
    let mut stream_q = HashMap::new();
    for (key, value) in block {
        if let Some((file, stream)) = parse_stream_q_key(key) {
            if let Some(q) = parse_f64(value) {
                stream_q.insert((file, stream), q);
            }
        }
    }
    ProgressUpdate {
        frame: get("frame").and_then(parse_u64),
        fps: get("fps").and_then(parse_f64),
        bitrate_kbps: get("bitrate").and_then(parse_bitrate),
        total_size_bytes: get("total_size").and_then(parse_u64),
        out_time: get("out_time_us")
            .and_then(parse_u64)
            .map(Duration::from_micros)
            .or_else(|| get("out_time").and_then(parse_out_time)),
        dup_frames: get("dup_frames").and_then(parse_u64),
        drop_frames: get("drop_frames").and_then(parse_u64),
        speed: get("speed").and_then(parse_speed),
        stream_q,
        finished,
    }
}

/// Parse a `stream_<file>_<stream>_q` key into its indices.
fn parse_stream_q_key(key: &str) -> Option<(u32, u32)> {
    let rest = key.strip_prefix("stream_")?.strip_suffix("_q")?;
    let (file, stream) = rest.split_once('_')?;
    Some((file.parse().ok()?, stream.parse().ok()?))
}

/// Parse an integer; `N/A` and garbage become `None`.
fn parse_u64(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("n/a") {
        return None;
    }
    s.parse().ok()
}

/// Parse a float; `N/A`, `-1` sentinels, and garbage become `None`.
/// (ffmpeg prints `-1`/`-1.00` for unknown quality values.)
fn parse_f64(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("n/a") {
        return None;
    }
    let value: f64 = s.parse().ok()?;
    if value.is_finite() && value >= 0.0 {
        Some(value)
    } else {
        None
    }
}

/// Parse `1234.5kbits/s` (or a bare number) into kbit/s.
fn parse_bitrate(s: &str) -> Option<f64> {
    parse_f64(s.trim().strip_suffix("kbits/s").unwrap_or(s.trim()))
}

/// Parse `1.52x` (or a bare number) into a multiplier.
fn parse_speed(s: &str) -> Option<f64> {
    parse_f64(s.trim().strip_suffix('x').unwrap_or(s.trim()))
}

/// Parse `HH:MM:SS.ffffff` (optional leading `-`) into a [`Duration`].
/// Fallback for when `out_time_us` is absent; garbage becomes `None`.
fn parse_out_time(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("n/a") {
        return None;
    }
    let s = s.strip_prefix('-').unwrap_or(s);
    let (hms, micros) = match s.split_once('.') {
        Some((hms, frac)) => (hms, frac),
        None => (s, "0"),
    };
    let mut parts = hms.split(':');
    let hours: u64 = parts.next()?.parse().ok()?;
    let mins: u64 = parts.next()?.parse().ok()?;
    let secs: u64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let frac: f64 = format!("0.{micros}").parse().ok()?;
    if !frac.is_finite() || !(0.0..1.0).contains(&frac) {
        return None;
    }
    Some(Duration::from_secs(hours * 3600 + mins * 60 + secs) + Duration::from_secs_f64(frac))
}

/// Progress ratio in `[0, 1]` from output time over total duration.
/// `None` when the total is unknown or zero — the UI shows an indeterminate
/// spinner instead of inventing progress (spec §7).
pub fn progress_ratio(out_time: Duration, total: Duration) -> Option<f64> {
    if total.is_zero() {
        return None;
    }
    Some((out_time.as_secs_f64() / total.as_secs_f64()).clamp(0.0, 1.0))
}

/// Remaining wall-clock seconds from the spec formula:
/// `(total - out_time) / speed`. `None` when any input is missing or the
/// job is already done.
pub fn eta_seconds(out_time: Duration, total: Duration, speed: f64) -> Option<f64> {
    if !speed.is_finite() || speed <= 0.0 || out_time >= total {
        return None;
    }
    Some((total - out_time).as_secs_f64() / speed)
}

/// Raw `speed` values jitter badly at encode start; smooth over the last
/// several updates before feeding the ETA.
#[derive(Debug)]
pub struct SpeedSmoother {
    samples: VecDeque<f64>,
    capacity: usize,
}

impl SpeedSmoother {
    /// Smoother keeping the last `capacity` samples.
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
        }
    }

    /// Record one update's speed (ignores missing/non-positive values).
    pub fn push(&mut self, speed: Option<f64>) {
        if let Some(speed) = speed {
            if speed.is_finite() && speed > 0.0 {
                if self.samples.len() == self.capacity {
                    self.samples.pop_front();
                }
                self.samples.push_back(speed);
            }
        }
    }

    /// Mean of the window, or `None` when no usable sample arrived yet.
    pub fn smoothed(&self) -> Option<f64> {
        if self.samples.is_empty() {
            return None;
        }
        Some(self.samples.iter().sum::<f64>() / self.samples.len() as f64)
    }
}

impl Default for SpeedSmoother {
    fn default() -> Self {
        Self::new(8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real block shape, per the keys verified in fftools/ffmpeg.c.
    const BLOCK: &str = "frame=1234\nfps=45.20\nstream_0_0_q=28.0\nbitrate=1234.5kbits/s\ntotal_size=5242880\nout_time_us=41234567\nout_time_ms=41234567\nout_time=00:00:41.234567\ndup_frames=0\ndrop_frames=3\nspeed=1.52x\nprogress=continue\n";

    #[test]
    fn parses_a_full_block() {
        let mut parser = ProgressParser::new();
        let updates = parser.feed_str(BLOCK);
        assert_eq!(updates.len(), 1);
        let update = &updates[0];
        assert_eq!(update.frame, Some(1234));
        assert!((update.fps.unwrap_or(0.0) - 45.2).abs() < 0.01);
        assert!((update.bitrate_kbps.unwrap_or(0.0) - 1234.5).abs() < 0.01);
        assert_eq!(update.total_size_bytes, Some(5242880));
        assert_eq!(update.out_time, Some(Duration::from_micros(41234567)));
        assert_eq!(update.dup_frames, Some(0));
        assert_eq!(update.drop_frames, Some(3));
        assert!((update.speed.unwrap_or(0.0) - 1.52).abs() < 0.01);
        assert_eq!(update.stream_q.get(&(0, 0)), Some(&28.0));
        assert!(!update.finished);
    }

    #[test]
    fn end_terminator_marks_finished() {
        let mut parser = ProgressParser::new();
        let updates = parser.feed_str("frame=10\nprogress=end\n");
        assert_eq!(updates.len(), 1);
        assert!(updates[0].finished);
    }

    #[test]
    fn na_values_become_none_not_panics() {
        let mut parser = ProgressParser::new();
        let updates = parser.feed_str(
            "frame=N/A\nfps=N/A\nbitrate=N/A\ntotal_size=N/A\nout_time_us=N/A\nout_time=N/A\nspeed=N/A\nprogress=continue\n",
        );
        assert_eq!(updates.len(), 1);
        let update = &updates[0];
        assert_eq!(update.frame, None);
        assert_eq!(update.out_time, None);
        assert_eq!(update.speed, None);
        assert_eq!(update.bitrate_kbps, None);
    }

    #[test]
    fn incomplete_blocks_emit_nothing() {
        let mut parser = ProgressParser::new();
        assert!(parser.feed_str("frame=10\nfps=5.0\n").is_empty());
        // The pending lines join the next block, not lost.
        let updates = parser.feed_str("speed=1.0x\nprogress=continue\n");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].frame, Some(10));
    }

    #[test]
    fn malformed_lines_are_ignored() {
        let mut parser = ProgressParser::new();
        let updates = parser.feed_str("garbage without equals\nframe=7\nprogress=continue\n");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].frame, Some(7));
    }

    #[test]
    fn out_time_string_is_a_fallback_for_missing_us() {
        let mut parser = ProgressParser::new();
        let updates = parser.feed_str("out_time=00:01:02.500000\nprogress=continue\n");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].out_time, Some(Duration::from_millis(62500)));
    }

    #[test]
    fn audio_only_blocks_have_no_frame_keys() {
        let mut parser = ProgressParser::new();
        let updates = parser.feed_str("bitrate=128.0kbits/s\nspeed=2.0x\nprogress=continue\n");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].frame, None);
        assert_eq!(updates[0].fps, None);
        assert!((updates[0].speed.unwrap_or(0.0) - 2.0).abs() < 0.01);
    }

    #[test]
    fn ratio_clamps_and_rejects_unknown_totals() {
        assert_eq!(
            progress_ratio(Duration::from_secs(41), Duration::from_secs(82)),
            Some(0.5)
        );
        assert_eq!(
            progress_ratio(Duration::from_secs(100), Duration::from_secs(82)),
            Some(1.0)
        );
        assert_eq!(progress_ratio(Duration::from_secs(1), Duration::ZERO), None);
    }

    #[test]
    fn eta_needs_positive_speed_and_remaining_work() {
        assert_eq!(
            eta_seconds(Duration::from_secs(40), Duration::from_secs(80), 2.0),
            Some(20.0)
        );
        assert_eq!(
            eta_seconds(Duration::from_secs(80), Duration::from_secs(80), 2.0),
            None
        );
        assert_eq!(
            eta_seconds(Duration::from_secs(40), Duration::from_secs(80), 0.0),
            None
        );
    }

    #[test]
    fn smoother_ignores_garbage_and_averages_the_window() {
        let mut smoother = SpeedSmoother::new(3);
        assert_eq!(smoother.smoothed(), None);
        smoother.push(None);
        smoother.push(Some(0.0));
        smoother.push(Some(1.0));
        smoother.push(Some(3.0));
        assert!((smoother.smoothed().unwrap_or(0.0) - 2.0).abs() < 0.01);
        smoother.push(Some(5.0));
        // Window holds [3.0, 5.0] plus the earlier 1.0? capacity 3: [1,3,5]→mean 3.
        assert!((smoother.smoothed().unwrap_or(0.0) - 3.0).abs() < 0.01);
    }
}
