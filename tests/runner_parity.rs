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
fn scratch_runner_vs_direct_wall_time() {
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
