//! Runner parity: the identical encode through ffkit's runner vs a direct
//! spawn must take the same wall time (within noise). Prints both timings
//! for inspection; asserts functional parity (success, outputs exist).
//! Skips gracefully where FFmpeg is not installed.

use std::path::PathBuf;
use std::time::Instant;

use ffkit::background::BackgroundMsg;
use ffkit::ffmpeg::builder::CommandSpec;
use ffkit::ffmpeg::runner::{spawn_job, JobRequest};
use tokio::sync::mpsc::unbounded_channel;

fn wait_done(rx: &mut tokio::sync::mpsc::UnboundedReceiver<BackgroundMsg>) -> (bool, u64) {
    let mut progress = 0;
    loop {
        match rx.blocking_recv() {
            Some(BackgroundMsg::JobProgress { .. }) => progress += 1,
            Some(BackgroundMsg::JobFinished { result, .. }) => return (result.success, progress),
            Some(_) => {}
            None => return (false, progress),
        }
    }
}

#[test]
fn runner_matches_direct_wall_time() {
    let Some(bin) = which::which("ffmpeg").ok() else {
        println!("skipping: no ffmpeg");
        return;
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let dir: PathBuf = std::env::temp_dir().join(format!("ffkit-perf-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let input = dir.join("in.mp4");
        assert!(tokio::process::Command::new(&bin)
            .args([
                "-hide_banner",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=1280x720:rate=30:duration=10",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p"
            ])
            .arg(&input)
            .status()
            .await
            .unwrap()
            .success());

        let argv = |out: &str| {
            let mut spec = CommandSpec::new(bin.to_string_lossy().to_string());
            for a in [
                "-hide_banner",
                "-y",
                "-progress",
                "pipe:1",
                "-nostats",
                "-i",
                &input.to_string_lossy(),
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
            ] {
                spec.arg(a);
            }
            spec.arg(out);
            spec
        };

        // Direct spawns (what a hand-run does).
        for i in 0..3 {
            let out = dir.join(format!("direct{i}.mp4"));
            let spec = argv(&out.to_string_lossy());
            let t = Instant::now();
            let st = tokio::process::Command::new(&spec.program)
                .args(&spec.args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .unwrap();
            assert!(st.success());
            println!("direct  run{i}: {:?}", t.elapsed());
        }
        // Through ffkit's runner (pipes drained, progress parsed, channel).
        for i in 0..3 {
            let out = dir.join(format!("runner{i}.mp4"));
            let spec = argv(&out.to_string_lossy());
            let (tx, mut rx) = unbounded_channel();
            let (_c, cancel) = tokio::sync::oneshot::channel();
            let t = Instant::now();
            spawn_job(
                tx,
                JobRequest {
                    spec,
                    output: out.clone(),
                },
                cancel,
                0,
            );
            let (ok, progress) = tokio::task::spawn_blocking(move || wait_done(&mut rx))
                .await
                .unwrap();
            assert!(ok);
            println!(
                "runner  run{i}: {:?} ({progress} progress blocks)",
                t.elapsed()
            );
            assert!(out.exists());
        }
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// §6: resize with center crop through the full stack — guards the
/// backslash-comma escaping and verifies real output dimensions.
/// Skips gracefully where FFmpeg is not installed.
#[test]
fn resize_crop_produces_square_live() {
    use ffkit::ops::fields::{BuildContext, FieldValue};
    use ffkit::ops::operation_for;
    use std::collections::HashMap;

    let Some(bin) = which::which("ffmpeg").ok() else {
        println!("skipping: no ffmpeg");
        return;
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("ffkit-resizelive-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let input = dir.join("src.mp4");
        let status = tokio::process::Command::new(&bin)
            .args([
                "-hide_banner",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=1092x863:rate=10:duration=1",
                "-c:v",
                "mpeg4",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&input)
            .status()
            .await
            .expect("generate odd-sized sample");
        assert!(status.success());

        let text = |v: &str| FieldValue::Text(v.to_string());
        let op = operation_for("resize").unwrap();
        let output = dir.join("square.mp4");
        let mut fields = HashMap::new();
        fields.insert("preset", text("1280:-2"));
        fields.insert("custom_w", text(""));
        fields.insert("custom_h", text(""));
        fields.insert("aspect", FieldValue::Toggle(0));
        fields.insert("crop", text("square"));
        fields.insert("video_codec", text("libx264"));
        fields.insert("crf", FieldValue::Int(23));
        let ctx = BuildContext {
            inputs: std::slice::from_ref(&input),
            output: Some(&output),
            probe: None,
            input_probes: Vec::new(),
            clips: Vec::new(),
            caps: None,
            fields,
        };
        let spec = op.build(&ctx).expect("resize builds");
        let (tx, mut rx) = unbounded_channel();
        let (_cancel, cancel) = tokio::sync::oneshot::channel();
        spawn_job(
            tx,
            JobRequest {
                spec,
                output: output.clone(),
            },
            cancel,
            0,
        );
        let (ok, _) = tokio::task::spawn_blocking(move || wait_done(&mut rx))
            .await
            .unwrap();
        assert!(ok, "crop run must succeed");
        let probed = tokio::process::Command::new(which::which("ffprobe").unwrap())
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(&output)
            .output()
            .await
            .expect("probe output");
        assert_eq!(String::from_utf8_lossy(&probed.stdout).trim(), "1280,1280");
        let _ = std::fs::remove_dir_all(&dir);
    });
}
