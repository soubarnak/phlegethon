//! Extractor Toolchain abstraction — primary test seam for probe-then-confirm.
//!
//! The Download Core owns the probe flow but delegates the actual extraction
//! to an `ExtractorToolchain` implementation.  In production this wraps a
//! yt-dlp sidecar subprocess; in tests a fake returns canned items.

use std::collections::HashMap;

/// A single quality/format available for a media item.
#[derive(Debug, Clone, PartialEq)]
pub struct Quality {
    /// Canonical identifier (e.g. "1080p", "720p", "best", "worst").
    pub id: String,
    /// Human-readable label displayed in the probe sheet.
    pub label: String,
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

/// The Extractor Toolchain — wraps yt-dlp (or a fake in tests).
pub trait ExtractorToolchain: Send + Sync {
    /// Probe a URL and return discovered items with available qualities.
    fn probe(&self, url: &str) -> Result<ProbeResult, String>;

    /// Download media for a Launch Site URL into `dest_dir` via the toolchain.
    ///
    /// `format` is a quality id from probe (e.g. `"720p"`, `"best"`) mapped to
    /// a yt-dlp `-f` selector by the production implementation.
    ///
    /// Default: not supported (clear HTTP jobs use the HTTP backend instead).
    fn download(
        &self,
        _url: &str,
        _format: &str,
        _dest_dir: &std::path::Path,
    ) -> Result<std::path::PathBuf, String> {
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
#[derive(Clone, Default)]
pub struct FakeExtractorToolchain {
    /// URL → (source_name, items)
    responses: HashMap<String, FakeProbeResponse>,
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
