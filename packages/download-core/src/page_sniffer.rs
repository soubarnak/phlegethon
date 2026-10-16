//! Core-native page sniffer — tier 3 of the non-Launch-Site fallback chain.
//!
//! When yt-dlp has no extractor for a page (or fails), Core fetches the HTML
//! itself and looks for media the page exposes in the open:
//!
//! - `og:video`, `og:video:secure_url`, `twitter:player:stream` meta tags
//! - `<video src>` and `<source src>` elements
//! - JSON-LD `VideoObject` `contentUrl` / `embedUrl`
//! - `<a href>` and `<link rel=alternate>` pointing at media files
//! - bare `.m3u8` / `.mp4` / `.webm` / `.mp3` URLs inside inline scripts
//!
//! The pure parser ([`sniff_html`]) has no I/O so it is tested against HTML
//! fixtures; [`HtmlPageSniffer`] wraps it with a HEAD probe and a capped GET
//! through the Core [`HttpBackend`].

use crate::extractor::{MediaFetchKind, MediaItem, Quality};
use crate::http::{HeadInfo, HttpBackend};
use crate::proxy_settings::ProxySettings;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::OnceLock;
use url::Url;

/// Maximum HTML bytes fetched for sniffing.
pub const MAX_SNIFF_BYTES: u64 = 2 * 1024 * 1024;

/// Browser-like User-Agent used for page fetches (many sites 403 bare clients).
pub const SNIFF_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/130.0 Safari/537.36";

/// Where on the page a media URL was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SniffSource {
    OgVideo,
    TwitterPlayerStream,
    VideoTag,
    SourceTag,
    JsonLdVideoObject,
    Anchor,
    LinkAlternate,
    InlineScript,
}

/// What kind of media a sniffed URL points at.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SniffedKind {
    /// Progressive file with this extension (mp4, webm, mp3, ...).
    DirectFile { ext: String },
    /// HLS playlist (`.m3u8`).
    Hls,
    /// DASH manifest (`.mpd`).
    Dash,
}

/// One media URL discovered on a page.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SniffedMedia {
    pub url: String,
    pub kind: SniffedKind,
    /// Quality hint from the markup (`label="720p"`, `size="1080"`, ...).
    pub label_hint: Option<String>,
    pub source: SniffSource,
}

/// Result of sniffing a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SniffOutcome {
    /// The URL serves a non-HTML body: download it as a file.
    Binary {
        content_type: String,
        content_length: Option<u64>,
        final_url: String,
        /// Filename from `Content-Disposition` when the server sent one.
        filename: Option<String>,
    },
    /// The URL is an HTML page; `media` may be empty.
    Page {
        title: Option<String>,
        media: Vec<SniffedMedia>,
    },
    /// Could not decide (no HEAD support, empty body, ...).
    Nothing,
}

/// Fetches and sniffs pages. Test seam: [`FakePageSniffer`].
pub trait PageSniffer: Send + Sync {
    /// HEAD-only check: is this URL a binary file or a page?
    fn sniff_head(&self, url: &str, proxy: &ProxySettings) -> Result<SniffOutcome, String>;
    /// HEAD then a capped GET; parses HTML for media URLs.
    fn sniff(&self, url: &str, proxy: &ProxySettings) -> Result<SniffOutcome, String>;
}

// ---------------------------------------------------------------------------
// Pure parsing
// ---------------------------------------------------------------------------

const MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "webm", "mkv", "mov", "avi", "flv", "ts", "mp3", "m4a", "aac", "ogg", "opus",
    "flac", "wav",
];

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid sniff regex"))
}

