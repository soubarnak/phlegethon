//! Storage — Library Root queue persistence and load reconciliation.
//!
//! Queue state lives at `{library_root}/.phlegethon/jobs.json`.
//! Writes are atomic and durable (temp file, fsync, rename, directory fsync).

use crate::cookie_channel::CookieChannel;
use crate::extractor::{ExtractionTier, ProbeTierSettings};
use crate::filename_template::DEFAULT_FILENAME_TEMPLATE;
use crate::integrity::ExpectedHash;
use crate::job::{Job, JobState};
use crate::media_extras::MediaExtrasOptions;
use crate::proxy_settings::ProxySettings;
use crate::quality_policy::DefaultQualityPolicy;
use crate::sidecar_updates::SidecarUpdateSettings;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Directory under Library Root for Core metadata.
pub const PHLEGETHON_DIR: &str = ".phlegethon";

/// Filename for the persisted job queue index.
pub const JOBS_FILENAME: &str = "jobs.json";

const QUEUE_VERSION: u32 = 1;

/// Default maximum concurrent transferring jobs.
pub const DEFAULT_ACTIVE_JOB_LIMIT: u32 = 3;

fn default_active_job_limit() -> u32 {
    DEFAULT_ACTIVE_JOB_LIMIT
}

fn default_quality_policy() -> DefaultQualityPolicy {
    DefaultQualityPolicy::default()
}

fn default_audio_only_mode() -> bool {
    false
}

fn default_media_extras() -> MediaExtrasOptions {
    MediaExtrasOptions::default()
}

/// Settings loaded from the persisted queue file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualitySettings {
    pub policy: DefaultQualityPolicy,
    pub audio_only_mode: bool,
}

/// Media extras settings loaded from the persisted queue file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaExtrasSettings {
    pub defaults: MediaExtrasOptions,
}

fn default_cookie_channel() -> CookieChannel {
    CookieChannel::default()
}

fn default_filename_template() -> String {
    DEFAULT_FILENAME_TEMPLATE.to_string()
}

fn default_proxy_settings() -> ProxySettings {
    ProxySettings::default()
}

fn default_sidecar_update_settings() -> SidecarUpdateSettings {
    SidecarUpdateSettings::default()
}

/// On-disk representation of the job queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedQueue {
    pub version: u32,
    pub global_bandwidth_cap: Option<u64>,
    #[serde(default = "default_active_job_limit")]
    pub active_job_limit: u32,
    #[serde(default = "default_quality_policy")]
    pub default_quality_policy: DefaultQualityPolicy,
    #[serde(default = "default_audio_only_mode")]
    pub audio_only_mode: bool,
    #[serde(default = "default_media_extras")]
    pub default_media_extras: MediaExtrasOptions,
    #[serde(default = "default_cookie_channel")]
    pub cookie_channel: CookieChannel,
    #[serde(default = "default_proxy_settings")]
    pub proxy_settings: ProxySettings,
    #[serde(default = "default_sidecar_update_settings")]
    pub sidecar_update_settings: SidecarUpdateSettings,
    #[serde(default = "default_filename_template")]
    pub filename_template: String,
    #[serde(default)]
    pub probe_tier_settings: ProbeTierSettings,
    pub jobs: Vec<PersistedJob>,
}

/// On-disk representation of a single job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedJob {
    pub id: String,
    pub mirror_urls: Vec<String>,
    pub mirror_index: usize,
    pub state: PersistedJobState,
    /// Absolute path to the Job Destination directory.
    pub destination: PathBuf,
    pub bytes_received: u64,
    pub bandwidth_cap: Option<u64>,
    pub source: Option<String>,
    pub extractor_format: Option<String>,
    pub filename: String,
    #[serde(default)]
    pub expected_hash: Option<ExpectedHash>,
    #[serde(default = "default_media_extras")]
    pub media_extras: MediaExtrasOptions,
    #[serde(default)]
    pub media_id: Option<String>,
    #[serde(default)]
    pub media_title: Option<String>,
    #[serde(default)]
    pub ytdlp_output_spec: Option<String>,
    #[serde(default)]
    pub media_uploader: Option<String>,
    #[serde(default)]
    pub media_upload_date: Option<String>,
    #[serde(default)]
    pub extraction_tier: Option<ExtractionTier>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub referer: Option<String>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub range_ignored: bool,
    /// Per-segment progress of a Segmented Transfer; empty for single-connection jobs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<Segment>,
    /// Cookie session applied to this job only (see `EnqueueOptions::cookie_session`).
    #[serde(default)]
    pub cookie_session: Option<CookieChannel>,
}

