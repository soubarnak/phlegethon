//! Download Core — shared Rust library for download management.
//!
//! The Core owns URL classification, job lifecycle, and storage.
//! Platform shells consume this library via its public command/event API.

mod cabi;
mod classification;
mod event;
mod extractor;
mod http;
mod job;
mod mirror_set;
mod storage;
pub mod ytdlp_extractor;
pub use ytdlp_extractor::{FakeYtDlpExtractor, YtDlpConfig, YtDlpExtractorToolchain};

pub use event::CoreEvent;
pub use extractor::{
    ExtractorToolchain, FakeExtractorToolchain, ItemSelection, MediaItem, ProbeResult,
    ProbeSelection, Quality,
};
pub use http::HttpBackend;
pub use job::EnqueueOutcome;
pub use mirror_set::MirrorSet;

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

/// The Download Core instance.
///
/// Create one per app session.  Drop to tear down.
/// Default number of concurrent segments for segmented transfer.
pub const DEFAULT_SEGMENT_COUNT: u64 = 4;

pub struct Core {
    http: Arc<dyn HttpBackend>,
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
            http: Arc::from(http),
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
    // Mirror Set management
    // -----------------------------------------------------------------------

    /// Add a mirror URL to an existing job's Mirror Set.
    ///
    /// The mirror is appended to the ordered list and will be tried
    /// if the current mirror fails.
    pub fn add_job_mirror(&mut self, job_id: &str, url: String) -> Result<(), String> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        job.add_mirror(url);
        Ok(())
    }

    /// Parse a Metalink / `.meta4` XML document and add discovered URLs
    /// as mirrors to the specified job.
    pub fn add_metalink_to_job(&mut self, job_id: &str, xml: &str) -> Result<usize, String> {
        let urls = MirrorSet::from_metalink(xml);
        let count = urls.len();
        if count == 0 {
            return Ok(0);
        }
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        for url in urls {
            job.add_mirror(url);
        }
        Ok(count)
    }

    /// Return all mirror URLs for a job.
    pub fn job_mirrors(&self, job_id: &str) -> Result<Vec<String>, String> {
        let job = self
            .jobs
            .iter()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        Ok(job.mirror_urls().to_vec())
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
    /// Paused and failed jobs are skipped.  HTTP failures cause the job to
    /// transition to `Failed` state and emit `CoreEvent::JobFailed`;
    /// processing continues with the next job.
    ///
    /// ## Segmented Transfer (new jobs)
    ///
    /// For a fresh job (`bytes_received == 0`) whose HTTP backend reports
    /// Range support, the Core splits the file into [`DEFAULT_SEGMENT_COUNT`]
    /// concurrent Range requests using scoped threads.  Results are assembled
    /// in order and written to disk in one pass.
    ///
    /// ## Fallback (no Range) / Resumable
    ///
    /// When Range is not supported, or when a job has partial data already
    /// written (`bytes_received > 0`), the single-connection path is used:
    /// a plain GET (or GET with `Range` for resume), with bandwidth-capped
    /// truncation if needed.
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
            let effective_cap: Option<u64> = match (global_remaining, job_cap) {
                (Some(g), Some(j)) => Some(g.min(j)),
                (Some(g), None) => Some(g),
                (None, Some(j)) => Some(j),
                (None, None) => None,
            };

            // --- Segmented Transfer for fresh jobs ---
            // When bytes_received == 0, check if the server supports Range.
            // If yes, use multi-connection segmented transfer.
            if bytes_received == 0 {
                // Check primary URL for Range support; if it fails, fall through
                // to single-connection path below.
                let supports_range = match self.http.supports_range(&url) {
                    Ok(true) => true,
                    _ => false,
                };

                if supports_range {
                    // Segmented fetch loop: retry with next mirror on failure
                    let seg_result = 'seg_loop: loop {
                        let current_url = {
                            let job = &self.jobs[i];
                            job.url().to_string()
                        };

                        match self.segmented_fetch(&current_url, effective_cap) {
                            Ok(result) => break 'seg_loop Some(result),
                            Err(err) => {
                                // Try next mirror
                                if self.jobs[i].advance_mirror() {
                                    let next_url = {
                                        let job = &self.jobs[i];
                                        job.url().to_string()
                                    };
                                    self.event_tx
                                        .send(CoreEvent::JobMirrorFailover {
                                            job_id: job_id.clone(),
                                            previous_url: current_url,
                                            next_url,
                                            error: err,
                                        })
                                        .map_err(|_| "event channel closed".to_string())?;
                                    continue 'seg_loop;
                                }
                                // All mirrors exhausted
                                self.jobs[i].mark_failed();
                                self.event_tx
                                    .send(CoreEvent::JobFailed {
                                        job_id: job_id.clone(),
                                        error: err,
                                    })
                                    .map_err(|_| "event channel closed".to_string())?;
                                break 'seg_loop None;
                            }
                        }
                    };

                    match seg_result {
                        Some((total_size, full_data)) => {
                            let bytes_this_call = full_data.len() as u64;

                            if let Some(ref mut remaining) = global_remaining {
                                *remaining = remaining.saturating_sub(bytes_this_call);
                            }

                            // Emit progress before write
                            self.event_tx
                                .send(CoreEvent::JobProgress {
                                    job_id: job_id.clone(),
                                    bytes_downloaded: 0,
                                    total_bytes: Some(total_size),
                                })
                                .map_err(|_| "event channel closed".to_string())?;

                            // Write to disk
                            std::fs::create_dir_all(&dest)
                                .map_err(|e| format!("create job dir {dest:?}: {e}"))?;
                            let filename = self.filename_from_url(&url);
                            let file_path = dest.join(&filename);
                            self.write_data_to_disk(&file_path, &full_data, false)?;

                            // Update job state
                            let final_bytes = bytes_this_call.min(total_size);
                            if let Some(job) =
                                self.jobs.iter_mut().find(|j| j.id() == job_id)
                            {
                                job.add_bytes_received(final_bytes);
                            }

                            // Emit final progress
                            self.event_tx
                                .send(CoreEvent::JobProgress {
                                    job_id: job_id.clone(),
                                    bytes_downloaded: final_bytes,
                                    total_bytes: Some(total_size),
                                })
                                .map_err(|_| "event channel closed".to_string())?;

                            // Mark complete if we got all the bytes
                            if final_bytes >= total_size {
                                self.event_tx
                                    .send(CoreEvent::JobCompleted {
                                        job_id: job_id.clone(),
                                        bytes_written: final_bytes,
                                        path: file_path,
                                    })
                                    .map_err(|_| "event channel closed".to_string())?;

                                if let Some(job) =
                                    self.jobs.iter_mut().find(|j| j.id() == job_id)
                                {
                                    job.mark_completed();
                                }
                            }
                        }
                        None => {
                            // All mirrors exhausted — move to next job
                            continue;
                        }
                    }
                    continue; // handled, move to next job
                }
            }

            // --- Single-connection path (fallback + resumable) ---
            // Try mirrors in order until one succeeds or all exhausted
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

            // Fetch loop: retry with next mirror on failure
            let job_fetch = 'fetch_loop: loop {
                let current_url = {
                    let job = &self.jobs[i];
                    job.url().to_string()
                };

                match self.http.fetch(&current_url, range.clone()) {
                    Ok(result) => break 'fetch_loop Some(result),
                    Err(err) => {
                        // Try next mirror
                        if self.jobs[i].advance_mirror() {
                            let next_url = {
                                let job = &self.jobs[i];
                                job.url().to_string()
                            };
                            self.event_tx
                                .send(CoreEvent::JobMirrorFailover {
                                    job_id: job_id.clone(),
                                    previous_url: current_url,
                                    next_url,
                                    error: err,
                                })
                                .map_err(|_| "event channel closed".to_string())?;
                            continue 'fetch_loop;
                        }
                        // All mirrors exhausted
                        self.jobs[i].mark_failed();
                        self.event_tx
                            .send(CoreEvent::JobFailed {
                                job_id: job_id.clone(),
                                error: err,
                            })
                            .map_err(|_| "event channel closed".to_string())?;
                        break 'fetch_loop None;
                    }
                }
            };

            let (mut data, total) = match job_fetch {
                Some(r) => r,
                None => continue, // all mirrors exhausted, move to next job
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

            let filename = self.filename_from_url(&url);
            let file_path = dest.join(&filename);

            self.write_data_to_disk(&file_path, &data, bytes_received > 0)?;

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
                self.event_tx
                    .send(CoreEvent::JobCompleted {
                        job_id: job_id.clone(),
                        bytes_written: total,
                        path: file_path,
                    })
                    .map_err(|_| "event channel closed".to_string())?;

                self.jobs[i].mark_completed();
            }
        }

        Ok(())
    }

    /// Perform a multi-connection segmented fetch for the given URL.
    ///
    /// 1. Probes the file size via [`HttpBackend::file_size`].
    /// 2. Splits the file into [`DEFAULT_SEGMENT_COUNT`] byte ranges.
    /// 3. Fetches each range concurrently via scoped threads.
    /// 4. Assembles data in order.
    ///
    /// Returns the total file size and the assembled (possibly capped) bytes.
    /// This does NOT write to disk or update job state — the caller does that
    /// so that `&mut self` is available for job mutation.
    fn segmented_fetch(
        &self,
        url: &str,
        cap: Option<u64>,
    ) -> Result<(u64, Vec<u8>), String> {
        // Determine total file size (does not download the body)
        let total_size = self.http.file_size(url)?;

        if total_size == 0 {
            return Err("empty file, nothing to download".to_string());
        }

        // Calculate segment boundaries
        let seg_count = DEFAULT_SEGMENT_COUNT.min(total_size); // at most one per byte
        let seg_size = total_size / seg_count;
        let mut segments: Vec<Range<u64>> = Vec::with_capacity(seg_count as usize);
        for s in 0..seg_count {
            let start = s * seg_size;
            let end = if s == seg_count - 1 {
                total_size
            } else {
                (s + 1) * seg_size
            };
            if start < end {
                segments.push(start..end);
            }
        }

        // Fetch segments concurrently using scoped threads.
        // We clone the Arc<dyn HttpBackend> so each thread gets its own ref.
        let http = self.http.clone();
        let url_owned = url.to_string();

        let segment_results: Vec<Result<(u64, Vec<u8>), String>> =
            std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for range in &segments {
                    let start = range.start;
                    let end = range.end;
                    let http_ref = &http;
                    let url_ref = &url_owned;
                    handles.push(scope.spawn(move || {
                        let (data, _total) =
                            http_ref.fetch(url_ref, Some(start..end))?;
                        Ok((start, data))
                    }));
                }
                // Collect results, joining each thread
                handles
                    .into_iter()
                    .map(|h| h.join().unwrap())
                    .collect()
            });

        // Collect results and assemble in order
        let mut assembled: Vec<(u64, Vec<u8>)> =
            Vec::with_capacity(segment_results.len());
        for result in segment_results {
            let (range_start, data) = result?;
            assembled.push((range_start, data));
        }
        assembled.sort_by_key(|(start, _)| *start);

        // Concatenate all segment data in order
        let mut full_data: Vec<u8> = Vec::with_capacity(total_size as usize);
        for (_, data) in &assembled {
            full_data.extend_from_slice(data);
        }

        // Apply bandwidth cap
        if let Some(cap) = cap {
            let cap_usize = cap as usize;
            if full_data.len() > cap_usize {
                full_data.truncate(cap_usize);
            }
        }

        Ok((total_size, full_data))
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Derive a filename from a URL.
    fn filename_from_url(&self, url: &str) -> String {
        url::Url::parse(url)
            .ok()
            .and_then(|u| {
                u.path_segments()
                    .and_then(|mut s| s.next_back())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "download".to_string())
    }

    /// Write (or append) data to a file on disk.
    fn write_data_to_disk(
        &self,
        file_path: &std::path::Path,
        data: &[u8],
        append: bool,
    ) -> Result<(), String> {
        if append {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(file_path)
                .and_then(|mut f| f.write_all(data))
                .map_err(|e| format!("append {file_path:?}: {e}"))
        } else {
            std::fs::write(file_path, data)
                .map_err(|e| format!("write {file_path:?}: {e}"))
        }
    }
}
