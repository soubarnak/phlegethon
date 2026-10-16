//! HTTP backend trait — primary test seam for the Core.

use crate::proxy_settings::ProxySettings;
use std::io::{Read, Write};
use std::ops::Range;
use std::sync::RwLock;

/// Parsed `Content-Range: bytes START-END/TOTAL` header of a 206 reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    /// Last byte position, inclusive.
    pub end: u64,
    /// Full object size; `None` when the server sent `*`.
    pub total: Option<u64>,
}

impl ContentRange {
    /// Parse a `Content-Range` header value such as `bytes 400-999/1000`.
    pub fn parse(value: &str) -> Option<Self> {
        let rest = value.trim().strip_prefix("bytes")?.trim_start();
        let (span, total) = rest.split_once('/')?;
        let (start, end) = span.trim().split_once('-')?;
        let start: u64 = start.trim().parse().ok()?;
        let end: u64 = end.trim().parse().ok()?;
        if end < start {
            return None;
        }
        let total = match total.trim() {
            "*" => None,
            n => Some(n.parse().ok()?),
        };
        Some(Self { start, end, total })
    }
}

/// Metadata returned after a successful HTTP fetch into a sink.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FetchResult {
    /// Full object size when known: the `Content-Range` total of a 206 reply,
    /// else the `Content-Length` of a 200 reply, else 0.
    pub total_length: u64,
    /// Bytes written to the sink during this fetch call.
    pub bytes_written: u64,
    /// HTTP status of the reply. `None` when the backend does not report one
    /// (test doubles); Core then trusts that a Range request was honoured.
    pub status: Option<u16>,
    /// `Content-Range` of a 206 reply.
    pub content_range: Option<ContentRange>,
    /// `ETag` validator of the object, for `If-Range` on resume.
    pub etag: Option<String>,
    /// `Last-Modified` validator of the object, for `If-Range` on resume.
    pub last_modified: Option<String>,
}

impl FetchResult {
    /// True when the reply to a Range request starting at `requested_start`
    /// is not the slice that was asked for (status 200, or a 206 whose
    /// `Content-Range` starts elsewhere).
    pub fn range_ignored(&self, requested_start: u64) -> bool {
        match self.status {
            Some(206) => self
                .content_range
                .map(|r| r.start != requested_start)
                .unwrap_or(true),
            Some(_) => true,
            None => false,
        }
    }
}

