//! HTTP backend trait — primary test seam for the Core.

use crate::proxy_settings::ProxySettings;
use std::io::{Read, Write};
use std::ops::Range;
use std::sync::RwLock;

/// Metadata returned after a successful HTTP fetch into a sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchResult {
    /// Total content length reported by the server (full object size when known).
    pub total_length: u64,
    /// Bytes written to the sink during this fetch call.
    pub bytes_written: u64,
}

/// Metadata from a HEAD request (or a `Range: bytes=0-0` GET fallback).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HeadInfo {
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
    pub accept_ranges: bool,
    /// URL after redirects.
    pub final_url: String,
    pub content_disposition: Option<String>,
}

/// Sink that accepts at most `max` bytes, then reports `Ok(0)` so streaming stops.
struct CappedSink {
    buf: Vec<u8>,
    max: u64,
}

impl Write for CappedSink {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let room = self.max.saturating_sub(self.buf.len() as u64) as usize;
        if room == 0 {
            return Ok(0);
        }
        let n = data.len().min(room);
        self.buf.extend_from_slice(&data[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// An HTTP backend that the Download Core uses to fetch remote content.
///
/// In production this wraps a real HTTP client.  In tests a fake impl
/// returns canned data (no live network).
pub trait HttpBackend: Send + Sync {
    /// Fetch (partial) content from `url`, streaming the body into `sink`.
    ///
    /// `range` — optional byte range `start..end`.  When `end` is `u64::MAX`
    /// it means "to the end of the file".
    ///
    /// On success returns how many bytes were written and the total object
    /// length when the server reports it.
    fn fetch(
        &self,
        url: &str,
        range: Option<Range<u64>>,
        sink: &mut dyn Write,
    ) -> Result<FetchResult, String>;

    /// Check whether the server at `url` supports byte-range requests.
    ///
    /// In production this sends a HEAD request and inspects the
    /// `Accept-Ranges` header.  In tests it returns a pre-configured value.
    fn supports_range(&self, url: &str) -> Result<bool, String>;

    /// Get the total content length for `url` without downloading the body.
    ///
    /// In production this sends a HEAD request and reads `Content-Length`.
    /// In tests it returns the length of canned data.
    fn file_size(&self, url: &str) -> Result<u64, String>;

    /// Apply global proxy settings to this backend when supported.
    ///
    /// Test fakes ignore this; production HTTP honors proxy changes.
    fn apply_proxy_settings(&self, _proxy: &ProxySettings) {}

    /// HEAD metadata used by the page sniffer to tell files from pages.
    ///
    /// Default composes [`Self::supports_range`] and [`Self::file_size`] and
    /// reports no content type (fakes then look like pages).
    fn head_info(&self, url: &str) -> Result<HeadInfo, String> {
        Ok(HeadInfo {
            content_type: None,
            content_length: self.file_size(url).ok().filter(|&n| n > 0),
            accept_ranges: self.supports_range(url).unwrap_or(false),
            final_url: url.to_string(),
            content_disposition: None,
        })
    }

    /// GET at most `max_bytes` of the body. Returns `(bytes, content_type, final_url)`.
    fn fetch_capped(
        &self,
        url: &str,
        max_bytes: u64,
    ) -> Result<(Vec<u8>, Option<String>, String), String> {
        let mut sink = CappedSink {
            buf: Vec::new(),
            max: max_bytes,
        };
        self.fetch(url, None, &mut sink)?;
        Ok((sink.buf, None, url.to_string()))
    }
}

/// Browser-like User-Agent for page fetches (many sites 403 bare clients).
const PAGE_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
// Production HTTP backend (ureq-based)
// ---------------------------------------------------------------------------

/// Production HTTP backend using the `ureq` crate.
///
/// - `fetch`: sends GET (with optional Range header), streams body.
/// - `supports_range`: sends HEAD and inspects `Accept-Ranges`.
/// - `file_size`: sends HEAD and reads `Content-Length`.
pub struct ProdHttpBackend {
    client: RwLock<ureq::Agent>,
}

fn build_ureq_agent(proxy: &ProxySettings) -> Result<ureq::Agent, String> {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout_read(std::time::Duration::from_secs(30));
    if let Some(url) = proxy.url.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let proxy_cfg = ureq::Proxy::new(url).map_err(|e| format!("proxy {url}: {e}"))?;
        builder = builder.proxy(proxy_cfg);
    } else {
        // No proxy configured in the app: honour HTTPS_PROXY / HTTP_PROXY / ALL_PROXY like
        // yt-dlp, curl and browsers do. Without this, plain downloads and the page sniffer
        // fail on machines that only reach the internet through an environment proxy.
        builder = builder.try_proxy_from_env(true);
    }
    Ok(builder.build())
}

impl ProdHttpBackend {
    pub fn new() -> Self {
        Self::with_proxy(&ProxySettings::default())
    }

    pub fn with_proxy(proxy: &ProxySettings) -> Self {
        Self {
            client: RwLock::new(
                build_ureq_agent(proxy).unwrap_or_else(|_| {
                    ureq::AgentBuilder::new()
                        .try_proxy_from_env(true)
                        .timeout_connect(std::time::Duration::from_secs(10))
                        .timeout_read(std::time::Duration::from_secs(30))
                        .build()
                }),
            ),
        }
    }

    fn client(&self) -> std::sync::RwLockReadGuard<'_, ureq::Agent> {
        self.client.read().unwrap_or_else(|e| e.into_inner())
    }
}

impl Default for ProdHttpBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpBackend for ProdHttpBackend {
    fn apply_proxy_settings(&self, proxy: &ProxySettings) {
        if let Ok(mut guard) = self.client.write() {
            if let Ok(agent) = build_ureq_agent(proxy) {
                *guard = agent;
            }
        }
    }

