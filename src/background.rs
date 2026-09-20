//! Background work: everything that must not block the render thread.
//!
//! ffprobe invocations and capability detection run as tokio tasks and
//! report back over an unbounded mpsc channel. The main loop drains the
//! channel once per frame (`App::poll_background`) — rendering and event
//! polling never wait on subprocess I/O.

use std::path::PathBuf;

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::config::settings::Settings;
use crate::ffmpeg::capabilities::{self, CapabilityReport};
use crate::ffmpeg::probe::{self, ProbeError, ProbeResult, PROBE_TIMEOUT};
use crate::ffmpeg::progress::ProgressUpdate;
use crate::ffmpeg::runner::JobResult;

/// Messages from background tasks to the main loop.
#[derive(Debug)]
pub enum BackgroundMsg {
    /// Startup capability detection finished (found or missing).
    CapabilitiesReady(CapabilityReport),
    /// Re-detection after the user typed a custom binary path finished.
    CustomPathChecked(CapabilityReport),
    /// A lazy file-browser probe finished (success or failure — both are
    /// displayable states, never panics).
    ProbeReady {
        /// The file that was probed.
        path: PathBuf,
        /// The outcome.
        result: Result<ProbeResult, ProbeError>,
    },
    /// An ffmpeg job spawned; carries the child pid for force-kill.
    JobStarted {
        /// Which job this belongs to (0 = ad-hoc single run).
        job_id: u64,
        /// OS process id of the ffmpeg child.
        pid: Option<u32>,
    },
    /// One parsed `-progress` block from a running job.
    JobProgress {
        /// Which job this belongs to.
        job_id: u64,
        /// The parsed update.
        update: ProgressUpdate,
    },
    /// One stderr line from a running job (live log pane source).
    JobStderrLine {
        /// Which job this belongs to.
        job_id: u64,
        /// The raw line.
        line: String,
    },
    /// A job ended — success, ffmpeg failure, or user cancellation.
    JobFinished {
        /// Which job this belongs to.
        job_id: u64,
        /// How it ended.
        result: JobResult,
    },
    /// A trim filmstrip extraction finished: thumbnails oldest-first, or
    /// the reason it failed (timeline degrades to ticks).
    FilmstripReady {
        /// The input the strip belongs to.
        input: PathBuf,
        /// Thumbnail files in timeline order, or the failure reason.
        frames: Result<Vec<PathBuf>, String>,
    },
}

/// Channel endpoints shared between the main loop and spawned tasks.
pub struct BackgroundChannel {
    /// Main loop → tasks (currently unused; reserved for M4 cancellation).
    pub tx: UnboundedSender<BackgroundMsg>,
    /// Tasks → main loop, drained once per frame.
    pub rx: UnboundedReceiver<BackgroundMsg>,
}

impl BackgroundChannel {
    /// Fresh channel pair.
    pub fn new() -> Self {
        let (tx, rx) = unbounded_channel();
        Self { tx, rx }
    }
}

