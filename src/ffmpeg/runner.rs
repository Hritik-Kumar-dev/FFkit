//! Spawn ffmpeg, stream progress, handle cancellation (spec section 7).
//!
//! The runner owns the child process and translates its two pipes into
//! channel messages: stdout `-progress` blocks become `JobProgress`, stderr
//! lines become `JobStderrLine`, and process exit becomes `JobFinished`.
//! Cancellation is graceful: `SIGTERM` on Unix (ffmpeg flushes and writes a
//! valid file) escalating to `SIGKILL` after a grace period; immediate
//! terminate on Windows.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::background::BackgroundMsg;
use crate::ffmpeg::builder::{CommandSpec, ConcatListFile};
use crate::ffmpeg::progress::ProgressParser;

/// Grace period between `SIGTERM` and `SIGKILL` on Unix.
const TERM_GRACE: Duration = Duration::from_secs(3);
/// Cap on retained stderr lines: enough for "copy error report", bounded
/// for memory. The UI ring-buffers the live view separately.
const MAX_STDERR_LINES: usize = 2000;

/// Everything the runner needs to execute one job.
#[derive(Debug, Clone)]
pub struct JobRequest {
    /// The built command (argv-executed, never shell-split).
    pub spec: CommandSpec,
    /// Expected output path — for the partial-file cleanup prompt.
    pub output: PathBuf,
}

/// How a job ended. Distinguishes ffkit-side cancellation from ffmpeg
/// failure from success so the UI never confuses them (spec §10).
#[derive(Debug, Clone)]
pub struct JobResult {
    /// Process exited 0.
    pub success: bool,
    /// The user cancelled (SIGTERM path), regardless of exit code.
    pub cancelled: bool,
    /// Exit code, when the process reported one (absent on signal death).
    pub exit_code: Option<i32>,
    /// Captured stderr, oldest first, capped at [`MAX_STDERR_LINES`].
    pub stderr: Vec<String>,
    /// Wall-clock runtime.
    pub wall_time: Duration,
}

impl JobResult {
    /// Last `n` stderr lines for the failure card (spec: generic
    /// "Conversion failed!" shows the last 20).
    pub fn stderr_tail(&self, n: usize) -> Vec<String> {
        let skip = self.stderr.len().saturating_sub(n);
        self.stderr[skip..].to_vec()
    }
}

/// Spawn the runner task. Reports through `tx`; stops early when `cancel`
/// fires (see [`cancel` semantics](self)). `job_id` tags every message so
/// the queue can route concurrent jobs. Never panics on I/O errors —
/// spawn failure becomes an unsuccessful `JobFinished`.
pub fn spawn_job(
    tx: UnboundedSender<BackgroundMsg>,
    request: JobRequest,
    cancel: oneshot::Receiver<()>,
    job_id: u64,
) {
    tokio::spawn(async move { run_job(tx, request, cancel, job_id).await });
}

