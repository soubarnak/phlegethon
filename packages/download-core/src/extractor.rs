//! Extractor Toolchain abstraction — primary test seam for probe-then-confirm.
//!
//! The Download Core owns the probe flow but delegates the actual extraction
//! to an `ExtractorToolchain` implementation.  In production this wraps a
//! yt-dlp sidecar subprocess; in tests a fake returns canned items.
//!
//! # Stop-signal contract (Pause / Discard)
//!
//! Core runs [`ExtractorToolchain::download`] on a worker thread and polls the
//! Job Destination for progress until that thread returns. **Ignoring `stop`
//! stalls Pause and Discard** — the process loop cannot advance while the
//! worker is still running.
//!
//! - Core sets the same `AtomicBool` for **Pause** and **Discard** (job
//!   `pause_requested` / `JobSignalRegistry::signal_discard_stop`).
//! - Implementations **must** observe `stop` promptly (poll between work
//!   units, or hard-kill a subprocess) and return
//!   [`ExtractorDownloadOutcome::Stopped`] — not hang, not complete after
//!   stop was set.
//! - Reference: [`FakeExtractorToolchain`] (chunk loop) and
//!   `YtDlpExtractorToolchain` (`run_command_interruptible`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

/// A single quality/format available for a media item.
#[derive(Debug, Clone, PartialEq)]
pub struct Quality {
    /// Canonical identifier (e.g. "1080p", "720p", "best", "worst").
    pub id: String,
    /// Human-readable label displayed in the probe sheet.
    pub label: String,
    /// Exact or approximate size in bytes from the Extractor Toolchain
    /// (`filesize` / positive `filesize_approx`). `None` when unknown.
    pub size_bytes: Option<u64>,
}

/// A single media item discovered during probing.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaItem {
    /// Unique identifier within the probe result.
    pub id: String,
    /// Item title (e.g. video title).
    pub title: String,
    /// The specific URL for this item.
    pub url: String,
    /// Available qualities for this item.
    pub qualities: Vec<Quality>,
}

/// Result of probing a URL via the Extractor Toolchain.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeResult {
    /// The URL that was probed.
    pub url: String,
    /// Discovered items (videos, media entries).
    pub items: Vec<MediaItem>,
    /// Source name derived from the probe, used for batch/channel
    /// Job Destination subfolder naming.  `None` for single-video probes.
    pub source_name: Option<String>,
}

/// Result of an Extractor Toolchain download attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractorDownloadOutcome {
    /// Media file written successfully.
    Completed(std::path::PathBuf),
    /// Pause or Discard requested; transfer stopped without completing.
    Stopped,
}

/// The Extractor Toolchain — wraps yt-dlp (or a fake in tests).
pub trait ExtractorToolchain: Send + Sync {
    /// Probe a URL and return discovered items with available qualities.
    fn probe(&self, url: &str) -> Result<ProbeResult, String>;

    /// Download media for a Launch Site URL into `dest_dir` via the toolchain.
    ///
    /// `format` is a quality id from probe (e.g. `"720p"`, `"best"`) mapped to
    /// a yt-dlp `-f` selector by the production implementation.
    ///
    /// # Stop signal
    ///
    /// `stop` is set by Core when **Pause or Discard** is requested (same flag).
    /// Implementations **must** honor it promptly and return
    /// [`ExtractorDownloadOutcome::Stopped`]:
    /// - Fake / in-process: exit the download loop between chunks.
    /// - Production: kill the yt-dlp (or other) subprocess; do not wait for
    ///   natural completion after `stop` is set.
    ///
    /// Core's process loop blocks until this method returns. Ignoring `stop`
    /// leaves Pause/Discard hanging. See module-level stop-signal contract.
    ///
    /// Whether this toolchain can resume a partial download after relaunch.
    ///
    /// When `false`, Core marks interrupted extractor jobs Failed on relaunch
    /// instead of silently restarting from zero.
    fn supports_extractor_resume(&self) -> bool {
        false
    }

    /// Default: not supported (clear HTTP jobs use the HTTP backend instead).
    fn download(
        &self,
        _url: &str,
        _format: &str,
        _dest_dir: &std::path::Path,
        _stop: &AtomicBool,
    ) -> Result<ExtractorDownloadOutcome, String> {
        Err("extractor download not supported for this toolchain".into())
    }
}

// ---------------------------------------------------------------------------
// Fake Extractor Toolchain (for deterministic tests)
// ---------------------------------------------------------------------------

/// A fake extractor that returns canned probe results.
///
/// Maps URLs to a list of `(item_id, title, url, qualities)` tuples.
/// Unknown URLs return an error.
///
/// Optional slow-download mode writes in chunks and honors `stop` between
/// chunks so Core Pause/Discard tests can assert prompt interruptibility.
#[derive(Clone, Default)]
pub struct FakeExtractorToolchain {
    /// URL → (source_name, items)
    responses: HashMap<String, FakeProbeResponse>,
    /// When set, `download` sleeps between writes and checks `stop` each chunk.
    slow_download: Option<SlowDownloadConfig>,
    /// Extra sibling filenames written beside the final file before Completed
    /// (simulates yt-dlp leftover `.part` / `.ytdl` artifacts).
    orphan_temp_names: Vec<String>,
}