/// Header value for a byte range; an open end (`u64::MAX`) becomes `bytes=N-`.
pub fn range_header_value(range: &Range<u64>) -> String {
    if range.end == u64::MAX {
        format!("bytes={}-", range.start)
    } else {
        format!("bytes={}-{}", range.start, range.end.saturating_sub(1))
    }
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

    /// Like [`Self::fetch`], but sends `If-Range: <if_range>` with the Range
    /// request, so a server whose object changed replies 200 with the new body
    /// instead of a 206 slice of it.
    ///
    /// When a Range request is answered with anything but 206, the backend
    /// should not stream the body into `sink`; it returns the reply head with
    /// `bytes_written == 0` and Core restarts the transfer from zero. The
    /// default implementation ignores `if_range`.
    fn fetch_conditional(
        &self,
        url: &str,
        range: Option<Range<u64>>,
        _if_range: Option<&str>,
        sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        self.fetch(url, range, sink)
    }

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
    if let Some(url) = proxy
        .url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
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
            client: RwLock::new(build_ureq_agent(proxy).unwrap_or_else(|_| {
                ureq::AgentBuilder::new()
                    .try_proxy_from_env(true)
                    .timeout_connect(std::time::Duration::from_secs(10))
                    .timeout_read(std::time::Duration::from_secs(30))
                    .build()
            })),
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
        self.fetch_conditional(url, range, None, sink)
    }

    fn fetch_conditional(
        &self,
        url: &str,
        range: Option<Range<u64>>,
        if_range: Option<&str>,
        sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        let mut req = self.client().get(url);
        if let Some(r) = &range {
            req = req.set("Range", &range_header_value(r));
            if let Some(validator) = if_range {
                req = req.set("If-Range", validator);
            }
        }
        let resp = req.call().map_err(|e| format!("GET {url}: {e}"))?;

        let status = resp.status();
        let content_range = resp.header("Content-Range").and_then(ContentRange::parse);
        let content_length: u64 = resp
            .header("Content-Length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        // On a 206 the Content-Length is the slice length; the object size
        // comes from Content-Range.
        let total = match (status, content_range) {
            (206, Some(r)) => r.total.unwrap_or(0),
            (206, None) => 0,
            _ => content_length,
        };
        let mut result = FetchResult {
            total_length: total,
            bytes_written: 0,
            status: Some(status),
            content_range,
            etag: resp.header("ETag").map(str::to_string),
            last_modified: resp.header("Last-Modified").map(str::to_string),
        };
        if range.is_some() && status != 206 {
            // The server ignored the Range; Core restarts from zero.
            return Ok(result);
        }

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

        result.bytes_written = written;
        Ok(result)
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
        let head = client.head(url).set("User-Agent", PAGE_USER_AGENT).call();
        let resp = match head {
            Ok(resp) => resp,
            // Some servers reject HEAD; ask for the first byte instead.
            Err(ureq::Error::Status(405 | 403 | 501, _)) | Err(ureq::Error::Status(400, _)) => {
                client
                    .get(url)
                    .set("User-Agent", PAGE_USER_AGENT)
                    .set("Range", "bytes=0-0")
                    .call()
                    .map_err(|e| format!("GET(range 0-0) {url}: {e}"))?
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_range() {
        assert_eq!(
            ContentRange::parse("bytes 400-999/1000"),
            Some(ContentRange {
                start: 400,
                end: 999,
                total: Some(1000)
            })
        );
        assert_eq!(
            ContentRange::parse("bytes 0-0/*"),
            Some(ContentRange {
                start: 0,
                end: 0,
                total: None
            })
        );
        assert_eq!(ContentRange::parse("bytes */1000"), None);
        assert_eq!(ContentRange::parse("garbage"), None);
    }

    #[test]
    fn open_range_uses_open_ended_header() {
        assert_eq!(range_header_value(&(400..u64::MAX)), "bytes=400-");
        assert_eq!(range_header_value(&(0..100)), "bytes=0-99");
    }

    #[test]
    fn range_ignored_detects_200_and_wrong_start() {
        let ok = FetchResult {
            status: Some(206),
            content_range: Some(ContentRange {
                start: 400,
                end: 999,
                total: Some(1000),
            }),
            ..Default::default()
        };
        assert!(!ok.range_ignored(400));
        assert!(ok.range_ignored(0));
        let full = FetchResult {
            status: Some(200),
            ..Default::default()
        };
        assert!(full.range_ignored(400));
        assert!(!FetchResult::default().range_ignored(400));
    }

    /// Serve one canned HTTP response on localhost and return the raw request.
    fn serve_once(response: &'static [u8]) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/object.bin", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = conn.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            conn.write_all(response).unwrap();
            String::from_utf8_lossy(&request).to_ascii_lowercase()
        });
        (url, handle)
    }

    #[test]
    fn prod_backend_sends_open_range_and_if_range() {
        let (url, server) = serve_once(
            b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 4-9/10\r\nContent-Length: 6\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n456789",
        );
        let mut sink = Vec::new();
        let result = ProdHttpBackend::new()
            .fetch_conditional(&url, Some(4..u64::MAX), Some("\"v1\""), &mut sink)
            .unwrap();
        let request = server.join().unwrap();

        assert!(
            request.contains("range: bytes=4-\r\n"),
            "request: {request}"
        );
        assert!(
            request.contains("if-range: \"v1\"\r\n"),
            "request: {request}"
        );
        assert_eq!(sink, b"456789");
        assert_eq!(result.status, Some(206));
        assert_eq!(result.total_length, 10, "total comes from Content-Range");
        assert_eq!(result.etag.as_deref(), Some("\"v1\""));
        assert!(!result.range_ignored(4));
    }

    #[test]
    fn prod_backend_does_not_stream_a_200_reply_to_a_range_request() {
        let (url, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nLast-Modified: Wed, 01 Jan 2025 00:00:00 GMT\r\nConnection: close\r\n\r\n0123456789",
        );
        let mut sink = Vec::new();
        let result = ProdHttpBackend::new()
            .fetch(&url, Some(4..u64::MAX), &mut sink)
            .unwrap();
        server.join().unwrap();

        assert!(sink.is_empty(), "the body must not reach the sink");
        assert_eq!(result.status, Some(200));
        assert_eq!(result.total_length, 10);
        assert!(result.range_ignored(4));
        assert_eq!(
            result.last_modified.as_deref(),
            Some("Wed, 01 Jan 2025 00:00:00 GMT")
        );
    }

    #[test]
    fn prod_backend_streams_a_plain_200_reply() {
        let (url, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789",
        );
        let mut sink = Vec::new();
        let result = ProdHttpBackend::new().fetch(&url, None, &mut sink).unwrap();
        server.join().unwrap();

        assert_eq!(sink, b"0123456789");
        assert_eq!(result.bytes_written, 10);
        assert_eq!(result.total_length, 10);
    }
}
