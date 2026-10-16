//! Download Archive — Library Root–scoped completed media identities.
//!
//! Persisted at `{library_root}/.phlegethon/archive.json`. Probe marks archived
//! items; History (P3) reads the same store.  No second History DB.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::storage::PHLEGETHON_DIR;

/// Filename for the persisted Download Archive index.
pub const ARCHIVE_FILENAME: &str = "archive.json";

const ARCHIVE_VERSION: u32 = 2;

/// Outcome of a completed or failed media job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchiveOutcome {
    Completed,
    Failed,
}

/// A completed media identity recorded in the Download Archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub media_id: String,
    pub title: String,
    pub url: String,
    pub completed_at_unix_secs: u64,
    pub destination: PathBuf,
    pub job_id: String,
    #[serde(default = "default_outcome")]
    pub outcome: ArchiveOutcome,
}

/// v1 compatibility: entries without `outcome` default to Completed.
fn default_outcome() -> ArchiveOutcome {
    ArchiveOutcome::Completed
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedArchive {
    version: u32,
    entries: HashMap<String, ArchiveEntry>,
}

/// Filename of the lock that serializes writers of `archive.json`.
const ARCHIVE_LOCK_FILENAME: &str = "archive.lock";

/// In-memory Download Archive backed by `archive.json` under the Library Root.
///
/// Changes since the last load or save are journaled in `pending_upserts` and
/// `pending_removals`. [`Self::save`] replays that journal onto whatever is on
/// disk at that moment, so two writers (for example two app instances on one
/// Library Root) do not overwrite each other's entries.
#[derive(Debug, Clone, Default)]
pub struct DownloadArchive {
    entries: HashMap<String, ArchiveEntry>,
    pending_upserts: HashMap<String, ArchiveEntry>,
    pending_removals: HashSet<String>,
}

impl DownloadArchive {
    pub fn load(library_root: &Path) -> Result<Self, String> {
        let path = archive_path(library_root);
        if !path.exists() {
            return Ok(Self::default());
        }

        let raw = fs::read_to_string(&path)
            .map_err(|e| format!("read archive {}: {e}", path.display()))?;
        let persisted: PersistedArchive = serde_json::from_str(&raw)
            .map_err(|e| format!("parse archive {}: {e}", path.display()))?;

        if persisted.version > ARCHIVE_VERSION {
            return Err(format!(
                "unsupported archive version {} (expected ≤{ARCHIVE_VERSION})",
                persisted.version
            ));
        }

        Ok(Self {
            entries: persisted.entries,
            ..Self::default()
        })
    }

    /// Like [`Self::load`], but an archive that cannot be parsed or has an
    /// unsupported version is moved to `archive.json.corrupt-<timestamp>` and
    /// an empty archive is returned with a warning, so a bad archive never
    /// blocks a Library Root. Only an I/O error is returned as `Err`.
    pub fn load_or_reset(library_root: &Path) -> Result<(Self, Option<String>), String> {
        let path = archive_path(library_root);
        match fs::read(&path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Self::default(), None));
            }
            Err(e) => return Err(format!("read archive {}: {e}", path.display())),
        }
        match Self::load(library_root) {
            Ok(archive) => Ok((archive, None)),
            Err(reason) => {
                let aside = crate::fsutil::move_aside_corrupt(&path)
                    .map_err(|e| format!("move aside archive {}: {e}", path.display()))?;
                Ok((
                    Self::default(),
                    Some(format!(
                        "Download Archive {} could not be read ({reason}). It was moved to {} and History starts empty.",
                        path.display(),
                        aside.display()
                    )),
                ))
            }
        }
    }

    /// Write the archive under the Library Root.
    ///
    /// Takes an exclusive lock on `archive.lock`, re-reads `archive.json`,
    /// replays this archive's journaled changes onto it and writes the result
    /// atomically, so concurrent writers never lose each other's updates. This
    /// archive then adopts the merged contents.
    pub fn save(&mut self, library_root: &Path) -> Result<(), String> {
        use fs2::FileExt;

        let dir = library_root.join(PHLEGETHON_DIR);
        fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

        let lock_path = dir.join(ARCHIVE_LOCK_FILENAME);
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| format!("open {}: {e}", lock_path.display()))?;
        lock.lock_exclusive()
            .map_err(|e| format!("lock {}: {e}", lock_path.display()))?;

        let path = dir.join(ARCHIVE_FILENAME);
        let mut merged = match Self::load(library_root) {
            Ok(on_disk) => on_disk.entries,
            Err(reason) => {
                // Unreadable on disk: keep a copy for inspection and fall back
                // to what this process knows.
                if path.exists() {
                    let _ = crate::fsutil::copy_aside_corrupt(&path);
                }
                eprintln!("phlegethon: Download Archive on disk was replaced: {reason}");
                self.entries.clone()
            }
        };
        for media_id in &self.pending_removals {
            merged.remove(media_id);
        }
        for entry in self.pending_upserts.values() {
            insert_entry(&mut merged, entry.clone());
        }

        let persisted = PersistedArchive {
            version: ARCHIVE_VERSION,
            entries: merged,
        };
        let raw = serde_json::to_string_pretty(&persisted)
            .map_err(|e| format!("serialize archive: {e}"))?;
        crate::fsutil::write_atomic(&path, raw.as_bytes())
            .map_err(|e| format!("write {}: {e}", path.display()))?;

        self.entries = persisted.entries;
        self.pending_upserts.clear();
        self.pending_removals.clear();
        drop(lock);
        Ok(())
    }

    pub fn contains(&self, media_id: &str) -> bool {
        self.entries.contains_key(media_id)
    }

    /// True when a Completed entry exists for this media_id (probe archival skip).
    pub fn contains_completed(&self, media_id: &str) -> bool {
        self.entries
            .get(media_id)
            .is_some_and(|e| e.outcome == ArchiveOutcome::Completed)
    }

    /// Record an entry. A Failed entry never replaces a Completed one for the
    /// same media id, so a failed retry cannot hide a download that exists.
    /// Returns whether the entry was stored.
    pub fn record(&mut self, entry: ArchiveEntry) -> bool {
        let media_id = entry.media_id.clone();
        if !insert_entry(&mut self.entries, entry) {
            return false;
        }
        self.pending_removals.remove(&media_id);
        if let Some(stored) = self.entries.get(&media_id) {
            self.pending_upserts.insert(media_id, stored.clone());
        }
        true
    }

    /// Remove an entry from the archive by media_id.
    /// Returns the removed entry if it existed.
    pub fn remove(&mut self, media_id: &str) -> Option<ArchiveEntry> {
        let removed = self.entries.remove(media_id);
        if removed.is_some() {
            self.pending_upserts.remove(media_id);
            self.pending_removals.insert(media_id.to_string());
        }
        removed
    }

    /// Remove entries matching `pred`; returns how many were removed.
    pub fn remove_where(&mut self, pred: impl Fn(&ArchiveEntry) -> bool) -> usize {
        let doomed: Vec<String> = self
            .entries
            .values()
            .filter(|entry| pred(entry))
            .map(|entry| entry.media_id.clone())
            .collect();
        for media_id in &doomed {
            self.remove(media_id);
        }
        doomed.len()
    }

    /// Return all archive entries sorted by completion time (most recent first).
    pub fn list(&self) -> Vec<&ArchiveEntry> {
        let mut entries: Vec<&ArchiveEntry> = self.entries.values().collect();
        entries.sort_by(|a, b| b.completed_at_unix_secs.cmp(&a.completed_at_unix_secs));
        entries
    }

    /// Return archive entries matching a given outcome (Completed / Failed).
    pub fn list_by_outcome(&self, outcome: ArchiveOutcome) -> Vec<&ArchiveEntry> {
        let mut entries: Vec<&ArchiveEntry> = self
            .entries
            .values()
            .filter(|e| e.outcome == outcome)
            .collect();
        entries.sort_by(|a, b| b.completed_at_unix_secs.cmp(&a.completed_at_unix_secs));
        entries
    }

    /// Number of entries in the archive.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Insert `entry`, unless it is Failed and a Completed entry already holds the
