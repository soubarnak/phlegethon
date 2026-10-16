//! Cookie auth channel — single Core-owned cookie source for extractor probe/download.
//!
//! Desktop settings and future Chrome Companion both feed this channel (no second store).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Cookie configuration passed to yt-dlp for authenticated Launch Sites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CookieChannel {
    /// Browser name for yt-dlp `--cookies-from-browser` (e.g. `chrome`, `firefox`, `edge`).
    #[serde(default)]
    pub cookies_from_browser: Option<String>,
    /// Absolute path to a Netscape cookie file for yt-dlp `--cookies`.
    #[serde(default)]
    pub cookie_file: Option<PathBuf>,
}

impl CookieChannel {
    /// Whether any cookie source is configured.
    pub fn is_configured(&self) -> bool {
        self.cookies_from_browser
            .as_ref()
            .is_some_and(|s| !s.trim().is_empty())
            || self.cookie_file.is_some()
    }

    /// Validate and normalize settings before persistence.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(browser) = &self.cookies_from_browser {
            if browser.trim().is_empty() {
                return Err("cookies-from-browser must not be empty when set".to_string());
            }
        }
        if let Some(path) = &self.cookie_file {
            if !path.is_absolute() {
                return Err(format!(
                    "cookie file must be an absolute path, got: {}",
                    path.display()
                ));
            }
        }
        Ok(())
    }

    /// Normalized copy with trimmed browser name and empty strings cleared.
    pub fn normalized(&self) -> Self {
        let cookies_from_browser = self
            .cookies_from_browser
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let cookie_file = self.cookie_file.clone();
        Self {
            cookies_from_browser,
            cookie_file,
        }
    }
}

/// A private, short-lived copy of the user's cookie file, deleted on drop.
///
/// yt-dlp writes its cookie jar back to the `--cookies` file on exit, so handing it the
/// user's own file lets it rewrite or truncate it. Each run gets a throwaway copy.
#[derive(Debug)]
pub struct CookieFileCopy {
    path: PathBuf,
}