/// One byte range of a Segmented Transfer and how much of it is on disk.
///
/// `written` counts the contiguous bytes from `start` that have been written
/// and synced, so the unfinished part of the range is `start + written..end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub start: u64,
    pub end: u64,
    pub written: u64,
}

impl Segment {
    /// Split `total` bytes into at most `count` contiguous segments.
    pub fn split(total: u64, count: u64) -> Vec<Segment> {
        let count = count.min(total);
        if count == 0 {
            return Vec::new();
        }
        let size = total / count;
        (0..count)
            .map(|s| Segment {
                start: s * size,
                end: if s == count - 1 {
                    total
                } else {
                    (s + 1) * size
                },
                written: 0,
            })
            .collect()
    }

    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_complete(&self) -> bool {
        self.written >= self.len()
    }

    /// Bytes still to fetch, as an absolute range; `None` when complete.
    pub fn remaining(&self) -> Option<std::ops::Range<u64>> {
        (!self.is_complete()).then(|| self.start + self.written..self.end)
    }

    /// True when the table is a gap-free cover of `0..total` with sane counts.
    pub fn table_is_valid(segments: &[Segment], total: u64) -> bool {
        let mut next = 0;
        for seg in segments {
            if seg.start != next || seg.end <= seg.start || seg.written > seg.len() {
                return false;
            }
            next = seg.end;
        }
        !segments.is_empty() && next == total
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PersistedJobState {
    Pending,
    /// In-flight download interrupted before completion (relaunch auto-resumes).
    Downloading,
    Paused,
    Failed,
    Completed,
}

impl From<PersistedJobState> for JobState {
    fn from(state: PersistedJobState) -> Self {
        match state {
            PersistedJobState::Pending | PersistedJobState::Downloading => Self::Pending,
            PersistedJobState::Paused => Self::Paused,
            PersistedJobState::Failed => Self::Failed,
            PersistedJobState::Completed => Self::Completed,
        }
    }
}

/// Path to the queue index file under a Library Root.
pub fn queue_path(library_root: &Path) -> PathBuf {
    library_root.join(PHLEGETHON_DIR).join(JOBS_FILENAME)
}

/// Everything read from `jobs.json`, ready for [`Core::set_library_root`] to
/// commit in one step.
///
/// [`Core::set_library_root`]: crate::Core::set_library_root
#[derive(Debug)]
pub struct LoadedQueue {
    pub jobs: Vec<Job>,
    pub global_bandwidth_cap: Option<u64>,
    pub active_job_limit: u32,
    pub quality: QualitySettings,
    pub media_extras: MediaExtrasSettings,
    pub cookie_channel: CookieChannel,
    pub proxy_settings: ProxySettings,
    pub sidecar_update_settings: SidecarUpdateSettings,
    pub filename_template: String,
    pub probe_tier_settings: ProbeTierSettings,
    /// Human-readable notes about anything skipped, reset or moved aside.
    pub warnings: Vec<String>,
}

impl LoadedQueue {
    fn empty() -> Self {
        Self {
            jobs: Vec::new(),
            global_bandwidth_cap: None,
            active_job_limit: DEFAULT_ACTIVE_JOB_LIMIT,
            quality: QualitySettings {
                policy: DefaultQualityPolicy::default(),
                audio_only_mode: false,
            },
            media_extras: MediaExtrasSettings {
                defaults: MediaExtrasOptions::default(),
            },
            cookie_channel: CookieChannel::default(),
            proxy_settings: ProxySettings::default(),
            sidecar_update_settings: SidecarUpdateSettings::default(),
            filename_template: DEFAULT_FILENAME_TEMPLATE.to_string(),
            probe_tier_settings: ProbeTierSettings::default(),
            warnings: Vec::new(),
        }
    }
}

/// Read the queue file under a Library Root once.
///
/// A missing file gives an empty queue with default settings. A file that
/// cannot be parsed at all (truncated, not JSON, wrong shape or version) is
/// moved to `jobs.json.corrupt-<timestamp>` and the queue starts empty. Rows
/// that fail to parse or validate are skipped one at a time, and the original
/// file is kept as `jobs.json.corrupt-<timestamp>` so nothing is lost when the
/// next save rewrites it. Every such event adds a warning. Only an I/O error
/// that leaves the file's state unknown is returned as `Err`.
pub fn load_queue(library_root: &Path) -> Result<LoadedQueue, String> {
    let path = queue_path(library_root);
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(LoadedQueue::empty()),
        Err(e) => return Err(format!("read queue {}: {e}", path.display())),
    };

    let (queue, rows) = match parse_queue_file(&raw) {
        Ok(parsed) => parsed,
        Err(reason) => {
            let aside = crate::fsutil::move_aside_corrupt(&path)
                .map_err(|e| format!("move aside unreadable queue {}: {e}", path.display()))?;
            let mut loaded = LoadedQueue::empty();
            loaded.warnings.push(format!(
                "Queue file {} could not be read ({reason}). It was moved to {} and the queue starts empty.",
                path.display(),
                aside.display()
            ));
            return Ok(loaded);
        }
    };

    let mut loaded = LoadedQueue::empty();
    loaded.global_bandwidth_cap = queue.global_bandwidth_cap.filter(|&cap| cap > 0);
    loaded.active_job_limit = if queue.active_job_limit >= 1 {
        queue.active_job_limit
    } else {
        loaded.warnings.push(format!(
            "Stored Active Job Limit {} is below 1; using {DEFAULT_ACTIVE_JOB_LIMIT}.",
            queue.active_job_limit
        ));
        DEFAULT_ACTIVE_JOB_LIMIT
    };
    loaded.quality = QualitySettings {
        policy: queue.default_quality_policy,
        audio_only_mode: queue.audio_only_mode,
    };
    loaded.media_extras = MediaExtrasSettings {
        defaults: queue.default_media_extras,
    };
    let cookie_channel = queue.cookie_channel.normalized();
    loaded.cookie_channel = match cookie_channel.validate() {
        Ok(()) => cookie_channel,
        Err(reason) => {
            loaded
                .warnings
                .push(format!("Stored cookie settings were reset: {reason}."));
            CookieChannel::default()
        }
    };
    loaded.proxy_settings = queue.proxy_settings.normalized();
    loaded.sidecar_update_settings = queue.sidecar_update_settings;
    loaded.filename_template =
        match crate::filename_template::validate_filename_template(&queue.filename_template) {
            Ok(()) => queue.filename_template.trim().to_string(),
            Err(reason) => {
                loaded.warnings.push(format!(
                    "Stored Filename Template was reset to the default: {reason}."
                ));
                DEFAULT_FILENAME_TEMPLATE.to_string()
            }
        };
    loaded.probe_tier_settings = queue.probe_tier_settings;

    let mut seen_ids = std::collections::HashSet::new();
    let mut skipped = 0usize;
    for (index, row) in rows.into_iter().enumerate() {
        match load_job_row(row, library_root, &mut seen_ids, &mut loaded.warnings) {
            Ok(job) => loaded.jobs.push(job),
            Err(reason) => {
                skipped += 1;
                loaded
                    .warnings
                    .push(format!("Skipped queue entry {}: {reason}.", index + 1));
            }
        }
    }
    if skipped > 0 {
        match crate::fsutil::copy_aside_corrupt(&path) {
            Ok(copy) => loaded.warnings.push(format!(
                "{skipped} queue entries were skipped; the original file is kept as {}.",
                copy.display()
            )),
            Err(e) => loaded.warnings.push(format!(
                "{skipped} queue entries were skipped and the original could not be kept: {e}."
            )),
        }
    }
    Ok(loaded)
}

/// Split a queue file into its settings (with an empty job list) and raw rows.
fn parse_queue_file(raw: &[u8]) -> Result<(PersistedQueue, Vec<serde_json::Value>), String> {
    let mut value: serde_json::Value =
        serde_json::from_slice(raw).map_err(|e| format!("invalid JSON: {e}"))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| "not a JSON object".to_string())?;
    let version = object.get("version").and_then(serde_json::Value::as_u64);
    if version != Some(u64::from(QUEUE_VERSION)) {
        return Err(match version {
            Some(v) => format!("unsupported queue version {v} (expected {QUEUE_VERSION})"),
            None => "missing queue version".to_string(),
        });
    }
    let rows = match object.insert("jobs".to_string(), serde_json::Value::Array(Vec::new())) {
        Some(serde_json::Value::Array(rows)) => rows,
        Some(_) => return Err("jobs is not a list".to_string()),
        None => return Err("missing jobs".to_string()),
    };
    let queue: PersistedQueue =
        serde_json::from_value(value).map_err(|e| format!("invalid settings: {e}"))?;
    Ok((queue, rows))
}

