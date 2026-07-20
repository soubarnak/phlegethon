//! Real Yt-dlp Extractor Toolchain — subprocess sidecar for Probe-then-Confirm.
//!
//! Wraps yt-dlp as an app-managed subprocess, parses its JSON probe output
//! into [`ProbeResult`], and enforces the hard-fail policy: when the
//! dedicated extractor cannot resolve a URL (falls through to `generic`),
//! the probe returns an error instead of silently reporting success.
//!
//! Supports single-video and multi-item (`_type: "playlist"`) probes for
//! YouTube (playlist / channel / user tabs) and xHamster (video + user/creator
//! lists) where the upstream extractor provides them.
//!
//! ## CI / fixture path
//!
//! The existing [`FakeExtractorToolchain`](crate::FakeExtractorToolchain) in
//! `extractor.rs` serves as the deterministic test seam — no live network.
//! [`FakeYtDlpExtractor`] returns canned yt-dlp JSON through the same parser.
//! This module's unit tests use canned JSON strings.

use crate::extractor::{ExtractorToolchain, MediaItem, ProbeResult, Quality};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;


// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the yt-dlp sidecar subprocess.
///
/// # Production defaults
///
/// ```ignore
/// YtDlpConfig {
///     binary_path: PathBuf::from("yt-dlp"),
///     ffmpeg_dir: None,
///     has_js_runtime: false,
///     probe_timeout_secs: 30,
///     plugin_dirs: vec![],  // or with_sidecar_plugins(root)
/// }
/// ```
#[derive(Debug, Clone)]
pub struct YtDlpConfig {
    /// Path to the yt-dlp binary (or bare name on PATH).
    pub binary_path: PathBuf,
    /// Optional directory containing ffmpeg/ffprobe binaries.
    pub ffmpeg_dir: Option<PathBuf>,
    /// Whether a JS runtime (e.g., deno, bun, node) is available for
    /// yt-dlp-ejs. Required for full YouTube format support.
    pub has_js_runtime: bool,
    /// Timeout for probe operations (seconds). Default 30.
    pub probe_timeout_secs: u64,
    /// Extra directories passed to yt-dlp as `--plugin-dirs`.
    ///
    /// App-managed Extractor Plugins (luxuretv, xxxbp.tv) live under the
    /// sidecar `yt-dlp-plugins/` folder — see [`crate::plugin_sidecar`].
    pub plugin_dirs: Vec<PathBuf>,
}

impl Default for YtDlpConfig {
    fn default() -> Self {
        Self {
            binary_path: PathBuf::from("yt-dlp"),
            ffmpeg_dir: None,
            has_js_runtime: false,
            probe_timeout_secs: 30,
            plugin_dirs: Vec::new(),
        }
    }
}

impl YtDlpConfig {
    /// Attach app-managed Extractor Plugins from a sidecar root, if present.
    ///
    /// When `sidecar_root/yt-dlp-plugins` exists, it is appended to
    /// [`Self::plugin_dirs`] for `--plugin-dirs`.
    pub fn with_sidecar_plugins(mut self, sidecar_root: impl Into<PathBuf>) -> Self {
        if let Some(dirs) = crate::plugin_sidecar::plugin_dirs_path(&sidecar_root.into()) {
            self.plugin_dirs.push(dirs);
        }
        self
    }
}

// ---------------------------------------------------------------------------
// YtDlpExtractorToolchain
// ---------------------------------------------------------------------------

/// The production Extractor Toolchain — invokes yt-dlp as a sidecar process.
///
/// ## Probe flow
///
/// 1. Runs `yt-dlp -J --flat-playlist --no-warnings <url>`.
/// 2. Parses the JSON output (single video **or** playlist with `entries`).
/// 3. Validates that the resolved extractor is the **dedicated** one for
///    the Launch Site (e.g. `Youtube` / `YoutubePlaylist` for youtube.com,
///    `XHamster` / `XHamsterUser` for xhamster).  If the extractor
///    is `Generic` or mismatched, the probe **hard-fails**.
/// 4. Maps formats → [`Quality`] list (or default `best` for flat entries)
///    and produces a [`ProbeResult`].
///
/// ## Hard-fail policy
///
/// A URL that falls through to yt-dlp's `generic` extractor (or any
/// extractor not matching the expected Launch Site key) is treated as
/// **unsupported** — the probe returns `Err`.  This prevents silent
/// "supported" status for sites that yt-dlp cannot actually resolve.
pub struct YtDlpExtractorToolchain {
    config: YtDlpConfig,
    /// Maps known site domains to their expected extractor_key prefixes.
    /// e.g. "youtube.com" → "Youtube"
    expected_extractors: HashMap<&'static str, &'static [&'static str]>,
}

impl YtDlpExtractorToolchain {
    /// Create a new toolchain with the given config.
    pub fn new(config: YtDlpConfig) -> Self {
        let mut expected_extractors: HashMap<&'static str, &'static [&'static str]> =
            HashMap::new();

        // YouTube: primary + playlist + channel + tab extractors
        expected_extractors.insert("youtube.com", &["Youtube"]);
        expected_extractors.insert("www.youtube.com", &["Youtube"]);
        expected_extractors.insert("m.youtube.com", &["Youtube"]);
        expected_extractors.insert("youtu.be", &["Youtube"]);

        // xHamster
        expected_extractors.insert("xhamster.com", &["XHamster"]);
        expected_extractors.insert("www.xhamster.com", &["XHamster"]);
        expected_extractors.insert("xhamster.desi", &["XHamster"]);
        expected_extractors.insert("xhamster2.com", &["XHamster"]);
        expected_extractors.insert("xhamster3.com", &["XHamster"]);

        // luxuretv — Phlegethon Extractor Plugin (LuxureTVIE)
        expected_extractors.insert("luxuretv.com", &["LuxureTV"]);
        expected_extractors.insert("www.luxuretv.com", &["LuxureTV"]);
        expected_extractors.insert("en.luxuretv.com", &["LuxureTV"]);

        // xxxbp.tv — Phlegethon Extractor Plugin (XxxbpTVIE)
        expected_extractors.insert("xxxbp.tv", &["XxxbpTV"]);
        expected_extractors.insert("www.xxxbp.tv", &["XxxbpTV"]);

        Self {
            config,
            expected_extractors,
        }
    }

    /// Resolve host to expected extractor key prefixes.
    fn expected_extractor_for_url(&self, url: &str) -> Option<&'static [&'static str]> {
        let parsed = url::Url::parse(url).ok()?;
        let host = parsed.host_str()?;
        // Try exact host match first, then suffix match for subdomains
        if let Some(keys) = self.expected_extractors.get(host) {
            return Some(keys);
        }
        // Check parent domains (e.g. "music.youtube.com" → youtube.com)
        for (domain, keys) in &self.expected_extractors {
            if host.ends_with(domain) && host != *domain {
                // Must be a subdomain, e.g. "music.youtube.com" ends with ".youtube.com"
                if host.ends_with(&format!(".{domain}")) {
                    return Some(keys);
                }
            }
        }
        None
    }

