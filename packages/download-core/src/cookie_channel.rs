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
