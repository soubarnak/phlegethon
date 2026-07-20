//! Download Core — shared Rust library for download management.
//!
//! The Core owns URL classification, job lifecycle, and storage.
//! Platform shells consume this library via its public command/event API.

mod classification;
mod event;
mod extractor;
mod http;
mod job;
mod storage;

pub use event::CoreEvent;
pub use extractor::{
    ExtractorToolchain, FakeExtractorToolchain, ItemSelection, MediaItem, ProbeResult,
    ProbeSelection, Quality,
};
pub use http::HttpBackend;
pub use job::EnqueueOutcome;

use std::collections::HashMap;
use std::path::PathBuf;

/// The Download Core instance.
///
/// Create one per app session.  Drop to tear down.
pub struct Core {
    http: Box<dyn HttpBackend>,
    extractor: Box<dyn ExtractorToolchain>,
    library_root: Option<PathBuf>,
    jobs: Vec<job::Job>,
    event_tx: crossbeam_channel::Sender<CoreEvent>,
    /// Global bandwidth cap in bytes per process_pending call.
    /// None = globally uncapped.
    bandwidth_cap: Option<u64>,
    /// Pending probe results, keyed by URL.
    /// Stored until confirm or replaced by a new probe for the same URL.
    pending_probes: HashMap<String, ProbeResult>,
}

impl Core {
    /// Create a new Core with the given HTTP backend and extractor toolchain.
    ///
    /// Returns a `(Core, Receiver<CoreEvent>)` pair.  The caller reads events
    /// from the receiver (typically in a shell event loop).
    pub fn new(
        http: Box<dyn HttpBackend>,
        extractor: Box<dyn ExtractorToolchain>,
    ) -> (Self, crossbeam_channel::Receiver<CoreEvent>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let core = Self {
            http,
            extractor,
            library_root: None,
            jobs: Vec::new(),
            event_tx: tx,
            bandwidth_cap: None,
            pending_probes: HashMap::new(),
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
    // Bandwidth cap
    // -----------------------------------------------------------------------

    /// Set the global bandwidth cap in bytes per `process_pending` call.
    ///
    /// When set, the total bytes transferred across ALL jobs in a single
    /// `process_pending` call will not exceed this value.
    /// None/unset = globally uncapped.
    pub fn set_global_bandwidth_cap(&mut self, cap: u64) {
        self.bandwidth_cap = Some(cap);
    }

    /// Remove the global bandwidth cap (restore uncapped default).
    pub fn remove_global_bandwidth_cap(&mut self) {
        self.bandwidth_cap = None;
    }

    /// Return the current global bandwidth cap, if set.
    pub fn global_bandwidth_cap(&self) -> Option<u64> {
        self.bandwidth_cap
    }

    /// Set a per-job bandwidth cap in bytes per `process_pending` call.
    ///
    /// This job will not transfer more than `cap` bytes in a single
    /// `process_pending` call, regardless of global cap status.
    pub fn set_job_bandwidth_cap(&mut self, job_id: &str, cap: u64) -> Result<(), String> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        job.set_bandwidth_cap(cap);
        Ok(())
    }

    /// Remove the per-job bandwidth cap for the given job.
    pub fn remove_job_bandwidth_cap(&mut self, job_id: &str) -> Result<(), String> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        job.remove_bandwidth_cap();
        Ok(())
    }

    /// Return the per-job bandwidth cap for the given job, if set.
    pub fn job_bandwidth_cap(&self, job_id: &str) -> Result<Option<u64>, String> {
        let job = self
            .jobs
            .iter()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        Ok(job.bandwidth_cap())
    }

    // -----------------------------------------------------------------------
    // Probe-then-Confirm
    // -----------------------------------------------------------------------

    /// Probe a URL using the Extractor Toolchain.
    ///
    /// Returns discovered items + qualities and emits a `CoreEvent::ProbeResult`
    /// event.  The probe result is stored internally so that
    /// [`confirm_probe_selection`] can validate and create jobs from the
    /// selection.
    ///
    /// Errors (unknown URL, toolchain failure) produce a `ProbeFailed` event
    /// instead and return the error.
    pub fn probe(&mut self, url: &str) -> Result<ProbeResult, String> {
        match self.extractor.probe(url) {
            Ok(result) => {
                // Store for subsequent confirm
                self.pending_probes
                    .insert(url.to_string(), result.clone());

                // Emit event
                self.event_tx
                    .send(CoreEvent::ProbeResult {
                        url: url.to_string(),
                        items: result.items.clone(),
                        source_name: result.source_name.clone(),
                    })
                    .map_err(|_| "event channel closed".to_string())?;

                Ok(result)
            }
            Err(err) => {
                self.event_tx
                    .send(CoreEvent::ProbeFailed {
                        url: url.to_string(),
                        error: err.clone(),
                    })
                    .map_err(|_| "event channel closed".to_string())?;
                Err(err)
            }
        }
    }

