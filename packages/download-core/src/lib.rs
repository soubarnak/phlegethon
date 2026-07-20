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

    // -----------------------------------------------------------------------
    // Enqueue
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // Lifecycle commands
    // -----------------------------------------------------------------------

    /// Pause a pending job.  Emits `CoreEvent::JobPaused`.
    ///
    /// A paused job is skipped by `process_pending` until resumed.
    pub fn pause_job(&mut self, job_id: &str) -> Result<(), String> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;

        job.pause()?;

        self.event_tx
            .send(CoreEvent::JobPaused {
                job_id: job_id.to_string(),
            })
            .map_err(|_| "event channel closed".to_string())?;

        Ok(())
    }

    /// Resume a paused job.  Emits `CoreEvent::JobResumed`.
    ///
    /// The job is marked as pending and will be processed on the next
    /// `process_pending` call.
    pub fn resume_job(&mut self, job_id: &str) -> Result<(), String> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;

        job.resume()?;

        self.event_tx
            .send(CoreEvent::JobResumed {
                job_id: job_id.to_string(),
            })
            .map_err(|_| "event channel closed".to_string())?;

        Ok(())
    }

    /// Retry a failed job.  Emits `CoreEvent::JobRetried`.
    ///
    /// The job is marked as pending and eligible for processing again.
    pub fn retry_job(&mut self, job_id: &str) -> Result<(), String> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;

        job.retry()?;

        self.event_tx
            .send(CoreEvent::JobRetried {
                job_id: job_id.to_string(),
            })
            .map_err(|_| "event channel closed".to_string())?;

        Ok(())
    }

    /// Discard a job from the queue.  Emits `CoreEvent::JobDiscarded`.
    ///
    /// Once discarded the job is removed and cannot be paused/resumed/retried.
    /// Discarding an already-discarded (or non-existent) job returns an error.
    pub fn discard_job(&mut self, job_id: &str) -> Result<(), String> {
        let pos = self
            .jobs
            .iter()
            .position(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;

        self.jobs.remove(pos);

        self.event_tx
            .send(CoreEvent::JobDiscarded {
                job_id: job_id.to_string(),
            })
            .map_err(|_| "event channel closed".to_string())?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Processing
    // -----------------------------------------------------------------------

    /// Process pending jobs — runs transfers for any queued jobs.
    ///
    /// In a real Core this runs the transfer loop.  For now, it processes
    /// the work synchronously.  Paused and failed jobs are skipped.
    /// HTTP failures cause the job to transition to `Failed` state and emit
    /// `CoreEvent::JobFailed`; processing continues with the next job.
    pub fn process_pending(&mut self) -> Result<(), String> {
        // Collect indices of jobs that need work (pending only)
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
            let (data, total) = match self.http.fetch(&url, None) {
                Ok(result) => result,
                Err(err) => {
                    // Mark job as failed and emit event
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id() == job_id) {
                        job.mark_failed();
                    }
                    self.event_tx
                        .send(CoreEvent::JobFailed {
                            job_id: job_id.clone(),
                            error: err,
                        })
                        .map_err(|_| "event channel closed".to_string())?;
                    continue;
                }
            };

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
            if let Err(e) = std::fs::write(&file_path, &data) {
                // I/O error — mark as failed
                if let Some(job) = self.jobs.iter_mut().find(|j| j.id() == job_id) {
                    job.mark_failed();
                }
                self.event_tx
                    .send(CoreEvent::JobFailed {
                        job_id: job_id.clone(),
                        error: format!("write {file_path:?}: {e}"),
                    })
                    .map_err(|_| "event channel closed".to_string())?;
                continue;
            }

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