    /// Check whether the resolved extractor key is expected for this URL.
    ///
    /// Returns `Ok(())` if the extractor matches, `Err` with a description
    /// if it falls through to generic or is unexpected.
    fn validate_extractor(
        &self,
        url: &str,
        extractor_key: &str,
    ) -> Result<(), String> {
        // If the extractor_key is "Generic" or "GenericAlias", that means
        // yt-dlp had no dedicated extractor — hard-fail.
        if extractor_key.eq_ignore_ascii_case("generic")
            || extractor_key.eq_ignore_ascii_case("genericalias")
        {
            return Err(format!(
                "unsupported Launch Site: yt-dlp resolved '{url}' via generic \
                 extractor — no dedicated extractor available"
            ));
        }

        // If we have a known site, check it matches at least one expected key.
        if let Some(expected) = self.expected_extractor_for_url(url) {
            let matched = expected
                .iter()
                .any(|prefix| extractor_key.starts_with(prefix));
            if !matched {
                return Err(format!(
                    "extractor mismatch for '{url}': \
                     expected one of {:?}, got '{extractor_key}'",
                    expected
                ));
            }
        }

        Ok(())
    }

    /// Build the argv for a yt-dlp probe invocation (test seam).
    ///
    /// `--flat-playlist` lists playlist/channel/user entries without fully
    /// extracting each video (Probe-then-Confirm lists items first; qualities
    /// per entry default to "best" until a dedicated per-item probe exists).
    /// `--plugin-dirs` is included when configured (Launch Site Extractor Plugins).
    pub(crate) fn probe_args(&self, url: &str) -> Vec<String> {
        let mut args = vec![
            "-J".to_string(),
            "--no-warnings".to_string(),
            "--flat-playlist".to_string(),
        ];
        for dir in &self.config.plugin_dirs {
            args.push("--plugin-dirs".to_string());
            args.push(dir.display().to_string());
        }
        args.push(url.to_string());
        args
    }