/// Parse, validate and reconcile one persisted job row.
fn load_job_row(
    row: serde_json::Value,
    library_root: &Path,
    seen_ids: &mut std::collections::HashSet<String>,
    warnings: &mut Vec<String>,
) -> Result<Job, String> {
    let mut persisted: PersistedJob =
        serde_json::from_value(row).map_err(|e| format!("unreadable entry ({e})"))?;
    validate_persisted_job(&persisted, library_root)?;
    if !seen_ids.insert(persisted.id.clone()) {
        return Err(format!("duplicate job id {}", persisted.id));
    }
    if let Some(session) = persisted.cookie_session.take() {
        let session = session.normalized();
        match session.validate() {
            Ok(()) if session.is_configured() => persisted.cookie_session = Some(session),
            Ok(()) => {}
            Err(reason) => warnings.push(format!(
                "Job {} lost its cookie session: {reason}.",
                persisted.id
            )),
        }
    }
    let mut job = Job::from_persisted(persisted)?;
    reconcile_job(&mut job)?;
    Ok(job)
}

/// Reject persisted values that would make Core touch paths outside a
/// job-owned Job Destination (Discard deletes it recursively).
fn validate_persisted_job(job: &PersistedJob, library_root: &Path) -> Result<(), String> {
    use std::path::Component;

    if job.id.is_empty() {
        return Err("empty job id".to_string());
    }
    if !job.filename.is_empty() {
        let mut parts = Path::new(&job.filename).components();
        let single = matches!(
            (parts.next(), parts.next()),
            (Some(Component::Normal(_)), None)
        );
        if !single || job.filename.contains('/') {
            return Err(format!(
                "job {}: filename {:?} is not a single path component",
                job.id, job.filename
            ));
        }
    }
    let dest = &job.destination;
    if !dest.is_absolute() {
        return Err(format!(
            "job {}: destination {} is not absolute",
            job.id,
            dest.display()
        ));
    }
    if dest
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(format!(
            "job {}: destination {} contains . or .. segments",
            job.id,
            dest.display()
        ));
    }
    let name = dest.file_name().ok_or_else(|| {
        format!(
            "job {}: destination {} has no folder name",
            job.id,
            dest.display()
        )
    })?;
    if library_root.starts_with(dest) {
        return Err(format!(
            "job {}: destination {} is the Library Root or contains it",
            job.id,
            dest.display()
        ));
    }
    // Every Job Destination Core creates ends in a folder named after the job id.
    let prefix: String = job.id.chars().take(8).collect();
    if !name.to_string_lossy().contains(&prefix) {
        return Err(format!(
            "job {}: destination {} is not a job-owned folder",
            job.id,
            dest.display()
        ));
    }
    Ok(())
}

