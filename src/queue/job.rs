//! A single queued ffmpeg job: pending/running/done/failed/cancelled.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ffmpeg::builder::CommandSpec;
use crate::ffmpeg::progress::{progress_ratio, ProgressUpdate};
use crate::ffmpeg::runner::JobResult;

/// Lifecycle of one queued job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    /// Waiting for a slot.
    Pending,
    /// Executing.
    Running,
    /// Exited 0.
    Done,
    /// ffmpeg failed; the short reason is displayable as-is.
    Failed(String),
    /// Cancelled by the user.
    Cancelled,
}

/// Constructor bundle for [`Job::pending`] — eight fields are clearer as a
/// struct than as an argument list.
#[derive(Debug, Clone)]
pub struct NewJob {
    /// Stable id shared with runner messages.
    pub id: u64,
    /// Operation id, e.g. `"compress"`.
    pub op_id: String,
    /// Operation display name.
    pub op_name: String,
    /// Input file.
    pub input: PathBuf,
    /// Output file.
    pub output: PathBuf,
    /// The built command.
    pub spec: CommandSpec,
    /// Total duration for percentage; `None` → row shows a spinner.
    pub total_duration: Option<Duration>,
    /// Input size for before/after, when known.
    pub input_size: Option<u64>,
}

/// One unit of batch work: the built spec plus display state.
#[derive(Debug, Clone)]
pub struct Job {
    /// Stable id shared with runner messages.
    pub id: u64,
    /// Operation id, e.g. `"compress"`.
    pub op_id: String,
    /// Operation display name.
    pub op_name: String,
    /// Input file.
    pub input: PathBuf,
    /// Output file.
    pub output: PathBuf,
    /// Shell-quoted command for display.
    pub display_command: String,
    /// The built command.
    pub spec: CommandSpec,
    /// Total duration for percentage; `None` → row shows a spinner.
    pub total_duration: Option<Duration>,
    /// Input size for before/after, when known.
    pub input_size: Option<u64>,
    /// Current lifecycle state.
    pub status: JobStatus,
    /// Latest progress ratio for the row bar.
    pub ratio: Option<f64>,
    /// Latest media timestamp reached.
    pub out_time: Option<Duration>,
    /// Latest speed multiplier.
    pub speed: Option<f64>,
    /// Final result, once settled.
    pub result: Option<JobResult>,
}

impl Job {
    /// Fresh pending job.
    pub fn pending(new: NewJob) -> Self {
        Self {
            display_command: new.spec.to_display(),
            status: JobStatus::Pending,
            ratio: None,
            out_time: None,
            speed: None,
            result: None,
            id: new.id,
            op_id: new.op_id,
            op_name: new.op_name,
            input: new.input,
            output: new.output,
            spec: new.spec,
            total_duration: new.total_duration,
            input_size: new.input_size,
        }
    }

    /// One-line status for the queue rows.
    pub fn status_text(&self) -> String {
        match &self.status {
            JobStatus::Pending => "pending".to_string(),
            JobStatus::Running => match self.ratio {
                Some(ratio) => format!("running {:>3.0}%", ratio * 100.0),
                None => "running…".to_string(),
            },
            JobStatus::Done => "done ✓".to_string(),
            JobStatus::Failed(reason) => format!("failed: {reason}"),
            JobStatus::Cancelled => "cancelled".to_string(),
        }
    }

    /// Fold a progress update into the row snapshot.
    pub fn apply_progress(&mut self, update: &ProgressUpdate) {
        if update.out_time.is_some() {
            self.out_time = update.out_time;
        }
        if update.speed.is_some() {
            self.speed = update.speed;
        }
        match (self.out_time, self.total_duration) {
            (Some(out), Some(total)) => self.ratio = progress_ratio(out, total),
            _ => self.ratio = None,
        }
    }

