//! Download Core — shared Rust library for download management.
//!
//! The Core owns URL classification, job lifecycle, and storage.
//! Platform shells consume this library via its public command/event API.

mod classification;
mod event;
mod http;
mod job;
mod storage;

pub use event::CoreEvent;
pub use http::HttpBackend;
pub use job::EnqueueOutcome;

use std::path::PathBuf;

/// The Download Core instance.
///
/// Create one per app session.  Drop to tear down.
pub struct Core {
    http: Box<dyn HttpBackend>,
    library_root: Option<PathBuf>,
    jobs: Vec<job::Job>,
    event_tx: crossbeam_channel::Sender<CoreEvent>,
}

impl Core {
    /// Create a new Core with the given HTTP backend.
    ///
    /// Returns a `(Core, Receiver<CoreEvent>)` pair.  The caller reads events
    /// from the receiver (typically in a shell event loop).
    pub fn new(http: Box<dyn HttpBackend>) -> (Self, crossbeam_channel::Receiver<CoreEvent>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let core = Self {
            http,
            library_root: None,
            jobs: Vec::new(),
            event_tx: tx,
        };
        (core, rx)
    }

    /// Set the Library Root — the base directory for all downloads.
    ///
    /// Creates the directory if it does not exist.
    pub fn set_library_root(&mut self, root: PathBuf) -> Result<(), String> {
        std::fs::create_dir_all(&root).map_err(|e| format!("create library root: {e}"))?;
        self.library_root = Some(root);
        Ok(())
    }

    /// Classify and enqueue a URL.
    ///
    /// Returns the classification outcome.  For `EnqueueDirect`, a job is
    /// created and ready to process.  For `ProbeThenConfirm`, the shell
    /// should present a probe sheet and call back with selections.
    pub fn enqueue(&mut self, url_str: &str) -> Result<EnqueueOutcome, String> {
        let parsed = url::Url::parse(url_str).map_err(|_| "invalid URL".to_string())?;
        let root = self
            .library_root
            .as_ref()
            .ok_or_else(|| "Library Root not set".to_string())?;

        match classification::classify(&parsed) {
            classification::Kind::EnqueueDirect => {
                let job = job::Job::new(url_str.to_string(), root.clone());
                let job_id = job.id().to_string();
                let dest = job.destination().to_path_buf();
                self.jobs.push(job);
                Ok(EnqueueOutcome::EnqueueDirect {
                    job_id,
                    destination: dest,
                })
            }
            classification::Kind::ProbeThenConfirm => {
                Ok(EnqueueOutcome::ProbeThenConfirm {
                    url: url_str.to_string(),
                })
            }
        }
    }

    /// Process pending jobs — runs transfers for any queued jobs.
    ///
    /// In a real Core this runs the transfer loop.  For now, it processes
    /// the work synchronously.
    pub fn process_pending(&mut self) -> Result<(), String> {
        // Collect indices of jobs that need work
        let indices: Vec<usize> = self
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| j.is_pending())
            .map(|(i, _)| i)
            .collect();

        for i in indices {
            let (job_id, url, dest) = {
                let job = &self.jobs[i];
                (job.id().to_string(), job.url().to_string(), job.destination().to_path_buf())
            };

            // Emit progress
            self.event_tx
                .send(CoreEvent::JobProgress {
                    job_id: job_id.clone(),
                    bytes_downloaded: 0,
                    total_bytes: None,
                })
                .map_err(|_| "event channel closed".to_string())?;

            // Fetch from HTTP backend
            let (data, total) = self.http.fetch(&url, None)?;

            // Write to disk
            std::fs::create_dir_all(&dest)
                .map_err(|e| format!("create job dir {dest:?}: {e}"))?;

            // Derive filename from URL
            let filename = url::Url::parse(&url)
                .ok()
                .and_then(|u| {
                    u.path_segments()
                        .and_then(|s| s.last())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "download".to_string());

            let file_path = dest.join(&filename);
            std::fs::write(&file_path, &data)
                .map_err(|e| format!("write {file_path:?}: {e}"))?;

            // Emit progress (complete)
            self.event_tx
                .send(CoreEvent::JobProgress {
                    job_id: job_id.clone(),
                    bytes_downloaded: total,
                    total_bytes: Some(total),
                })
                .map_err(|_| "event channel closed".to_string())?;

            // Emit completed
            self.event_tx
                .send(CoreEvent::JobCompleted {
                    job_id: job_id.clone(),
                    bytes_written: total,
                    path: file_path,
                })
                .map_err(|_| "event channel closed".to_string())?;

            // Mark job complete
            self.jobs[i].mark_completed();
        }

        Ok(())
    }
}
