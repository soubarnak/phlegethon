//! Job struct — state machine per download job.

use crate::mirror_set::MirrorSet;
use crate::storage::{PersistedJob, PersistedJobState};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Outcome returned from `Core::enqueue`.
#[derive(Debug, Clone, PartialEq)]
pub enum EnqueueOutcome {
    /// The URL was classified as a direct download — a job was created.
    EnqueueDirect {
        job_id: String,
        /// Path for this job's files (under Library Root by default; may be an override).
        destination: PathBuf,
    },
    /// The URL requires probe-then-confirm.
    ProbeThenConfirm {
        url: String,
    },
}

/// Derive a filename from a URL path segment.
pub fn filename_from_url(url: &str) -> String {
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

/// A single download job, backed by a Mirror Set.
///
/// The job tries the primary URL first.  If that mirror fails, the next
/// mirror in the set is tried (failover).  If all mirrors are exhausted
/// the job transitions to Failed.
#[derive(Debug)]
pub struct Job {
    id: String,
    /// Ordered set of equivalent source URLs.
    mirrors: MirrorSet,
    state: JobState,
    /// The directory where this job's files land.
    destination: PathBuf,
    /// Number of bytes successfully written to disk so far.
    /// Used for resumable transfers via Range headers.
    bytes_received: u64,
    /// Live download counter updated mid-transfer (may lead `bytes_received`).
    live_downloaded: Arc<AtomicU64>,
    /// Known total object size when the server reports it.
    total_bytes: Option<u64>,
    /// Per-job bandwidth cap in bytes per second.
    /// None = uncapped for this job.
    bandwidth_cap: Option<u64>,
    /// Set by `pause_job` to stop an in-flight transfer without holding Core lock.
    pause_requested: Arc<AtomicBool>,
    /// Set by `discard_job` so an in-flight stop finalizes as Discard, not Pause.
    discard_requested: Arc<AtomicBool>,
    /// True while a transfer is actively writing body bytes for this job.
    transfer_active: Arc<AtomicBool>,
    /// Optional source name for batch/channel subfolder naming.
    source: Option<String>,
    /// When set, `process_pending` downloads via the Extractor Toolchain
    /// (yt-dlp) using this quality id instead of plain HTTP.
    extractor_format: Option<String>,
    /// Final filename under the Job Destination (stable across restarts).
    filename: String,
}

impl Job {
    pub fn new(url: String, library_root: PathBuf) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let destination = library_root.join("jobs").join(&id);
        Self::assemble(id, url, destination, None)
    }

    /// Create a new job for a probe-then-confirm item.
    ///
    /// `source_name` is optional; when present (batch/channel), the Job
    /// Destination is a dedicated subfolder derived from the source under
    /// `{library_root}/jobs/`.
    pub fn new_with_source(
        url: String,
        library_root: PathBuf,
        source: Option<String>,
    ) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let destination = match &source {
            Some(name) => {
                let safe_name = sanitize_folder_name(name);
                library_root
                    .join("jobs")
                    .join(format!("{}-{}", safe_name, &id[..8]))
            }
            None => library_root.join("jobs").join(&id),
        };
        Self::assemble(id, url, destination, source)
    }

    /// Like [`Self::new_with_source`], but destinations live directly under
    /// `destination_parent` (no `jobs/` segment) — used for Save to… overrides.
    pub fn new_with_source_at(
        url: String,
        destination_parent: PathBuf,
        source: Option<String>,
    ) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let destination = match &source {
            Some(name) => {
                let safe_name = sanitize_folder_name(name);
                destination_parent.join(format!("{}-{}", safe_name, &id[..8]))
            }
            None => destination_parent.join(&id),
        };
        Self::assemble(id, url, destination, source)
    }

    fn assemble(
        id: String,
        url: String,
        destination: PathBuf,
        source: Option<String>,
    ) -> Self {
        let filename = filename_from_url(&url);
        Self {
            id,
            mirrors: MirrorSet::new(url),
            state: JobState::Pending,
            destination,
            bytes_received: 0,
            live_downloaded: Arc::new(AtomicU64::new(0)),
            total_bytes: None,
            bandwidth_cap: None,
            pause_requested: Arc::new(AtomicBool::new(false)),
            discard_requested: Arc::new(AtomicBool::new(false)),
            transfer_active: Arc::new(AtomicBool::new(false)),
            source,
            extractor_format: None,
            filename,
        }
    }

    /// Reconstruct a job from persisted queue metadata.
    pub fn from_persisted(persisted: PersistedJob) -> Result<Self, String> {
        let mirrors = MirrorSet::from_persisted(persisted.mirror_urls, persisted.mirror_index)?;
        let mut job = Self {
            id: persisted.id,
            mirrors,
            state: JobState::from(persisted.state),
            destination: persisted.destination,
            bytes_received: persisted.bytes_received,
            live_downloaded: Arc::new(AtomicU64::new(persisted.bytes_received)),
            total_bytes: None,
            bandwidth_cap: persisted.bandwidth_cap,
            pause_requested: Arc::new(AtomicBool::new(false)),
            discard_requested: Arc::new(AtomicBool::new(false)),
            transfer_active: Arc::new(AtomicBool::new(false)),
            source: persisted.source,
            extractor_format: persisted.extractor_format,
            filename: persisted.filename,
        };
        if job.filename.is_empty() {
            job.filename = filename_from_url(job.url());
        }
        Ok(job)
    }

    /// On-disk queue state for this job.
    pub fn persisted_state(&self) -> PersistedJobState {
        match self.state {
            JobState::Pending if self.bytes_received > 0 => PersistedJobState::Downloading,
            JobState::Pending => PersistedJobState::Pending,
            JobState::Paused => PersistedJobState::Paused,
            JobState::Failed => PersistedJobState::Failed,
            JobState::Completed => PersistedJobState::Completed,
        }
    }

    /// Snapshot this job for queue persistence.
    pub fn to_persisted(&self) -> PersistedJob {
        PersistedJob {
            id: self.id.clone(),
            mirror_urls: self.mirror_urls().to_vec(),
            mirror_index: self.mirrors.current_index(),
            state: self.persisted_state(),
            destination: self.destination.clone(),
            bytes_received: self.bytes_received,
            bandwidth_cap: self.bandwidth_cap,
            source: self.source.clone(),
            extractor_format: self.extractor_format.clone(),
            filename: self.filename.clone(),
        }
    }

    /// Mark this job to download via the Extractor Toolchain (yt-dlp).
    pub fn set_extractor_format(&mut self, format: impl Into<String>) {
        self.extractor_format = Some(format.into());
    }

    /// Quality / format id for extractor-backed download, if any.
    pub fn extractor_format(&self) -> Option<&str> {
        self.extractor_format.as_deref()
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Primary/current URL for this job.
    pub fn url(&self) -> &str {
        self.mirrors.current_url()
    }

    /// All mirror URLs for this job.
    pub fn mirror_urls(&self) -> &[String] {
        self.mirrors.all_urls()
    }

    /// Access the underlying MirrorSet (mutably).
    #[allow(dead_code)]
    pub fn mirrors_mut(&mut self) -> &mut MirrorSet {
        &mut self.mirrors
    }

    /// Access the underlying MirrorSet (immutably).
    #[allow(dead_code)]
    pub fn mirrors(&self) -> &MirrorSet {
        &self.mirrors
    }

    /// Add a mirror URL to this job's Mirror Set.
    pub fn add_mirror(&mut self, url: String) {
        self.mirrors.add_mirror(url);
    }

    /// Advance to the next mirror after the current one failed.
    /// Returns true if there is another mirror to try.
    pub fn advance_mirror(&mut self) -> bool {
        self.mirrors.advance_on_failure()
    }

    pub fn destination(&self) -> &PathBuf {
        &self.destination
    }

    #[allow(dead_code)]
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

    /// Bytes downloaded for UI snapshots (includes mid-transfer live count).
    pub fn bytes_downloaded_snapshot(&self) -> u64 {
        self.live_downloaded
            .load(Ordering::Acquire)
            .max(self.bytes_received)
    }

    pub fn live_downloaded(&self) -> Arc<AtomicU64> {
        self.live_downloaded.clone()
    }

    pub fn set_bytes_received(&mut self, n: u64) {
        self.bytes_received = n;
        self.live_downloaded.store(n, Ordering::Release);
    }

    pub fn add_bytes_received(&mut self, n: u64) {
        self.bytes_received += n;
        self.live_downloaded
            .store(self.bytes_received, Ordering::Release);
    }

    pub fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    pub fn set_total_bytes(&mut self, total: Option<u64>) {
        self.total_bytes = total.filter(|&n| n > 0);
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    pub fn set_filename(&mut self, filename: impl Into<String>) {
        self.filename = filename.into();
    }

    /// Final file path for this job under its Job Destination.
    pub fn final_path(&self) -> PathBuf {
        self.destination.join(&self.filename)
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

    pub fn pause_requested(&self) -> &Arc<AtomicBool> {
        &self.pause_requested
    }

    pub fn discard_requested(&self) -> &Arc<AtomicBool> {
        &self.discard_requested
    }

    pub fn is_discard_requested(&self) -> bool {
        self.discard_requested.load(Ordering::Acquire)
    }

    pub fn transfer_active(&self) -> &Arc<AtomicBool> {
        &self.transfer_active
    }

    pub fn clear_pause_requested(&self) {
        self.pause_requested.store(false, Ordering::Release);
    }

    pub fn is_completed(&self) -> bool {
        self.state == JobState::Completed
    }

    pub fn mark_completed(&mut self) {
        self.state = JobState::Completed;
    }

    #[allow(dead_code)]
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
                self.clear_pause_requested();
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
                self.clear_pause_requested();
                self.state = JobState::Pending;
                self.bytes_received = 0;
                self.live_downloaded.store(0, Ordering::Release);
                self.total_bytes = None;
                self.mirrors.reset();
                Ok(())
            }
            _ => Err(format!(
                "cannot retry job {}: current state is {:?}",
                self.id, self.state
            )),
        }
    }
}