    /// Run yt-dlp with `-J` and return the raw JSON stdout.
    fn run_probe(&self, url: &str) -> Result<String, String> {
        let mut cmd = Command::new(&self.config.binary_path);
        for arg in self.probe_args(url) {
            cmd.arg(arg);
        }

        // Add ffmpeg dir to PATH if configured
        if let Some(ffmpeg_dir) = &self.config.ffmpeg_dir {
            cmd.env(
                "PATH",
                format!(
                    "{}:{}",
                    ffmpeg_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        }

        let output = cmd
            .output()
            .map_err(|e| format!("failed to execute yt-dlp: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("yt-dlp probe failed (exit {:?}): {stderr}", output.status.code()));
        }

        let stdout = String::from_utf8(output.stdout)
            .map_err(|e| format!("yt-dlp output is not valid UTF-8: {e}"))?;

        if stdout.trim().is_empty() {
            return Err("yt-dlp produced empty output for probe".into());
        }

        Ok(stdout)
    }

    /// Test-only alias for `parse_probe_result` — used by integration tests.
    pub fn parse_probe_result_for_test(
        &self,
        json: &str,
        url: &str,
    ) -> Result<ProbeResult, String> {
        self.parse_probe_result(json, url)
    }

    pub(crate) fn parse_probe_result(&self, json: &str, url: &str) -> Result<ProbeResult, String> {
        let info: YtDlpVideoInfo = serde_json::from_str(json)
            .map_err(|e| format!("failed to parse yt-dlp JSON: {e}"))?;

        // Validate extractor key
        self.validate_extractor(url, &info.extractor_key)?;

        let is_playlist = info
            .type_
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case("playlist"))
            || info.entries.is_some();

        if is_playlist {
            return self.parse_playlist_probe(&info, url);
        }

        // Determine source name (None for single-video probes)
        let source_name = info.playlist_title.clone();

        // Map formats → qualities
        let qualities = self.formats_to_qualities(&info.formats.unwrap_or_default());

        let prefix = item_id_prefix(&info.extractor_key);
        let item = MediaItem {
            id: format!("{prefix}-{}", info.id),
            title: info.title.clone(),
            url: info.original_url.unwrap_or_else(|| url.to_string()),
            qualities,
        };

        Ok(ProbeResult {
            url: url.to_string(),
            items: vec![item],
            source_name,
        })
    }

    /// Parse a multi-item playlist / channel / user probe result.
    fn parse_playlist_probe(
        &self,
        info: &YtDlpVideoInfo,
        url: &str,
    ) -> Result<ProbeResult, String> {
        let source_name = {
            let from_title = info.title.trim();
            if !from_title.is_empty() {
                Some(from_title.to_string())
            } else {
                info.playlist_title.clone()
            }
        };

        let prefix = item_id_prefix(&info.extractor_key);
        let mut items = Vec::new();

        for entry in info.entries.iter().flatten().flatten() {
            let Some(id) = entry.id.as_deref().filter(|s| !s.is_empty()) else {
                continue;
            };
            let title = entry
                .title
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(id)
                .to_string();
            let Some(item_url) = entry
                .url
                .clone()
                .or_else(|| entry.webpage_url.clone())
                .or_else(|| entry.original_url.clone())
                .filter(|s| !s.is_empty())
            else {
                continue;
            };

            let qualities = match &entry.formats {
                Some(formats) if !formats.is_empty() => self.formats_to_qualities(formats),
                _ => default_flat_qualities(),
            };

            items.push(MediaItem {
                id: format!("{prefix}-{id}"),
                title,
                url: item_url,
                qualities,
            });
        }

        if items.is_empty() {
            return Err(format!(
                "playlist probe for '{url}' returned no usable entries"
            ));
        }

        Ok(ProbeResult {
            url: url.to_string(),
            items,
            source_name,
        })
    }

    /// Map yt-dlp format entries to Quality items.
    ///
    /// Strategy:
    /// - Video formats (has height or vcodec != "none"): use resolution
    /// - Audio-only formats: use bitrate label
    /// - Combined audio+video: prefer those
    /// - Deduplicate by quality label if possible
    fn formats_to_qualities(&self, formats: &[YtDlpFormat]) -> Vec<Quality> {
        // Group by resolution/quality label for deduplication
        let mut seen_labels: Vec<String> = Vec::new();
        let mut result: Vec<Quality> = Vec::new();

        let has_video = |f: &YtDlpFormat| -> bool {
            f.vcodec.as_deref().map(|c| c != "none").unwrap_or(true)
        };
        let has_audio = |f: &YtDlpFormat| -> bool {
            f.acodec.as_deref().map(|c| c != "none").unwrap_or(true)
        };

        for format in formats {
            let label = self.format_label(format);

            // Skip if we already have this quality label
            if seen_labels.contains(&label) {
                continue;
            }

            // Determine quality id: use format_id, or a synthetic stable id
            let quality_id = if has_video(format) && has_audio(format) {
                // Combined format: use height-based id
                if let Some(h) = format.height {
                    format!("{}p", h)
                } else if let Some(res) = format.resolution.as_deref() {
                    res.replace('x', "p")
                } else {
                    format.format_id.clone()
                }
            } else if has_video(format) && !has_audio(format) {
                // Video only
                if let Some(h) = format.height {
                    format!("{}p-videoonly", h)
                } else {
                    format!("{}-videoonly", format.format_id)
                }
            } else if has_audio(format) {
                // Audio only
                if let Some(tbr) = format.tbr {
                    let kbps = (tbr / 1000.0).round() as u64;
                    format!("{}k-audio", kbps.max(1))
                } else {
                    format!("{}-audio", format.format_id)
                }
            } else {
                format.format_id.clone()
            };

            seen_labels.push(label.clone());
            result.push(Quality {
                id: quality_id,
                label,
            });
        }

        result
    }

    /// Build a human-readable label for a format entry.
    fn format_label(&self, format: &YtDlpFormat) -> String {
        let has_video = format.vcodec.as_deref().map(|c| c != "none").unwrap_or(true);
        let has_audio = format.acodec.as_deref().map(|c| c != "none").unwrap_or(true);

        let resolution_part = if let Some(h) = format.height {
            if let Some(w) = format.width {
                format!("{}p ({w}×{h})", h)
            } else {
                format!("{}p", h)
            }
        } else if let Some(res) = &format.resolution {
            res.clone()
        } else if let Some(note) = &format.format_note {
            note.clone()
        } else if let Some(tbr) = format.tbr {
            let kbps = (tbr / 1000.0).round() as u64;
            format!("{} kb/s", kbps.max(1))
        } else {
            format.format_id.clone()
        };

        let suffix = match (has_video, has_audio) {
            (true, true) => String::new(),
            (true, false) => " (video only)".to_string(),
            (false, true) => " (audio only)".to_string(),
            (false, false) => String::new(),
        };

        format!("{resolution_part}{suffix}")
    }
}

impl ExtractorToolchain for YtDlpExtractorToolchain {
    fn probe(&self, url: &str) -> Result<ProbeResult, String> {
        // Validate URL first
        url::Url::parse(url)
            .map_err(|e| format!("invalid URL '{url}': {e}"))?;

        // Run yt-dlp
        let json = self.run_probe(url)?;

        // Parse into ProbeResult
        let result = self.parse_probe_result(&json, url)?;

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// yt-dlp JSON schema (deserialization structs)
// ---------------------------------------------------------------------------

/// Top-level yt-dlp JSON output for `-J` / `--dump-single-json`.
///
/// Covers both single-video and playlist (`_type: "playlist"`) responses.
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
struct YtDlpVideoInfo {
    /// YouTube video ID (or site-specific ID / playlist id).
    id: String,
    /// Video title or playlist/channel title.
    #[serde(default)]
    title: String,
    /// Short extractor name (e.g. "youtube").
    #[serde(default)]
    extractor: String,
    /// Capitalised extractor key (e.g. "Youtube", "YoutubePlaylist").
    #[serde(default)]
    extractor_key: String,
    /// yt-dlp result type: `"video"`, `"playlist"`, `"url"`, etc.
    #[serde(default, rename = "_type")]
    type_: Option<String>,
    /// Available formats (may be absent for flat playlist entries).
    #[serde(default)]
    formats: Option<Vec<YtDlpFormat>>,
    /// Playlist / channel / user entries (null slots = deleted / private).
    #[serde(default)]
    entries: Option<Vec<Option<YtDlpEntry>>>,
    /// The webpage URL.
    #[serde(default)]
    webpage_url: String,
    /// The originally requested URL.
    #[serde(default)]
    original_url: Option<String>,
    /// Playlist title if this came from a playlist/ channel.
    #[serde(default)]
    playlist_title: Option<String>,
    /// Playlist index within a playlist.
    #[serde(default)]
    playlist_index: Option<u64>,
    /// Duration in seconds.
    #[serde(default)]
    duration: Option<f64>,
    /// Thumbnail URL.
    #[serde(default)]
    thumbnail: Option<String>,
    /// Description text.
    #[serde(default)]
    description: Option<String>,
    /// Upload date (YYYYMMDD).
    #[serde(default)]
    upload_date: Option<String>,
    /// Uploader name.
    #[serde(default)]
    uploader: Option<String>,
}

/// A single entry inside a yt-dlp playlist JSON (`entries` array).
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
struct YtDlpEntry {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    webpage_url: Option<String>,
    #[serde(default)]
    original_url: Option<String>,
    #[serde(default)]
    formats: Option<Vec<YtDlpFormat>>,
    #[serde(default)]
    ie_key: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
}

/// Stable MediaItem id prefix for a Launch Site extractor family.
fn item_id_prefix(extractor_key: &str) -> &'static str {
    if extractor_key.starts_with("XHamster") {
        "xh"
    } else if extractor_key.starts_with("Youtube") {
        "yt"
    } else if extractor_key.starts_with("LuxureTV") {
        "lux"
    } else if extractor_key.starts_with("XxxbpTV") {
        "xbp"
    } else {
        "item"
    }
}

/// Default quality list for flat playlist entries (no formats yet).
fn default_flat_qualities() -> Vec<Quality> {
    vec![Quality {
        id: "best".to_string(),
        label: "Best available".to_string(),
    }]
}

/// A single format entry from yt-dlp's format list.
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
struct YtDlpFormat {
    /// Canonical format identifier.
    format_id: String,
    /// Short note from yt-dlp (e.g. "1080p", "medium").
    #[serde(default)]
    format_note: Option<String>,
    /// File extension.
    #[serde(default)]
    ext: Option<String>,
    /// Video height in pixels (if video stream).
    #[serde(default)]
    height: Option<u64>,
    /// Video width in pixels.
    #[serde(default)]
    width: Option<u64>,
    /// Total bitrate in bps.
    #[serde(default)]
    tbr: Option<f64>,
    /// Video codec name, "none" if no video.
    #[serde(default)]
    vcodec: Option<String>,
    /// Audio codec name, "none" if no audio.
    #[serde(default)]
    acodec: Option<String>,
    /// Resolution string (e.g. "1920x1080").
    #[serde(default)]
    resolution: Option<String>,
    /// Download URL for this format (may be None for some merged formats).
    #[serde(default)]
    url: Option<String>,
    /// Approximate filesize in bytes.
    #[serde(default)]
    filesize: Option<u64>,
    /// Approximate filesize (when not known precisely).
    #[serde(default)]
    filesize_approx: Option<u64>,
    /// Audio sampling rate.
    #[serde(default)]
    asr: Option<u64>,
    /// Frames per second.
    #[serde(default)]
    fps: Option<f64>,
    /// Number of audio channels.
    #[serde(default)]
    audio_channels: Option<u64>,
}

// ---------------------------------------------------------------------------
// Fixture helpers (canned yt-dlp JSON for tests)
// ---------------------------------------------------------------------------

/// Returns a canned yt-dlp JSON response for a YouTube single video.
///
/// Matches what `yt-dlp -J https://www.youtube.com/watch?v=dQw4w9WgXcQ`
/// returns — a typical Rick Astley video with several quality options.
#[cfg(test)]
pub(crate) fn canned_youtube_probe_json() -> &'static str {
    r#"{
  "id": "dQw4w9WgXcQ",
  "title": "Rick Astley - Never Gonna Give You Up (Official Music Video)",
  "extractor": "youtube",
  "extractor_key": "Youtube",
  "webpage_url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
  "original_url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
  "duration": 212.0,
  "thumbnail": "https://i.ytimg.com/vi/dQw4w9WgXcQ/maxresdefault.jpg",
  "uploader": "Rick Astley",
  "upload_date": "20091025",
  "description": "The official video for 'Never Gonna Give You Up'.",
  "formats": [
    {
      "format_id": "18",
      "format_note": "360p",
      "ext": "mp4",
      "height": 360,
      "width": 640,
      "tbr": 250000.0,
      "vcodec": "avc1.42001E",
      "acodec": "mp4a.40.2",
      "resolution": "640x360",
      "url": "https://r1.example.com/video18.mp4",
      "filesize": 7510000,
      "fps": 30.0,
      "audio_channels": 2
    },
    {
      "format_id": "22",
      "format_note": "720p",
      "ext": "mp4",
      "height": 720,
      "width": 1280,
      "tbr": 1150000.0,
      "vcodec": "avc1.64001F",
      "acodec": "mp4a.40.2",
      "resolution": "1280x720",
      "url": "https://r1.example.com/video22.mp4",
      "filesize": 28100000,
      "fps": 30.0,
      "audio_channels": 2
    },
    {
      "format_id": "137",
      "format_note": "1080p",
      "ext": "mp4",
      "height": 1080,
      "width": 1920,
      "tbr": 4500000.0,
      "vcodec": "avc1.640028",
      "acodec": "none",
      "resolution": "1920x1080",
      "url": "https://r1.example.com/video137.mp4",
      "filesize": 87000000,
      "fps": 30.0,
      "audio_channels": null
    },
    {
      "format_id": "140",
      "format_note": "medium",
      "ext": "m4a",
      "height": null,
      "width": null,
      "tbr": 130000.0,
      "vcodec": "none",
      "acodec": "mp4a.40.2",
      "resolution": null,
      "url": "https://r1.example.com/audio140.m4a",
      "filesize": 3500000,
      "fps": null,
      "audio_channels": 2
    },
    {
      "format_id": "247",
      "format_note": "720p (60fps)",
      "ext": "webm",
      "height": 720,
      "width": 1280,
      "tbr": 2000000.0,
      "vcodec": "vp9",
      "acodec": "none",
      "resolution": "1280x720",
      "url": "https://r1.example.com/video247.webm",
      "filesize": 42000000,
      "fps": 60.0,
      "audio_channels": null
    },
    {
      "format_id": "251",
      "format_note": "medium",
      "ext": "webm",
      "height": null,
      "width": null,
      "tbr": 160000.0,
      "vcodec": "none",
      "acodec": "opus",
      "resolution": null,
      "url": "https://r1.example.com/audio251.webm",
      "filesize": 4100000,
      "fps": null,
      "audio_channels": 2
    }
  ]
}"#
}

/// Returns a canned yt-dlp JSON for a URL that falls through to generic extractor.
#[cfg(test)]
pub(crate) fn canned_generic_probe_json() -> &'static str {
    r#"{
  "id": "unknown",
  "title": "Generic Fallback Video",
  "extractor": "generic",
  "extractor_key": "Generic",
  "webpage_url": "https://example.com/video/12345",
  "original_url": "https://example.com/video/12345",
  "formats": [
    {
      "format_id": "default",
      "format_note": "default",
      "ext": "mp4",
      "height": 720,
      "width": 1280,
      "tbr": 1000000.0,
      "vcodec": "avc1",
      "acodec": "mp4a",
      "resolution": "1280x720",
      "url": "https://cdn.example.com/video.mp4",
      "filesize": 50000000
    }
  ]
}"#
}

