//! URL parsing helpers for shell input flows (multi-paste, normalization).

/// Normalize an HTTP(S) URL for duplicate detection and enqueue.
///
/// Strips the fragment and a trailing slash on the path (keeps query).
pub fn normalize_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut parsed = url::Url::parse(trimmed).ok()?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return None;
    }

    parsed.set_fragment(None);

    let mut path = parsed.path().to_string();
    if path.len() > 1 && path.ends_with('/') {
        path.pop();
        parsed.set_path(&path);
    }

    Some(parsed.to_string())
}

/// Split pasted text into normalized HTTP(S) URLs.
///
/// Accepts newline-, comma-, semicolon-, and whitespace-separated URLs.
pub fn split_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for token in text.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if let Some(normalized) = normalize_url(token) {
            if seen.insert(normalized.clone()) {
                urls.push(normalized);
            }
        }
    }

    urls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_fragment_and_trailing_slash() {
        assert_eq!(
            normalize_url("https://example.com/path/#frag"),
            Some("https://example.com/path".to_string())
        );
        assert_eq!(
            normalize_url("https://example.com/path/?q=1#frag"),
            Some("https://example.com/path?q=1".to_string())
        );
        assert_eq!(
            normalize_url("https://example.com/path/"),
            Some("https://example.com/path".to_string())
        );
    }

    #[test]
    fn normalize_keeps_query_and_root_path() {
        assert_eq!(
            normalize_url("https://example.com/?q=1"),
            Some("https://example.com/?q=1".to_string())
        );
        assert_eq!(
            normalize_url("https://example.com/"),
            Some("https://example.com/".to_string())
        );
    }

    #[test]
    fn normalize_rejects_non_http() {
        assert_eq!(normalize_url("ftp://example.com/file"), None);
        assert_eq!(normalize_url("not a url"), None);
    }

    #[test]
    fn split_urls_handles_multiline_paste() {
        let text = "https://a.example/one\nhttps://b.example/two\r\nhttps://c.example/three";
        assert_eq!(
            split_urls(text),
            vec![
                "https://a.example/one".to_string(),
                "https://b.example/two".to_string(),
                "https://c.example/three".to_string(),
            ]
        );
    }

    #[test]
    fn split_urls_deduplicates_normalized_urls() {
        let text = "https://example.com/path/#a\nhttps://example.com/path/#b";
        assert_eq!(
            split_urls(text),
            vec!["https://example.com/path".to_string()]
        );
    }
}