async fn run_job(
    tx: UnboundedSender<BackgroundMsg>,
    request: JobRequest,
    cancel: oneshot::Receiver<()>,
    job_id: u64,
) {
    let started = Instant::now();
    let finish = |tx: &UnboundedSender<BackgroundMsg>,
                  success: bool,
                  cancelled: bool,
                  exit_code: Option<i32>,
                  stderr: Vec<String>| {
        let _ = tx.send(BackgroundMsg::JobFinished {
            job_id,
            result: JobResult {
                success,
                cancelled,
                exit_code,
                stderr,
                wall_time: started.elapsed(),
            },
        });
    };

    // Concat demuxer list files are materialized here: builders are pure
    // and cannot do I/O, so the spec carries a descriptor instead.
    if let Some(list) = &request.spec.concat_list {
        if let Err(e) = write_concat_list(list).await {
            finish(
                &tx,
                false,
                false,
                None,
                vec![format!(
                    "failed to write concat list {}: {e}",
                    list.path.display()
                )],
            );
            return;
        }
    }

    // Pre-commands (palettegen passes, list-file prep in M5) run first,
    // sequentially — they are short and have no progress channel.
    for pre in &request.spec.pre_commands {
        match Command::new(&pre.program).args(&pre.args).output().await {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                finish(
                    &tx,
                    false,
                    false,
                    output.status.code(),
                    capped_lines(&output.stderr),
                );
                return;
            }
            Err(e) => {
                finish(
                    &tx,
                    false,
                    false,
                    None,
                    vec![format!("failed to start: {e}")],
                );
                return;
            }
        }
    }

    let mut child = match Command::new(&request.spec.program)
        .args(&request.spec.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            let message = if e.kind() == std::io::ErrorKind::NotFound {
                format!("ffmpeg binary not found: {}", request.spec.program)
            } else {
                format!("failed to start ffmpeg: {e}")
            };
            finish(&tx, false, false, None, vec![message]);
            return;
        }
    };
    let pid = child.id();
    let _ = tx.send(BackgroundMsg::JobStarted { job_id, pid });

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_lines = BufReader::new(stderr).lines();
    let mut parser = ProgressParser::new();
    let mut stderr_kept: VecDeque<String> = VecDeque::new();
    let mut cancelled = false;
    // `cancel` resolves once; fuse it so the loop can keep selecting.
    let mut cancel = cancel;
    let mut cancel_fired = false;

    loop {
        tokio::select! {
            biased;
            _ = &mut cancel, if !cancel_fired => {
                cancel_fired = true;
                cancelled = true;
                terminate_gracefully(&mut child).await;
            }
            next = stdout_lines.next_line() => {
                match next {
                    Ok(Some(line)) => {
                        if let Some(update) = parser.feed_line(&line) {
                            let finished = update.finished;
                            let _ = tx.send(BackgroundMsg::JobProgress { job_id, update });
                            if finished {
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        // Stdout closed; keep draining stderr, then reap.
                        drain_stderr(&mut stderr_lines, &tx, &mut stderr_kept, job_id).await;
                        break;
                    }
                    Err(_) => break,
                }
            }
            next = stderr_lines.next_line() => {
                match next {
                    Ok(Some(line)) => {
                        push_stderr(&mut stderr_kept, &tx, job_id, line);
                    }
                    Ok(None) => {}
                    Err(_) => {}
                }
            }
        }
        if cancel_fired {
            // After cancellation the graceful terminate already reaped or
            // will reap shortly; drain remaining stderr, then exit the loop.
            drain_stderr(&mut stderr_lines, &tx, &mut stderr_kept, job_id).await;
            break;
        }
    }

    let exit = child.wait().await.ok();
    // The list file is regenerable; remove it best-effort either way.
    if let Some(list) = &request.spec.concat_list {
        let _ = tokio::fs::remove_file(&list.path).await;
    }
    let success = !cancelled && exit.is_some_and(|status| status.success());
    finish(
        &tx,
        success,
        cancelled,
        exit.and_then(|status| status.code()),
        Vec::from(stderr_kept),
    );
}

/// Read remaining stderr lines until EOF (used after stdout closes).
async fn drain_stderr(
    stderr_lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStderr>>,
    tx: &UnboundedSender<BackgroundMsg>,
    kept: &mut VecDeque<String>,
    job_id: u64,
) {
    while let Ok(Some(line)) = stderr_lines.next_line().await {
        push_stderr(kept, tx, job_id, line);
    }
}

/// Keep one stderr line in a bounded buffer. The buffer is a [`VecDeque`]
/// so eviction past the cap is O(1) — a `Vec::remove(0)` here would be an
/// O(n) shift per line on verbose encodes (thousands of lines), i.e.
/// accidental quadratic work in ffkit's own code on the hot path.
fn push_capped(buffer: &mut VecDeque<String>, line: String) {
    if buffer.len() >= MAX_STDERR_LINES {
        buffer.pop_front();
    }
    buffer.push_back(line);
}

/// Keep one stderr line (capped) and mirror it to the UI log pane.
fn push_stderr(
    kept: &mut VecDeque<String>,
    tx: &UnboundedSender<BackgroundMsg>,
    job_id: u64,
    line: String,
) {
    push_capped(kept, line.clone());
    let _ = tx.send(BackgroundMsg::JobStderrLine { job_id, line });
}

/// Materialize a concat demuxer list file: one `file '…'` line per input
/// with absolute paths (relative entries would resolve against the list
/// file's directory, not ours) and single-quote escaping.
async fn write_concat_list(list: &ConcatListFile) -> std::io::Result<()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut content = String::from("ffconcat version 1.0\n");
    for input in &list.inputs {
        let absolute = if input.is_absolute() {
            input.clone()
        } else {
            cwd.join(input)
        };
        let escaped = absolute.to_string_lossy().replace('\'', "'\\''");
        content.push_str(&format!("file '{escaped}'\n"));
    }
    if let Some(parent) = list.path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    tokio::fs::write(&list.path, content).await
}

/// Decode stderr bytes lossily into capped lines (pre-command path).
fn capped_lines(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let skip = lines.len().saturating_sub(MAX_STDERR_LINES);
    lines[skip..].to_vec()
}

/// Graceful termination: `SIGTERM` (ffmpeg flushes and writes a valid file),
/// escalating to `SIGKILL` after [`TERM_GRACE`]. Windows terminates
/// immediately — there is no console-control equivalent over pipes.
async fn terminate_gracefully(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            let term = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status()
                .await;
            if term.is_err() {
                let _ = child.kill().await;
                return;
            }
            if tokio::time::timeout(TERM_GRACE, child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
            }
        } else {
            let _ = child.kill().await;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill().await;
    }
}

/// Second-press escalation: kill `pid` immediately without grace.
/// Fire-and-forget from the UI; failure just means the graceful path is
/// already handling it.
pub async fn force_kill(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .arg("-KILL")
            .arg(pid.to_string())
            .status()
            .await;
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status()
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::push_capped;
    use super::MAX_STDERR_LINES;

    #[test]
    fn stderr_buffer_stays_capped_and_keeps_the_tail() {
        let mut buffer = std::collections::VecDeque::new();
        for i in 0..MAX_STDERR_LINES + 500 {
            push_capped(&mut buffer, format!("line {i}"));
        }
        assert_eq!(buffer.len(), MAX_STDERR_LINES);
        assert_eq!(buffer.front().map(String::as_str), Some("line 500"));
        let expected_last = format!("line {}", MAX_STDERR_LINES + 499);
        assert_eq!(buffer.back(), Some(&expected_last));
    }
}