    /// Confirm a probe selection and create jobs for the selected items.
    ///
    /// Takes a [`ProbeSelection`] that references a previously probed URL
    /// and lists which items at which qualities to download.  Returns the
    /// job IDs of the created jobs.
    ///
    /// Each selected item creates one job.  Batch/channel jobs use a
    /// dedicated Job Destination subfolder derived from the source name.
    ///
    /// Errors if no pending probe exists for the URL, or if the selection
    /// references unknown items or qualities.
    pub fn confirm_probe_selection(
        &mut self,
        selection: &ProbeSelection,
    ) -> Result<Vec<String>, String> {
        let root = self
            .library_root
            .as_ref()
            .ok_or_else(|| "Library Root not set".to_string())?;

        let probe = self
            .pending_probes
            .get(&selection.url)
            .ok_or_else(|| {
                format!(
                    "no pending probe for {}. Call probe() first.",
                    selection.url
                )
            })?;

        // Build lookup maps
        let items_map: HashMap<&str, &MediaItem> =
            probe.items.iter().map(|i| (i.id.as_str(), i)).collect();

        let source_name = probe.source_name.clone();
        let mut job_ids = Vec::new();

        for sel in &selection.items {
            let item = items_map.get(sel.item_id.as_str()).ok_or_else(|| {
                format!(
                    "item '{}' not found in probe result for {}",
                    sel.item_id, selection.url
                )
            })?;

            // Validate quality exists
            let _quality = item
                .qualities
                .iter()
                .find(|q| q.id == sel.quality_id)
                .ok_or_else(|| {
                    format!(
                        "quality '{}' not found for item '{}' (available: {:?})",
                        sel.quality_id,
                        sel.item_id,
                        item.qualities.iter().map(|q| &q.id).collect::<Vec<_>>()
                    )
                })?;

            // For single-video probes without a source name, use the
            // item title for the subfolder if available.
            let job_source = source_name.clone().or_else(|| {
                if probe.items.len() == 1 {
                    Some(item.title.clone())
                } else {
                    None
                }
            });

            let job = job::Job::new_with_source(
                item.url.clone(),
                root.clone(),
                job_source,
            );

            job_ids.push(job.id().to_string());
            self.jobs.push(job);
        }

        Ok(job_ids)
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
    ///
    /// When a job has partial data already written (`bytes_received > 0`),
    /// the HTTP backend is called with a `Range` header to resume from the
    /// last successful byte.  New data is appended to the existing file.
    /// If `bytes_received < total` after the fetch, the job stays pending
    /// so the next `process_pending` call can continue where it left off.
    pub fn process_pending(&mut self) -> Result<(), String> {
        // Collect indices of jobs that need work (pending only)
        let indices: Vec<usize> = self
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| j.is_pending())
            .map(|(i, _)| i)
            .collect();

        // Track remaining global budget for this call
        let mut global_remaining = self.bandwidth_cap;

        for i in indices {
            // Exhausted global cap — skip remaining jobs
            if global_remaining == Some(0) {
                break;
            }

            let (job_id, url, dest, bytes_received, job_cap) = {
                let job = &self.jobs[i];
                (
                    job.id().to_string(),
                    job.url().to_string(),
                    job.destination().to_path_buf(),
                    job.bytes_received(),
                    job.bandwidth_cap(),
                )
            };

            // Effective cap = min(global_remaining, per-job cap)
            // None = uncapped for that axis
            let effective_cap: Option<u64> = match (global_remaining, job_cap) {
                (Some(g), Some(j)) => Some(g.min(j)),
                (Some(g), None) => Some(g),
                (None, Some(j)) => Some(j),
                (None, None) => None,
            };

            // Build optional Range header if we have partial data
            let range = if bytes_received > 0 {
                Some(bytes_received..u64::MAX)
            } else {
                None
            };

            // Emit progress (with cumulative bytes so far)
            self.event_tx
                .send(CoreEvent::JobProgress {
                    job_id: job_id.clone(),
                    bytes_downloaded: bytes_received,
                    total_bytes: None,
                })
                .map_err(|_| "event channel closed".to_string())?;

            // Fetch from HTTP backend (with Range if resuming)
            let (mut data, total) = match self.http.fetch(&url, range) {
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

            // Apply bandwidth cap: truncate data if it exceeds the effective cap
            let bytes_this_call = if let Some(cap) = effective_cap {
                let cap_usize = cap as usize;
                if data.len() > cap_usize {
                    data.truncate(cap_usize);
                }
                let actual = data.len() as u64;
                if let Some(ref mut remaining) = global_remaining {
                    *remaining = remaining.saturating_sub(actual);
                }
                actual
            } else {
                data.len() as u64
            };

            // Write to disk
            std::fs::create_dir_all(&dest)
                .map_err(|e| format!("create job dir {dest:?}: {e}"))?;

            // Derive filename from URL
            let filename = url::Url::parse(&url)
                .ok()
                .and_then(|u| {
                    u.path_segments()
                        .and_then(|mut s| s.next_back())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "download".to_string());

            let file_path = dest.join(&filename);

            // Append to existing file (or create if first write)
            let write_result = if bytes_received > 0 {
                use std::io::Write;
                std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(&file_path)
                    .and_then(|mut f| f.write_all(&data))
                    .map_err(|e| format!("append {file_path:?}: {e}"))
            } else {
                std::fs::write(&file_path, &data)
                    .map_err(|e| format!("write {file_path:?}: {e}"))
            };

            if let Err(e) = write_result {
                // I/O error — mark as failed
                if let Some(job) = self.jobs.iter_mut().find(|j| j.id() == job_id) {
                    job.mark_failed();
                }
                self.event_tx
                    .send(CoreEvent::JobFailed {
                        job_id: job_id.clone(),
                        error: e,
                    })
                    .map_err(|_| "event channel closed".to_string())?;
                continue;
            }

            // Update bytes_received with what we just wrote
            let new_bytes = bytes_received + bytes_this_call;
            if let Some(job) = self.jobs.iter_mut().find(|j| j.id() == job_id) {
                job.add_bytes_received(bytes_this_call);
            }

            // Emit progress with cumulative total
            self.event_tx
                .send(CoreEvent::JobProgress {
                    job_id: job_id.clone(),
                    bytes_downloaded: new_bytes,
                    total_bytes: Some(total),
                })
                .map_err(|_| "event channel closed".to_string())?;

            // Mark complete only if we have all the bytes
            if new_bytes >= total {
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
            // Otherwise the job stays pending — next process_pending
            // call will resume with a Range header.
        }

        Ok(())
    }
}