#[derive(Clone)]
struct SlowDownloadConfig {
    total_chunks: u32,
    chunk_delay: std::time::Duration,
    /// Shared counter so tests can wait until the download is in flight.
    chunks_written: std::sync::Arc<std::sync::Mutex<u32>>,
}

#[derive(Clone)]
struct FakeProbeResponse {
    source_name: Option<String>,
    items: Vec<MediaItem>,
}

impl FakeExtractorToolchain {
    /// Create an empty fake (all URLs fail).
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable a long download that stops when Pause/Discard sets `stop`.
    ///
    /// Returns a shared chunk counter so tests can wait until transfer is active.
    pub fn enable_slow_download(
        &mut self,
        total_chunks: u32,
        chunk_delay: std::time::Duration,
    ) -> std::sync::Arc<std::sync::Mutex<u32>> {
        let chunks_written = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        self.slow_download = Some(SlowDownloadConfig {
            total_chunks,
            chunk_delay,
            chunks_written: chunks_written.clone(),
        });
        chunks_written
    }

    /// Leave named temp siblings under `dest_dir` when download Completes.
    ///
    /// Used to regression-test Core cleanup of Extractor orphans (issue #42).
    pub fn leave_orphan_temps(&mut self, names: &[&str]) {
        self.orphan_temp_names = names.iter().map(|s| (*s).to_string()).collect();
    }

    /// Insert a single-item probe response.
    ///
    /// When `url` is probed, returns one item with the given qualities.
    pub fn insert(
        &mut self,
        url: &str,
        title: &str,
        item_url: &str,
        qualities: Vec<(&str, &str)>,
    ) {
        let item = MediaItem {
            id: "item-1".to_string(),
            title: title.to_string(),
            url: item_url.to_string(),
            qualities: qualities
                .into_iter()
                .map(|(id, label)| Quality {
                    id: id.to_string(),
                    label: label.to_string(),
                    size_bytes: None,
                })
                .collect(),
        };
        self.responses.insert(
            url.to_string(),
            FakeProbeResponse {
                source_name: None,
                items: vec![item],
            },
        );
    }

    /// Insert a single-item probe response with optional per-quality sizes.
    ///
    /// Each quality is `(id, label, size_bytes)`. Used to regression-test
    /// determinate Extractor progress when the toolchain knows a size (#43).
    pub fn insert_with_sizes(
        &mut self,
        url: &str,
        title: &str,
        item_url: &str,
        qualities: Vec<(&str, &str, Option<u64>)>,
    ) {
        let item = MediaItem {
            id: "item-1".to_string(),
            title: title.to_string(),
            url: item_url.to_string(),
            qualities: qualities
                .into_iter()
                .map(|(id, label, size_bytes)| Quality {
                    id: id.to_string(),
                    label: label.to_string(),
                    size_bytes: size_bytes.filter(|&n| n > 0),
                })
                .collect(),
        };
        self.responses.insert(
            url.to_string(),
            FakeProbeResponse {
                source_name: None,
                items: vec![item],
            },
        );
    }

    /// Insert a multi-item probe response (batch/channel).
    ///
    /// `source_name` is used for the Job Destination subfolder.
    pub fn insert_batch(
        &mut self,
        url: &str,
        source_name: &str,
        items: Vec<(&str, &str, &str, Vec<(&str, &str)>)>,
    ) {
        let media_items: Vec<MediaItem> = items
            .into_iter()
            .map(|(id, title, item_url, qualities)| MediaItem {
                id: id.to_string(),
                title: title.to_string(),
                url: item_url.to_string(),
                qualities: qualities
                    .into_iter()
                    .map(|(qid, label)| Quality {
                        id: qid.to_string(),
                        label: label.to_string(),
                        size_bytes: None,
                    })
                    .collect(),
            })
            .collect();
        self.responses.insert(
            url.to_string(),
            FakeProbeResponse {
                source_name: Some(source_name.to_string()),
                items: media_items,
            },
        );
    }
}

impl ExtractorToolchain for FakeExtractorToolchain {
    fn probe(&self, url: &str) -> Result<ProbeResult, String> {
        let response = self
            .responses
            .get(url)
            .ok_or_else(|| format!("fake extractor: no response configured for {url}"))?;
        Ok(ProbeResult {
            url: url.to_string(),
            items: response.items.clone(),
            source_name: response.source_name.clone(),
        })
    }

