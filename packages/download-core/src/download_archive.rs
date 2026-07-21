//! Download Archive — Library Root–scoped completed media identities.
//!
//! Persisted at `{library_root}/.phlegethon/archive.json`. Probe marks archived
//! items; History (P3) reads the same store.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::storage::PHLEGETHON_DIR;

/// Filename for the persisted Download Archive index.
pub const ARCHIVE_FILENAME: &str = "archive.json";

const ARCHIVE_VERSION: u32 = 1;

/// A completed media identity recorded in the Download Archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub media_id: String,
    pub title: String,
    pub url: String,
    pub completed_at_unix_secs: u64,
    pub destination: PathBuf,
    pub job_id: String,
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

        if persisted.version != ARCHIVE_VERSION {
            return Err(format!(
                "unsupported archive version {} (expected {ARCHIVE_VERSION})",
                persisted.version
            ));
        }

        Ok(Self {
            entries: persisted.entries,
        })
    }

    pub fn save(&self, library_root: &Path) -> Result<(), String> {
        let dir = library_root.join(PHLEGETHON_DIR);
        fs::create_dir_all(&dir)
            .map_err(|e| format!("create {}: {e}", dir.display()))?;

        let path = dir.join(ARCHIVE_FILENAME);
        let tmp = dir.join(".archive.json.tmp");
        let persisted = PersistedArchive {
            version: ARCHIVE_VERSION,
            entries: self.entries.clone(),
        };
        let raw = serde_json::to_string_pretty(&persisted)
            .map_err(|e| format!("serialize archive: {e}"))?;
        fs::write(&tmp, &raw).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        fs::rename(&tmp, &path).map_err(|e| {
            format!(
                "rename {} -> {}: {e}",
                tmp.display(),
                path.display()
            )
        })?;
        Ok(())
    }

    pub fn contains(&self, media_id: &str) -> bool {
        self.entries.contains_key(media_id)
    }

    pub fn record(&mut self, entry: ArchiveEntry) {
        self.entries.insert(entry.media_id.clone(), entry);
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
        });
        archive.save(&root).unwrap();

        let loaded = DownloadArchive::load(&root).unwrap();
        assert!(loaded.contains("yt-abc123"));
        assert_eq!(loaded.entries.get("yt-abc123").unwrap().title, "Example");
    }
}