impl Default for BackgroundChannel {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn startup capability detection. The report arrives as
/// [`BackgroundMsg::CapabilitiesReady`]; the app shows a loading screen
/// until then.
pub fn spawn_capability_detection(tx: UnboundedSender<BackgroundMsg>, settings: Settings) {
    tokio::spawn(async move {
        let report = capabilities::detect(&settings).await;
        let _ = tx.send(BackgroundMsg::CapabilitiesReady(report));
    });
}

/// Spawn re-detection with a user-typed ffmpeg path. The report arrives as
/// [`BackgroundMsg::CustomPathChecked`]; a valid report is persisted to
/// settings by the main loop.
pub fn spawn_custom_path_check(
    tx: UnboundedSender<BackgroundMsg>,
    ffmpeg_path: PathBuf,
    ffprobe_override: Option<PathBuf>,
) {
    tokio::spawn(async move {
        let settings = Settings {
            general: crate::config::settings::GeneralSettings {
                ffmpeg_path: Some(ffmpeg_path),
                ffprobe_path: ffprobe_override,
                ..crate::config::settings::GeneralSettings::default()
            },
            ..Settings::default()
        };
        let report = capabilities::detect(&settings).await;
        let _ = tx.send(BackgroundMsg::CustomPathChecked(report));
    });
}

/// Spawn a lazy probe of one file. The outcome arrives as
/// [`BackgroundMsg::ProbeReady`]. No ffprobe binary → immediate
/// `ProbeError::BinaryNotFound` without spawning.
pub fn spawn_probe(tx: UnboundedSender<BackgroundMsg>, ffprobe: Option<PathBuf>, path: PathBuf) {
    let Some(ffprobe) = ffprobe else {
        let _ = tx.send(BackgroundMsg::ProbeReady {
            path,
            result: Err(ProbeError::BinaryNotFound),
        });
        return;
    };
    tokio::spawn(async move {
        let result = probe::probe_file(&ffprobe, &path, PROBE_TIMEOUT).await;
        let _ = tx.send(BackgroundMsg::ProbeReady { path, result });
    });
}

/// Thumbnails per filmstrip and the extraction timeout.
pub const STRIP_FRAMES: u32 = 24;
/// Generous: thumbnail extraction is I/O on potentially huge files.
pub const STRIP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Spawn trim filmstrip extraction: one ffmpeg run emitting [`STRIP_FRAMES`]
/// evenly spaced thumbnails (`fps=N/D` over the known duration). Result
/// arrives as [`BackgroundMsg::FilmstripReady`]; failures degrade the
/// timeline to ticks, never an error screen.
pub fn spawn_filmstrip(
    tx: UnboundedSender<BackgroundMsg>,
    ffmpeg: PathBuf,
    input: PathBuf,
    duration_secs: f64,
) {
    tokio::spawn(async move {
        let result = extract_strip(&ffmpeg, &input, duration_secs).await;
        let _ = tx.send(BackgroundMsg::FilmstripReady {
            input,
            frames: result,
        });
    });
}

/// Run the extraction synchronously (in its spawned task): small JPEGs in
/// a fresh temp dir, oldest-first. Errors name the cause for the timeline
/// fallback note.
async fn extract_strip(
    ffmpeg: &std::path::Path,
    input: &PathBuf,
    duration_secs: f64,
) -> Result<Vec<PathBuf>, String> {
    let dir = std::env::temp_dir().join(format!("ffkit-strip-{}", std::process::id()));
    // Fresh dir per extraction run (stale frames would desync the strip).
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("cannot create strip dir: {e}"))?;
    let fps = if duration_secs > 0.0 {
        format!("{}", STRIP_FRAMES as f64 / duration_secs)
    } else {
        "1".to_string()
    };
    let pattern = dir.join("thumb_%03d.jpg");
    let timed = tokio::time::timeout(
        STRIP_TIMEOUT,
        tokio::process::Command::new(ffmpeg)
            .args(["-hide_banner", "-y", "-v", "error", "-i"])
            .arg(input)
            .args(["-vf", &format!("fps={fps},scale=320:-1"), "-q:v", "5"])
            .arg(&pattern)
            .output(),
    )
    .await;
    let output = match timed {
        Err(_) => return Err("thumbnail extraction timed out".to_string()),
        Ok(Err(e)) => return Err(format!("cannot start ffmpeg: {e}")),
        Ok(Ok(output)) => output,
    };
    if !output.status.success() {
        return Err(format!(
            "ffmpeg strip failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut frames: Vec<PathBuf> = (1..=STRIP_FRAMES)
        .map(|i| dir.join(format!("thumb_{i:03}.jpg")))
        .filter(|p| p.exists())
        .collect();
    frames.sort();
    if frames.is_empty() {
        return Err("ffmpeg produced no thumbnails".to_string());
    }
    Ok(frames)
}