/// Canned yt-dlp JSON as produced by the LuxureTV Extractor Plugin.
#[cfg(test)]
pub(crate) fn canned_luxuretv_probe_json() -> &'static str {
    r#"{
  "id": "16982",
  "title": "Sample LuxureTV Video",
  "extractor": "LuxureTV",
  "extractor_key": "LuxureTV",
  "webpage_url": "https://www.luxuretv.com/videos/sample-video-16982.html",
  "original_url": "https://www.luxuretv.com/videos/sample-video-16982.html",
  "duration": 420.0,
  "formats": [
    {
      "format_id": "480p",
      "format_note": "480p",
      "ext": "mp4",
      "height": 480,
      "width": 854,
      "tbr": 800000.0,
      "vcodec": "avc1",
      "acodec": "mp4a",
      "resolution": "854x480",
      "url": "https://cdn.luxuretv.com/videos/16982_480.mp4"
    },
    {
      "format_id": "720p",
      "format_note": "720p",
      "ext": "mp4",
      "height": 720,
      "width": 1280,
      "tbr": 1600000.0,
      "vcodec": "avc1",
      "acodec": "mp4a",
      "resolution": "1280x720",
      "url": "https://cdn.luxuretv.com/videos/16982_720.mp4"
    },
    {
      "format_id": "1080p",
      "format_note": "1080p",
      "ext": "mp4",
      "height": 1080,
      "width": 1920,
      "tbr": 3200000.0,
      "vcodec": "avc1",
      "acodec": "mp4a",
      "resolution": "1920x1080",
      "url": "https://cdn.luxuretv.com/videos/16982_1080.mp4"
    }
  ]
}"#
}

/// Canned yt-dlp JSON as produced by the XxxbpTV Extractor Plugin.
#[cfg(test)]
pub(crate) fn canned_xxxbp_probe_json() -> &'static str {
    r#"{
  "id": "12345",
  "title": "Sample XXXBP Video",
  "extractor": "XxxbpTV",
  "extractor_key": "XxxbpTV",
  "webpage_url": "https://www.xxxbp.tv/v/12345",
  "original_url": "https://www.xxxbp.tv/v/12345",
  "duration": 300.0,
  "formats": [
    {
      "format_id": "360p",
      "format_note": "360p",
      "ext": "mp4",
      "height": 360,
      "width": 640,
      "tbr": 500000.0,
      "vcodec": "avc1",
      "acodec": "mp4a",
      "resolution": "640x360",
      "url": "https://cdn.xxxbp.tv/media/12345_360.mp4"
    },
    {
      "format_id": "720p",
      "format_note": "720p",
      "ext": "mp4",
      "height": 720,
      "width": 1280,
      "tbr": 1500000.0,
      "vcodec": "avc1",
      "acodec": "mp4a",
      "resolution": "1280x720",
      "url": "https://cdn.xxxbp.tv/media/12345_720.mp4"
    }
  ]
}"#
}

/// Returns a canned yt-dlp JSON for a YouTube playlist entry (flat).
#[cfg(test)]
pub(crate) fn canned_youtube_playlist_item_json() -> &'static str {
    r#"{
  "id": "abc123def",
  "title": "Playlist Video Title",
  "extractor": "youtube",
  "extractor_key": "Youtube",
  "webpage_url": "https://www.youtube.com/watch?v=abc123def",
  "original_url": "https://www.youtube.com/watch?v=abc123def",
  "playlist_title": "My Awesome Playlist",
  "playlist_index": 1,
  "duration": 180.0,
  "formats": [
    {
      "format_id": "18",
      "format_note": "360p",
      "ext": "mp4",
      "height": 360,
      "width": 640,
      "tbr": 250000.0,
      "vcodec": "avc1.42001E",
      "acodec": "mp4a.40.2",
      "resolution": "640x360",
      "url": "https://r1.example.com/playlist_video.mp4",
      "filesize": 6000000,
      "fps": 30.0,
      "audio_channels": 2
    }
  ]
}"#
}

