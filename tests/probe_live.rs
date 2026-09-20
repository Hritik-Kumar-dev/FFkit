//! Live end-to-end test: capability detection + ffprobe against the real
//! system binaries. Skips gracefully where FFmpeg is not installed (e.g.
//! minimal CI images) — the deterministic fixture tests in
//! `src/ffmpeg/probe.rs` and `src/ffmpeg/capabilities.rs` always run.

use std::path::PathBuf;
use std::time::Duration;

use ffkit::config::settings::Settings;
use ffkit::ffmpeg::capabilities::detect;
use ffkit::ffmpeg::probe::{probe_file, PROBE_TIMEOUT};

/// Generate a 1s test file with the system ffmpeg, or `None` when generation
/// is impossible (missing binary, encode failure) so the test can skip.
async fn generate_sample(ffmpeg: &std::path::Path, out: &std::path::Path) -> bool {
    let generated = tokio::process::Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=10:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(out)
        .output()
        .await;
    match generated {
        Ok(output) => output.status.success(),
        Err(_) => false,
    }
}

#[tokio::test]
async fn live_detection_and_probe_round_trip() {
    let report = detect(&Settings::default()).await;
    if !report.found || !report.usable {
        println!("skipping live test: no usable ffmpeg on PATH");
        return;
    }
    let Some(ffmpeg) = report.ffmpeg_path.clone() else {
        println!("skipping live test: unresolved ffmpeg path");
        return;
    };
    let Some(ffprobe) = report.ffprobe_path.clone() else {
        println!("skipping live test: unresolved ffprobe path");
        return;
    };
    assert!(
        report.has_encoder("libx264") || !report.encoders.is_empty(),
        "expected a non-empty encoder set from a real build"
    );

    let dir: PathBuf = std::env::temp_dir().join(format!("ffkit-live-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let media = dir.join("live.mp4");
    if !generate_sample(&ffmpeg, &media).await {
        println!("skipping live test: could not generate sample media");
        return;
    }

    let probed = probe_file(&ffprobe, &media, PROBE_TIMEOUT)
        .await
        .expect("live probe of generated file must succeed");
    assert!(probed.has_video(), "generated file must have video");
    assert_eq!(probed.video_codec(), Some("h264"));
    assert!(
        probed.duration.unwrap_or(Duration::ZERO) >= Duration::from_millis(900),
        "duration should be ~1s, got {:?}",
        probed.duration
    );

    let _ = std::fs::remove_dir_all(&dir);
}
