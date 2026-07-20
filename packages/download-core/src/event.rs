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
}
