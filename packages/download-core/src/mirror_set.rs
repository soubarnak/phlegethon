//! Mirror Set — ordered collection of equivalent source URLs for one job.
//!
//! A job backed by a Mirror Set tries mirrors in order.  When the current
//! mirror fails, the caller advances to the next.  If all mirrors are
//! exhausted the job fails permanently.
//!
//! Sources:
//! - Metalink / `.meta4` discovery
//! - Page-local mirror lists
//! - User-added URLs
//! - Web-wide mirror search is OUT for v1.

/// An ordered set of equivalent URLs for one download job.
///
/// Always has at least one URL (the primary).  Additional mirrors are
/// appended and tried in insertion order.
#[derive(Debug, Clone)]
pub struct MirrorSet {
    urls: Vec<String>,
    current: usize,
}

impl MirrorSet {
    /// Create a MirrorSet with a single primary URL.
    pub fn new(primary: String) -> Self {
        Self {
            urls: vec![primary],
            current: 0,
        }
    }

    /// Create a MirrorSet from an existing list of URLs (must be non-empty).
    pub fn from_urls(urls: Vec<String>) -> Result<Self, String> {
        if urls.is_empty() {
            return Err("MirrorSet must have at least one URL".into());
        }
        Ok(Self { urls, current: 0 })
    }

    /// Parse a Metalink / `.meta4` XML document and extract mirror URLs.
    ///
    /// v1 parser: extracts `<url>` element text content.  No XML namespace
    /// handling, no hash/piece verification — just URL discovery.
    /// Returns the discovered URLs (may be empty).
    pub fn from_metalink(xml: &str) -> Vec<String> {
        let mut urls = Vec::new();
        // Simple extraction of <url>…</url> — no XML dep for v1.
        // Handles <url>https://...</url> and <url location="…">https://...</url>
        for tag_start in xml.match_indices("<url").map(|(i, _)| i) {
            // Find closing >
            let content_start = match xml[tag_start..].find('>') {
                Some(pos) => tag_start + pos + 1,
                None => continue,
            };
            // Find </url>
            let end_tag = match xml[content_start..].find("</url>") {
                Some(pos) => content_start + pos,
                None => continue,
            };
            let url = xml[content_start..end_tag].trim().to_string();
            if !url.is_empty() {
                urls.push(url);
            }
        }
        urls
    }

    /// Append a mirror URL.  Ignores duplicates (string equality).
    pub fn add_mirror(&mut self, url: String) {
        if !self.urls.contains(&url) {
            self.urls.push(url);
        }
    }

    /// Extend with multiple mirrors (deduplicated).
    pub fn extend(&mut self, more: Vec<String>) {
        for url in more {
            self.add_mirror(url);
        }
    }

    /// Return the current (active) mirror URL.
    pub fn current_url(&self) -> &str {
        &self.urls[self.current]
    }

    /// Return all mirrors (for inspection / display).
    pub fn all_urls(&self) -> &[String] {
        &self.urls
    }

    /// Number of mirrors in the set.
    pub fn len(&self) -> usize {
        self.urls.len()
    }

    /// Index of the active mirror.
    pub fn current_index(&self) -> usize {
        self.current
    }

    /// Advance to the next mirror after the current one failed.
    ///
    /// Returns `true` if there is another mirror to try, `false` if all
    /// mirrors have been exhausted (the job should fail permanently).
    pub fn advance_on_failure(&mut self) -> bool {
        if self.current + 1 < self.urls.len() {
            self.current += 1;
            true
        } else {
            false
        }
    }

