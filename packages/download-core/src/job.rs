//! Job struct — state machine per download job.

use std::path::PathBuf;

/// Outcome returned from `Core::enqueue`.
#[derive(Debug, Clone, PartialEq)]
pub enum EnqueueOutcome {
    /// The URL was classified as a direct download — a job was created.
    EnqueueDirect {
        job_id: String,
        /// Path under Library Root for this job's files.
        destination: PathBuf,
    },
    /// The URL requires probe-then-confirm.
    ProbeThenConfirm {
        url: String,
    },
}

/// Internal state of a download job.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JobState {
    Pending,
    Completed,
}

/// A single download job.
#[derive(Debug)]
pub struct Job {
    id: String,
    url: String,
    state: JobState,
    /// The directory where this job's files land.
    destination: PathBuf,
}

impl Job {
    pub fn new(url: String, library_root: PathBuf) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let destination = library_root.join("jobs").join(&id);
        Self {
            id,
            url,
            state: JobState::Pending,
            destination,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn destination(&self) -> &PathBuf {
        &self.destination
    }

    pub fn state(&self) -> JobState {
        self.state
    }

    pub fn is_pending(&self) -> bool {
        self.state == JobState::Pending
    }

    pub fn mark_completed(&mut self) {
        self.state = JobState::Completed;
    }
}