    /// Settle the job from its final result. Failures keep a one-line
    /// reason (last stderr line or exit code); the full stderr stays in
    /// `result` for the error report.
    pub fn settle(&mut self, result: JobResult) {
        if result.success {
            self.status = JobStatus::Done;
        } else if result.cancelled {
            self.status = JobStatus::Cancelled;
        } else {
            let reason = result
                .stderr
                .last()
                .filter(|line| !line.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| match result.exit_code {
                    Some(code) => format!("exit code {code}"),
                    None => "unknown failure".to_string(),
                });
            // Keep rows readable: first 100 chars of the failing line.
            let mut short: String = reason.chars().take(100).collect();
            if short.len() < reason.len() {
                short.push('…');
            }
            self.status = JobStatus::Failed(short);
        }
        self.result = Some(result);
    }
}

/// Expand a batch naming template. Variables: `{stem}` (input file stem),
/// `{ext}`, `{parent}` (input's directory, `.` when relative and bare),
/// `{index}` (1-based position in the batch), `{date}` (YYYY-MM-DD),
/// `{suffix}` (caller-supplied, e.g. the builder's own suffix).
pub fn expand_template(
    template: &str,
    input: &Path,
    index: usize,
    suffix: &str,
    ext: &str,
) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_string());
    let parent = input
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                ".".to_string()
            } else {
                p.to_string_lossy().into_owned()
            }
        })
        .unwrap_or_else(|| ".".to_string());
    let text = template
        .replace("{stem}", &stem)
        .replace("{ext}", ext)
        .replace("{parent}", &parent)
        .replace("{index}", &index.to_string())
        .replace("{date}", &today())
        .replace("{suffix}", suffix);
    // Bare input names render parent as "." — strip the cosmetic "./"
    // so batch outputs stay clean relative paths.
    PathBuf::from(text.strip_prefix("./").unwrap_or(&text).to_string())
}

/// Today's date as YYYY-MM-DD, via days-to-civil (no date dependency).
fn today() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86400)
        .unwrap_or(0);
    civil_from_days(days as i64)
}

/// Howard Hinnant's days-to-civil algorithm: days since 1970-01-01 →
/// (year, month, day). Handles the full civil range, negatives included.
fn civil_from_days(days: i64) -> String {
    let shifted = days + 719468;
    let era = shifted.div_euclid(146097);
    let day_of_era = shifted.rem_euclid(146097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_expands_all_variables() {
        let out = expand_template(
            "{parent}/{stem}_{suffix}.{ext}",
            Path::new("/v/holiday.mp4"),
            3,
            "compressed",
            "mp4",
        );
        assert_eq!(out, PathBuf::from("/v/holiday_compressed.mp4"));
    }

    #[test]
    fn template_supports_index_and_date() {
        let out = expand_template(
            "{stem}-{index}-{date}.{ext}",
            Path::new("clip.mkv"),
            2,
            "x",
            "mp4",
        );
        let text = out.to_string_lossy();
        assert!(text.starts_with("clip-2-20"), "{text}");
        assert!(text.ends_with(".mp4"), "{text}");
    }

    #[test]
    fn civil_dates_match_known_values() {
        assert_eq!(civil_from_days(0), "1970-01-01");
        assert_eq!(civil_from_days(20361), "2025-09-30");
        assert_eq!(civil_from_days(-1), "1969-12-31");
    }

    #[test]
    fn settle_keeps_a_short_failure_reason() {
        let mut job = Job::pending(NewJob {
            id: 1,
            op_id: "compress".into(),
            op_name: "Compress".into(),
            input: PathBuf::from("in.mp4"),
            output: PathBuf::from("out.mp4"),
            spec: CommandSpec::new("ffmpeg"),
            total_duration: None,
            input_size: None,
        });
        job.settle(JobResult {
            success: false,
            cancelled: false,
            exit_code: Some(1),
            stderr: vec!["Error: something broke badly".into()],
            wall_time: Duration::ZERO,
        });
        assert_eq!(
            job.status,
            JobStatus::Failed("Error: something broke badly".into())
        );
    }
}