    /// Reset the active mirror to index 0 (e.g. after retry).
    pub fn reset(&mut self) {
        self.current = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_set_created_with_primary() {
        let ms = MirrorSet::new("https://primary.example.com/file.zip".into());
        assert_eq!(ms.len(), 1);
        assert_eq!(ms.current_url(), "https://primary.example.com/file.zip");
        assert_eq!(ms.current_index(), 0);
    }

    #[test]
    fn mirror_set_from_urls_rejects_empty() {
        let err = MirrorSet::from_urls(vec![]).unwrap_err();
        assert!(err.contains("at least one URL"), "got: {err}");
    }

    #[test]
    fn mirror_set_add_mirror_appends() {
        let mut ms = MirrorSet::new("https://a.example/file.zip".into());
        ms.add_mirror("https://b.example/file.zip".into());
        assert_eq!(ms.len(), 2);
        assert_eq!(ms.current_url(), "https://a.example/file.zip");
    }

    #[test]
    fn mirror_set_ignores_duplicate_mirrors() {
        let mut ms = MirrorSet::new("https://a.example/file.zip".into());
        ms.add_mirror("https://a.example/file.zip".into());
        assert_eq!(ms.len(), 1);
    }

    #[test]
    fn mirror_set_advance_moves_to_next_mirror() {
        let mut ms = MirrorSet::new("https://a.example/file.zip".into());
        ms.add_mirror("https://b.example/file.zip".into());
        ms.add_mirror("https://c.example/file.zip".into());

        assert!(ms.advance_on_failure());
        assert_eq!(ms.current_index(), 1);
        assert_eq!(ms.current_url(), "https://b.example/file.zip");

        assert!(ms.advance_on_failure());
        assert_eq!(ms.current_index(), 2);
        assert_eq!(ms.current_url(), "https://c.example/file.zip");
    }

    #[test]
    fn mirror_set_advance_on_last_mirror_returns_false() {
        let mut ms = MirrorSet::new("https://a.example/file.zip".into());
        // Only one mirror — advancing should fail
        assert!(!ms.advance_on_failure());
        assert_eq!(ms.current_index(), 0);
    }

    #[test]
    fn mirror_set_all_mirrors_exhausted() {
        let mut ms = MirrorSet::new("https://a.example/file.zip".into());
        ms.add_mirror("https://b.example/file.zip".into());

        assert!(ms.advance_on_failure()); // now on b
        assert!(!ms.advance_on_failure()); // exhausted
    }

    #[test]
    fn mirror_set_reset_returns_to_start() {
        let mut ms = MirrorSet::new("https://a.example/file.zip".into());
        ms.add_mirror("https://b.example/file.zip".into());
        ms.advance_on_failure();
        ms.advance_on_failure();

        ms.reset();
        assert_eq!(ms.current_index(), 0);
        assert_eq!(ms.current_url(), "https://a.example/file.zip");
    }

    #[test]
    fn mirror_set_extend_appends_multiple() {
        let mut ms = MirrorSet::new("https://a.example/file.zip".into());
        ms.extend(vec![
            "https://b.example/file.zip".into(),
            "https://c.example/file.zip".into(),
        ]);
        assert_eq!(ms.len(), 3);
    }

    #[test]
    fn mirror_set_extend_deduplicates() {
        let mut ms = MirrorSet::new("https://a.example/file.zip".into());
        ms.extend(vec![
            "https://b.example/file.zip".into(),
            "https://a.example/file.zip".into(), // duplicate
        ]);
        assert_eq!(ms.len(), 2);
    }

    // --- Metalink parsing ---

    #[test]
    fn from_metalink_extracts_simple_urls() {
        let xml = r#"
            <?xml version="1.0" encoding="UTF-8"?>
            <metalink version="3.0" xmlns="http://www.metalinker.org/">
                <file name="test.zip">
                    <url>https://mirror1.example.com/test.zip</url>
                    <url>https://mirror2.example.com/test.zip</url>
                </file>
            </metalink>
        "#;
        let urls = MirrorSet::from_metalink(xml);
        assert_eq!(urls.len(), 2);
        assert!(urls.contains(&"https://mirror1.example.com/test.zip".into()));
        assert!(urls.contains(&"https://mirror2.example.com/test.zip".into()));
    }

    #[test]
    fn from_metalink_handles_attributes_in_url_tag() {
        let xml = r#"
            <metalink>
                <file name="file.zip">
                    <url location="us" priority="1">https://us.example.com/file.zip</url>
                    <url location="eu" priority="2">https://eu.example.com/file.zip</url>
                </file>
            </metalink>
        "#;
        let urls = MirrorSet::from_metalink(xml);
        assert_eq!(urls.len(), 2);
        assert!(urls.contains(&"https://us.example.com/file.zip".into()));
    }

    #[test]
    fn from_metalink_returns_empty_for_no_urls() {
        let xml = r#"<metalink><file name="test.zip"></file></metalink>"#;
        let urls = MirrorSet::from_metalink(xml);
        assert!(urls.is_empty());
    }

    #[test]
    fn from_metalink_returns_empty_for_non_metalink_xml() {
        let xml = r#"<html><body>not metalink</body></html>"#;
        let urls = MirrorSet::from_metalink(xml);
        assert!(urls.is_empty());
    }

    #[test]
    fn from_metalink_skips_empty_url_tags() {
        let xml = r#"
            <metalink>
                <file name="test.zip">
                    <url></url>
                    <url>https://mirror.example.com/test.zip</url>
                </file>
            </metalink>
        "#;
        let urls = MirrorSet::from_metalink(xml);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], "https://mirror.example.com/test.zip");
    }
}