    fn fetch(
        &self,
        url: &str,
        range: Option<Range<u64>>,
        sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        let mut req = self.client().get(url);
        if let Some(r) = range {
            let header_val = format!("bytes={}-{}", r.start, r.end.saturating_sub(1));
            req = req.set("Range", &header_val);
        }
        let resp = req
            .call()
            .map_err(|e| format!("GET {url}: {e}"))?;

        let total: u64 = resp
            .header("Content-Length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        let mut reader = resp.into_reader();
        let mut written = 0u64;
        let mut buf = [0u8; 8192];
        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| format!("read body {url}: {e}"))?;
            if n == 0 {
                break;
            }
            let mut chunk_written = 0;
            while chunk_written < n {
                let w = sink
                    .write(&buf[chunk_written..n])
                    .map_err(|e| format!("write sink {url}: {e}"))?;
                if w == 0 {
                    break;
                }
                chunk_written += w;
                written += w as u64;
            }
            if chunk_written < n {
                break;
            }
        }

        Ok(FetchResult {
            total_length: total,
            bytes_written: written,
        })
    }

    fn supports_range(&self, url: &str) -> Result<bool, String> {
        let resp = self
            .client()
            .head(url)
            .call()
            .map_err(|e| format!("HEAD {url}: {e}"))?;

        Ok(resp
            .header("Accept-Ranges")
            .map(|v| v == "bytes")
            .unwrap_or(false))
    }

    fn file_size(&self, url: &str) -> Result<u64, String> {
        let resp = self
            .client()
            .head(url)
            .call()
            .map_err(|e| format!("HEAD {url}: {e}"))?;

        resp.header("Content-Length")
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("no Content-Length for {url}"))
    }

    fn head_info(&self, url: &str) -> Result<HeadInfo, String> {
        let client = self.client();
        let head = client
            .head(url)
            .set("User-Agent", PAGE_USER_AGENT)
            .call();
        let resp = match head {
            Ok(resp) => resp,
            // Some servers reject HEAD; ask for the first byte instead.
            Err(ureq::Error::Status(405 | 403 | 501, _)) | Err(ureq::Error::Status(400, _)) => client
                .get(url)
                .set("User-Agent", PAGE_USER_AGENT)
                .set("Range", "bytes=0-0")
                .call()
                .map_err(|e| format!("GET(range 0-0) {url}: {e}"))?,
            Err(e) => return Err(format!("HEAD {url}: {e}")),
        };
        let content_length = resp
            .header("Content-Range")
            .and_then(|v| v.rsplit('/').next())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .or_else(|| resp.header("Content-Length").and_then(|v| v.parse().ok()));
        Ok(HeadInfo {
            content_type: resp.header("Content-Type").map(str::to_string),
            content_length,
            accept_ranges: resp
                .header("Accept-Ranges")
                .map(|v| v == "bytes")
                .unwrap_or(false),
            final_url: resp.get_url().to_string(),
            content_disposition: resp.header("Content-Disposition").map(str::to_string),
        })
    }

    fn fetch_capped(
        &self,
        url: &str,
        max_bytes: u64,
    ) -> Result<(Vec<u8>, Option<String>, String), String> {
        let resp = self
            .client()
            .get(url)
            .set("User-Agent", PAGE_USER_AGENT)
            .set("Accept", "text/html,application/xhtml+xml,*/*;q=0.8")
            .call()
            .map_err(|e| format!("GET {url}: {e}"))?;
        let content_type = resp.header("Content-Type").map(str::to_string);
        let final_url = resp.get_url().to_string();
        let mut reader = resp.into_reader().take(max_bytes);
        let mut buf = Vec::new();
        reader
            .read_to_end(&mut buf)
            .map_err(|e| format!("read body {url}: {e}"))?;
        Ok((buf, content_type, final_url))
    }
}