static META_RE: OnceLock<Regex> = OnceLock::new();
static VIDEO_SRC_RE: OnceLock<Regex> = OnceLock::new();
static SOURCE_TAG_RE: OnceLock<Regex> = OnceLock::new();
static ATTR_RE: OnceLock<Regex> = OnceLock::new();
static ANCHOR_RE: OnceLock<Regex> = OnceLock::new();
static LINK_ALT_RE: OnceLock<Regex> = OnceLock::new();
static JSONLD_RE: OnceLock<Regex> = OnceLock::new();
static SCRIPT_RE: OnceLock<Regex> = OnceLock::new();
static BARE_URL_RE: OnceLock<Regex> = OnceLock::new();
static TITLE_RE: OnceLock<Regex> = OnceLock::new();
static OG_TITLE_RE: OnceLock<Regex> = OnceLock::new();
static HEIGHT_RE: OnceLock<Regex> = OnceLock::new();
static HEIGHT_IN_URL_RE: OnceLock<Regex> = OnceLock::new();
static CD_FILENAME_RE: OnceLock<Regex> = OnceLock::new();

fn meta_re() -> &'static Regex {
    re(&META_RE, r#"(?is)<meta\b[^>]*>"#)
}
fn attr_re() -> &'static Regex {
    re(
        &ATTR_RE,
        r#"(?is)([a-zA-Z_:-]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+))"#,
    )
}

