//! HTTP backend trait — primary test seam for the Core.

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