/// Canned yt-dlp `-J --flat-playlist` output for a YouTube playlist URL.
#[cfg(test)]
pub(crate) fn canned_youtube_playlist_json() -> &'static str {
    r#"{
  "_type": "playlist",
  "id": "PLabc123",
  "title": "My Awesome Playlist",
  "extractor": "youtube:playlist",
  "extractor_key": "YoutubePlaylist",
  "webpage_url": "https://www.youtube.com/playlist?list=PLabc123",
  "original_url": "https://www.youtube.com/playlist?list=PLabc123",
  "entries": [
    {
      "_type": "url",
      "id": "vidOne111",
      "title": "First Playlist Video",
      "url": "https://www.youtube.com/watch?v=vidOne111",
      "ie_key": "Youtube",
      "duration": 120.0
    },
    {
      "_type": "url",
      "id": "vidTwo222",
      "title": "Second Playlist Video",
      "url": "https://www.youtube.com/watch?v=vidTwo222",
      "ie_key": "Youtube",
      "duration": 200.0
    },
    null
  ]
}"#
}

/// Canned yt-dlp output for a YouTube channel tab (multi-item).
#[cfg(test)]
pub(crate) fn canned_youtube_channel_json() -> &'static str {
    r#"{
  "_type": "playlist",
  "id": "UCchannel123",
  "title": "Example Channel",
  "extractor": "youtube:tab",
  "extractor_key": "YoutubeTab",
  "webpage_url": "https://www.youtube.com/@ExampleChannel/videos",
  "original_url": "https://www.youtube.com/@ExampleChannel/videos",
  "entries": [
    {
      "_type": "url",
      "id": "chanVid001",
      "title": "Channel Upload One",
      "url": "https://www.youtube.com/watch?v=chanVid001",
      "ie_key": "Youtube"
    },
    {
      "_type": "url",
      "id": "chanVid002",
      "title": "Channel Upload Two",
      "url": "https://www.youtube.com/watch?v=chanVid002",
      "ie_key": "Youtube"
    }
  ]
}"#
}

/// Canned yt-dlp output for a YouTube user URL (multi-item).
#[cfg(test)]
pub(crate) fn canned_youtube_user_json() -> &'static str {
    r#"{
  "_type": "playlist",
  "id": "UCUserUploads",
  "title": "ExampleUser",
  "extractor": "youtube:tab",
  "extractor_key": "YoutubeTab",
  "webpage_url": "https://www.youtube.com/user/ExampleUser",
  "original_url": "https://www.youtube.com/user/ExampleUser",
  "entries": [
    {
      "_type": "url",
      "id": "userVid001",
      "title": "User Video One",
      "url": "https://www.youtube.com/watch?v=userVid001",
      "ie_key": "Youtube"
    }
  ]
}"#
}

/// Canned yt-dlp JSON for an xHamster single video with qualities.
#[cfg(test)]
pub(crate) fn canned_xhamster_probe_json() -> &'static str {
    r#"{
  "id": "xh-video-12345",
  "title": "Example xHamster Video",
  "extractor": "xhamster",
  "extractor_key": "XHamster",
  "webpage_url": "https://xhamster.com/videos/example-xh-video-12345",
  "original_url": "https://xhamster.com/videos/example-xh-video-12345",
  "duration": 600.0,
  "formats": [
    {
      "format_id": "240p",
      "format_note": "240p",
      "ext": "mp4",
      "height": 240,
      "width": 426,
      "tbr": 300000.0,
      "vcodec": "avc1",
      "acodec": "mp4a.40.2",
      "resolution": "426x240",
      "url": "https://cdn.xh.example/video_240.mp4"
    },
    {
      "format_id": "720p",
      "format_note": "720p",
      "ext": "mp4",
      "height": 720,
      "width": 1280,
      "tbr": 1500000.0,
      "vcodec": "avc1",
      "acodec": "mp4a.40.2",
      "resolution": "1280x720",
      "url": "https://cdn.xh.example/video_720.mp4"
    },
    {
      "format_id": "hls-1080",
      "format_note": "1080p",
      "ext": "mp4",
      "height": 1080,
      "width": 1920,
      "tbr": 4000000.0,
      "vcodec": "avc1",
      "acodec": "mp4a.40.2",
      "resolution": "1920x1080",
      "url": "https://cdn.xh.example/hls/1080.m3u8"
    }
  ]
}"#
}

/// Canned yt-dlp flat playlist for an xHamster user/creator list.
#[cfg(test)]
pub(crate) fn canned_xhamster_user_json() -> &'static str {
    r#"{
  "_type": "playlist",
  "id": "example-creator",
  "title": "example-creator",
  "extractor": "xhamster:user",
  "extractor_key": "XHamsterUser",
  "webpage_url": "https://xhamster.com/creators/example-creator",
  "original_url": "https://xhamster.com/creators/example-creator",
  "entries": [
    {
      "_type": "url",
      "id": "xh-item-aaa",
      "title": "Creator Video A",
      "url": "https://xhamster.com/videos/creator-video-a-xh-item-aaa",
      "ie_key": "XHamster"
    },
    {
      "_type": "url",
      "id": "xh-item-bbb",
      "title": "Creator Video B",
      "url": "https://xhamster.com/videos/creator-video-b-xh-item-bbb",
      "ie_key": "XHamster"
    }
  ]
}"#
}

// ---------------------------------------------------------------------------
// Fake YtDlpProcess (for tests that don't want to go through FakeExtractorToolchain)
// ---------------------------------------------------------------------------

/// A fake version of YtDlpExtractorToolchain that returns canned JSON
/// without invoking a real subprocess.
///
/// Used in unit tests to verify JSON parsing, extractor key validation,
/// and quality mapping without a live yt-dlp binary.
pub struct FakeYtDlpExtractor {
    /// URL → canned JSON response string
    responses: HashMap<String, String>,
    /// URLs that should simulate a subprocess failure
    failing_urls: Vec<String>,
}

impl FakeYtDlpExtractor {
    pub fn new() -> Self {
        Self {
            responses: HashMap::new(),
            failing_urls: Vec::new(),
        }
    }

    /// Register a canned JSON response for a URL.
    pub fn insert(&mut self, url: &str, json: &str) {
        self.responses.insert(url.to_string(), json.to_string());
    }

    /// Mark a URL as failing (simulates yt-dlp crash / non-zero exit).
    pub fn insert_failure(&mut self, url: &str) {
        self.failing_urls.push(url.to_string());
    }
}

impl ExtractorToolchain for FakeYtDlpExtractor {
    fn probe(&self, url: &str) -> Result<ProbeResult, String> {
        // Simulate subprocess failure
        if self.failing_urls.iter().any(|f| f == url) {
            return Err(format!("yt-dlp process error for {url}"));
        }

        let json = self
            .responses
            .get(url)
            .ok_or_else(|| format!("no canned response for {url}"))?;

        let config = YtDlpConfig::default();
        let extractor = YtDlpExtractorToolchain::new(config);
        extractor.parse_probe_result(json, url)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- YtDlpConfig ----

    #[test]
    fn config_defaults_are_sensible() {
        let cfg = YtDlpConfig::default();
        assert_eq!(cfg.binary_path, PathBuf::from("yt-dlp"));
        assert!(!cfg.has_js_runtime);
        assert_eq!(cfg.probe_timeout_secs, 30);
        assert!(cfg.plugin_dirs.is_empty());
    }

    #[test]
    fn config_with_sidecar_plugins_attaches_plugin_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = tmp.path().join("yt-dlp-plugins");
        std::fs::create_dir_all(&dirs).unwrap();
        let cfg = YtDlpConfig::default().with_sidecar_plugins(tmp.path());
        assert_eq!(cfg.plugin_dirs, vec![dirs]);
    }

    // ---- Extractor key validation ----