/// Parse `name="value"` attributes of one tag into a lowercase-keyed map.
fn attrs(tag: &str) -> HashMap<String, String> {
    attr_re()
        .captures_iter(tag)
        .map(|c| {
            let key = c[1].to_ascii_lowercase();
            let val = c
                .get(2)
                .or_else(|| c.get(3))
                .or_else(|| c.get(4))
                .map(|m| html_unescape(m.as_str()))
                .unwrap_or_default();
            (key, val)
        })
        .collect()
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn json_unescape(s: &str) -> String {
    s.replace("\\/", "/")
        .replace("\\u0026", "&")
        .replace("\\&", "&")
}

/// Classify a URL by its path extension; `None` when it is not media.
pub fn classify_media_url(url: &str) -> Option<SniffedKind> {
    let parsed = Url::parse(url).ok()?;
    let path = parsed.path().to_ascii_lowercase();
    let last = path.rsplit('/').next().unwrap_or("");
    let ext = last.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    if ext == "m3u8" {
        return Some(SniffedKind::Hls);
    }
    if ext == "mpd" {
        return Some(SniffedKind::Dash);
    }
    if MEDIA_EXTENSIONS.contains(&ext) {
        return Some(SniffedKind::DirectFile {
            ext: ext.to_string(),
        });
    }
    None
}

/// Resolve `candidate` against `base`, keeping only http(s) results.
fn resolve(base: &Url, candidate: &str) -> Option<String> {
    let trimmed = candidate.trim();
    if trimmed.is_empty() || trimmed.starts_with("data:") || trimmed.starts_with("blob:") {
        return None;
    }
    let joined = base.join(trimmed).ok()?;
    if joined.scheme() != "http" && joined.scheme() != "https" {
        return None;
    }
    Some(joined.to_string())
}

/// Hover previews / thumbnails are not the main media.
fn looks_like_preview(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("_preview.")
        || lower.contains("/preview/")
        || lower.contains("/previews/")
        || lower.contains("/thumb")
}

/// Extract media URLs from an HTML document. Pure; no network.
pub fn sniff_html(base: &Url, html: &str) -> Vec<SniffedMedia> {
    let mut out: Vec<SniffedMedia> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut push = |url: String, kind: SniffedKind, label: Option<String>, source: SniffSource| {
        if looks_like_preview(&url) {
            return;
        }
        if seen.insert(url.clone()) {
            out.push(SniffedMedia {
                url,
                kind,
                label_hint: label,
                source,
            });
        }
    };

    // 1. <meta property="og:video" ...> / twitter:player:stream
    for m in meta_re().find_iter(html) {
        let a = attrs(m.as_str());
        let key = a
            .get("property")
            .or_else(|| a.get("name"))
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let Some(content) = a.get("content") else {
            continue;
        };
        let source = match key.as_str() {
            "og:video" | "og:video:url" | "og:video:secure_url" => SniffSource::OgVideo,
            "twitter:player:stream" => SniffSource::TwitterPlayerStream,
            _ => continue,
        };
        if let Some(url) = resolve(base, content) {
            if let Some(kind) = classify_media_url(&url) {
                push(url, kind, None, source);
            }
        }
    }

    // 2. <video src=...> and <source src=... label=...>
    for m in re(&VIDEO_SRC_RE, r#"(?is)<video\b[^>]*>"#).find_iter(html) {
        let a = attrs(m.as_str());
        if let Some(src) = a.get("src").or_else(|| a.get("data-src")) {
            if let Some(url) = resolve(base, src) {
                let kind = classify_media_url(&url).unwrap_or(SniffedKind::DirectFile {
                    ext: "mp4".to_string(),
                });
                push(url, kind, None, SniffSource::VideoTag);
            }
        }
    }
    for m in re(&SOURCE_TAG_RE, r#"(?is)<source\b[^>]*>"#).find_iter(html) {
        let a = attrs(m.as_str());
        let ty = a
            .get("type")
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if ty.starts_with("image/") {
            continue;
        }
        let Some(src) = a.get("src").or_else(|| a.get("data-src")) else {
            continue;
        };
        let Some(url) = resolve(base, src) else {
            continue;
        };
        let kind = match classify_media_url(&url) {
            Some(k) => k,
            None if ty.contains("mpegurl") => SniffedKind::Hls,
            None if ty.starts_with("video/") || ty.starts_with("audio/") => {
                SniffedKind::DirectFile {
                    ext: ty.rsplit('/').next().unwrap_or("mp4").to_string(),
                }
            }
            None => continue,
        };
        let label = a
            .get("label")
            .or_else(|| a.get("title"))
            .or_else(|| a.get("size"))
            .or_else(|| a.get("res"))
            .or_else(|| a.get("data-quality"))
            .cloned();
        push(url, kind, label, SniffSource::SourceTag);
    }

    // 3. JSON-LD VideoObject
    for m in re(
        &JSONLD_RE,
        r#"(?is)<script\b[^>]*type\s*=\s*["']application/ld\+json["'][^>]*>(.*?)</script>"#,
    )
    .captures_iter(html)
    {
        let body = &m[1];
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(body.trim()) {
            let mut urls = Vec::new();
            collect_jsonld_video_urls(&value, &mut urls);
            for u in urls {
                if let Some(url) = resolve(base, &u) {
                    if let Some(kind) = classify_media_url(&url) {
                        push(url, kind, None, SniffSource::JsonLdVideoObject);
                    }
                }
            }
        }
    }

    // 4. <a href> and <link rel=alternate> to media files
    for m in re(&ANCHOR_RE, r#"(?is)<a\b[^>]*>"#).find_iter(html) {
        let a = attrs(m.as_str());
        let Some(href) = a.get("href") else { continue };
        let Some(url) = resolve(base, href) else {
            continue;
        };
        if let Some(kind) = classify_media_url(&url) {
            let label = a.get("title").or_else(|| a.get("data-quality")).cloned();
            push(url, kind, label, SniffSource::Anchor);
        }
    }
    for m in re(&LINK_ALT_RE, r#"(?is)<link\b[^>]*>"#).find_iter(html) {
        let a = attrs(m.as_str());
        let rel = a
            .get("rel")
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if !rel.contains("alternate") && !rel.contains("enclosure") {
            continue;
        }
        let Some(href) = a.get("href") else { continue };
        let Some(url) = resolve(base, href) else {
            continue;
        };
        if let Some(kind) = classify_media_url(&url) {
            push(url, kind, None, SniffSource::LinkAlternate);
        }
    }

    // 5. Bare media URLs inside inline scripts
    for m in re(&SCRIPT_RE, r#"(?is)<script\b[^>]*>(.*?)</script>"#).captures_iter(html) {
        let body = json_unescape(&m[1]);
        for u in re(
            &BARE_URL_RE,
            r#"(?i)(https?:)?//[^\s"'<>\\()]+?\.(?:m3u8|mpd|mp4|m4v|webm|mkv|mov|mp3|m4a|aac|ogg|opus|flac|wav)(?:\?[^\s"'<>\\()]*)?"#,
        )
        .find_iter(&body)
        {
            let raw = u.as_str();
            let candidate = if raw.starts_with("//") {
                format!("{}:{}", base.scheme(), raw)
            } else {
                raw.to_string()
            };
            if let Some(url) = resolve(base, &candidate) {
                if let Some(kind) = classify_media_url(&url) {
                    push(url, kind, None, SniffSource::InlineScript);
                }
            }
        }
    }

    out
}

fn collect_jsonld_video_urls(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_jsonld_video_urls(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            let is_video = map
                .get("@type")
                .map(|t| match t {
                    serde_json::Value::String(s) => s.eq_ignore_ascii_case("VideoObject"),
                    serde_json::Value::Array(a) => a.iter().any(|s| {
                        s.as_str()
                            .is_some_and(|s| s.eq_ignore_ascii_case("VideoObject"))
                    }),
                    _ => false,
                })
                .unwrap_or(false);
            if is_video {
                for key in ["contentUrl", "embedUrl"] {
                    if let Some(serde_json::Value::String(u)) = map.get(key) {
                        out.push(u.clone());
                    }
                }
            }
            for (key, child) in map {
                if key == "@graph" || child.is_object() || child.is_array() {
                    collect_jsonld_video_urls(child, out);
                }
            }
        }
        _ => {}
    }
}

