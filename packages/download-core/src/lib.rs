//! Download Core — shared Rust library for download management.
//!
//! The Core owns URL classification, job lifecycle, and storage.
//! Platform shells consume this library via its public command/event API.

mod bandwidth;
mod cabi;
mod classification;
mod event;
mod extractor;
mod http;
mod job;
mod job_signals;
mod mirror_set;
mod transfer;
pub mod plugin_sidecar;
mod storage;
pub mod ytdlp_extractor;
pub use plugin_sidecar::{
    install_plugin_package, load_manifest, needs_update, plugin_dirs_path, write_manifest,
    PluginManifest, MANIFEST_FILENAME, PLUGIN_DIRS_REL, PLUGIN_PACKAGE_NAME,
};
pub use ytdlp_extractor::{FakeYtDlpExtractor, YtDlpConfig, YtDlpExtractorToolchain};

pub use bandwidth::{Clock, ManualClock, SystemClock, TransferProgress};
pub use event::CoreEvent;
pub use extractor::{
    ExtractorDownloadOutcome, ExtractorToolchain, FakeExtractorToolchain, ItemSelection,
    MediaItem, ProbeResult, ProbeSelection, Quality,
};
pub use http::{FetchResult, HttpBackend, ProdHttpBackend};
pub use job::{EnqueueOutcome, JobState};
pub use job_signals::JobSignals;
pub use mirror_set::MirrorSet;

use bandwidth::{shared_token_bucket, SharedTokenBucket};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use transfer::{
    execute_segment_fetch, execute_single_fetch, make_job_bucket, SegmentFetchParams,
    SingleFetchParams,
};

/// Serializable job row for platform shells.
#[derive(Debug, Clone, serde::Serialize)]
pub struct JobSnapshot {
    pub job_id: String,
    pub url: String,
    pub destination: String,
    pub status: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub title: String,
}

/// Resolved Job Destination preview for Probe-then-Confirm before jobs exist.
///
/// Unique id suffixes are not known until confirm; those slots use an ellipsis
/// placeholder (`…`, U+2026) so the shell can show where files will land.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProbeDestinationPreview {
    pub path_display: String,
}

/// Placeholder for the unique job-id suffix in destination previews.
const DEST_ID_PLACEHOLDER: &str = "\u{2026}";

