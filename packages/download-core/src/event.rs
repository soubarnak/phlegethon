//! Core event types — the observable surface for platform shells.

/// Events emitted by the Download Core.
///
/// Shells consume these in their event loop to update UI state.
#[derive(Debug, Clone, PartialEq)]
pub enum CoreEvent {
    /// A job made progress.
    JobProgress {
        job_id: String,
        bytes_downloaded: u64,
        total_bytes: Option<u64>,
    },
    /// A job completed successfully.
    JobCompleted {
        job_id: String,
        bytes_written: u64,
        path: std::path::PathBuf,
    },
    /// A job was paused by the user.
    JobPaused {
        job_id: String,
    },
    /// A paused job was resumed.
    JobResumed {
        job_id: String,
    },
    /// A job failed due to an HTTP or I/O error.
    JobFailed {
        job_id: String,
        error: String,
    },
    /// A failed job was retried by the user.
    JobRetried {
        job_id: String,
    },
    /// A job was discarded (removed from the queue) by the user.
    JobDiscarded {
        job_id: String,
    },
}