    #[test]
    fn validate_youtube_extractor_passes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(
            extractor
                .validate_extractor("https://www.youtube.com/watch?v=abc", "Youtube")
                .is_ok()
        );
    }

    #[test]
    fn validate_youtube_extractor_with_playlist_key_passes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(
            extractor
                .validate_extractor(
                    "https://www.youtube.com/playlist?list=ABC",
                    "YoutubePlaylist",
                )
                .is_ok()
        );
    }

    #[test]
    fn validate_generic_extractor_hard_fails() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .validate_extractor("https://example.com/video/12345", "Generic")
            .unwrap_err();
        assert!(
            err.contains("generic extractor"),
            "error should mention generic, got: {err}"
        );
    }

    #[test]
    fn validate_generic_alias_extractor_hard_fails() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .validate_extractor("https://unknown.example.com/video", "GenericAlias")
            .unwrap_err();
        assert!(
            err.contains("generic extractor"),
            "error should mention generic, got: {err}"
        );
    }

    #[test]
    fn validate_xhamster_extractor_passes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(
            extractor
                .validate_extractor("https://xhamster.com/videos/example-12345", "XHamster")
                .is_ok()
        );
    }

    #[test]
    fn validate_unknown_extractor_for_known_site_fails() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .validate_extractor("https://www.youtube.com/watch?v=abc", "SomeUnknownExtractor")
            .unwrap_err();
        assert!(
            err.contains("extractor mismatch"),
            "error should mention mismatch, got: {err}"
        );
    }

    #[test]
    fn validate_unknown_site_with_unknown_extractor_passes() {
        // For sites we don't have in our expected map, any non-generic
        // extractor is accepted (it might be a plugin or custom extractor).
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(
            extractor
                .validate_extractor("https://example.com/video", "CustomExtractor")
                .is_ok()
        );
    }

    #[test]
    fn validate_unknown_site_with_generic_fails() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .validate_extractor("https://example.com/video", "Generic")
            .unwrap_err();
        assert!(
            err.contains("generic extractor"),
            "error should mention generic, got: {err}"
        );
    }

    #[test]
    fn youtu_be_short_url_expected_youtube() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(
            extractor
                .validate_extractor("https://youtu.be/abc123", "Youtube")
                .is_ok()
        );
    }

    // ---- YouTube JSON parsing ----

    #[test]
    fn parse_youtube_probe_returns_items_and_qualities() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = canned_youtube_probe_json();
        let result = extractor
            .parse_probe_result(json, "https://www.youtube.com/watch?v=dQw4w9WgXcQ")
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "yt-dQw4w9WgXcQ");
        assert_eq!(
            result.items[0].title,
            "Rick Astley - Never Gonna Give You Up (Official Music Video)"
        );
        assert_eq!(
            result.items[0].url,
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
        assert_eq!(result.source_name, None);
    }

    #[test]
    fn parse_youtube_probe_qualities_include_resolution_labels() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = canned_youtube_probe_json();
        let result = extractor
            .parse_probe_result(json, "https://www.youtube.com/watch?v=dQw4w9WgXcQ")
            .unwrap();

        let qualities = &result.items[0].qualities;

        // Should have combined audio+video formats (18=360p, 22=720p)
        let has_360p = qualities.iter().any(|q| q.id == "360p");
        let has_720p = qualities.iter().any(|q| q.id == "720p");
        assert!(has_360p, "should have 360p quality, got: {qualities:?}");
        assert!(has_720p, "should have 720p quality, got: {qualities:?}");

        // Should have video-only formats (137=1080p-videoonly, 247=720p-videoonly)
        let has_1080p_videoonly = qualities.iter().any(|q| q.id == "1080p-videoonly");
        let has_720p_60fps = qualities.iter().any(|q| q.id == "720p-videoonly");
        assert!(
            has_1080p_videoonly,
            "should have 1080p video-only, got: {qualities:?}"
        );
        assert!(
            has_720p_60fps,
            "should have 720p 60fps video-only, got: {qualities:?}"
        );

        // Should have audio-only format (140=130k-audio).
        // Format 251 has the same note "medium" so it gets deduplicated.
        let has_audio = qualities.iter().any(|q| q.id.ends_with("-audio"));
        assert!(
            has_audio,
            "should have audio quality, got: {qualities:?}"
        );

        // Verify quality labels are human-readable
        let q18 = qualities.iter().find(|q| q.id == "360p").unwrap();
        assert_eq!(q18.label, "360p (640×360)");

        let q137 = qualities.iter().find(|q| q.id == "1080p-videoonly").unwrap();
        assert_eq!(q137.label, "1080p (1920×1080) (video only)");

        let q140 = qualities.iter().find(|q| q.id == "130k-audio").unwrap();
        assert_eq!(q140.label, "medium (audio only)");
    }

    #[test]
    fn parse_youtube_probe_qualities_deduplicate_by_label() {
        // If two formats share the same label, only one quality should appear
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = canned_youtube_probe_json();
        let result = extractor
            .parse_probe_result(json, "https://www.youtube.com/watch?v=dQw4w9WgXcQ")
            .unwrap();

        let qualities = &result.items[0].qualities;

        // Each quality ID should be unique
        let mut ids: Vec<&str> = qualities.iter().map(|q| q.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(
            ids.len(),
            qualities.len(),
            "quality IDs should be unique, got: {ids:?}"
        );

        // Should have exactly 5 unique qualities from our 6 formats
        // (247 and 22 both resolve to 720p-videoonly label, dedupped)
        assert!(
            qualities.len() >= 4,
            "should have at least 4 unique quality entries, got {}: {qualities:?}",
            qualities.len()
        );
    }

    #[test]
    fn parse_youtube_probe_source_name_none_for_single_video() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = canned_youtube_probe_json();
        let result = extractor
            .parse_probe_result(json, "https://www.youtube.com/watch?v=dQw4w9WgXcQ")
            .unwrap();
        assert_eq!(result.source_name, None);
    }

    #[test]
    fn parse_playlist_item_includes_source_name() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = canned_youtube_playlist_item_json();
        let result = extractor
            .parse_probe_result(
                json,
                "https://www.youtube.com/watch?v=abc123def&list=PLabc",
            )
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(
            result.source_name,
            Some("My Awesome Playlist".to_string())
        );
    }

    // ---- YouTube multi-item (playlist / channel / user) ----

    #[test]
    fn parse_youtube_playlist_returns_multi_item_probe() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_youtube_playlist_json(),
                "https://www.youtube.com/playlist?list=PLabc123",
            )
            .unwrap();

        assert_eq!(result.items.len(), 2, "null entries must be skipped");
        assert_eq!(
            result.source_name,
            Some("My Awesome Playlist".to_string())
        );
        assert_eq!(result.items[0].id, "yt-vidOne111");
        assert_eq!(result.items[0].title, "First Playlist Video");
        assert_eq!(
            result.items[0].url,
            "https://www.youtube.com/watch?v=vidOne111"
        );
        assert_eq!(result.items[1].id, "yt-vidTwo222");
        // Flat playlist entries expose a selectable default quality
        assert!(
            result.items[0]
                .qualities
                .iter()
                .any(|q| q.id == "best"),
            "flat entries need a default quality, got: {:?}",
            result.items[0].qualities
        );
    }

    #[test]
    fn parse_youtube_channel_returns_multi_item_probe() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_youtube_channel_json(),
                "https://www.youtube.com/@ExampleChannel/videos",
            )
            .unwrap();

        assert_eq!(result.items.len(), 2);
        assert_eq!(result.source_name, Some("Example Channel".to_string()));
        assert_eq!(result.items[0].title, "Channel Upload One");
        assert_eq!(result.items[1].title, "Channel Upload Two");
    }

    #[test]
    fn parse_youtube_user_returns_multi_item_probe() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_youtube_user_json(),
                "https://www.youtube.com/user/ExampleUser",
            )
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.source_name, Some("ExampleUser".to_string()));
        assert_eq!(result.items[0].id, "yt-userVid001");
    }

    #[test]
    fn validate_youtube_tab_extractor_passes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(extractor
            .validate_extractor(
                "https://www.youtube.com/@ExampleChannel/videos",
                "YoutubeTab",
            )
            .is_ok());
    }

    // ---- xHamster ----

    #[test]
    fn parse_xhamster_single_video_returns_items_and_qualities() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_xhamster_probe_json(),
                "https://xhamster.com/videos/example-xh-video-12345",
            )
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.source_name, None);
        assert_eq!(result.items[0].id, "xh-xh-video-12345");
        assert_eq!(result.items[0].title, "Example xHamster Video");
        let qualities = &result.items[0].qualities;
        assert!(
            qualities.iter().any(|q| q.id == "240p"),
            "expected 240p, got: {qualities:?}"
        );
        assert!(
            qualities.iter().any(|q| q.id == "720p"),
            "expected 720p, got: {qualities:?}"
        );
        assert!(
            qualities.iter().any(|q| q.id == "1080p"),
            "expected 1080p, got: {qualities:?}"
        );
    }

    #[test]
    fn parse_xhamster_user_list_returns_multi_item_probe() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_xhamster_user_json(),
                "https://xhamster.com/creators/example-creator",
            )
            .unwrap();

        assert_eq!(result.items.len(), 2);
        assert_eq!(result.source_name, Some("example-creator".to_string()));
        assert_eq!(result.items[0].id, "xh-xh-item-aaa");
        assert_eq!(result.items[1].title, "Creator Video B");
        assert!(
            result.items[0]
                .qualities
                .iter()
                .any(|q| q.id == "best"),
            "flat user entries need default quality"
        );
    }

    #[test]
    fn validate_xhamster_user_extractor_passes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(extractor
            .validate_extractor(
                "https://xhamster.com/creators/example-creator",
                "XHamsterUser",
            )
            .is_ok());
    }

    #[test]
    fn parse_generic_extractor_hard_fails() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = canned_generic_probe_json();
        let err = extractor
            .parse_probe_result(json, "https://example.com/video/12345")
            .unwrap_err();
        assert!(
            err.contains("generic extractor") || err.contains("unsupported"),
            "generic probe should hard-fail, got: {err}"
        );
    }

    #[test]
    fn parse_luxuretv_plugin_probe_returns_items_and_qualities() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_luxuretv_probe_json(),
                "https://www.luxuretv.com/videos/sample-video-16982.html",
            )
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "lux-16982");
        assert_eq!(result.items[0].title, "Sample LuxureTV Video");
        assert_eq!(result.source_name, None);

        let qualities = &result.items[0].qualities;
        assert!(
            qualities.iter().any(|q| q.id == "480p"),
            "expected 480p, got: {qualities:?}"
        );
        assert!(
            qualities.iter().any(|q| q.id == "720p"),
            "expected 720p, got: {qualities:?}"
        );
        assert!(
            qualities.iter().any(|q| q.id == "1080p"),
            "expected 1080p, got: {qualities:?}"
        );
    }

    #[test]
    fn parse_xxxbp_plugin_probe_returns_items_and_qualities() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_xxxbp_probe_json(),
                "https://www.xxxbp.tv/v/12345",
            )
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "xbp-12345");
        assert_eq!(result.items[0].title, "Sample XXXBP Video");

        let qualities = &result.items[0].qualities;
        assert!(
            qualities.iter().any(|q| q.id == "360p"),
            "expected 360p, got: {qualities:?}"
        );
        assert!(
            qualities.iter().any(|q| q.id == "720p"),
            "expected 720p, got: {qualities:?}"
        );
    }

    #[test]
    fn luxuretv_generic_fallback_hard_fails_even_with_plugin_site() {
        // Plugin miss: yt-dlp fell through to Generic — must not count as supported.
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .parse_probe_result(
                canned_generic_probe_json(),
                "https://www.luxuretv.com/videos/sample-video-16982.html",
            )
            .unwrap_err();
        assert!(
            err.contains("generic") || err.contains("unsupported"),
            "plugin miss must hard-fail, got: {err}"
        );
    }

    #[test]
    fn xxxbp_wrong_extractor_hard_fails() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .validate_extractor("https://www.xxxbp.tv/v/12345", "SomeOther")
            .unwrap_err();
        assert!(
            err.contains("extractor mismatch"),
            "wrong IE must hard-fail, got: {err}"
        );
    }

    #[test]
    fn probe_args_include_plugin_dirs() {
        let config = YtDlpConfig {
            plugin_dirs: vec![PathBuf::from("/app/sidecar/yt-dlp-plugins")],
            ..YtDlpConfig::default()
        };
        let extractor = YtDlpExtractorToolchain::new(config);
        let args = extractor.probe_args("https://www.luxuretv.com/videos/x-1.html");
        assert!(args.contains(&"--plugin-dirs".to_string()));
        assert!(args.contains(&"/app/sidecar/yt-dlp-plugins".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("https://www.luxuretv.com/videos/x-1.html"));
    }

    #[test]
    fn probe_args_omit_plugin_dirs_when_empty() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let args = extractor.probe_args("https://www.youtube.com/watch?v=abc");
        assert!(!args.iter().any(|a| a == "--plugin-dirs"));
    }

    // ---- FakeYtDlpExtractor ----

    #[test]
    fn fake_ytdlp_extractor_returns_canned_probe() {
        let mut fake = FakeYtDlpExtractor::new();
        fake.insert(
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            canned_youtube_probe_json(),
        );

        let result = fake
            .probe("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
            .unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].qualities.len(), 5);
    }

    #[test]
    fn fake_ytdlp_extractor_fails_for_unknown_url() {
        let fake = FakeYtDlpExtractor::new();
        let err = fake
            .probe("https://www.youtube.com/watch?v=unknown")
            .unwrap_err();
        assert!(err.contains("no canned response"), "got: {err}");
    }

    #[test]
    fn fake_ytdlp_extractor_fails_for_configured_failure_url() {
        let mut fake = FakeYtDlpExtractor::new();
        fake.insert_failure("https://www.youtube.com/watch?v=fail");

        let err = fake
            .probe("https://www.youtube.com/watch?v=fail")
            .unwrap_err();
        assert!(
            err.contains("process error"),
            "should mention process error, got: {err}"
        );
    }

    // ---- Format label generation ----

    #[test]
    fn format_label_combined_audio_video() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let fmt = YtDlpFormat {
            format_id: "18".into(),
            format_note: Some("360p".into()),
            ext: Some("mp4".into()),
            height: Some(360),
            width: Some(640),
            tbr: Some(250000.0),
            vcodec: Some("avc1".into()),
            acodec: Some("mp4a".into()),
            resolution: Some("640x360".into()),
            url: Some("https://example.com/v.mp4".into()),
            filesize: Some(1000),
            filesize_approx: None,
            asr: None,
            fps: Some(30.0),
            audio_channels: Some(2),
        };
        let label = extractor.format_label(&fmt);
        assert_eq!(label, "360p (640×360)");
    }

    #[test]
    fn format_label_video_only() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let fmt = YtDlpFormat {
            format_id: "137".into(),
            format_note: Some("1080p".into()),
            ext: Some("mp4".into()),
            height: Some(1080),
            width: Some(1920),
            tbr: Some(4500000.0),
            vcodec: Some("avc1".into()),
            acodec: Some("none".into()),
            resolution: Some("1920x1080".into()),
            url: Some("https://example.com/v.mp4".into()),
            filesize: Some(87000000),
            filesize_approx: None,
            asr: None,
            fps: Some(30.0),
            audio_channels: None,
        };
        let label = extractor.format_label(&fmt);
        assert_eq!(label, "1080p (1920×1080) (video only)");
    }

    #[test]
    fn format_label_audio_only() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let fmt = YtDlpFormat {
            format_id: "140".into(),
            format_note: Some("medium".into()),
            ext: Some("m4a".into()),
            height: None,
            width: None,
            tbr: Some(130000.0),
            vcodec: Some("none".into()),
            acodec: Some("mp4a".into()),
            resolution: None,
            url: Some("https://example.com/a.m4a".into()),
            filesize: Some(3500000),
            filesize_approx: None,
            asr: None,
            fps: None,
            audio_channels: Some(2),
        };
        let label = extractor.format_label(&fmt);
        assert_eq!(label, "medium (audio only)");
    }

    #[test]
    fn format_label_unknown_codec_falls_back_to_format_id() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let fmt = YtDlpFormat {
            format_id: "999".into(),
            format_note: None,
            ext: None,
            height: None,
            width: None,
            tbr: None,
            vcodec: None,
            acodec: None,
            resolution: None,
            url: None,
            filesize: None,
            filesize_approx: None,
            asr: None,
            fps: None,
            audio_channels: None,
        };
        let label = extractor.format_label(&fmt);
        // Both vcodec and acodec are None -> treated as having video and audio
        // No height/width -> falls back to format_note (None) -> tbr (None) -> format_id
        assert_eq!(label, "999");
    }

    // ---- Edge cases ----

    #[test]
    fn formats_to_qualities_empty_list() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let qualities = extractor.formats_to_qualities(&[]);
        assert!(qualities.is_empty());
    }

    #[test]
    fn formats_to_qualities_deduplicates_duplicate_labels() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let formats = vec![
            YtDlpFormat {
                format_id: "a".into(),
                format_note: None,
                ext: Some("mp4".into()),
                height: Some(720),
                width: Some(1280),
                tbr: Some(1000000.0),
                vcodec: Some("avc1".into()),
                acodec: Some("mp4a".into()),
                resolution: Some("1280x720".into()),
                url: Some("https://example.com/a.mp4".into()),
                filesize: Some(1000),
                filesize_approx: None,
                asr: None,
                fps: Some(30.0),
                audio_channels: Some(2),
            },
            YtDlpFormat {
                format_id: "b".into(),
                format_note: None,
                ext: Some("webm".into()),
                height: Some(720),
                width: Some(1280),
                tbr: Some(2000000.0),
                vcodec: Some("vp9".into()),
                acodec: Some("opus".into()),
                resolution: Some("1280x720".into()),
                url: Some("https://example.com/b.webm".into()),
                filesize: Some(2000),
                filesize_approx: None,
                asr: None,
                fps: Some(60.0),
                audio_channels: Some(2),
            },
        ];
        let qualities = extractor.formats_to_qualities(&formats);
        // Both have the same label "720p (1280\u00d7720)" -> deduplicated to one entry
        assert_eq!(
            qualities.len(),
            1,
            "duplicate labels should deduplicate to 1, got {n}: {qualities:?}",
            n = qualities.len()
        );
        assert_eq!(qualities[0].id, "720p");
        assert_eq!(qualities[0].label, "720p (1280×720)");
    }

    #[test]
    fn invalid_url_returns_error() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor.probe("not-a-valid-url").unwrap_err();
        assert!(
            err.contains("invalid URL"),
            "should mention invalid URL, got: {err}"
        );
    }

    #[test]
    fn invalid_json_returns_parse_error() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .parse_probe_result("{invalid json}", "https://youtube.com/watch?v=abc")
            .unwrap_err();
        assert!(
            err.contains("failed to parse"),
            "should mention parse error, got: {err}"
        );
    }

    #[test]
    fn probe_on_unknown_site_with_generic_json_hard_fails() {
        // Even with a valid site we don't track, if the extractor is Generic
        // the probe should still fail.
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .parse_probe_result(
                canned_generic_probe_json(),
                "https://some-unknown-site.com/video/1",
            )
            .unwrap_err();
        assert!(
            err.contains("generic"),
            "generic extractor should always hard-fail, got: {err}"
        );
    }

    // ---- Expected extractor URL matching ----

    #[test]
    fn expected_extractor_for_youtube_com() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let keys = extractor
            .expected_extractor_for_url("https://www.youtube.com/watch?v=abc")
            .unwrap();
        assert!(keys.contains(&"Youtube"));
    }

    #[test]
    fn expected_extractor_for_youtu_be() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let keys = extractor
            .expected_extractor_for_url("https://youtu.be/abc")
            .unwrap();
        assert!(keys.contains(&"Youtube"));
    }

    #[test]
    fn expected_extractor_for_xhamster() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let keys = extractor
            .expected_extractor_for_url("https://xhamster.com/videos/example-12345")
            .unwrap();
        assert!(keys.contains(&"XHamster"));
    }

    #[test]
    fn expected_extractor_for_luxuretv_is_plugin() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let keys = extractor
            .expected_extractor_for_url("https://www.luxuretv.com/videos/sample-16982.html")
            .expect("luxuretv should map to plugin IE");
        assert!(keys.contains(&"LuxureTV"));
    }

    #[test]
    fn expected_extractor_for_xxxbp_tv_is_plugin() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let keys = extractor
            .expected_extractor_for_url("https://www.xxxbp.tv/v/12345")
            .expect("xxxbp.tv should map to plugin IE");
        assert!(keys.contains(&"XxxbpTV"));
    }

    #[test]
    fn validate_luxuretv_plugin_extractor_passes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(extractor
            .validate_extractor(
                "https://en.luxuretv.com/videos/sample-16982.html",
                "LuxureTV",
            )
            .is_ok());
    }

    #[test]
    fn validate_xxxbp_plugin_extractor_passes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(extractor
            .validate_extractor("https://xxxbp.tv/v/12345", "XxxbpTV")
            .is_ok());
    }

    #[test]
    fn expected_extractor_for_unknown_site_returns_none() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let keys = extractor
            .expected_extractor_for_url("https://example.com/file.zip");
        assert!(keys.is_none());
    }

    #[test]
    fn expected_extractor_for_youtube_subdomain() {
        // Subdomains like music.youtube.com should still match
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let keys = extractor
            .expected_extractor_for_url("https://music.youtube.com/playlist?list=ABC");
        assert!(
            keys.is_some(),
            "music.youtube.com should match youtube.com extractors"
        );
        let keys = keys.unwrap();
        assert!(keys.contains(&"Youtube"));
    }
}