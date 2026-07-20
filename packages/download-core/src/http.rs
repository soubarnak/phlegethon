//! HTTP backend trait — primary test seam for the Core.

use std::io::Read;
use std::ops::Range;

/// An HTTP backend that the Download Core uses to fetch remote content.
///
/// In production this wraps a real HTTP client.  In tests a fake impl
/// returns canned data (no live network).
pub trait HttpBackend: Send + Sync {
    /// Fetch (partial) content from `url`.
    ///
    /// `range` — optional byte range `start..end`.  When `end` is `u64::MAX`
    /// it means "to the end of the file".
    ///
    /// Returns `(bytes, total_length)` on success.
    fn fetch(&self, url: &str, range: Option<Range<u64>>) -> Result<(Vec<u8>, u64), String>;

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
}

// ---------------------------------------------------------------------------
// Production HTTP backend (ureq-based)
// ---------------------------------------------------------------------------

/// Production HTTP backend using the `ureq` crate.
///
/// - `fetch`: sends GET (with optional Range header), streams body.
/// - `supports_range`: sends HEAD and inspects `Accept-Ranges`.
/// - `file_size`: sends HEAD and reads `Content-Length`.
pub struct ProdHttpBackend {
    client: ureq::Agent,
}

impl ProdHttpBackend {
    pub fn new() -> Self {
        Self {
            client: ureq::AgentBuilder::new()
                .timeout_connect(std::time::Duration::from_secs(10))
                .timeout_read(std::time::Duration::from_secs(30))
                .build(),
        }
    }
}

impl Default for ProdHttpBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpBackend for ProdHttpBackend {
    fn fetch(
        &self,
        url: &str,
        range: Option<Range<u64>>,
    ) -> Result<(Vec<u8>, u64), String> {
        let mut req = self.client.get(url);
        if let Some(r) = range {
            let header_val = format!("bytes={}-{}", r.start, r.end.saturating_sub(1));
            req = req.set("Range", &header_val);
        }
        let resp = req
            .call()
            .map_err(|e| format!("GET {url}: {e}"))?;

        // Read Content-Length for total size
        let total: u64 = resp
            .header("Content-Length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        // Read body
        let mut body: Vec<u8> = Vec::new();
        resp.into_reader()
            .read_to_end(&mut body)
            .map_err(|e| format!("read body {url}: {e}"))?;

        Ok((body, total))
    }

    fn supports_range(&self, url: &str) -> Result<bool, String> {
        let resp = self
            .client
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
            .client
            .head(url)
            .call()
            .map_err(|e| format!("HEAD {url}: {e}"))?;

        resp.header("Content-Length")
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("no Content-Length for {url}"))
    }
}
