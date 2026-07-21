//! Download Core — shared Rust library for download management.
//!
//! The Core owns URL classification, job lifecycle, and storage.
//! Platform shells consume this library via its public command/event API.

mod bandwidth;
mod cabi;
mod classification;
mod cookie_channel;
mod disk_space;
mod download_archive;
mod media_extras;
mod quality_policy;
mod event;
mod extractor;
mod filename_template;
mod http;
mod integrity;
mod job;
mod job_signals;
mod mirror_set;
mod transfer;
mod url_input;
mod proxy_settings;
pub mod plugin_sidecar;
mod storage;
pub mod ytdlp_extractor;
pub use plugin_sidecar::{
    install_plugin_package, load_manifest, needs_update, plugin_dirs_path, write_manifest,
    PluginManifest, MANIFEST_FILENAME, PLUGIN_DIRS_REL, PLUGIN_PACKAGE_NAME,
};
pub use ytdlp_extractor::{FakeYtDlpExtractor, YtDlpConfig, YtDlpExtractorToolchain};

pub use cookie_channel::{append_ytdlp_cookie_args, ytdlp_cookie_args, CookieChannel};
pub use proxy_settings::{append_ytdlp_proxy_args, ytdlp_proxy_args, ProxySettings};
pub use bandwidth::{Clock, ManualClock, SystemClock, TransferProgress};
pub use event::CoreEvent;
pub use extractor::{
    ExtractorDownloadOutcome, ExtractorToolchain, FakeExtractorToolchain, ItemSelection,
    MediaItem, ProbeResult, ProbeSelection, Quality,
};
pub use filename_template::{
    media_item_raw_id, prepare_extractor_output, render_template, resolve_collision,
    sanitize_filename, TemplateVars, DEFAULT_FILENAME_TEMPLATE,
};
pub use http::{FetchResult, HttpBackend, ProdHttpBackend};
pub use disk_space::{
    disk_space_margin_bytes, required_free_bytes, FreeSpaceLookup, SystemFreeSpace,
};
pub use download_archive::{ArchiveEntry, ArchiveOutcome, DownloadArchive};
pub use integrity::{hash_from_metalink, ExpectedHash, HashAlgorithm};
pub use job::{EnqueueOutcome, JobState};
pub use job_signals::JobSignals;
pub use mirror_set::MirrorSet;
pub use media_extras::{
    apply_remux_postprocess, effective_options, ffmpeg_available, ytdlp_media_extras_args,
    MediaExtrasCapabilities, MediaExtrasOptions, PostprocessOutcome,
};
pub use quality_policy::{select_default_quality, DefaultQualityPolicy, DEFAULT_MAX_HEIGHT};
pub use url_input::{normalize_url, split_urls};

use bandwidth::{shared_token_bucket, SharedTokenBucket};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
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

/// Match for a URL already active or queued in Core.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ActiveJobMatch {
    pub job_id: String,
    pub url: String,
    pub status: String,
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

/// Default maximum number of jobs allowed to transfer at once.
pub use storage::DEFAULT_ACTIVE_JOB_LIMIT;

pub struct Core {
    http: Arc<dyn HttpBackend>,
    extractor: Box<dyn ExtractorToolchain>,
    library_root: Option<PathBuf>,
    jobs: Vec<job::Job>,
    event_tx: crossbeam_channel::Sender<CoreEvent>,
    /// Global bandwidth cap in bytes per second.
    /// None = globally uncapped.
    bandwidth_cap: Option<u64>,
    /// Maximum concurrent transferring jobs (default [`DEFAULT_ACTIVE_JOB_LIMIT`]).
    active_job_limit: u32,
    default_quality_policy: DefaultQualityPolicy,
    audio_only_mode: bool,
    default_media_extras: MediaExtrasOptions,
    cookie_channel: CookieChannel,
    proxy_settings: ProxySettings,
    filename_template: String,
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
    free_space: Arc<dyn disk_space::FreeSpaceLookup>,
    download_archive: DownloadArchive,
}

enum UnlockedTransferPlan {
    UnderLock {
        job_index: usize,
    },
    SingleFetch {
        job_index: usize,
        bytes_received: u64,
        url: String,
        part_path: PathBuf,
        file_path: PathBuf,
        range: Option<Range<u64>>,
        append: bool,
        truncate: bool,
        http: Arc<dyn HttpBackend>,
        global_bucket: Option<SharedTokenBucket>,
        job_bucket: Option<SharedTokenBucket>,
        pause_requested: Arc<std::sync::atomic::AtomicBool>,
        transfer_active: Arc<std::sync::atomic::AtomicBool>,
        progress: TransferProgress,
    },
}

enum UnlockedTransferOutcome {
    UnderLock {
        job_index: usize,
    },
    Fetched {
        job_index: usize,
        bytes_received: u64,
        part_path: PathBuf,
        file_path: PathBuf,
        outcome: transfer::SingleFetchOutcome,
        failed_url: String,
    },
}