/// media id. Returns whether it was inserted.
fn insert_entry(entries: &mut HashMap<String, ArchiveEntry>, entry: ArchiveEntry) -> bool {
    let keeps_completed = entry.outcome == ArchiveOutcome::Failed
        && entries
            .get(&entry.media_id)
            .is_some_and(|existing| existing.outcome == ArchiveOutcome::Completed);
    if keeps_completed {
        return false;
    }
    entries.insert(entry.media_id.clone(), entry);
    true
}

/// Path to the archive index file under a Library Root.
pub fn archive_path(library_root: &Path) -> PathBuf {
    library_root.join(PHLEGETHON_DIR).join(ARCHIVE_FILENAME)
}

pub fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        fs::create_dir_all(&root).unwrap();

        let mut archive = DownloadArchive::default();
        archive.record(ArchiveEntry {
            media_id: "yt-abc123".to_string(),
            title: "Example".to_string(),
            url: "https://www.youtube.com/watch?v=abc123".to_string(),
            completed_at_unix_secs: 1_700_000_000,
            destination: root.join("jobs").join("job-1"),
            job_id: "job-1".to_string(),
            outcome: ArchiveOutcome::Completed,
        });
        archive.save(&root).unwrap();

        let loaded = DownloadArchive::load(&root).unwrap();
        assert!(loaded.contains("yt-abc123"));
        assert_eq!(loaded.entries.get("yt-abc123").unwrap().title, "Example");
    }

    #[test]
    fn list_returns_sorted_by_time_descending() {
        let mut archive = DownloadArchive::default();
        archive.record(ArchiveEntry {
            media_id: "old".to_string(),
            title: "Old".to_string(),
            url: "https://example.com/old".to_string(),
            completed_at_unix_secs: 100,
            destination: PathBuf::from("/tmp/old"),
            job_id: "job-old".to_string(),
            outcome: ArchiveOutcome::Completed,
        });
        archive.record(ArchiveEntry {
            media_id: "new".to_string(),
            title: "New".to_string(),
            url: "https://example.com/new".to_string(),
            completed_at_unix_secs: 200,
            destination: PathBuf::from("/tmp/new"),
            job_id: "job-new".to_string(),
            outcome: ArchiveOutcome::Completed,
        });

        let list = archive.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].media_id, "new");
        assert_eq!(list[1].media_id, "old");
    }

    #[test]
    fn list_by_outcome_filters_correctly() {
        let mut archive = DownloadArchive::default();
        archive.record(ArchiveEntry {
            media_id: "completed-1".to_string(),
            title: "C1".to_string(),
            url: "https://example.com/c1".to_string(),
            completed_at_unix_secs: 100,
            destination: PathBuf::from("/tmp/c1"),
            job_id: "jc1".to_string(),
            outcome: ArchiveOutcome::Completed,
        });
        archive.record(ArchiveEntry {
            media_id: "failed-1".to_string(),
            title: "F1".to_string(),
            url: "https://example.com/f1".to_string(),
            completed_at_unix_secs: 200,
            destination: PathBuf::from("/tmp/f1"),
            job_id: "jf1".to_string(),
            outcome: ArchiveOutcome::Failed,
        });

        let completed = archive.list_by_outcome(ArchiveOutcome::Completed);
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].media_id, "completed-1");

        let failed = archive.list_by_outcome(ArchiveOutcome::Failed);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].media_id, "failed-1");
    }

    #[test]
    fn remove_removes_entry_and_returns_it() {
        let mut archive = DownloadArchive::default();
        archive.record(ArchiveEntry {
            media_id: "yt-abc".to_string(),
            title: "Test".to_string(),
            url: "https://youtube.com/watch?v=abc".to_string(),
            completed_at_unix_secs: 100,
            destination: PathBuf::from("/tmp/test"),
            job_id: "job-test".to_string(),
            outcome: ArchiveOutcome::Completed,
        });

        let removed = archive.remove("yt-abc");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().title, "Test");
        assert!(!archive.contains("yt-abc"));
        assert!(archive.is_empty());
    }

    #[test]
    fn remove_nonexistent_returns_none() {
        let mut archive = DownloadArchive::default();
        assert!(archive.remove("nonexistent").is_none());
    }

    #[test]
    fn loaded_version_1_is_backward_compatible() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        fs::create_dir_all(root.join(".phlegethon")).unwrap();

        // Write a v1 archive (no outcome field)
        let v1 = r#"{
            "version": 1,
            "entries": {
                "yt-abc": {
                    "media_id": "yt-abc",
                    "title": "Legacy",
                    "url": "https://youtube.com/watch?v=abc",
                    "completed_at_unix_secs": 100,
                    "destination": "/tmp/legacy",
                    "job_id": "job-legacy"
                }
            }
        }"#;
        fs::write(root.join(".phlegethon").join("archive.json"), v1).unwrap();

        let loaded = DownloadArchive::load(&root).unwrap();
        assert!(loaded.contains("yt-abc"));
        // v1 entries should get default outcome (Completed via serde default)
        let entry = loaded.entries.get("yt-abc").unwrap();
        assert_eq!(entry.outcome, ArchiveOutcome::Completed);
    }
}
