//! Batch job queue (spec section 12).
//!
//! Selecting multiple files applies the same operation to each. Jobs run
//! **sequentially by default** — parallel ffmpeg processes contend for CPU
//! and usually finish slower overall — with `max_concurrent_jobs`
//! configurable for users who know better. A failed job never halts the
//! queue; the end summarizes done/failed/cancelled.

pub mod job;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use tokio::sync::oneshot;

use crate::ffmpeg::progress::ProgressUpdate;

pub use job::{expand_template, Job, JobStatus, NewJob};

/// Live queue state. Owned by [`App`](crate::app::App).
pub struct QueueState {
    /// All jobs in enqueue order.
    pub jobs: Vec<Job>,
    /// Execution in progress (started, not yet settled).
    pub running: bool,
    /// Cursor on the queue screen.
    pub selected: usize,
    /// Ids currently executing.
    pub active: HashSet<u64>,
    /// Cancel triggers for active jobs.
    pub cancels: HashMap<u64, oneshot::Sender<()>>,
    /// Next job id.
    pub next_id: u64,
    /// Outputs that already exist when a batch was armed. With
    /// `confirm_overwrite` the queue waits for an explicit `y` instead of
    /// asking per file; empty means "clear to run".
    pub overwrite_conflicts: Vec<PathBuf>,
    /// End-of-run summary, e.g. `"Queue done: 4 ok, 1 failed"`.
    pub summary: Option<String>,
}

impl QueueState {
    /// Empty, idle queue.
    pub fn new() -> Self {
        Self {
            jobs: Vec::new(),
            running: false,
            selected: 0,
            active: HashSet::new(),
            cancels: HashMap::new(),
            next_id: 1,
            overwrite_conflicts: Vec::new(),
            summary: None,
        }
    }

    /// Hand out a fresh job id (also used by single runs to share the space).
    pub fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Add jobs and reset the end summary.
    pub fn enqueue(&mut self, jobs: Vec<Job>) {
        self.jobs.extend(jobs);
        self.summary = None;
    }

    /// Look up a job mutably by id.
    pub fn find_mut(&mut self, id: u64) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|job| job.id == id)
    }

    /// Ids still waiting to start, in order.
    pub fn pending_ids(&self) -> Vec<u64> {
        self.jobs
            .iter()
            .filter(|job| job.status == JobStatus::Pending)
            .map(|job| job.id)
            .collect()
    }

    /// True when nothing is running or pending (execution settled).
    pub fn settled(&self) -> bool {
        self.active.is_empty() && !self.jobs.iter().any(|job| job.status == JobStatus::Pending)
    }

    /// Aggregate line for the queue header: completed/total plus the live
    /// ratio of the first active job.
    pub fn aggregate_line(&self) -> String {
        let total = self.jobs.len();
        let done = self
            .jobs
            .iter()
            .filter(|job| !matches!(job.status, JobStatus::Pending | JobStatus::Running))
            .count();
        let failed = self
            .jobs
            .iter()
            .filter(|job| matches!(job.status, JobStatus::Failed(_)))
            .count();
        let mut line = format!("{done}/{total} settled");
        if failed > 0 {
            line.push_str(&format!(" · {failed} failed"));
        }
        line
    }

    /// End-of-run summary; also stored for the screen to keep showing.
    pub fn finish_summary(&mut self) -> String {
        let mut done = 0;
        let mut failed = 0;
        let mut cancelled = 0;
        for job in &self.jobs {
            match &job.status {
                JobStatus::Done => done += 1,
                JobStatus::Failed(_) => failed += 1,
                JobStatus::Cancelled => cancelled += 1,
                JobStatus::Pending | JobStatus::Running => {}
            }
        }
        let summary = format!("Queue done: {done} ok, {failed} failed, {cancelled} cancelled");
        self.summary = Some(summary.clone());
        summary
    }

    /// Drop finished jobs (done/failed/cancelled) from the list.
    pub fn clear_finished(&mut self) {
        self.jobs
            .retain(|job| matches!(job.status, JobStatus::Pending | JobStatus::Running));
        self.selected = 0;
    }

    /// Remove one pending job (queue screen `x`). Running jobs must be
    /// cancelled first — never yanked from under the runner.
    pub fn remove_pending(&mut self, id: u64) -> bool {
        match self.jobs.iter().position(|job| job.id == id) {
            Some(index) if self.jobs[index].status == JobStatus::Pending => {
                self.jobs.remove(index);
                true
            }
            _ => false,
        }
    }

    /// Fold one progress update into the job's snapshot for the queue rows.
    pub fn apply_progress(&mut self, job_id: u64, update: &ProgressUpdate) {
        if let Some(job) = self.find_mut(job_id) {
            job.apply_progress(update);
        }
    }
}

impl Default for QueueState {
    fn default() -> Self {
        Self::new()
    }
}