/// Page `<title>` or `og:title`.
pub fn page_title(html: &str) -> Option<String> {
    for m in re(&OG_TITLE_RE, r#"(?is)<meta\b[^>]*>"#).find_iter(html) {
        let a = attrs(m.as_str());
        let key = a
            .get("property")
            .or_else(|| a.get("name"))
            .map(|s| s.to_ascii_lowercase());
        if key.as_deref() == Some("og:title") {
            if let Some(c) = a.get("content").map(|s| s.trim()).filter(|s| !s.is_empty()) {
                return Some(c.to_string());
            }
        }
    }
    re(&TITLE_RE, r#"(?is)<title[^>]*>(.*?)</title>"#)
        .captures(html)
        .map(|c| html_unescape(c[1].trim()))
        .filter(|t| !t.is_empty())
}

/// Height in pixels from a label (`720p`, `HD 1080`) or URL (`_720.mp4`, `/480p/`).
pub fn height_hint(label: Option<&str>, url: &str) -> Option<u32> {
    if let Some(label) = label {
        if let Some(c) = re(&HEIGHT_RE, r"(?i)(\d{3,4})\s*p?\b").captures(label) {
            if let Ok(h) = c[1].parse::<u32>() {
                if (144..=4320).contains(&h) {
                    return Some(h);
                }
            }
        }
    }
    re(
        &HEIGHT_IN_URL_RE,
        r"(?i)(?:^|[^0-9])(\d{3,4})p?(?:[._/-]|$)",
    )
    .captures_iter(url.rsplit('/').next().unwrap_or(url))
    .filter_map(|c| c[1].parse::<u32>().ok())
    .find(|h| {
        matches!(
            h,
            144 | 240 | 360 | 480 | 540 | 720 | 1080 | 1440 | 2160 | 4320
        )
    })
}

/// Build a [`Quality`] from URL and markup hints.
pub fn quality_from_hints(url: &str, label_hint: Option<&str>, size_bytes: Option<u64>) -> Quality {
    let kind = classify_media_url(url);
    let container = match &kind {
        Some(SniffedKind::DirectFile { ext }) => Some(ext.clone()),
        Some(SniffedKind::Hls) => Some("m3u8".to_string()),
        Some(SniffedKind::Dash) => Some("mpd".to_string()),
        None => None,
    };
    match height_hint(label_hint, url) {
        Some(h) => Quality {
            id: format!("{h}p"),
            label: label_hint
                .map(str::to_string)
                .filter(|l| l.to_ascii_lowercase() != format!("{h}p"))
                .map(|l| format!("{h}p ({l})"))
                .unwrap_or_else(|| format!("{h}p")),
            size_bytes,
            container,
        },
        None => Quality {
            id: match &kind {
                Some(SniffedKind::Hls) | Some(SniffedKind::Dash) => "best".to_string(),
                _ => "file".to_string(),
            },
            label: match (&kind, label_hint) {
                (_, Some(l)) => l.to_string(),
                (Some(SniffedKind::Hls), None) => "HLS stream (best)".to_string(),
                (Some(SniffedKind::Dash), None) => "DASH stream (best)".to_string(),
                _ => "Original file".to_string(),
            },
            size_bytes,
            container,
        },
    }
}

fn stem_of(url: &str) -> String {
    let parsed = match Url::parse(url) {
        Ok(p) => p,
        Err(_) => return url.to_string(),
    };
    let path = parsed.path();
    let last = path.rsplit('/').next().unwrap_or("");
    let no_ext = last.rsplit_once('.').map(|(s, _)| s).unwrap_or(last);
    // Strip trailing quality markers: `_720p`, `-1080`, `.480`
    let stripped = re(
        &HEIGHT_IN_URL_RE,
        r"(?i)(?:^|[^0-9])(\d{3,4})p?(?:[._/-]|$)",
    )
    .replace_all(no_ext, "")
    .to_string();
    let stem = stripped.trim_matches(|c: char| c == '_' || c == '-' || c == '.');
    if stem.is_empty() {
        no_ext.to_string()
    } else {
        stem.to_string()
    }
}

fn media_identity(page_url: &str, stem: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(page_url.as_bytes());
    hasher.update(b"\n");
    hasher.update(stem.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("sniff-{hex}")
}

/// Turn sniffed URLs into [`MediaItem`]s.
///
/// Progressive variants that share a filename stem (`clip_480.mp4`,
/// `clip_720.mp4`) collapse into one item with several qualities. HLS/DASH
/// manifests become extractor items (yt-dlp + ffmpeg handle the stream) with
/// a single `best` quality; plain files are fetched by Core's HTTP backend.
pub fn media_items_from_sniff(
    page_url: &str,
    page_title: Option<&str>,
    media: Vec<SniffedMedia>,
) -> Vec<MediaItem> {
    let mut groups: Vec<(String, MediaFetchKind, Vec<(SniffedMedia, Quality)>)> = Vec::new();
    for m in media {
        let quality = quality_from_hints(&m.url, m.label_hint.as_deref(), None);
        let (stem, fetch_kind) = match m.kind {
            SniffedKind::DirectFile { .. } => (stem_of(&m.url), MediaFetchKind::DirectHttp),
            SniffedKind::Hls | SniffedKind::Dash => (m.url.clone(), MediaFetchKind::Extractor),
        };
        match groups
            .iter_mut()
            .find(|(s, k, _)| *s == stem && *k == fetch_kind)
        {
            Some((_, _, entries)) => {
                if !entries.iter().any(|(_, q)| q.id == quality.id) {
                    entries.push((m, quality));
                }
            }
            None => groups.push((stem, fetch_kind, vec![(m, quality)])),
        }
    }

    let total = groups.len();
    groups
        .into_iter()
        .enumerate()
        .map(|(idx, (stem, fetch_kind, mut entries))| {
            entries.sort_by(|a, b| {
                let ha = height_hint(None, &a.1.id).unwrap_or(0);
                let hb = height_hint(None, &b.1.id).unwrap_or(0);
                hb.cmp(&ha)
            });
            let title = match (page_title, total) {
                (Some(t), 1) => t.to_string(),
                (Some(t), _) => format!("{t} — {stem}"),
                (None, _) => stem.clone(),
            };
            // Each quality must map back to one URL on confirm; keep the URL of
            // the first (best) variant as the item URL and encode the others in
            // the quality id when they differ.
            let primary_url = entries[0].0.url.clone();
            let qualities = entries
                .iter()
                .map(|(m, q)| {
                    let mut q = q.clone();
                    if m.url != primary_url {
                        q.id = format!("{}@{}", q.id, m.url);
                    }
                    q
                })
                .collect();
            MediaItem {
                id: media_identity(page_url, &format!("{stem}#{idx}")),
                title,
                url: primary_url,
                qualities,
                archived: false,
                uploader: None,
                upload_date: None,
                fetch_kind,
                qualities_are_presets: false,
                referer: Some(page_url.to_string()),
            }
        })
        .collect()
}

/// Split a sniff quality id (`720p@https://cdn/x_720.mp4`) into `(quality, url)`.
pub fn split_quality_url(quality_id: &str) -> (&str, Option<&str>) {
    match quality_id.split_once('@') {
        Some((q, u)) if u.starts_with("http") => (q, Some(u)),
        _ => (quality_id, None),
    }
}

/// Whether a `Content-Type` means "not an HTML page".
pub fn is_binary_content_type(content_type: &str) -> bool {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if ct.is_empty() {
        return false;
    }
    !(ct.starts_with("text/html")
        || ct.starts_with("application/xhtml")
        || ct.starts_with("text/plain")
        || ct == "application/json"
        || ct == "text/xml"
        || ct == "application/xml"
        || ct.starts_with("text/"))
}

/// Filename from a `Content-Disposition` header value.
pub fn filename_from_content_disposition(value: &str) -> Option<String> {
    let caps = re(
        &CD_FILENAME_RE,
        r#"(?i)filename\*?\s*=\s*(?:UTF-8''([^;]+)|"([^"]+)"|([^;]+))"#,
    )
    .captures(value)?;
    let raw = caps
        .get(1)
        .or_else(|| caps.get(2))
        .or_else(|| caps.get(3))
        .map(|m| m.as_str().trim())
        .filter(|s| !s.is_empty())?;
    let decoded = percent_decode(raw);
    let name = decoded
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Build the `Binary` outcome from HEAD metadata, or `None` for pages.
pub fn binary_outcome_from_head(head: &HeadInfo) -> Option<SniffOutcome> {
    let ct = head.content_type.as_deref()?;
    if !is_binary_content_type(ct) {
        return None;
    }
    Some(SniffOutcome::Binary {
        content_type: ct.to_string(),
        content_length: head.content_length,
        final_url: head.final_url.clone(),
        filename: head
            .content_disposition
            .as_deref()
            .and_then(filename_from_content_disposition),
    })
}

// ---------------------------------------------------------------------------
// Production sniffer over HttpBackend
// ---------------------------------------------------------------------------

/// Sniffer that uses the Core [`HttpBackend`] for HEAD + capped GET.
pub struct HtmlPageSniffer {
    http: Arc<dyn HttpBackend>,
}

impl HtmlPageSniffer {
    pub fn new(http: Arc<dyn HttpBackend>) -> Self {
        Self { http }
    }
}

impl PageSniffer for HtmlPageSniffer {
    fn sniff_head(&self, url: &str, proxy: &ProxySettings) -> Result<SniffOutcome, String> {
        self.http.apply_proxy_settings(proxy);
        let head = self.http.head_info(url)?;
        Ok(binary_outcome_from_head(&head).unwrap_or(SniffOutcome::Nothing))
    }

    fn sniff(&self, url: &str, proxy: &ProxySettings) -> Result<SniffOutcome, String> {
        self.http.apply_proxy_settings(proxy);
        if let Ok(head) = self.http.head_info(url) {
            if let Some(binary) = binary_outcome_from_head(&head) {
                return Ok(binary);
            }
        }
        let (bytes, content_type, final_url) = self.http.fetch_capped(url, MAX_SNIFF_BYTES)?;
        if let Some(ct) = content_type.as_deref() {
            if is_binary_content_type(ct) {
                return Ok(SniffOutcome::Binary {
                    content_type: ct.to_string(),
                    content_length: None,
                    final_url,
                    filename: None,
                });
            }
        }
        let html = String::from_utf8_lossy(&bytes);
        if html.trim().is_empty() {
            return Ok(SniffOutcome::Nothing);
        }
        let base = Url::parse(&final_url)
            .or_else(|_| Url::parse(url))
            .map_err(|e| format!("invalid URL {url}: {e}"))?;
        Ok(SniffOutcome::Page {
            title: page_title(&html),
            media: sniff_html(&base, &html),
        })
    }
}

// ---------------------------------------------------------------------------
// Fake sniffer for tests
// ---------------------------------------------------------------------------

/// Canned sniff outcomes keyed by URL (tests).
#[derive(Default)]
pub struct FakePageSniffer {
    heads: std::sync::Mutex<HashMap<String, SniffOutcome>>,
    pages: std::sync::Mutex<HashMap<String, SniffOutcome>>,
    calls: std::sync::Mutex<Vec<(String, &'static str)>>,
}

impl FakePageSniffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// `sniff_head(url)` returns `outcome` (and `sniff` too unless overridden).
    pub fn insert_head(&self, url: &str, outcome: SniffOutcome) {
        self.heads.lock().unwrap().insert(url.to_string(), outcome);
    }

    /// `sniff(url)` returns `outcome`.
    pub fn insert_page(&self, url: &str, outcome: SniffOutcome) {
        self.pages.lock().unwrap().insert(url.to_string(), outcome);
    }

    /// `(url, "head" | "sniff")` in call order.
    pub fn calls(&self) -> Vec<(String, &'static str)> {
        self.calls.lock().unwrap().clone()
    }
}

impl PageSniffer for FakePageSniffer {
    fn sniff_head(&self, url: &str, _proxy: &ProxySettings) -> Result<SniffOutcome, String> {
        self.calls.lock().unwrap().push((url.to_string(), "head"));
        Ok(self
            .heads
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .unwrap_or(SniffOutcome::Nothing))
    }

    fn sniff(&self, url: &str, _proxy: &ProxySettings) -> Result<SniffOutcome, String> {
        self.calls.lock().unwrap().push((url.to_string(), "sniff"));
        if let Some(page) = self.pages.lock().unwrap().get(url).cloned() {
            return Ok(page);
        }
        Ok(self
            .heads
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .unwrap_or(SniffOutcome::Nothing))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://example.com/watch/clip-123").unwrap()
    }

    #[test]
    fn og_video_and_sources_are_found_and_resolved() {
        let html = r#"<html><head>
            <meta property="og:title" content="A &amp; B">
            <meta property="og:video:secure_url" content="https://cdn.example.com/v/clip_720.mp4">
            </head><body>
            <video poster="/p.jpg"><source src="/media/clip_480.mp4" type="video/mp4" label="480p">
            <source src="//cdn.example.com/hls/master.m3u8" type="application/x-mpegURL"></video>
            </body></html>"#;
        let media = sniff_html(&base(), html);
        let urls: Vec<_> = media.iter().map(|m| m.url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://cdn.example.com/v/clip_720.mp4",
                "https://example.com/media/clip_480.mp4",
                "https://cdn.example.com/hls/master.m3u8",
            ]
        );
        assert_eq!(media[2].kind, SniffedKind::Hls);
        assert_eq!(media[1].label_hint.as_deref(), Some("480p"));
        assert_eq!(page_title(html).as_deref(), Some("A & B"));
    }

    #[test]
    fn jsonld_video_object_content_url_is_found() {
        let html = r#"<script type="application/ld+json">{"@context":"https://schema.org","@graph":[{"@type":"VideoObject","name":"x","contentUrl":"https:\/\/cdn.example.com\/full.mp4","embedUrl":"https://example.com/embed/1"}]}</script>"#;
        let media = sniff_html(&base(), html);
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].url, "https://cdn.example.com/full.mp4");
        assert_eq!(media[0].source, SniffSource::JsonLdVideoObject);
    }

    #[test]
    fn inline_script_urls_are_found_and_previews_skipped() {
        let html = r#"<script>var s = {file: "https:\/\/cdn.example.com\/a\/b_1080.mp4?t=1", pre: "https://cdn.example.com/x_preview.mp4"};</script>"#;
        let media = sniff_html(&base(), html);
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].url, "https://cdn.example.com/a/b_1080.mp4?t=1");
        assert_eq!(media[0].source, SniffSource::InlineScript);
    }

    #[test]
    fn no_media_yields_empty() {
        let html = "<html><body><p>hello</p><img src=\"/a.png\"></body></html>";
        assert!(sniff_html(&base(), html).is_empty());
    }

    #[test]
    fn quality_hints_from_label_and_url() {
        assert_eq!(
            quality_from_hints("https://c/x_720.mp4", None, None).id,
            "720p"
        );
        assert_eq!(
            quality_from_hints("https://c/x.mp4", Some("HD 1080"), None).id,
            "1080p"
        );
        let q = quality_from_hints("https://c/x.mp4", None, Some(10));
        assert_eq!(q.id, "file");
        assert_eq!(q.size_bytes, Some(10));
        assert_eq!(q.container.as_deref(), Some("mp4"));
        assert_eq!(
            quality_from_hints("https://c/m.m3u8", None, None).id,
            "best"
        );
    }

    #[test]
    fn variants_group_into_one_item_with_multiple_qualities() {
        let media = vec![
            SniffedMedia {
                url: "https://cdn/x/clip_480.mp4".into(),
                kind: SniffedKind::DirectFile { ext: "mp4".into() },
                label_hint: None,
                source: SniffSource::SourceTag,
            },
            SniffedMedia {
                url: "https://cdn/x/clip_1080.mp4".into(),
                kind: SniffedKind::DirectFile { ext: "mp4".into() },
                label_hint: None,
                source: SniffSource::SourceTag,
            },
            SniffedMedia {
                url: "https://cdn/x/master.m3u8".into(),
                kind: SniffedKind::Hls,
                label_hint: None,
                source: SniffSource::SourceTag,
            },
        ];
        let items = media_items_from_sniff("https://example.com/p", Some("Page"), media);
        assert_eq!(items.len(), 2);
        let file_item = items
            .iter()
            .find(|i| i.fetch_kind == MediaFetchKind::DirectHttp)
            .unwrap();
        assert_eq!(
            file_item.url, "https://cdn/x/clip_1080.mp4",
            "best variant first"
        );
        let ids: Vec<_> = file_item.qualities.iter().map(|q| q.id.clone()).collect();
        assert_eq!(ids[0], "1080p");
        assert_eq!(ids[1], "480p@https://cdn/x/clip_480.mp4");
        assert_eq!(
            split_quality_url(&ids[1]),
            ("480p", Some("https://cdn/x/clip_480.mp4"))
        );
        let hls_item = items
            .iter()
            .find(|i| i.fetch_kind == MediaFetchKind::Extractor)
            .unwrap();
        assert_eq!(hls_item.qualities[0].id, "best");
        assert_eq!(hls_item.referer.as_deref(), Some("https://example.com/p"));
        assert!(items[0].id.starts_with("sniff-"));
    }

    #[test]
    fn binary_content_types_are_classified() {
        assert!(is_binary_content_type("application/octet-stream"));
        assert!(is_binary_content_type("video/mp4; charset=binary"));
        assert!(is_binary_content_type("application/zip"));
        assert!(!is_binary_content_type("text/html; charset=utf-8"));
        assert!(!is_binary_content_type("application/json"));
        assert!(!is_binary_content_type(""));
    }

    #[test]
    fn content_disposition_filename_is_parsed() {
        assert_eq!(
            filename_from_content_disposition(r#"attachment; filename="my file.iso""#).as_deref(),
            Some("my file.iso")
        );
        assert_eq!(
            filename_from_content_disposition("attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf")
                .as_deref(),
            Some("résumé.pdf")
        );
        assert_eq!(filename_from_content_disposition("inline"), None);
    }

    #[test]
    fn head_binary_outcome_includes_size_and_name() {
        let head = HeadInfo {
            content_type: Some("application/x-iso9660-image".into()),
            content_length: Some(42),
            accept_ranges: true,
            final_url: "https://mirror/x.iso".into(),
            content_disposition: Some("attachment; filename=\"void.iso\"".into()),
        };
        match binary_outcome_from_head(&head) {
            Some(SniffOutcome::Binary {
                content_length,
                filename,
                ..
            }) => {
                assert_eq!(content_length, Some(42));
                assert_eq!(filename.as_deref(), Some("void.iso"));
            }
            other => panic!("unexpected {other:?}"),
        }
        let page = HeadInfo {
            content_type: Some("text/html".into()),
            ..head
        };
        assert!(binary_outcome_from_head(&page).is_none());
    }
}
