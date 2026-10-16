//! Download Archive — Library Root–scoped completed media identities.
//!
//! Persisted at `{library_root}/.phlegethon/archive.json`. Probe marks archived
//! items; History (P3) reads the same store.  No second History DB.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

/// In-memory Download Archive backed by `archive.json` under the Library Root.
#[derive(Debug, Clone, Default)]
pub struct DownloadArchive {
    entries: HashMap<String, ArchiveEntry>,
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
        })
    }

    pub fn save(&self, library_root: &Path) -> Result<(), String> {
        let dir = library_root.join(PHLEGETHON_DIR);
        fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

        let path = dir.join(ARCHIVE_FILENAME);
        let tmp = dir.join(".archive.json.tmp");
        let persisted = PersistedArchive {
            version: ARCHIVE_VERSION,
            entries: self.entries.clone(),
        };
        let raw = serde_json::to_string_pretty(&persisted)
            .map_err(|e| format!("serialize archive: {e}"))?;
        fs::write(&tmp, &raw).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        fs::rename(&tmp, &path)
            .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), path.display()))?;
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

    pub fn record(&mut self, entry: ArchiveEntry) {
        self.entries.insert(entry.media_id.clone(), entry);
    }

    /// Remove an entry from the archive by media_id.
    /// Returns the removed entry if it existed.
    pub fn remove(&mut self, media_id: &str) -> Option<ArchiveEntry> {
        self.entries.remove(media_id)
    }

    /// Remove entries matching `pred`; returns how many were removed.
    pub fn remove_where(&mut self, pred: impl Fn(&ArchiveEntry) -> bool) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, entry| !pred(entry));
        before - self.entries.len()
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
