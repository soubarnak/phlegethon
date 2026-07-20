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
    Paused,
    Failed,
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
    /// Number of bytes successfully written to disk so far.
    /// Used for resumable transfers via Range headers.
    bytes_received: u64,
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
            bytes_received: 0,
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

    #[allow(dead_code)]
    pub fn state(&self) -> JobState {
        self.state
    }

    pub fn is_pending(&self) -> bool {
        self.state == JobState::Pending
    }

    #[allow(dead_code)]
    pub fn has_partial_data(&self) -> bool {
        self.bytes_received > 0
    }

    pub fn bytes_received(&self) -> u64 {
        self.bytes_received
    }

    pub fn add_bytes_received(&mut self, n: u64) {
        self.bytes_received += n;
    }

    pub fn mark_completed(&mut self) {
        self.state = JobState::Completed;
    }

    pub fn pause(&mut self) -> Result<(), String> {
        match self.state {
            JobState::Pending => {
                self.state = JobState::Paused;
                Ok(())
            }
            _ => Err(format!(
                "cannot pause job {}: current state is {:?}",
                self.id, self.state
            )),
        }
    }

    pub fn resume(&mut self) -> Result<(), String> {
        match self.state {
            JobState::Paused => {
                self.state = JobState::Pending;
                Ok(())
            }
            _ => Err(format!(
                "cannot resume job {}: current state is {:?}",
                self.id, self.state
            )),
        }
    }

    pub fn mark_failed(&mut self) {
        self.state = JobState::Failed;
    }

    pub fn retry(&mut self) -> Result<(), String> {
        match self.state {
            JobState::Failed => {
                self.state = JobState::Pending;
                self.bytes_received = 0;
                Ok(())
            }
            _ => Err(format!(
                "cannot retry job {}: current state is {:?}",
                self.id, self.state
            )),
        }
    }
}