impl UnlockedTransferPlan {
    fn run(self) -> UnlockedTransferOutcome {
        match self {
            Self::UnderLock { job_index } => UnlockedTransferOutcome::UnderLock { job_index },
            Self::SingleFetch {
                job_index,
                bytes_received,
                url,
                part_path,
                file_path,
                range,
                append,
                truncate,
                http,
                global_bucket,
                job_bucket,
                pause_requested,
                transfer_active,
                progress,
                ..
            } => {
                let failed_url = url.clone();
                let outcome = execute_single_fetch(SingleFetchParams {
                    http,
                    url,
                    part_path: part_path.clone(),
                    range,
                    append,
                    truncate,
                    global_bucket,
                    job_bucket,
                    pause_requested,
                    transfer_active,
                    progress: Some(progress),
                });
                UnlockedTransferOutcome::Fetched {
                    job_index,
                    bytes_received,
                    part_path,
                    file_path,
                    outcome,
                    failed_url,
                }
            }
        }
    }
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
        Self::new_with_clock_signals_and_free_space(
            http,
            extractor,
            clock,
            job_signals,
            Arc::new(disk_space::SystemFreeSpace),
        )
    }

    /// Create a Core with shared job signals and injectable free-space lookup (tests).
    pub fn new_with_clock_signals_and_free_space(
        http: Box<dyn HttpBackend>,
        extractor: Box<dyn ExtractorToolchain>,
        clock: Arc<dyn bandwidth::Clock>,
        job_signals: Arc<job_signals::JobSignals>,
        free_space: Arc<dyn disk_space::FreeSpaceLookup>,
    ) -> (Self, crossbeam_channel::Receiver<CoreEvent>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let core = Self {
            http: Arc::from(http),
            extractor,
            library_root: None,
            jobs: Vec::new(),
            event_tx: tx,
            bandwidth_cap: None,
            active_job_limit: DEFAULT_ACTIVE_JOB_LIMIT,
            default_quality_policy: DefaultQualityPolicy::default(),
            audio_only_mode: false,
            default_media_extras: MediaExtrasOptions::default(),
            cookie_channel: CookieChannel::default(),
            proxy_settings: ProxySettings::default(),
            filename_template: DEFAULT_FILENAME_TEMPLATE.to_string(),
            clock,
            pending_probes: HashMap::new(),
            last_progress_flush: Instant::now(),
            bytes_since_progress_flush: 0,
            progress_flush_pending: false,
            job_signals,
            free_space,
            download_archive: DownloadArchive::default(),
        };
        (core, rx)
    }

    /// Create a Core with injectable free-space lookup (acceptance tests).
    pub fn new_with_free_space(
        http: Box<dyn HttpBackend>,
        extractor: Box<dyn ExtractorToolchain>,
        free_space: Arc<dyn disk_space::FreeSpaceLookup>,
    ) -> (Self, crossbeam_channel::Receiver<CoreEvent>) {
        Self::new_with_clock_signals_and_free_space(
            http,
            extractor,
            Arc::new(SystemClock::new()),
            Arc::new(job_signals::JobSignals::default()),
            free_space,
        )
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
        self.active_job_limit = storage::load_active_job_limit(&root)?;
        let quality_settings = storage::load_quality_settings(&root)?;
        self.default_quality_policy = quality_settings.policy;
        self.audio_only_mode = quality_settings.audio_only_mode;
        let media_settings = storage::load_media_extras_settings(&root)?;
        self.default_media_extras = media_settings.defaults;
        self.cookie_channel = storage::load_cookie_channel(&root)?;
        self.proxy_settings = storage::load_proxy_settings(&root)?;
        self.http.apply_proxy_settings(&self.proxy_settings);
        self.filename_template = storage::load_filename_template(&root)?;
        self.download_archive = download_archive::DownloadArchive::load(&root)?;
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
            let queue = storage::build_queue(
                &self.jobs,
                self.bandwidth_cap,
                self.active_job_limit,
                &self.default_quality_policy,
                self.audio_only_mode,
                &self.default_media_extras,
                &self.cookie_channel,
                &self.proxy_settings,
                &self.filename_template,
            );
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

    fn resolve_known_total(&self, job_index: usize, url: &str) -> Option<u64> {
        self.jobs[job_index]
            .total_bytes()
            .or_else(|| self.http.file_size(url).ok().filter(|&n| n > 0))
    }

    fn check_disk_space_before_start(
        &self,
        destination: &Path,
        total_bytes: u64,
        bytes_received: u64,
    ) -> Result<(), String> {
        let remaining = total_bytes.saturating_sub(bytes_received);
        if remaining == 0 {
            return Ok(());
        }
        let free = self.free_space.free_bytes(destination)?;
        if disk_space::has_sufficient_free_space(free, remaining) {
            Ok(())
        } else {
            Err(disk_space::insufficient_disk_space_error(
                destination,
                free,
                remaining,
            ))
        }
    }

    fn fail_job_for_disk_space(
        &mut self,
        job_index: usize,
        job_id: &str,
        error: String,
    ) -> Result<(), String> {
        self.finalize_failed_job(job_id, job_index, error)
    }

    /// Mark a job Failed, emit `JobFailed`, and record media identity in the archive.
    fn finalize_failed_job(
        &mut self,
        job_id: &str,
        job_index: usize,
        error: String,
    ) -> Result<(), String> {
        self.jobs[job_index].mark_failed();
        self.event_tx
            .send(CoreEvent::JobFailed {
                job_id: job_id.to_string(),
                error,
            })
            .map_err(|_| "event channel closed".to_string())?;
        self.record_media_identity(job_id, job_index)?;
        self.persist_queue_now()
    }

    fn guard_disk_space_for_transfer(
        &mut self,
        job_index: usize,
        job_id: &str,
        destination: &Path,
        url: &str,
        bytes_received: u64,
    ) -> Result<bool, String> {
        let Some(total) = self.resolve_known_total(job_index, url) else {
            return Ok(true);
        };
        if let Err(err) = self.check_disk_space_before_start(destination, total, bytes_received) {
            self.fail_job_for_disk_space(job_index, job_id, err)?;
            return Ok(false);
        }
        self.jobs[job_index].set_total_bytes(Some(total));
        Ok(true)
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
        let url_str = normalize_url(url_str).ok_or_else(|| "invalid URL".to_string())?;
        let parsed = url::Url::parse(&url_str).map_err(|_| "invalid URL".to_string())?;
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
                        job::Job::new_with_source_at(url_str.clone(), dest, None)
                    }
                    None => job::Job::new(url_str.clone(), root.clone()),
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
                Ok(EnqueueOutcome::ProbeThenConfirm { url: url_str })
            }
        }
    }

    /// Find an active or queued job matching a normalized URL.
    ///
    /// Completed jobs are ignored. Pending probes for the same URL also match.
    pub fn find_active_job_for_url(&self, url_str: &str) -> Option<ActiveJobMatch> {
        let normalized = normalize_url(url_str)?;

        for job in &self.jobs {
            if job.state() == JobState::Completed {
                continue;
            }
            if normalize_url(job.url()).as_deref() == Some(normalized.as_str()) {
                let title = job
                    .source()
                    .map(str::to_string)
                    .unwrap_or_else(|| job.filename().to_string());
                return Some(ActiveJobMatch {
                    job_id: job.id().to_string(),
                    url: job.url().to_string(),
                    status: match job.state() {
                        JobState::Pending => "pending",
                        JobState::Paused => "paused",
                        JobState::Failed => "failed",
                        JobState::Completed => "completed",
                    }
                    .to_string(),
                    title,
                });
            }
        }

        if self.pending_probes.contains_key(&normalized) {
            return Some(ActiveJobMatch {
                job_id: String::new(),
                url: normalized.clone(),
                status: "probing".to_string(),
                title: normalized,
            });
        }

        None
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
    // Active Job Limit + queue reorder
    // -----------------------------------------------------------------------

    /// Return the Active Job Limit (maximum concurrent transferring jobs).
    pub fn active_job_limit(&self) -> u32 {
        self.active_job_limit
    }

    /// Set the Active Job Limit.  Must be at least 1.
    ///
    /// Lowering the limit does not preempt in-flight transfers; it only
    /// affects how many waiting jobs may start on the next dispatch.
    pub fn set_active_job_limit(&mut self, limit: u32) -> Result<(), String> {
        if limit == 0 {
            return Err("Active Job Limit must be at least 1".to_string());
        }
        self.active_job_limit = limit;
        self.persist_queue_now()?;
        Ok(())
    }

    /// Reorder the queue by moving the job at `from_index` to `to_index`.
    ///
    /// Queue order controls which waiting job starts next when a slot frees.
    /// Already-active transfers are not preempted.
    pub fn reorder_job(&mut self, from_index: usize, to_index: usize) -> Result<(), String> {
        if from_index >= self.jobs.len() || to_index >= self.jobs.len() {
            return Err(format!(
                "reorder index out of bounds (len {}, from {}, to {})",
                self.jobs.len(),
                from_index,
                to_index
            ));
        }
        if from_index == to_index {
            return Ok(());
        }
        let job = self.jobs.remove(from_index);
        self.jobs.insert(to_index, job);
        self.persist_queue_now()?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Default Quality Policy + audio-only mode
    // -----------------------------------------------------------------------

    /// Return the Default Quality Policy (max height, prefer mp4).
    pub fn default_quality_policy(&self) -> DefaultQualityPolicy {
        self.default_quality_policy
    }

    /// Update the Default Quality Policy and persist it.
    pub fn set_default_quality_policy(
        &mut self,
        policy: DefaultQualityPolicy,
    ) -> Result<(), String> {
        if policy.max_height == 0 {
            return Err("Default Quality Policy max height must be at least 1".to_string());
        }
        self.default_quality_policy = policy;
        self.persist_queue_now()?;
        Ok(())
    }

    /// Whether audio-only mode overrides the video Default Quality Policy.
    pub fn audio_only_mode(&self) -> bool {
        self.audio_only_mode
    }

    /// Enable or disable audio-only mode and persist the setting.
    pub fn set_audio_only_mode(&mut self, enabled: bool) -> Result<(), String> {
        self.audio_only_mode = enabled;
        self.persist_queue_now()?;
        Ok(())
    }

    /// Apply the active Default Quality Policy to a quality list.
    pub fn select_default_quality(&self, qualities: &[Quality]) -> Option<String> {
        select_default_quality(
            qualities,
            &self.default_quality_policy,
            self.audio_only_mode,
        )
    }

    // -----------------------------------------------------------------------
    // Media extras defaults + capabilities
    // -----------------------------------------------------------------------

    /// Settings-backed defaults for write/embed media options.
    pub fn default_media_extras(&self) -> MediaExtrasOptions {
        self.default_media_extras
    }

    /// Update media extras defaults and persist them.
    pub fn set_default_media_extras(
        &mut self,
        options: MediaExtrasOptions,
    ) -> Result<(), String> {
        self.default_media_extras = options;
        self.persist_queue_now()?;
        Ok(())
    }

    /// Capabilities for shells to disable embed toggles when ffmpeg is missing.
    pub fn media_extras_capabilities(&self) -> MediaExtrasCapabilities {
        let ffmpeg_dir = self.extractor.ffmpeg_dir();
        if ffmpeg_available(ffmpeg_dir) {
            MediaExtrasCapabilities {
                ffmpeg_available: true,
                embed_disabled_reason: None,
            }
        } else {
            MediaExtrasCapabilities::unavailable()
        }
    }

    // -----------------------------------------------------------------------
    // Cookie auth channel
    // -----------------------------------------------------------------------

    /// Current cookie auth channel (settings + future Companion hand-off).
    pub fn cookie_channel(&self) -> CookieChannel {
        self.cookie_channel.clone()
    }

    /// Update the cookie auth channel and persist it.
    ///
    /// Companion and desktop settings both use this single channel.
    pub fn set_cookie_channel(&mut self, channel: CookieChannel) -> Result<(), String> {
        let channel = channel.normalized();
        channel.validate()?;
        self.cookie_channel = channel;
        self.persist_queue_now()
    }

    /// Current global proxy settings.
    pub fn proxy_settings(&self) -> ProxySettings {
        self.proxy_settings.clone()
    }

    /// Update global proxy settings and persist them.
    ///
    /// HTTP transfers and future Extractor Toolchain invocations honor the proxy.
    /// Clearing the URL restores direct connectivity.
    pub fn set_proxy_settings(&mut self, settings: ProxySettings) -> Result<(), String> {
        let settings = settings.normalized();
        settings.validate()?;
        self.proxy_settings = settings.clone();
        self.http.apply_proxy_settings(&settings);
        self.persist_queue_now()
    }

    /// Current Filename Template for extractor job outputs.
    pub fn filename_template(&self) -> &str {
        &self.filename_template
    }

    /// Update the Filename Template and persist it.
    pub fn set_filename_template(&mut self, template: String) -> Result<(), String> {
        let trimmed = template.trim();
        if trimmed.is_empty() {
            return Err("Filename Template cannot be empty".to_string());
        }
        self.filename_template = trimmed.to_string();
        self.persist_queue_now()
    }

    fn apply_extractor_filename(
        &self,
        job: &mut job::Job,
        item: &MediaItem,
        quality: &Quality,
    ) {
        let ext = quality
            .container
            .as_deref()
            .unwrap_or("mp4");
        let raw_id = media_item_raw_id(&item.id);
        let vars = TemplateVars {
            title: &item.title,
            id: raw_id,
            uploader: item.uploader.as_deref(),
            upload_date: item.upload_date.as_deref(),
            ext,
        };
        let (ytdlp_spec, basename) =
            prepare_extractor_output(job.destination(), &self.filename_template, &vars);
        job.set_ytdlp_output_spec(ytdlp_spec);
        job.set_filename(basename);
        job.set_media_probe_metadata(item.uploader.clone(), item.upload_date.clone());
    }

    fn extractor_output_for_job(&self, job: &job::Job) -> (String, String) {
        if let (Some(media_id), Some(title)) = (job.media_id(), job.media_title()) {
            let ext = job
                .filename()
                .rsplit_once('.')
                .map(|(_, e)| e)
                .unwrap_or("mp4");
            let vars = TemplateVars {
                title,
                id: media_item_raw_id(media_id),
                uploader: job.media_uploader(),
                upload_date: job.media_upload_date(),
                ext,
            };
            return prepare_extractor_output(
                job.destination(),
                &self.filename_template,
                &vars,
            );
        }
        (
            self.filename_template.clone(),
            render_template(
                &self.filename_template,
                &TemplateVars {
                    title: "download",
                    id: "id",
                    uploader: None,
                    upload_date: None,
                    ext: "mp4",
                },
            ),
        )
    }

    /// Effective media extras for a probe confirm (defaults + optional override).
    pub fn resolve_media_extras(
        &self,
        override_options: Option<MediaExtrasOptions>,
    ) -> MediaExtrasOptions {
        let base = override_options.unwrap_or(self.default_media_extras);
        effective_options(&base, &self.media_extras_capabilities())
    }

    /// Media extras stored on a job (after confirm).
    pub fn job_media_extras(&self, job_id: &str) -> Result<MediaExtrasOptions, String> {
        self.jobs
            .iter()
            .find(|j| j.id() == job_id)
            .map(|j| j.media_extras())
            .ok_or_else(|| format!("job {job_id} not found"))
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
        let expected_hash = hash_from_metalink(xml);
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        let mut changed = false;
        if let Some(hash) = expected_hash {
            job.set_expected_hash(hash);
            changed = true;
        }
        for url in urls {
            job.add_mirror(url);
            changed = true;
        }
        if changed {
            self.persist_queue_now()?;
        }
        Ok(count)
    }

    /// Set a user-supplied expected hash for post-completion Integrity Check.
    pub fn set_job_expected_hash(
        &mut self,
        job_id: &str,
        algorithm: &str,
        digest: &str,
    ) -> Result<(), String> {
        let hash = ExpectedHash::parse(algorithm, digest)?;
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        job.set_expected_hash(hash);
        self.persist_queue_now()?;
        Ok(())
    }

    /// Load an expected hash from a GNU checksum sidecar file on disk.
    pub fn set_job_expected_hash_from_sidecar(
        &mut self,
        job_id: &str,
        sidecar_path: &Path,
    ) -> Result<(), String> {
        let text = std::fs::read_to_string(sidecar_path)
            .map_err(|e| format!("read sidecar {}: {e}", sidecar_path.display()))?;
        let hash = ExpectedHash::from_sidecar_contents(&text)?;
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        job.set_expected_hash(hash);
        self.persist_queue_now()?;
        Ok(())
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
        let url = normalize_url(url).ok_or_else(|| "invalid URL".to_string())?;
        match self.extractor.probe(&url, &self.cookie_channel, &self.proxy_settings) {
            Ok(mut result) => {
                self.mark_archived_probe_items(&mut result.items);

                // Store for subsequent confirm
                self.pending_probes
                    .insert(url.clone(), result.clone());

                // Emit event
                self.event_tx
                    .send(CoreEvent::ProbeResult {
                        url: url.clone(),
                        items: result.items.clone(),
                        source_name: result.source_name.clone(),
                    })
                    .map_err(|_| "event channel closed".to_string())?;

                Ok(result)
            }
            Err(err) => {
                self.event_tx
                    .send(CoreEvent::ProbeFailed {
                        url: url.clone(),
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

        let url_key = normalize_url(&selection.url)
            .ok_or_else(|| format!("invalid URL: {}", selection.url))?;
        let probe = self.pending_probes.get(&url_key).ok_or_else(|| {
            format!(
                "no pending probe for {}. Call probe() first.",
                url_key
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

        let url_key = normalize_url(&selection.url)
            .ok_or_else(|| format!("invalid URL: {}", selection.url))?;
        let probe = self
            .pending_probes
            .get(&url_key)
            .ok_or_else(|| {
                format!(
                    "no pending probe for {}. Call probe() first.",
                    url_key
                )
            })?;

        // Build lookup maps
        let items_map: HashMap<&str, &MediaItem> =
            probe.items.iter().map(|i| (i.id.as_str(), i)).collect();

        let mut job_ids = Vec::new();
        let media_extras = self.resolve_media_extras(selection.media_extras);

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
            job.set_media_extras(media_extras);
            job.set_media_identity(item.id.clone(), item.title.clone());
            self.apply_extractor_filename(&mut job, item, quality);
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

    /// Process pending jobs synchronously — each transfer runs to completion
    /// before the next starts.  Prefer [`Self::dispatch_pending`] in shells.
    pub fn process_pending(&mut self) -> Result<(), String> {
        let global_bucket = self
            .bandwidth_cap
            .map(|rate| shared_token_bucket(rate, self.clock.clone()));

        let indices: Vec<usize> = self
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| j.is_pending())
            .map(|(i, _)| i)
            .collect();

        for i in indices {
            self.transfer_job_at_index(i, &global_bucket)?;
        }

        self.flush_pending_progress()?;
        Ok(())
    }

    /// Start pending transfers up to the Active Job Limit without blocking.
    pub fn dispatch_pending(core: Arc<Mutex<Self>>) -> Result<(), String> {
        let (global_bucket, job_ids) = {
            let core = core.lock().map_err(|e| e.to_string())?;
            let slots = core
                .active_job_limit
                .saturating_sub(core.job_signals.active_transfer_count() as u32) as usize;
            if slots == 0 {
                return Ok(());
            }
            let global_bucket = core
                .bandwidth_cap
                .map(|rate| shared_token_bucket(rate, core.clock.clone()));
            let mut job_ids = Vec::new();
            for job in &core.jobs {
                if !job.is_pending() {
                    continue;
                }
                if job.transfer_active().load(Ordering::Acquire) {
                    continue;
                }
                job_ids.push(job.id().to_string());
                if job_ids.len() >= slots {
                    break;
                }
            }
            (global_bucket, job_ids)
        };

        for job_id in job_ids {
            let core = Arc::clone(&core);
            let bucket = global_bucket.clone();
            std::thread::spawn(move || {
                if let Err(e) = Self::run_transfer_outside_lock(core, job_id, bucket) {
                    eprintln!("transfer job failed: {e}");
                }
            });
        }
        Ok(())
    }

    /// Run a job transfer, releasing the Core mutex during blocking body I/O.
    fn run_transfer_outside_lock(
        core: Arc<Mutex<Self>>,
        job_id: String,
        global_bucket: Option<SharedTokenBucket>,
    ) -> Result<(), String> {
        loop {
            let plan = {
                let mut core = core.lock().map_err(|e| e.to_string())?;
                core.plan_unlocked_transfer(&job_id, &global_bucket)?
            };
            let Some(plan) = plan else {
                return Ok(());
            };

            let outcome = plan.run();

            let retry = {
                let mut core = core.lock().map_err(|e| e.to_string())?;
                core.finish_unlocked_transfer(&job_id, outcome)?
            };
            if !retry {
                return Ok(());
            }
        }
    }

    /// Prepare a transfer that can run without holding the Core mutex.
    fn plan_unlocked_transfer(
        &mut self,
        job_id: &str,
        global_bucket: &Option<SharedTokenBucket>,
    ) -> Result<Option<UnlockedTransferPlan>, String> {
        let i = self
            .jobs
            .iter()
            .position(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        if !self.jobs[i].is_pending() {
            return Ok(None);
        }
        if self.jobs[i].transfer_active().load(Ordering::Acquire) {
            return Ok(None);
        }

        if self.jobs[i].extractor_format().is_some() {
            return Ok(Some(UnlockedTransferPlan::UnderLock { job_index: i }));
        }

        let url = self.jobs[i].url().to_string();
        let dest = self.jobs[i].destination().to_path_buf();
        let bytes_received = self.jobs[i].bytes_received();
        if !self.guard_disk_space_for_transfer(i, job_id, &dest, &url, bytes_received)? {
            return Ok(None);
        }

        let filename = self.jobs[i].filename().to_string();
        let job_cap = self.jobs[i].bandwidth_cap();
        let pause_requested = self.jobs[i].pause_requested().clone();
        let transfer_active = self.jobs[i].transfer_active().clone();

        if bytes_received == 0 {
            let supports_range = matches!(self.http.supports_range(&url), Ok(true));
            let job_bucket = job_cap.map(|rate| make_job_bucket(rate, self.clock.clone()));
            let rate_limited = global_bucket.is_some() || job_bucket.is_some();
            if supports_range && !rate_limited {
                return Ok(Some(UnlockedTransferPlan::UnderLock { job_index: i }));
            }
        }

        let known_total = self.http.file_size(&url).ok().filter(|&n| n > 0);
        if let Some(total) = known_total {
            self.jobs[i].set_total_bytes(Some(total));
        }
        let total_for_progress = self.jobs[i].total_bytes();

        self.event_tx
            .send(CoreEvent::JobProgress {
                job_id: job_id.to_string(),
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
            job_id: job_id.to_string(),
            event_tx: self.event_tx.clone(),
            total_bytes: total_for_progress,
            live_bytes,
        };

        let range = if bytes_received > 0 {
            Some(bytes_received..u64::MAX)
        } else {
            None
        };
        let job_bucket = job_cap.map(|rate| make_job_bucket(rate, self.clock.clone()));

        Ok(Some(UnlockedTransferPlan::SingleFetch {
            job_index: i,
            bytes_received,
            url,
            part_path,
            file_path,
            range,
            append: bytes_received > 0,
            truncate: bytes_received == 0,
            http: self.http.clone(),
            global_bucket: global_bucket.clone(),
            job_bucket,
            pause_requested,
            transfer_active,
            progress,
        }))
    }

    /// Apply the result of an unlocked transfer.  Returns `true` when the
    /// mirror failover loop should retry.
    fn finish_unlocked_transfer(
        &mut self,
        job_id: &str,
        outcome: UnlockedTransferOutcome,
    ) -> Result<bool, String> {
        match outcome {
            UnlockedTransferOutcome::UnderLock { job_index } => {
                let bucket = self
                    .bandwidth_cap
                    .map(|rate| shared_token_bucket(rate, self.clock.clone()));
                self.transfer_job_at_index(job_index, &bucket)?;
                Ok(false)
            }
            UnlockedTransferOutcome::Fetched {
                job_index,
                bytes_received,
                part_path,
                file_path,
                outcome,
                failed_url,
            } => {
                if outcome.paused {
                    self.sync_job_progress(job_id, bytes_received)?;
                    self.finalize_stop_after_transfer(job_id)?;
                    return Ok(false);
                }

                match outcome.fetch {
                    Ok(result) => {
                        let reported = result.total_length;
                        let on_disk = self.sync_job_progress(job_id, bytes_received)?;
                        let total_opt = self.jobs[job_index].total_bytes().or_else(|| {
                            if reported > 0 {
                                Some(reported)
                            } else {
                                None
                            }
                        });
                        if self.jobs[job_index].total_bytes().is_none() {
                            self.jobs[job_index].set_total_bytes(total_opt);
                        }

                        self.event_tx
                            .send(CoreEvent::JobProgress {
                                job_id: job_id.to_string(),
                                bytes_downloaded: on_disk,
                                total_bytes: total_opt,
                            })
                            .map_err(|_| "event channel closed".to_string())?;

                        match total_opt {
                            Some(total) if on_disk >= total => {
                                self.complete_job_file(job_id, job_index, &part_path, &file_path, total)?;
                            }
                            Some(_) => {
                                self.flush_pending_progress()?;
                            }
                            None => {
                                self.complete_job_file(job_id, job_index, &part_path, &file_path, on_disk)?;
                            }
                        }
                        Ok(false)
                    }
                    Err(err) => {
                        if self.jobs[job_index].advance_mirror() {
                            let next_url = self.jobs[job_index].url().to_string();
                            self.event_tx
                                .send(CoreEvent::JobMirrorFailover {
                                    job_id: job_id.to_string(),
                                    previous_url: failed_url,
                                    next_url,
                                    error: err,
                                })
                                .map_err(|_| "event channel closed".to_string())?;
                            self.persist_queue_now()?;
                            Ok(true)
                        } else {
                            self.finalize_failed_job(&job_id, job_index, err)?;
                            Ok(false)
                        }
                    }
                }
            }
        }
    }

    fn transfer_job_at_index(
        &mut self,
        i: usize,
        global_bucket: &Option<SharedTokenBucket>,
    ) -> Result<(), String> {
            let (
                job_id,
                url,
                dest,
                filename,
                bytes_received,
                job_cap,
                extractor_format,
                media_extras,
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
                    job.media_extras(),
                    job.pause_requested().clone(),
                    job.transfer_active().clone(),
                )
            };

            let ytdlp_output_spec = extractor_format
                .as_ref()
                .map(|_| {
                    let (spec, basename) = self.extractor_output_for_job(&self.jobs[i]);
                    self.jobs[i].set_filename(basename);
                    spec
                });

            if !self.guard_disk_space_for_transfer(i, &job_id, &dest, &url, bytes_received)? {
                return Ok(());
            }

            if let Some(format) = extractor_format {
                let output_spec_owned = ytdlp_output_spec
                    .unwrap_or_else(|| self.filename_template.clone());
                let on_disk = storage::destination_byte_total(&dest);
                if on_disk > 0 && !self.extractor.supports_extractor_resume() {
                    self.finalize_failed_job(
                        &job_id,
                        i,
                        "extractor cannot resume partial download; use Retry".into(),
                    )?;
                    return Ok(());
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
                    let output_spec = output_spec_owned.as_str();
                    let extras = media_extras;
                    let cookies = self.cookie_channel.clone();
                    let proxy = self.proxy_settings.clone();
                    s.spawn(move || {
                        let outcome = extractor.download(
                            url_ref,
                            format_ref,
                            dest_ref,
                            stop,
                            &extras,
                            &cookies,
                            &proxy,
                            output_spec,
                        );
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
                        let ffmpeg_dir = self.extractor.ffmpeg_dir();
                        let post = apply_remux_postprocess(
                            &path,
                            &media_extras,
                            ffmpeg_dir,
                            pause_requested.as_ref(),
                        );
                        let (final_path, bytes) = match post {
                            PostprocessOutcome::Unchanged(p) | PostprocessOutcome::Remuxed(p) => {
                                let file_bytes =
                                    std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                                let bytes = Self::destination_byte_total(&dest).max(file_bytes);
                                (p, bytes)
                            }
                            PostprocessOutcome::Failed {
                                error,
                                pre_remux_path,
                            } => {
                                let _ = pre_remux_path;
                                self.finalize_failed_job(&job_id, i, error)?;
                                return Ok(());
                            }
                        };
                        live_bytes.store(bytes, Ordering::Release);
                        let _ = self.event_tx.send(CoreEvent::JobProgress {
                            job_id: job_id.clone(),
                            bytes_downloaded: bytes,
                            total_bytes: total_for_progress,
                        });
                        self.finalize_completed_job(&job_id, i, &final_path, bytes)?;
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
                            self.finalize_failed_job(&job_id, i, err)?;
                        }
                    }
                }
                return Ok(());
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
                                self.finalize_failed_job(&job_id, i, err)?;
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
                                return Ok(());
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
                        None => return Ok(()),
                    }
                    return Ok(());
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
                        self.finalize_failed_job(&job_id, i, err)?;
                        break 'fetch_loop None;
                    }
                }
            };

            let outcome = match job_fetch {
                Some(outcome) => outcome,
                None => return Ok(()),
            };

            if outcome.paused {
                self.sync_job_progress(&job_id, bytes_received)?;
                self.finalize_stop_after_transfer(&job_id)?;
                return Ok(());
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

        self.finalize_completed_job(job_id, job_index, file_path, total)
    }

    /// Mark a job Completed after optional Integrity Check, or Failed on mismatch.
    fn finalize_completed_job(
        &mut self,
        job_id: &str,
        job_index: usize,
        file_path: &Path,
        bytes_written: u64,
    ) -> Result<(), String> {
        if let Some(expected) = self.jobs[job_index].expected_hash().cloned() {
            if let Err(err) = expected.verify_file(file_path) {
                return self.finalize_failed_job(job_id, job_index, err);
            }
        }

        self.event_tx
            .send(CoreEvent::JobCompleted {
                job_id: job_id.to_string(),
                bytes_written,
                path: file_path.to_path_buf(),
            })
            .map_err(|_| "event channel closed".to_string())?;

        self.jobs[job_index].mark_completed();
        if let Some(name) = file_path.file_name().and_then(|n| n.to_str()) {
            self.jobs[job_index].set_filename(name);
        }
        self.record_media_identity(job_id, job_index)?;
        self.persist_queue_now()
    }

    fn mark_archived_probe_items(&self, items: &mut [MediaItem]) {
        for item in items {
            item.archived = self.download_archive.contains_completed(&item.id);
        }
    }

    fn record_media_identity(
        &mut self,
        job_id: &str,
        job_index: usize,
    ) -> Result<(), String> {
        let Some(media_id) = self.jobs[job_index].media_id().map(str::to_string) else {
            return Ok(());
        };
        let is_completed = self.jobs[job_index].is_completed();
        let title = self.jobs[job_index]
            .media_title()
            .map(str::to_string)
            .unwrap_or_else(|| self.jobs[job_index].filename().to_string());
        let reveal_path = if is_completed {
            self.jobs[job_index].final_path()
        } else {
            self.jobs[job_index].destination().clone()
        };
        let entry = download_archive::ArchiveEntry {
            media_id,
            title,
            url: self.jobs[job_index].url().to_string(),
            completed_at_unix_secs: download_archive::unix_now_secs(),
            destination: reveal_path,
            job_id: job_id.to_string(),
            outcome: if is_completed {
                download_archive::ArchiveOutcome::Completed
            } else {
                download_archive::ArchiveOutcome::Failed
            },
        };
        self.download_archive.record(entry);
        if let Some(root) = &self.library_root {
            self.download_archive.save(root)?;
        }
        Ok(())
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
    // History (Download Archive query)
    // -----------------------------------------------------------------------

    /// Return all archive entries (Completed + Failed), most recent first.
    ///
    /// This is the single source of truth for History views — there is no
    /// second History DB.  The archive only records media identities; plain
    /// HTTP jobs without media_id are not archived.
    pub fn list_history(&self) -> Vec<ArchiveEntry> {
        self.download_archive
            .list()
            .into_iter()
            .cloned()
            .collect()
    }

    /// Return archive entries matching the given outcome, most recent first.
    pub fn list_history_by_outcome(&self, outcome: ArchiveOutcome) -> Vec<ArchiveEntry> {
        self.download_archive
            .list_by_outcome(outcome)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Remove an entry from the Download Archive by media_id.
    ///
    /// Files on disk are NOT touched — only the archive entry is deleted.
    pub fn remove_history_entry(&mut self, media_id: &str) -> Result<ArchiveEntry, String> {
        let removed = self
            .download_archive
            .remove(media_id)
            .ok_or_else(|| format!("no history entry for media_id '{media_id}'"))?;
        if let Some(root) = &self.library_root {
            self.download_archive.save(root)?;
        }
        Ok(removed)
    }

    /// Number of entries in the Download Archive.
    pub fn history_len(&self) -> usize {
        self.download_archive.len()
    }

    /// Whether the archive is empty.
    pub fn is_history_empty(&self) -> bool {
        self.download_archive.is_empty()
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
