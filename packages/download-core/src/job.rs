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

/// Sanitize a source name for use as a folder name.
///
/// Replaces non-alphanumeric characters (except `-`, `_`, `.`) with `_`
/// and trims to a reasonable length.
pub fn sanitize_folder_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Trim to 64 chars max
    let truncated: String = sanitized.chars().take(64).collect();
    let trimmed = truncated.trim_end_matches('_').to_string();
    if trimmed.is_empty() {
        "untitled".to_string()
    } else {
        trimmed
    }
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
    /// Per-job bandwidth cap in bytes per process_pending call.
    /// None = uncapped for this job.
    bandwidth_cap: Option<u64>,
    /// Optional source name for batch/channel subfolder naming.
    source: Option<String>,
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
            bandwidth_cap: None,
            source: None,
        }
    }

    /// Create a new job for a probe-then-confirm item.
    ///
    /// `source_name` is optional; when present (batch/channel), the Job
    /// Destination is a dedicated subfolder derived from the source.
    pub fn new_with_source(
        url: String,
        library_root: PathBuf,
        source: Option<String>,
    ) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let destination = match &source {
            Some(name) => {
                let safe_name = sanitize_folder_name(name);
                library_root.join("jobs").join(format!("{}-{}", safe_name, &id[..8]))
            }
            None => library_root.join("jobs").join(&id),
        };
        Self {
            id,
            url,
            state: JobState::Pending,
            destination,
            bytes_received: 0,
            bandwidth_cap: None,
            source,
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

    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
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

    pub fn bandwidth_cap(&self) -> Option<u64> {
        self.bandwidth_cap
    }

    pub fn set_bandwidth_cap(&mut self, cap: u64) {
        self.bandwidth_cap = Some(cap);
    }

    pub fn remove_bandwidth_cap(&mut self) {
        self.bandwidth_cap = None;
    }

    pub fn mark_completed(&mut self) {
        self.state = JobState::Completed;
    }

    pub fn set_source(&mut self, source: Option<String>) {
        self.source = source;
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
