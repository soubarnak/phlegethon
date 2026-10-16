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

/// Result of splitting pasted text: accepted URLs plus the tokens that were not
/// usable, so the shell can tell the user what was dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SplitUrlsReport {
    /// Normalized, de-duplicated HTTP(S) URLs in paste order.
    pub urls: Vec<String>,
    /// Non-empty tokens that are not HTTP(S) URLs.
    pub rejected: Vec<String>,
}

/// Split pasted text into normalized HTTP(S) URLs.
///
/// Accepts newline- and whitespace-separated URLs. A `,` or `;` also separates
/// two URLs when the next one starts with `http://` or `https://`; any other
/// comma or semicolon belongs to the URL, so `https://a.example/dl?ids=1,2,3`
/// stays whole. Use [`split_urls_report`] to also learn which tokens were
/// rejected.
pub fn split_urls(text: &str) -> Vec<String> {
    split_urls_report(text).urls
}

/// Like [`split_urls`], but also reports every token that was not accepted.
pub fn split_urls_report(text: &str) -> SplitUrlsReport {
    let mut report = SplitUrlsReport::default();
    let mut seen = std::collections::HashSet::new();

    for chunk in text.split_whitespace() {
        for token in split_on_url_boundaries(chunk) {
            match normalize_url(token) {
                Some(normalized) => {
                    if seen.insert(normalized.clone()) {
                        report.urls.push(normalized);
                    }
                }
                None => report.rejected.push(token.to_string()),
            }
        }
    }

    report
}

/// Split one whitespace-free chunk at each `,` or `;` that is followed by a
/// URL scheme, dropping empty pieces.
fn split_on_url_boundaries(chunk: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, c) in chunk.char_indices() {
        if (c == ',' || c == ';') && starts_with_http_scheme(&chunk[i + 1..]) {
            parts.push(&chunk[start..i]);
            start = i + 1;
        }
    }
    parts.push(&chunk[start..]);
    // A trailing separator is list punctuation ("a, b"), not part of the URL.
    let mut parts: Vec<&str> = parts
        .into_iter()
        .map(|p| p.trim_end_matches([',', ';']))
        .collect();
    parts.retain(|p| !p.is_empty());
    parts
}

fn starts_with_http_scheme(s: &str) -> bool {
    let lower = |n: usize| s.get(..n).map(str::to_ascii_lowercase);
    lower(7).as_deref() == Some("http://") || lower(8).as_deref() == Some("https://")
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

    #[test]
    fn split_urls_keeps_commas_inside_a_url() {
        assert_eq!(
            split_urls("https://example.com/dl?ids=1,2,3"),
            vec!["https://example.com/dl?ids=1,2,3".to_string()]
        );
    }

    #[test]
    fn split_urls_splits_comma_and_semicolon_before_a_scheme() {
        assert_eq!(
            split_urls("https://a.example/x,https://b.example/y;HTTP://c.example/z"),
            vec![
                "https://a.example/x".to_string(),
                "https://b.example/y".to_string(),
                "http://c.example/z".to_string(),
            ]
        );
    }

    #[test]
    fn split_urls_drops_trailing_separator_before_whitespace() {
        assert_eq!(
            split_urls("https://a.example/one, https://b.example/two;\nhttps://c.example/three,"),
            vec![
                "https://a.example/one".to_string(),
                "https://b.example/two".to_string(),
                "https://c.example/three".to_string(),
            ]
        );
    }

    #[test]
    fn split_urls_report_lists_rejected_tokens() {
        let report = split_urls_report("https://a.example/x ftp://b.example/y not-a-url");
        assert_eq!(report.urls, vec!["https://a.example/x".to_string()]);
        assert_eq!(
            report.rejected,
            vec!["ftp://b.example/y".to_string(), "not-a-url".to_string()]
        );
    }
}