    fn download(
        &self,
        url: &str,
        format: &str,
        dest_dir: &std::path::Path,
        stop: &AtomicBool,
    ) -> Result<ExtractorDownloadOutcome, String> {
        if stop.load(Ordering::Acquire) {
            return Ok(ExtractorDownloadOutcome::Stopped);
        }

        std::fs::create_dir_all(dest_dir)
            .map_err(|e| format!("create extractor dest {dest_dir:?}: {e}"))?;

        if let Some(slow) = &self.slow_download {
            let path = dest_dir.join("extractor-partial.bin");
            std::fs::write(&path, b"")
                .map_err(|e| format!("create partial {path:?}: {e}"))?;

            for chunk in 0..slow.total_chunks {
                if stop.load(Ordering::Acquire) {
                    return Ok(ExtractorDownloadOutcome::Stopped);
                }
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .map_err(|e| format!("open partial {path:?}: {e}"))?;
                write!(file, "chunk:{chunk}:{url}:{format}\n")
                    .map_err(|e| format!("write partial {path:?}: {e}"))?;
                *slow.chunks_written.lock().unwrap() = chunk + 1;
                std::thread::sleep(slow.chunk_delay);
            }

            if stop.load(Ordering::Acquire) {
                return Ok(ExtractorDownloadOutcome::Stopped);
            }
            return Ok(ExtractorDownloadOutcome::Completed(path));
        }

        let filename = url::Url::parse(url)
            .ok()
            .and_then(|u| {
                u.path_segments()
                    .and_then(|mut s| s.next_back())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "download".to_string());

        let path = dest_dir.join(&filename);
        let body = format!("fake extractor download:{url}:{format}");
        std::fs::write(&path, body.as_bytes())
            .map_err(|e| format!("write extractor output {path:?}: {e}"))?;
        for name in &self.orphan_temp_names {
            let orphan = dest_dir.join(name);
            std::fs::write(&orphan, b"orphan partial")
                .map_err(|e| format!("write orphan temp {orphan:?}: {e}"))?;
        }
        Ok(ExtractorDownloadOutcome::Completed(path))
    }
}

// ---------------------------------------------------------------------------
// Selection types (user confirmation)
// ---------------------------------------------------------------------------

/// A single item selection: which item at which quality to download.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ItemSelection {
    /// The item ID from the probe result.
    pub item_id: String,
    /// The quality ID to download for this item.
    pub quality_id: String,
}

/// User confirmation payload after viewing probe results.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ProbeSelection {
    /// The URL that was probed (links back to the pending probe).
    pub url: String,
    /// Selected items and their chosen qualities.
    pub items: Vec<ItemSelection>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_extractor_returns_canned_single_item() {
        let mut fake = FakeExtractorToolchain::new();
        fake.insert(
            "https://example.com/video",
            "Test Video",
            "https://cdn.example.com/video.mp4",
            vec![
                ("1080p", "1080p (1920×1080)"),
                ("720p", "720p (1280×720)"),
            ],
        );

        let result = fake.probe("https://example.com/video").unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].title, "Test Video");
        assert_eq!(result.items[0].qualities.len(), 2);
        assert_eq!(result.items[0].url, "https://cdn.example.com/video.mp4");
        assert_eq!(result.source_name, None);
    }

    #[test]
    fn fake_extractor_returns_canned_batch() {
        let mut fake = FakeExtractorToolchain::new();
        fake.insert_batch(
            "https://youtube.com/playlist?list=ABC",
            "My Playlist",
            vec![
                ("vid-1", "Video One", "https://youtube.com/watch?v=1", vec![("1080p", "1080p")]),
                ("vid-2", "Video Two", "https://youtube.com/watch?v=2", vec![("720p", "720p")]),
            ],
        );

        let result = fake.probe("https://youtube.com/playlist?list=ABC").unwrap();
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.source_name, Some("My Playlist".to_string()));
        assert_eq!(result.items[0].title, "Video One");
        assert_eq!(result.items[1].title, "Video Two");
    }

    #[test]
    fn fake_extractor_fails_for_unknown_url() {
        let fake = FakeExtractorToolchain::new();
        let err = fake.probe("https://unknown.example.com/video").unwrap_err();
        assert!(err.contains("no response configured"), "error should mention missing config, got: {err}");
    }

    #[test]
    fn fake_extractor_supports_direct_single_item_url() {
        // A "clear single-file HTTP" URL that was classified as EnqueueDirect
        // should still be probable if needed — but normally it skips probe.
        // This test verifies the fake handles it if called.
        let mut fake = FakeExtractorToolchain::new();
        fake.insert(
            "https://cdn.example.com/video.mp4",
            "Direct Video",
            "https://cdn.example.com/video.mp4",
            vec![("best", "Best quality")],
        );
        let result = fake.probe("https://cdn.example.com/video.mp4").unwrap();
        assert_eq!(result.items[0].id, "item-1");
        assert_eq!(result.items[0].qualities[0].id, "best");
    }
}