impl CookieFileCopy {
    fn create(original: &std::path::Path) -> Result<Self, String> {
        use std::io::Write;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("phlegethon-cookies-{}-{n}.txt", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut copy = options
            .open(&path)
            .map_err(|e| format!("create cookie file copy: {e}"))?;
        let guard = Self { path };
        match std::fs::read(original) {
            Ok(bytes) => copy
                .write_all(&bytes)
                .map_err(|e| format!("copy cookie file: {e}"))?,
            // yt-dlp treats a missing cookie file as an empty jar.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("read cookie file {}: {e}", original.display())),
        }
        Ok(guard)
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for CookieFileCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl CookieChannel {
    /// The channel to hand to yt-dlp for one run: `cookie_file` replaced by a private
    /// copy that lives as long as the returned guard.
    pub fn with_private_cookie_file(&self) -> Result<(Self, Option<CookieFileCopy>), String> {
        let Some(original) = &self.cookie_file else {
            return Ok((self.clone(), None));
        };
        let copy = CookieFileCopy::create(original)?;
        let channel = Self {
            cookies_from_browser: self.cookies_from_browser.clone(),
            cookie_file: Some(copy.path().to_path_buf()),
        };
        Ok((channel, Some(copy)))
    }
}

/// Build yt-dlp argv fragments for the configured cookie channel.
pub fn ytdlp_cookie_args(channel: &CookieChannel) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(browser) = channel
        .cookies_from_browser
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        args.push("--cookies-from-browser".to_string());
        args.push(browser.to_string());
    }
    if let Some(path) = &channel.cookie_file {
        args.push("--cookies".to_string());
        args.push(path.display().to_string());
    }
    args
}

/// Append cookie argv fragments to a yt-dlp command builder.
pub fn append_ytdlp_cookie_args(cmd: &mut std::process::Command, channel: &CookieChannel) {
    for arg in ytdlp_cookie_args(channel) {
        cmd.arg(arg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn private_cookie_copy_keeps_the_original_and_is_removed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("cookies.txt");
        std::fs::write(
            &original,
            "# Netscape HTTP Cookie File\n.example.com\tTRUE\t/\tTRUE\t0\tk\tv\n",
        )
        .unwrap();
        let channel = CookieChannel {
            cookies_from_browser: None,
            cookie_file: Some(original.clone()),
        };
        let (private, guard) = channel.with_private_cookie_file().unwrap();
        let guard = guard.expect("a cookie file gets a copy");
        let copy = private.cookie_file.clone().unwrap();
        assert_ne!(copy, original);
        assert_eq!(
            std::fs::read(&copy).unwrap(),
            std::fs::read(&original).unwrap()
        );
        // yt-dlp rewrites the file it is given; the user's file must not change.
        std::fs::write(&copy, "rewritten by yt-dlp").unwrap();
        assert!(std::fs::read_to_string(&original)
            .unwrap()
            .contains("Netscape"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(guard);
        assert!(!copy.exists());
    }

    #[test]
    fn private_cookie_copy_of_a_missing_file_is_empty_and_leaves_no_original() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("absent.txt");
        let channel = CookieChannel {
            cookies_from_browser: Some("firefox".into()),
            cookie_file: Some(original.clone()),
        };
        let (private, guard) = channel.with_private_cookie_file().unwrap();
        assert_eq!(private.cookies_from_browser.as_deref(), Some("firefox"));
        assert_eq!(
            std::fs::read(private.cookie_file.as_ref().unwrap()).unwrap(),
            b""
        );
        drop(guard);
        assert!(!original.exists());
        let (same, none) = CookieChannel::default().with_private_cookie_file().unwrap();
        assert_eq!(same, CookieChannel::default());
        assert!(none.is_none());
    }

    #[test]
    fn ytdlp_cookie_args_empty_when_unconfigured() {
        assert!(ytdlp_cookie_args(&CookieChannel::default()).is_empty());
    }

    #[test]
    fn ytdlp_cookie_args_from_browser() {
        let channel = CookieChannel {
            cookies_from_browser: Some("chrome".to_string()),
            cookie_file: None,
        };
        assert_eq!(
            ytdlp_cookie_args(&channel),
            vec!["--cookies-from-browser".to_string(), "chrome".to_string()]
        );
    }

    #[test]
    fn ytdlp_cookie_args_from_file() {
        let channel = CookieChannel {
            cookies_from_browser: None,
            cookie_file: Some(PathBuf::from("C:\\cookies.txt")),
        };
        assert_eq!(
            ytdlp_cookie_args(&channel),
            vec!["--cookies".to_string(), "C:\\cookies.txt".to_string()]
        );
    }

    #[test]
    fn ytdlp_cookie_args_both_sources() {
        let channel = CookieChannel {
            cookies_from_browser: Some("firefox".to_string()),
            cookie_file: Some(PathBuf::from("/tmp/cookies.txt")),
        };
        let args = ytdlp_cookie_args(&channel);
        assert!(args
            .windows(2)
            .any(|w| w == ["--cookies-from-browser", "firefox"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["--cookies", "/tmp/cookies.txt"]));
    }

    #[test]
    fn validate_rejects_relative_cookie_file() {
        let channel = CookieChannel {
            cookies_from_browser: None,
            cookie_file: Some(PathBuf::from("cookies.txt")),
        };
        let err = channel.validate().unwrap_err();
        assert!(err.contains("absolute path"), "got: {err}");
    }

    #[test]
    fn validate_accepts_absolute_cookie_file() {
        let channel = CookieChannel {
            cookies_from_browser: Some("edge".to_string()),
            cookie_file: Some(Path::new("/tmp/data/cookies.txt").to_path_buf()),
        };
        channel.validate().unwrap();
    }
}
