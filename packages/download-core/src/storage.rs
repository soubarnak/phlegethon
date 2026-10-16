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

/// Load jobs from disk, reconciling each with its on-disk `.part` file.
pub fn load_jobs(library_root: &Path) -> Result<Vec<Job>, String> {
    let path = queue_path(library_root);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let raw =
        fs::read_to_string(&path).map_err(|e| format!("read queue {}: {e}", path.display()))?;
    let queue: PersistedQueue =
        serde_json::from_str(&raw).map_err(|e| format!("parse queue {}: {e}", path.display()))?;

    if queue.version != QUEUE_VERSION {
        return Err(format!(
            "unsupported queue version {} (expected {QUEUE_VERSION})",
            queue.version
        ));
    }

    let mut jobs = Vec::with_capacity(queue.jobs.len());
    for persisted in queue.jobs {
        let mut job = Job::from_persisted(persisted)?;
        reconcile_job(&mut job)?;
        jobs.push(job);
    }
    Ok(jobs)
}

/// Load the global bandwidth cap from the queue file, if present.
pub fn load_global_bandwidth_cap(library_root: &Path) -> Result<Option<u64>, String> {
    let path = queue_path(library_root);
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        fs::read_to_string(&path).map_err(|e| format!("read queue {}: {e}", path.display()))?;
    let queue: PersistedQueue =
        serde_json::from_str(&raw).map_err(|e| format!("parse queue {}: {e}", path.display()))?;
    Ok(queue.global_bandwidth_cap.filter(|&cap| cap > 0))
}

/// Load the Active Job Limit from the queue file, if present.
pub fn load_active_job_limit(library_root: &Path) -> Result<u32, String> {
    let path = queue_path(library_root);
    if !path.exists() {
        return Ok(DEFAULT_ACTIVE_JOB_LIMIT);
    }
    let raw =
        fs::read_to_string(&path).map_err(|e| format!("read queue {}: {e}", path.display()))?;
    let queue: PersistedQueue =
        serde_json::from_str(&raw).map_err(|e| format!("parse queue {}: {e}", path.display()))?;
    Ok(queue.active_job_limit)
}

/// Load Default Quality Policy settings from the queue file, if present.
pub fn load_quality_settings(library_root: &Path) -> Result<QualitySettings, String> {
    let path = queue_path(library_root);
    if !path.exists() {
        return Ok(QualitySettings {
            policy: DefaultQualityPolicy::default(),
            audio_only_mode: false,
        });
    }
    let raw =
        fs::read_to_string(&path).map_err(|e| format!("read queue {}: {e}", path.display()))?;
    let queue: PersistedQueue =
        serde_json::from_str(&raw).map_err(|e| format!("parse queue {}: {e}", path.display()))?;
    Ok(QualitySettings {
        policy: queue.default_quality_policy,
        audio_only_mode: queue.audio_only_mode,
    })
}

/// Load Default Media Extras settings from the queue file, if present.
pub fn load_media_extras_settings(library_root: &Path) -> Result<MediaExtrasSettings, String> {
    let path = queue_path(library_root);
    if !path.exists() {
        return Ok(MediaExtrasSettings {
            defaults: MediaExtrasOptions::default(),
        });
    }
    let raw =
        fs::read_to_string(&path).map_err(|e| format!("read queue {}: {e}", path.display()))?;
    let queue: PersistedQueue =
        serde_json::from_str(&raw).map_err(|e| format!("parse queue {}: {e}", path.display()))?;
    Ok(MediaExtrasSettings {
        defaults: queue.default_media_extras,
    })
}

/// Load cookie auth channel settings from the queue file, if present.
pub fn load_cookie_channel(library_root: &Path) -> Result<CookieChannel, String> {
    let path = queue_path(library_root);
    if !path.exists() {
        return Ok(CookieChannel::default());
    }
    let raw =
        fs::read_to_string(&path).map_err(|e| format!("read queue {}: {e}", path.display()))?;
    let queue: PersistedQueue =
        serde_json::from_str(&raw).map_err(|e| format!("parse queue {}: {e}", path.display()))?;
    Ok(queue.cookie_channel.normalized())
}

/// Load global proxy settings from the queue file, if present.
pub fn load_proxy_settings(library_root: &Path) -> Result<ProxySettings, String> {
    let path = queue_path(library_root);
    if !path.exists() {
        return Ok(ProxySettings::default());
    }
    let raw =
        fs::read_to_string(&path).map_err(|e| format!("read queue {}: {e}", path.display()))?;
    let queue: PersistedQueue =
        serde_json::from_str(&raw).map_err(|e| format!("parse queue {}: {e}", path.display()))?;
    Ok(queue.proxy_settings.normalized())
}

/// Load sidecar update manager settings from the queue file, if present.
pub fn load_sidecar_update_settings(library_root: &Path) -> Result<SidecarUpdateSettings, String> {
    let path = queue_path(library_root);
    if !path.exists() {
        return Ok(SidecarUpdateSettings::default());
    }
    let raw =
        fs::read_to_string(&path).map_err(|e| format!("read queue {}: {e}", path.display()))?;
    let queue: PersistedQueue =
        serde_json::from_str(&raw).map_err(|e| format!("parse queue {}: {e}", path.display()))?;
    Ok(queue.sidecar_update_settings)
}

/// Load Filename Template from the queue file, if present.
pub fn load_filename_template(library_root: &Path) -> Result<String, String> {
    let path = queue_path(library_root);
    if !path.exists() {
        return Ok(DEFAULT_FILENAME_TEMPLATE.to_string());
    }
    let raw =
        fs::read_to_string(&path).map_err(|e| format!("read queue {}: {e}", path.display()))?;
    let queue: PersistedQueue =
        serde_json::from_str(&raw).map_err(|e| format!("parse queue {}: {e}", path.display()))?;
    Ok(queue.filename_template)
}

/// Load probe tier (fallback chain) settings from the queue file, if present.
pub fn load_probe_tier_settings(library_root: &Path) -> Result<ProbeTierSettings, String> {
    let path = queue_path(library_root);
    if !path.exists() {
        return Ok(ProbeTierSettings::default());
    }
    let raw =
        fs::read_to_string(&path).map_err(|e| format!("read queue {}: {e}", path.display()))?;
    let queue: PersistedQueue =
        serde_json::from_str(&raw).map_err(|e| format!("parse queue {}: {e}", path.display()))?;
    Ok(queue.probe_tier_settings)
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
/// - When partial data is missing but stored progress > 0 (and job not completed),
///   the job is marked Failed.
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
        Err(_) if stored > 0 && job.state() != JobState::Completed => {
            job.mark_failed();
        }
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

        let loaded = load_jobs(&root).unwrap();
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

        let loaded = load_jobs(&root).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].state(), JobState::Pending);
        assert_eq!(loaded[0].bytes_received(), 512);
    }
}
