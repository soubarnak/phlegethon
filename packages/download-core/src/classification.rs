//! URL classification — Download Core owns intent.
//!
//! | Pattern | Classification |
//! |---|---|
//! | Clear single-file HTTP(S) | `EnqueueDirect` |
//! | Everything else | `ProbeThenConfirm` |

use url::Url;

/// How the Core classifies a URL for the enqueue path.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// A clear single-file HTTP(S) URL — may enqueue directly.
    EnqueueDirect,
    /// A URL that requires probe-then-confirm (Launch Site, page media, etc.).
    ProbeThenConfirm,
}

/// Classify a parsed URL.
///
/// The heuristic is conservative: only clear single-file HTTP(S) URLs
/// (those whose path ends in a common file extension or has no query
/// fragment) get `EnqueueDirect`.  Everything else (YouTube, etc.) goes
/// through `ProbeThanConfirm`.
pub fn classify(url: &Url) -> Kind {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Kind::ProbeThenConfirm;
    }

    // If the path looks like a file with an extension, assume direct.
    let path = url.path();
    if let Some(ext) = extension(path) {
        // Known file extensions = clear single-file download
        if is_direct_extension(ext) {
            return Kind::EnqueueDirect;
        }
    }

    // No recognizable extension → probe
    Kind::ProbeThenConfirm
}

fn extension(path: &str) -> Option<&str> {
    let last_segment = path.rsplit('/').next().filter(|s| !s.is_empty())?;
    let dot = last_segment.rfind('.')?;
    let ext = &last_segment[dot + 1..];
    if ext.is_empty() { None } else { Some(ext) }
}

fn is_direct_extension(ext: &str) -> bool {
    matches!(
        ext.to_lowercase().as_str(),
        // Archives & compression
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" | "zst"
        // Documents
        | "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt"
        // Images
        | "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tiff" | "svg" | "ico"
        // Audio / video
        | "mp3" | "mp4" | "mkv" | "avi" | "mov" | "wav" | "flac" | "ogg" | "webm"
        // Executables & packages
        | "exe" | "msi" | "dmg" | "deb" | "rpm" | "apk"
        // Compressed text / data
        | "csv" | "json" | "xml" | "sql" | "dump"
        // Generic binary
        | "bin" | "dat" | "iso" | "img"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_clear_http_file_zip() {
        let u = Url::parse("https://example.com/file.zip").unwrap();
        assert_eq!(classify(&u), Kind::EnqueueDirect);
    }

    #[test]
    fn classify_clear_http_file_mp4() {
        let u = Url::parse("https://cdn.example.com/video.mp4").unwrap();
        assert_eq!(classify(&u), Kind::EnqueueDirect);
    }

    #[test]
    fn classify_youtube_url_is_probe() {
        let u = Url::parse("https://www.youtube.com/watch?v=abc123").unwrap();
        assert_eq!(classify(&u), Kind::ProbeThenConfirm);
    }

    #[test]
    fn classify_no_extension_is_probe() {
        let u = Url::parse("https://example.com/page").unwrap();
        assert_eq!(classify(&u), Kind::ProbeThenConfirm);
    }

    #[test]
    fn classify_https_file_with_query_is_direct() {
        // Even with query params, a .zip path is direct
        let u = Url::parse("https://example.com/file.zip?token=abc").unwrap();
        assert_eq!(classify(&u), Kind::EnqueueDirect);
    }

    #[test]
    fn classify_non_http_scheme_is_probe() {
        let u = Url::parse("ftp://example.com/file.zip").unwrap();
        assert_eq!(classify(&u), Kind::ProbeThenConfirm);
    }
}
