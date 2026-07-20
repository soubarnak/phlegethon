//! HTTP backend trait — primary test seam for the Core.

use std::ops::Range;

/// An HTTP backend that the Download Core uses to fetch remote content.
///
/// In production this wraps a real HTTP client.  In tests a fake impl
/// returns canned data (no live network).
pub trait HttpBackend: Send + Sync {
    /// Fetch (partial) content from `url`.
    ///
    /// Returns `(bytes, total_length)` on success.
    fn fetch(&self, url: &str, range: Option<Range<u64>>) -> Result<(Vec<u8>, u64), String>;
}