/// Build a persisted queue snapshot from live jobs.
#[allow(clippy::too_many_arguments)]
pub fn build_queue(
    jobs: &[Job],
    global_bandwidth_cap: Option<u64>,
    active_job_limit: u32,
    default_quality_policy: &DefaultQualityPolicy,
    audio_only_mode: bool,
    default_media_extras: &MediaExtrasOptions,
    cookie_channel: &CookieChannel,
    proxy_settings: &ProxySettings,
    sidecar_update_settings: &SidecarUpdateSettings,
    filename_template: &str,
    probe_tier_settings: &ProbeTierSettings,
) -> PersistedQueue {
    PersistedQueue {
        version: QUEUE_VERSION,
        global_bandwidth_cap,
        active_job_limit,
        default_quality_policy: *default_quality_policy,
        audio_only_mode,
        default_media_extras: *default_media_extras,
        cookie_channel: cookie_channel.clone(),
        proxy_settings: proxy_settings.clone(),
        sidecar_update_settings: sidecar_update_settings.clone(),
        filename_template: filename_template.to_string(),
        probe_tier_settings: *probe_tier_settings,
        jobs: jobs.iter().map(Job::to_persisted).collect(),
    }
}

/// Atomically write the queue index to disk.
pub fn save_queue(library_root: &Path, queue: &PersistedQueue) -> Result<(), String> {
    let dir = library_root.join(PHLEGETHON_DIR);
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

    let path = dir.join(JOBS_FILENAME);
    let raw = serde_json::to_string_pretty(queue).map_err(|e| format!("serialize queue: {e}"))?;
    crate::fsutil::write_atomic(&path, raw.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

/// Sum sizes of regular files under a Job Destination (recursive).
pub fn destination_byte_total(dest: &Path) -> u64 {
    fn walk(path: &Path) -> u64 {
        let mut total = 0u64;
        let Ok(entries) = fs::read_dir(path) else {
            return 0;
        };
        for entry in entries.flatten() {
            let child = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                total = total.saturating_add(walk(&child));
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
        total
    }
    walk(dest)
}

/// Reconcile stored progress with on-disk partial data.
///
/// - HTTP jobs: `.part` length is authoritative when present.
/// - Extractor jobs: sum of files under the Job Destination.
/// - When partial data is missing but stored progress > 0, a Pending job is
///   marked Failed and a Paused job stays Paused with its progress reset.
pub fn reconcile_job(job: &mut Job) -> Result<(), String> {
    if job.extractor_format().is_some() {
        let on_disk = destination_byte_total(job.destination());
        if on_disk > 0 {
            job.set_bytes_received(on_disk);
        }
        return Ok(());
    }

    let part_path = part_path_for_job(job);
    let stored = job.bytes_received();

    if !job.segments().is_empty() {
        return reconcile_segmented_job(job, &part_path, stored);
    }

    match fs::metadata(&part_path) {
        Ok(meta) => {
            let part_len = meta.len();
            if part_len != stored {
                job.set_bytes_received(part_len);
            }
        }
        Err(_) if stored > 0 => match job.state() {
            JobState::Pending => {
                job.mark_failed();
                job.set_last_error(Some(
                    "The partial download is missing on disk; Retry starts over.".to_string(),
                ));
            }
            // A user-Paused job stays Paused; Resume restarts from zero.
            JobState::Paused => {
                job.set_bytes_received(0);
                job.set_last_error(Some(
                    "The partial download is missing on disk; Resume starts over.".to_string(),
                ));
            }
            JobState::Failed | JobState::Completed => {}
        },
        Err(_) => {}
    }
    Ok(())
}

/// A Segmented Transfer's `.part` is preallocated and written at independent
/// offsets, so its length says nothing about progress: the segment table does.
/// A table that no longer describes the file is dropped and the job restarts.
fn reconcile_segmented_job(job: &mut Job, part_path: &Path, stored: u64) -> Result<(), String> {
    let table_total = job.segments().last().map_or(0, |s| s.end);
    let valid = Segment::table_is_valid(job.segments(), table_total)
        && fs::metadata(part_path).is_ok_and(|m| m.len() == table_total);
    if valid {
        let written = job.segments().iter().map(|s| s.written).sum();
        job.set_bytes_received(written);
        job.set_total_bytes(Some(table_total));
    } else if stored > 0 && job.state() != JobState::Completed && !part_path.exists() {
        job.clear_segments();
        job.mark_failed();
    } else {
        job.clear_segments();
        job.set_bytes_received(0);
    }
    Ok(())
}

/// Path for an in-progress download (`{final}.part`).
pub fn part_path_for(final_path: &Path) -> PathBuf {
    let mut part = final_path.as_os_str().to_os_string();
    part.push(".part");
    PathBuf::from(part)
}

fn part_path_for_job(job: &Job) -> PathBuf {
    let file_path = job.destination().join(job.filename());
    part_path_for(&file_path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn segment_split_covers_the_object_without_gaps() {
        let table = Segment::split(10, 4);
        assert_eq!(table.len(), 4);
        assert!(Segment::table_is_valid(&table, 10));
        assert_eq!(
            table.last().unwrap().end,
            10,
            "the last segment takes the remainder"
        );
        assert_eq!(Segment::split(3, 4).len(), 3, "no empty segments");
        assert!(Segment::split(0, 4).is_empty());
    }

    #[test]
    fn segment_table_with_a_gap_or_overcount_is_invalid() {
        let mut table = Segment::split(100, 4);
        table[1].written = 26;
        assert!(
            !Segment::table_is_valid(&table, 100),
            "written past the end"
        );
        let mut gap = Segment::split(100, 4);
        gap[2].start += 1;
        assert!(!Segment::table_is_valid(&gap, 100));
        assert!(!Segment::table_is_valid(&Segment::split(100, 4), 99));
    }

    #[test]
    fn segment_remaining_starts_after_the_written_bytes() {
        let seg = Segment {
            start: 16,
            end: 32,
            written: 5,
        };
        assert_eq!(seg.remaining(), Some(21..32));
        let done = Segment { written: 16, ..seg };
        assert_eq!(done.remaining(), None);
    }

    use super::*;
    use std::io::Write;

    #[test]
    fn save_and_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        fs::create_dir_all(&root).unwrap();

        let job = Job::new("https://example.com/file.zip".into(), root.clone());

        let queue = build_queue(
            &[job],
            Some(1024),
            DEFAULT_ACTIVE_JOB_LIMIT,
            &DefaultQualityPolicy::default(),
            false,
            &MediaExtrasOptions::default(),
            &CookieChannel::default(),
            &ProxySettings::default(),
            &SidecarUpdateSettings::default(),
            DEFAULT_FILENAME_TEMPLATE,
            &ProbeTierSettings::default(),
        );
        save_queue(&root, &queue).unwrap();

        let loaded = load_queue(&root).unwrap().jobs;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id(), queue.jobs[0].id);
    }

    #[test]
    fn reconcile_uses_part_length_when_meta_ahead() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        let mut job = Job::new("https://example.com/a.zip".into(), root);
        let dest = job.destination().clone();
        fs::create_dir_all(&dest).unwrap();
        job.set_bytes_received(1000);

        let part_path = part_path_for(&dest.join("a.zip"));
        let mut f = fs::File::create(&part_path).unwrap();
        f.write_all(&vec![0u8; 800]).unwrap();

        reconcile_job(&mut job).unwrap();
        assert_eq!(job.bytes_received(), 800);
    }

    #[test]
    fn reconcile_uses_part_length_when_disk_ahead() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        let mut job = Job::new("https://example.com/a.zip".into(), root);
        let dest = job.destination().clone();
        fs::create_dir_all(&dest).unwrap();
        job.set_bytes_received(500);

        let part_path = part_path_for(&dest.join("a.zip"));
        let mut f = fs::File::create(&part_path).unwrap();
        f.write_all(&vec![0u8; 800]).unwrap();

        reconcile_job(&mut job).unwrap();
        assert_eq!(job.bytes_received(), 800);
    }

    #[test]
    fn reconcile_marks_failed_when_partial_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        let mut job = Job::new("https://example.com/a.zip".into(), root);
        job.set_bytes_received(500);

        reconcile_job(&mut job).unwrap();
        assert_eq!(job.state(), JobState::Failed);
    }

    #[test]
    fn reconcile_syncs_extractor_destination_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        let mut job = Job::new("https://example.com/video".into(), root);
        job.set_extractor_format("720p");
        let dest = job.destination().clone();
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("partial.bin"), b"partial extractor data").unwrap();

        reconcile_job(&mut job).unwrap();
        assert_eq!(job.bytes_received(), 22);
    }

    #[test]
    fn downloading_state_round_trips_as_pending() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        fs::create_dir_all(&root).unwrap();

        let mut job = Job::new("https://example.com/a.zip".into(), root.clone());
        let dest = job.destination().clone();
        fs::create_dir_all(&dest).unwrap();
        let part_path = part_path_for(&dest.join("a.zip"));
        fs::write(&part_path, vec![0u8; 512]).unwrap();
        job.set_bytes_received(512);
        assert_eq!(job.persisted_state(), PersistedJobState::Downloading);

        let queue = build_queue(
            &[job],
            None,
            DEFAULT_ACTIVE_JOB_LIMIT,
            &DefaultQualityPolicy::default(),
            false,
            &MediaExtrasOptions::default(),
            &CookieChannel::default(),
            &ProxySettings::default(),
            &SidecarUpdateSettings::default(),
            DEFAULT_FILENAME_TEMPLATE,
            &ProbeTierSettings::default(),
        );
        save_queue(&root, &queue).unwrap();

        let loaded = load_queue(&root).unwrap().jobs;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].state(), JobState::Pending);
        assert_eq!(loaded[0].bytes_received(), 512);
    }
}