/// Debounce interval for progress-only queue flushes.
const PROGRESS_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// Debounce byte threshold for progress-only queue flushes.
const PROGRESS_FLUSH_BYTES: u64 = 1024 * 1024;
/// How often Core samples Job Destination size while a silent Extractor runs.
const EXTRACTOR_DISK_POLL_INTERVAL: Duration = Duration::from_millis(50);

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
    /// Global bandwidth cap in bytes per second.
    /// None = globally uncapped.
    bandwidth_cap: Option<u64>,
    clock: Arc<dyn bandwidth::Clock>,
    /// Pending probe results, keyed by URL.
    /// Stored until confirm or replaced by a new probe for the same URL.
    pending_probes: HashMap<String, ProbeResult>,
    /// Last time a debounced progress flush was written to disk.
    last_progress_flush: Instant,
    /// Bytes written since the last debounced progress flush.
    bytes_since_progress_flush: u64,
    /// Whether a debounced progress flush is pending.
    progress_flush_pending: bool,
    job_signals: Arc<job_signals::JobSignals>,
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
        Self::new_with_clock(http, extractor, Arc::new(SystemClock::new()))
    }

    /// Create a Core with an injectable clock (used in acceptance tests).
    pub fn new_with_clock(
        http: Box<dyn HttpBackend>,
        extractor: Box<dyn ExtractorToolchain>,
        clock: Arc<dyn bandwidth::Clock>,
    ) -> (Self, crossbeam_channel::Receiver<CoreEvent>) {
        Self::new_with_clock_and_signals(
            http,
            extractor,
            clock,
            Arc::new(job_signals::JobSignals::default()),
        )
    }

    /// Create a Core with shared job signals (used by the C ABI).
    pub fn new_with_clock_and_signals(
        http: Box<dyn HttpBackend>,
        extractor: Box<dyn ExtractorToolchain>,
        clock: Arc<dyn bandwidth::Clock>,
        job_signals: Arc<job_signals::JobSignals>,
    ) -> (Self, crossbeam_channel::Receiver<CoreEvent>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let core = Self {
            http: Arc::from(http),
            extractor,
            library_root: None,
            jobs: Vec::new(),
            event_tx: tx,
            bandwidth_cap: None,
            clock,
            pending_probes: HashMap::new(),
            last_progress_flush: Instant::now(),
            bytes_since_progress_flush: 0,
            progress_flush_pending: false,
            job_signals,
        };
        (core, rx)
    }

    pub fn job_signals(&self) -> Arc<job_signals::JobSignals> {
        self.job_signals.clone()
    }

    fn register_job_signals(&self, job: &job::Job) {
        self.job_signals.register(
            job.id(),
            job.pause_requested().clone(),
            job.discard_requested().clone(),
            job.transfer_active().clone(),
        );
    }

    /// Finalize a pause when no transfer is active (C ABI / post-transfer).
    pub fn can_pause_job(&self, job_id: &str) -> Result<(), String> {
        let job = self
            .jobs
            .iter()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        if !job.is_pending() {
            return Err(format!(
                "cannot pause job {job_id}: current state is {:?}",
                job.state()
            ));
        }
        Ok(())
    }

    pub fn finish_pause_if_idle(&mut self, job_id: &str) -> Result<(), String> {
        let transfer_active = self
            .jobs
            .iter()
            .find(|j| j.id() == job_id)
            .map(|j| j.transfer_active().load(Ordering::Acquire))
            .unwrap_or(false);
        if transfer_active {
            return Ok(());
        }
        self.finalize_pause(job_id)
    }

    fn finalize_pause(&mut self, job_id: &str) -> Result<(), String> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        if !job.is_pending() {
            return Ok(());
        }
        job.pause()?;
        job.clear_pause_requested();
        self.event_tx
            .send(CoreEvent::JobPaused {
                job_id: job_id.to_string(),
            })
            .map_err(|_| "event channel closed".to_string())?;
        self.persist_queue_now()
    }

    /// Set the Library Root — the base directory for default Job Destinations
    /// and Core metadata (`.phlegethon/`).
    ///
    /// Creates the directory if it does not exist and restores any persisted
    /// queue state from `{library_root}/.phlegethon/jobs.json`.
    pub fn set_library_root(&mut self, root: PathBuf) -> Result<(), String> {
        std::fs::create_dir_all(&root).map_err(|e| format!("create library root: {e}"))?;
        self.jobs = storage::load_jobs(&root)?;
        for job in &self.jobs {
            self.register_job_signals(job);
        }
        if let Some(cap) = storage::load_global_bandwidth_cap(&root)? {
            self.bandwidth_cap = Some(cap);
        }
        self.library_root = Some(root);
        self.last_progress_flush = Instant::now();
        self.bytes_since_progress_flush = 0;
        self.progress_flush_pending = false;
        Ok(())
    }

    /// Current Library Root, if set.
    pub fn library_root(&self) -> Option<&Path> {
        self.library_root.as_deref()
    }

    fn require_absolute_destination(path: &Path) -> Result<(), String> {
        if path.is_absolute() {
            Ok(())
        } else {
            Err(format!(
                "Job Destination override must be an absolute path, got: {}",
                path.display()
            ))
        }
    }

    fn persist_queue_now(&mut self) -> Result<(), String> {
        if let Some(root) = &self.library_root {
            let queue = storage::build_queue(&self.jobs, self.bandwidth_cap);
            storage::save_queue(root, &queue)?;
        }
        self.last_progress_flush = Instant::now();
        self.bytes_since_progress_flush = 0;
        self.progress_flush_pending = false;
        Ok(())
    }

    fn note_progress_bytes(&mut self, bytes: u64) -> Result<(), String> {
        if bytes == 0 {
            return Ok(());
        }
        self.bytes_since_progress_flush += bytes;
        self.progress_flush_pending = true;
        let due = self.bytes_since_progress_flush >= PROGRESS_FLUSH_BYTES
            || self.last_progress_flush.elapsed() >= PROGRESS_FLUSH_INTERVAL;
        if due {
            self.persist_queue_now()?;
        }
        Ok(())
    }

    fn flush_pending_progress(&mut self) -> Result<(), String> {
        if self.progress_flush_pending {
            self.persist_queue_now()?;
        }
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
    ///
    /// `destination` — optional absolute parent for a job-owned Job
    /// Destination.  When `None`, the destination is derived under the
    /// Library Root.  When `Some`, Core creates a unique nested folder under
    /// that absolute parent (including outside the Library Root) — never the
    /// parent itself.  Relative paths are rejected.
    pub fn enqueue(
        &mut self,
        url_str: &str,
        destination: Option<PathBuf>,
    ) -> Result<EnqueueOutcome, String> {
        let parsed = url::Url::parse(url_str).map_err(|_| "invalid URL".to_string())?;
        let root = self
            .library_root
            .as_ref()
            .ok_or_else(|| "Library Root not set".to_string())?;

        if let Some(ref dest) = destination {
            Self::require_absolute_destination(dest)?;
        }

        match classification::classify(&parsed) {
            classification::Kind::EnqueueDirect => {
                let job = match destination {
                    Some(dest) => {
                        job::Job::new_with_source_at(url_str.to_string(), dest, None)
                    }
                    None => job::Job::new(url_str.to_string(), root.clone()),
                };
                let job_id = job.id().to_string();
                let dest = job.destination().to_path_buf();
                self.register_job_signals(&job);
                self.jobs.push(job);
                self.persist_queue_now()?;
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
    /// When a transfer is in flight, pause takes effect without waiting
    /// for the full body read to finish.
    pub fn pause_job(&mut self, job_id: &str) -> Result<(), String> {
        self.can_pause_job(job_id)?;

        if let Some(flag) = self.job_signals.request_pause(job_id) {
            flag.store(true, Ordering::Release);
        }

        if self.job_signals.is_transfer_active(job_id) {
            return Ok(());
        }

        self.finalize_pause(job_id)
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

        self.persist_queue_now()?;
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

        self.persist_queue_now()?;
        Ok(())
    }

    /// Discard a job from the queue.  Emits `CoreEvent::JobDiscarded`.
    ///
    /// Unfinished jobs (Pending / Paused / Failed, including in-flight) are
    /// stopped, removed from the queue, and have their Job Destination files
    /// deleted.  Completed jobs lose only the queue row — files stay on disk.
    ///
    /// Once discarded the job cannot be paused/resumed/retried.
    /// Discarding an already-discarded (or non-existent) job returns an error.
    pub fn discard_job(&mut self, job_id: &str) -> Result<(), String> {
        let pos = self
            .jobs
            .iter()
            .position(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;

        let completed = self.jobs[pos].is_completed();
        let destination = self.jobs[pos].destination().to_path_buf();

        if self.job_signals.signal_discard_stop(job_id) {
            // Transfer thread will finalize discard when the body read stops.
            return Ok(());
        }

        self.finalize_discard(job_id, completed, &destination)
    }

    fn finalize_discard(
        &mut self,
        job_id: &str,
        completed: bool,
        destination: &Path,
    ) -> Result<(), String> {
        let pos = self
            .jobs
            .iter()
            .position(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;

        self.jobs.remove(pos);
        self.job_signals.unregister(job_id);

        if !completed && destination.exists() {
            std::fs::remove_dir_all(destination).map_err(|e| {
                format!("delete Job Destination {destination:?}: {e}")
            })?;
        }

        self.event_tx
            .send(CoreEvent::JobDiscarded {
                job_id: job_id.to_string(),
            })
            .map_err(|_| "event channel closed".to_string())?;

        self.persist_queue_now()?;
        Ok(())
    }

    /// After an in-flight body read stops, either Discard or Pause.
    fn finalize_stop_after_transfer(&mut self, job_id: &str) -> Result<(), String> {
        let (discard, completed, destination) = {
            let job = self
                .jobs
                .iter()
                .find(|j| j.id() == job_id)
                .ok_or_else(|| format!("job {job_id} not found"))?;
            (
                job.is_discard_requested(),
                job.is_completed(),
                job.destination().to_path_buf(),
            )
        };
        if discard {
            self.finalize_discard(job_id, completed, &destination)
        } else {
            self.finalize_pause(job_id)
        }
    }

    /// List current jobs for shell queue UIs.
    pub fn list_job_snapshots(&self) -> Vec<JobSnapshot> {
        self.jobs
            .iter()
            .map(|job| {
                let title = job
                    .source()
                    .map(str::to_string)
                    .unwrap_or_else(|| job.filename().to_string());
                JobSnapshot {
                    job_id: job.id().to_string(),
                    url: job.url().to_string(),
                    destination: job.destination().display().to_string(),
                    status: match job.state() {
                        JobState::Pending => "pending",
                        JobState::Paused => "paused",
                        JobState::Failed => "failed",
                        JobState::Completed => "completed",
                    }
                    .to_string(),
                    bytes_downloaded: job.bytes_downloaded_snapshot(),
                    total_bytes: job.total_bytes(),
                    title,
                }
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Bandwidth cap
    // -----------------------------------------------------------------------

    /// Set the global bandwidth cap in bytes per second.
    ///
    /// When set, aggregate throughput across all jobs is limited to this rate.
    /// None/unset = globally uncapped.
    pub fn set_global_bandwidth_cap(&mut self, cap: u64) {
        self.bandwidth_cap = Some(cap);
        let _ = self.persist_queue_now();
    }

    /// Remove the global bandwidth cap (restore uncapped default).
    pub fn remove_global_bandwidth_cap(&mut self) {
        self.bandwidth_cap = None;
        let _ = self.persist_queue_now();
    }

    /// Return the current global bandwidth cap, if set.
    pub fn global_bandwidth_cap(&self) -> Option<u64> {
        self.bandwidth_cap
    }

    /// Set a per-job bandwidth cap in bytes per second.
    ///
    /// Effective rate is `min(global, per-job)` when both are set.
    pub fn set_job_bandwidth_cap(&mut self, job_id: &str, cap: u64) -> Result<(), String> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        job.set_bandwidth_cap(cap);
        self.persist_queue_now()?;
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
        self.persist_queue_now()?;
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
        self.persist_queue_now()?;
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
        self.persist_queue_now()?;
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

    /// Preview where [`Self::confirm_probe_selection`] will place Job
    /// Destinations, without creating jobs.
    ///
    /// Always a unique nested Job Destination: under Library Root `jobs/` by
    /// default, or under an absolute Save-to parent when overridden.  Preview
    /// includes a sanitized source/title subfolder when available, with an
    /// ellipsis standing in for the unique id suffix.
    pub fn preview_probe_destination(
        &self,
        selection: &ProbeSelection,
        destination: Option<PathBuf>,
    ) -> Result<ProbeDestinationPreview, String> {
        let root = self
            .library_root
            .as_ref()
            .ok_or_else(|| "Library Root not set".to_string())?;

        if let Some(ref dest) = destination {
            Self::require_absolute_destination(dest)?;
        }

        if selection.items.is_empty() {
            return Err("selection has no items".to_string());
        }

        let probe = self.pending_probes.get(&selection.url).ok_or_else(|| {
            format!(
                "no pending probe for {}. Call probe() first.",
                selection.url
            )
        })?;

        let items_map: HashMap<&str, &MediaItem> =
            probe.items.iter().map(|i| (i.id.as_str(), i)).collect();

        let first = &selection.items[0];
        let item = items_map.get(first.item_id.as_str()).ok_or_else(|| {
            format!(
                "item '{}' not found in probe result for {}",
                first.item_id, selection.url
            )
        })?;

        let job_source = Self::probe_job_source(probe, item);
        let path = Self::preview_destination_path(
            root,
            destination.as_deref(),
            job_source.as_deref(),
        );

        Ok(ProbeDestinationPreview {
            path_display: path.display().to_string(),
        })
    }

    fn probe_job_source(probe: &ProbeResult, item: &MediaItem) -> Option<String> {
        probe.source_name.clone().or_else(|| {
            if probe.items.len() == 1 {
                Some(item.title.clone())
            } else {
                None
            }
        })
    }

    fn preview_destination_path(
        library_root: &Path,
        destination: Option<&Path>,
        job_source: Option<&str>,
    ) -> PathBuf {
        let parent = match destination {
            Some(dest) => dest.to_path_buf(),
            None => library_root.join("jobs"),
        };

        match job_source {
            Some(name) => {
                let safe = job::sanitize_folder_name(name);
                parent.join(format!("{safe}-{DEST_ID_PLACEHOLDER}"))
            }
            None => parent.join(DEST_ID_PLACEHOLDER),
        }
    }

    /// Confirm a probe selection and create jobs for the selected items.
    ///
    /// Takes a [`ProbeSelection`] that references a previously probed URL
    /// and lists which items at which qualities to download.  Returns the
    /// job IDs of the created jobs.
    ///
    /// Each selected item creates one job with a unique job-owned Job
    /// Destination.  Without `destination`, destinations live under the
    /// Library Root `jobs/` segment (source/title naming when available).
    /// With an absolute `destination` override, each item nests under that
    /// parent (same naming rules, no extra `jobs/` segment) — never the
    /// shared parent itself.
    ///
    /// Errors if no pending probe exists for the URL, or if the selection
    /// references unknown items or qualities.
    pub fn confirm_probe_selection(
        &mut self,
        selection: &ProbeSelection,
        destination: Option<PathBuf>,
    ) -> Result<Vec<String>, String> {
        let root = self
            .library_root
            .as_ref()
            .ok_or_else(|| "Library Root not set".to_string())?;

        if let Some(ref dest) = destination {
            Self::require_absolute_destination(dest)?;
        }

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

        let mut job_ids = Vec::new();

        for sel in &selection.items {
            let item = items_map.get(sel.item_id.as_str()).ok_or_else(|| {
                format!(
                    "item '{}' not found in probe result for {}",
                    sel.item_id, selection.url
                )
            })?;

            // Validate quality exists; carry toolchain size onto the job when known.
            let quality = item
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

            let job_source = Self::probe_job_source(probe, item);

            let mut job = if let Some(ref dest_parent) = destination {
                job::Job::new_with_source_at(item.url.clone(), dest_parent.clone(), job_source)
            } else {
                job::Job::new_with_source(item.url.clone(), root.clone(), job_source)
            };
            // Launch Site media is fetched by yt-dlp, not plain HTTP of the page URL.
            job.set_extractor_format(&sel.quality_id);
            if let Some(size) = quality.size_bytes {
                job.set_total_bytes(Some(size));
            }

            job_ids.push(job.id().to_string());
            self.register_job_signals(&job);
            self.jobs.push(job);
        }

        self.persist_queue_now()?;
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
    /// a plain GET (or GET with `Range` for resume), rate-limited via token
    /// bucket and interruptible via pause.
    pub fn process_pending(&mut self) -> Result<(), String> {
        let indices: Vec<usize> = self
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| j.is_pending())
            .map(|(i, _)| i)
            .collect();

        let global_bucket = self
            .bandwidth_cap
            .map(|rate| shared_token_bucket(rate, self.clock.clone()));

        for i in indices {
            let (
                job_id,
                url,
                dest,
                filename,
                bytes_received,
                job_cap,
                extractor_format,
                pause_requested,
                transfer_active,
            ) = {
                let job = &self.jobs[i];
                (
                    job.id().to_string(),
                    job.url().to_string(),
                    job.destination().to_path_buf(),
                    job.filename().to_string(),
                    job.bytes_received(),
                    job.bandwidth_cap(),
                    job.extractor_format().map(|s| s.to_string()),
                    job.pause_requested().clone(),
                    job.transfer_active().clone(),
                )
            };

            if let Some(format) = extractor_format {
                let on_disk = storage::destination_byte_total(&dest);
                if on_disk > 0 && !self.extractor.supports_extractor_resume() {
                    self.jobs[i].mark_failed();
                    self.event_tx
                        .send(CoreEvent::JobFailed {
                            job_id: job_id.clone(),
                            error: "extractor cannot resume partial download; use Retry".into(),
                        })
                        .map_err(|_| "event channel closed".to_string())?;
                    self.persist_queue_now()?;
                    continue;
                }

                let live_bytes = self.jobs[i].live_downloaded();
                live_bytes.store(0, Ordering::Release);
                let total_for_progress = self.jobs[i].total_bytes();

                self.event_tx
                    .send(CoreEvent::JobProgress {
                        job_id: job_id.clone(),
                        bytes_downloaded: 0,
                        total_bytes: total_for_progress,
                    })
                    .map_err(|_| "event channel closed".to_string())?;

                // `pause_requested` is also set by Discard (`signal_discard_stop`);
                // both Pause and Discard use it as the extractor stop signal.
                let stop = pause_requested.as_ref();
                transfer_active.store(true, Ordering::Release);
                // Extractor Toolchain is often silent mid-transfer: run it on a
                // scoped thread and emit JobProgress from on-disk growth under
                // the Job Destination until it finishes or stops.
                let download_result = std::thread::scope(|s| {
                    let (result_tx, result_rx) = mpsc::channel();
                    let extractor = self.extractor.as_ref();
                    let dest_ref = dest.as_path();
                    let url_ref = url.as_str();
                    let format_ref = format.as_str();
                    s.spawn(move || {
                        let outcome =
                            extractor.download(url_ref, format_ref, dest_ref, stop);
                        let _ = result_tx.send(outcome);
                    });

                    let mut last_emitted: Option<u64> = None;
                    loop {
                        match result_rx.try_recv() {
                            Ok(outcome) => break outcome,
                            Err(mpsc::TryRecvError::Empty) => {
                                let on_disk = Self::destination_byte_total(&dest);
                                if last_emitted != Some(on_disk) {
                                    live_bytes.store(on_disk, Ordering::Release);
                                    let _ = self.event_tx.send(CoreEvent::JobProgress {
                                        job_id: job_id.clone(),
                                        bytes_downloaded: on_disk,
                                        total_bytes: total_for_progress,
                                    });
                                    last_emitted = Some(on_disk);
                                }
                                std::thread::sleep(EXTRACTOR_DISK_POLL_INTERVAL);
                            }
                            Err(mpsc::TryRecvError::Disconnected) => {
                                break Err(
                                    "extractor download thread disconnected".into(),
                                );
                            }
                        }
                    }
                });
                transfer_active.store(false, Ordering::Release);

                match download_result {
                    Ok(ExtractorDownloadOutcome::Completed(path)) => {
                        Self::cleanup_extractor_temp_artifacts(&dest, &path);
                        let file_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                        let bytes = Self::destination_byte_total(&dest).max(file_bytes);
                        live_bytes.store(bytes, Ordering::Release);
                        let _ = self.event_tx.send(CoreEvent::JobProgress {
                            job_id: job_id.clone(),
                            bytes_downloaded: bytes,
                            total_bytes: total_for_progress,
                        });
                        self.jobs[i].mark_completed();
                        self.event_tx
                            .send(CoreEvent::JobCompleted {
                                job_id: job_id.clone(),
                                bytes_written: bytes,
                                path,
                            })
                            .map_err(|_| "event channel closed".to_string())?;
                        self.persist_queue_now()?;
                    }
                    Ok(ExtractorDownloadOutcome::Stopped) => {
                        self.finalize_stop_after_transfer(&job_id)?;
                    }
                    Err(err) => {
                        // Kill/stop races can surface as toolchain errors; prefer
                        // Pause/Discard finalize when the user already requested stop.
                        if pause_requested.load(Ordering::Acquire) {
                            self.finalize_stop_after_transfer(&job_id)?;
                        } else {
                            self.jobs[i].mark_failed();
                            self.event_tx
                                .send(CoreEvent::JobFailed {
                                    job_id: job_id.clone(),
                                    error: err,
                                })
                                .map_err(|_| "event channel closed".to_string())?;
                            self.persist_queue_now()?;
                        }
                    }
                }
                continue;
            }

            let job_bucket = job_cap.map(|rate| make_job_bucket(rate, self.clock.clone()));

            if bytes_received == 0 {
                let supports_range = matches!(self.http.supports_range(&url), Ok(true));
                let rate_limited = global_bucket.is_some() || job_bucket.is_some();

                if supports_range && !rate_limited {
                    std::fs::create_dir_all(&dest)
                        .map_err(|e| format!("create job dir {dest:?}: {e}"))?;
                    let file_path = dest.join(&filename);
                    let part_path = Self::part_path_for(&file_path);

                    let seg_result = 'seg_loop: loop {
                        let current_url = self.jobs[i].url().to_string();
                        let known_total = self.http.file_size(&current_url).ok().filter(|&n| n > 0);
                        if let Some(total) = known_total {
                            self.jobs[i].set_total_bytes(Some(total));
                        }
                        let live_bytes = self.jobs[i].live_downloaded();
                        live_bytes.store(0, Ordering::Release);
                        let progress = TransferProgress {
                            job_id: job_id.clone(),
                            event_tx: self.event_tx.clone(),
                            total_bytes: self.jobs[i].total_bytes(),
                            live_bytes,
                        };
                        self.event_tx
                            .send(CoreEvent::JobProgress {
                                job_id: job_id.clone(),
                                bytes_downloaded: 0,
                                total_bytes: self.jobs[i].total_bytes(),
                            })
                            .map_err(|_| "event channel closed".to_string())?;
                        match self.segmented_fetch(
                            &current_url,
                            &part_path,
                            global_bucket.clone(),
                            job_bucket.clone(),
                            pause_requested.clone(),
                            transfer_active.clone(),
                            Some(progress),
                        ) {
                            Ok(result) => break 'seg_loop Some(result),
                            Err(err) => {
                                let _ = std::fs::remove_file(&part_path);
                                if self.jobs[i].advance_mirror() {
                                    let next_url = self.jobs[i].url().to_string();
                                    self.event_tx
                                        .send(CoreEvent::JobMirrorFailover {
                                            job_id: job_id.clone(),
                                            previous_url: current_url,
                                            next_url,
                                            error: err,
                                        })
                                        .map_err(|_| "event channel closed".to_string())?;
                                    self.persist_queue_now()?;
                                    continue 'seg_loop;
                                }
                                self.jobs[i].mark_failed();
                                self.event_tx
                                    .send(CoreEvent::JobFailed {
                                        job_id: job_id.clone(),
                                        error: err,
                                    })
                                    .map_err(|_| "event channel closed".to_string())?;
                                self.persist_queue_now()?;
                                break 'seg_loop None;
                            }
                        }
                    };

                    match seg_result {
                        Some((total_size, _bytes_written, paused)) => {
                            self.jobs[i].set_total_bytes(Some(total_size));
                            if paused {
                                self.sync_job_progress(&job_id, bytes_received)?;
                                self.finalize_stop_after_transfer(&job_id)?;
                                continue;
                            }

                            let on_disk = self.sync_job_progress(&job_id, bytes_received)?;
                            self.event_tx
                                .send(CoreEvent::JobProgress {
                                    job_id: job_id.clone(),
                                    bytes_downloaded: on_disk,
                                    total_bytes: Some(total_size),
                                })
                                .map_err(|_| "event channel closed".to_string())?;

                            if on_disk >= total_size {
                                self.complete_job_file(
                                    &job_id, i, &part_path, &file_path, total_size,
                                )?;
                            } else {
                                self.flush_pending_progress()?;
                            }
                        }
                        None => continue,
                    }
                    continue;
                }
            }

            let range = if bytes_received > 0 {
                Some(bytes_received..u64::MAX)
            } else {
                None
            };

            let known_total = self.http.file_size(&url).ok().filter(|&n| n > 0);
            if let Some(total) = known_total {
                self.jobs[i].set_total_bytes(Some(total));
            }
            let total_for_progress = self.jobs[i].total_bytes();

            self.event_tx
                .send(CoreEvent::JobProgress {
                    job_id: job_id.clone(),
                    bytes_downloaded: bytes_received,
                    total_bytes: total_for_progress,
                })
                .map_err(|_| "event channel closed".to_string())?;

            std::fs::create_dir_all(&dest)
                .map_err(|e| format!("create job dir {dest:?}: {e}"))?;

            let file_path = dest.join(&filename);
            let part_path = Self::part_path_for(&file_path);

            if bytes_received > 0 && !part_path.exists() && file_path.exists() {
                std::fs::rename(&file_path, &part_path).map_err(|e| {
                    format!("migrate partial {file_path:?} -> {part_path:?}: {e}")
                })?;
            }

            let live_bytes = self.jobs[i].live_downloaded();
            live_bytes.store(bytes_received, Ordering::Release);
            let progress = TransferProgress {
                job_id: job_id.clone(),
                event_tx: self.event_tx.clone(),
                total_bytes: total_for_progress,
                live_bytes,
            };

            let job_fetch = 'fetch_loop: loop {
                let current_url = self.jobs[i].url().to_string();
                let outcome = execute_single_fetch(SingleFetchParams {
                    http: self.http.clone(),
                    url: current_url.clone(),
                    part_path: part_path.clone(),
                    range: range.clone(),
                    append: bytes_received > 0,
                    truncate: bytes_received == 0,
                    global_bucket: global_bucket.clone(),
                    job_bucket: job_bucket.clone(),
                    pause_requested: pause_requested.clone(),
                    transfer_active: transfer_active.clone(),
                    progress: Some(progress.clone()),
                });

                if outcome.paused {
                    break 'fetch_loop Some(outcome);
                }

                match outcome.fetch {
                    Ok(_result) => break 'fetch_loop Some(outcome),
                    Err(err) => {
                        if self.jobs[i].advance_mirror() {
                            let next_url = self.jobs[i].url().to_string();
                            self.event_tx
                                .send(CoreEvent::JobMirrorFailover {
                                    job_id: job_id.clone(),
                                    previous_url: current_url,
                                    next_url,
                                    error: err,
                                })
                                .map_err(|_| "event channel closed".to_string())?;
                            self.persist_queue_now()?;
                            continue 'fetch_loop;
                        }
                        self.jobs[i].mark_failed();
                        self.event_tx
                            .send(CoreEvent::JobFailed {
                                job_id: job_id.clone(),
                                error: err,
                            })
                            .map_err(|_| "event channel closed".to_string())?;
                        self.persist_queue_now()?;
                        break 'fetch_loop None;
                    }
                }
            };

            let outcome = match job_fetch {
                Some(outcome) => outcome,
                None => continue,
            };

            if outcome.paused {
                self.sync_job_progress(&job_id, bytes_received)?;
                self.finalize_stop_after_transfer(&job_id)?;
                continue;
            }

            let reported = outcome.fetch?.total_length;
            let on_disk = self.sync_job_progress(&job_id, bytes_received)?;
            let total_opt = self.jobs[i].total_bytes().or_else(|| {
                if reported > 0 {
                    Some(reported)
                } else {
                    None
                }
            });
            if self.jobs[i].total_bytes().is_none() {
                self.jobs[i].set_total_bytes(total_opt);
            }

            self.event_tx
                .send(CoreEvent::JobProgress {
                    job_id: job_id.clone(),
                    bytes_downloaded: on_disk,
                    total_bytes: total_opt,
                })
                .map_err(|_| "event channel closed".to_string())?;

            match total_opt {
                Some(total) if on_disk >= total => {
                    self.complete_job_file(&job_id, i, &part_path, &file_path, total)?;
                }
                Some(_) => {
                    self.flush_pending_progress()?;
                }
                None => {
                    // Unknown size: successful fetch to EOF is complete.
                    self.complete_job_file(&job_id, i, &part_path, &file_path, on_disk)?;
                }
            }
        }

        self.flush_pending_progress()?;
        Ok(())
    }

    fn sync_job_progress(&mut self, job_id: &str, bytes_before: u64) -> Result<u64, String> {
        let job = self
            .jobs
            .iter()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        let part_path = Self::part_path_for(&job.final_path());
        let on_disk = std::fs::metadata(&part_path)
            .map(|m| m.len())
            .map_err(|e| format!("stat partial {part_path:?}: {e}"))?;

        if on_disk > bytes_before {
            if let Some(job) = self.jobs.iter_mut().find(|j| j.id() == job_id) {
                let delta = on_disk - bytes_before;
                job.add_bytes_received(delta);
                self.note_progress_bytes(delta)?;
            }
        }
        Ok(on_disk)
    }

    /// Sum sizes of regular files under a Job Destination (recursive).
    ///
    /// Used when the Extractor Toolchain is silent mid-transfer so Core can
    /// still emit [`CoreEvent::JobProgress`] from on-disk growth.
    fn destination_byte_total(dest: &Path) -> u64 {
        storage::destination_byte_total(dest)
    }

    /// Remove Extractor Toolchain temp/partial siblings under a Job Destination.
    ///
    /// Keeps the finalized media path reported by the toolchain. Targets known
    /// yt-dlp leftover suffixes (`.part`, `.ytdl`) so Completed jobs do not
    /// leave orphan partials beside the durable output (issue #42).
    fn cleanup_extractor_temp_artifacts(dest: &Path, keep: &Path) {
        let Ok(entries) = std::fs::read_dir(dest) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path == keep {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let is_temp = name.ends_with(".part")
                || name.ends_with(".ytdl")
                || name.ends_with(".temp")
                || name.ends_with(".tmp");
            if is_temp {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    fn complete_job_file(
        &mut self,
        job_id: &str,
        job_index: usize,
        part_path: &Path,
        file_path: &Path,
        total: u64,
    ) -> Result<(), String> {
        std::fs::rename(part_path, file_path)
            .map_err(|e| format!("rename {part_path:?} -> {file_path:?}: {e}"))?;

        self.event_tx
            .send(CoreEvent::JobCompleted {
                job_id: job_id.to_string(),
                bytes_written: total,
                path: file_path.to_path_buf(),
            })
            .map_err(|_| "event channel closed".to_string())?;

        self.jobs[job_index].mark_completed();
        self.persist_queue_now()
    }

    fn segmented_fetch(
        &self,
        url: &str,
        part_path: &Path,
        global_bucket: Option<SharedTokenBucket>,
        job_bucket: Option<SharedTokenBucket>,
        pause_requested: Arc<std::sync::atomic::AtomicBool>,
        transfer_active: Arc<std::sync::atomic::AtomicBool>,
        progress: Option<TransferProgress>,
    ) -> Result<(u64, u64, bool), String> {
        transfer_active.store(true, Ordering::Release);
        let result = self.run_segmented_fetch(
            url,
            part_path,
            global_bucket,
            job_bucket,
            pause_requested,
            progress,
        );
        transfer_active.store(false, Ordering::Release);
        result
    }

    fn run_segmented_fetch(
        &self,
        url: &str,
        part_path: &Path,
        global_bucket: Option<SharedTokenBucket>,
        job_bucket: Option<SharedTokenBucket>,
        pause_requested: Arc<std::sync::atomic::AtomicBool>,
        progress: Option<TransferProgress>,
    ) -> Result<(u64, u64, bool), String> {
        let total_size = self.http.file_size(url)?;
        if total_size == 0 {
            return Err("empty file, nothing to download".to_string());
        }

        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(part_path)
            .map_err(|e| format!("open partial {part_path:?}: {e}"))?;

        let seg_count = DEFAULT_SEGMENT_COUNT.min(total_size);
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

        let http = self.http.clone();
        let url_owned = url.to_string();
        let part_path_owned = part_path.to_path_buf();

        let segment_results: Vec<Result<u64, String>> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for range in segments {
                let http_ref = http.clone();
                let url_ref = url_owned.clone();
                let part_ref = part_path_owned.clone();
                let global = global_bucket.clone();
                let job = job_bucket.clone();
                let pause = pause_requested.clone();
                let prog = progress.clone();
                handles.push(scope.spawn(move || {
                    execute_segment_fetch(SegmentFetchParams {
                        http: http_ref,
                        url: url_ref,
                        part_path: part_ref,
                        range,
                        global_bucket: global,
                        job_bucket: job,
                        pause_requested: pause,
                        progress: prog,
                    })
                }));
            }
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        let mut bytes_written = 0u64;
        for result in segment_results {
            bytes_written = bytes_written.saturating_add(result?);
        }

        let paused = pause_requested.load(Ordering::Acquire);
        Ok((total_size, bytes_written, paused))
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Path for an in-progress download (`{final}.part`).
    fn part_path_for(final_path: &Path) -> PathBuf {
        storage::part_path_for(final_path)
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.flush_pending_progress();
    }
}
