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
}
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
}
