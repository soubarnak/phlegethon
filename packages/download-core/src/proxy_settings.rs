//! Global HTTP(S)/SOCKS proxy settings for Segmented Transfer and Extractor Toolchain.

use serde::{Deserialize, Serialize};

/// Global proxy configuration. Empty / unset means direct connectivity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProxySettings {
    /// Proxy URL (`http://`, `https://`, `socks4://`, `socks5://`).
    #[serde(default)]
    pub url: Option<String>,
}

impl ProxySettings {
    /// Whether a proxy URL is configured.
    pub fn is_configured(&self) -> bool {
        self.url.as_ref().is_some_and(|s| !s.trim().is_empty())
    }

    /// Validate before persistence.
    pub fn validate(&self) -> Result<(), String> {
        let Some(url) = self.url.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let parsed = url::Url::parse(url).map_err(|e| format!("invalid proxy URL: {e}"))?;
        match parsed.scheme() {
            "http" | "https" | "socks4" | "socks4a" | "socks5" | "socks5h" => Ok(()),
            other => Err(format!(
                "proxy URL scheme must be http, https, socks4, or socks5, got: {other}"
            )),
        }
    }

    /// Trim whitespace and clear empty strings.
    pub fn normalized(&self) -> Self {
        let url = self
            .url
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Self { url }
    }
}

/// Build yt-dlp argv fragments for the configured global proxy.
pub fn ytdlp_proxy_args(settings: &ProxySettings) -> Vec<String> {
    let Some(url) = settings
        .url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Vec::new();
    };
    vec!["--proxy".to_string(), url.to_string()]
}

/// Append proxy argv fragments to a yt-dlp command builder.
pub fn append_ytdlp_proxy_args(cmd: &mut std::process::Command, settings: &ProxySettings) {
    for arg in ytdlp_proxy_args(settings) {
        cmd.arg(arg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ytdlp_proxy_args_empty_when_unconfigured() {
        assert!(ytdlp_proxy_args(&ProxySettings::default()).is_empty());
    }

    #[test]
    fn ytdlp_proxy_args_includes_proxy_flag() {
        let settings = ProxySettings {
            url: Some("socks5://127.0.0.1:1080".to_string()),
        };
        assert_eq!(
            ytdlp_proxy_args(&settings),
            vec!["--proxy".to_string(), "socks5://127.0.0.1:1080".to_string()]
        );
    }

    #[test]
    fn validate_accepts_http_and_socks_schemes() {
        for url in [
            "http://proxy.example.com:8080",
            "https://proxy.example.com:8443",
            "socks5://user:pass@proxy:1080",
            "socks4://127.0.0.1:1080",
        ] {
            ProxySettings {
                url: Some(url.to_string()),
            }
            .validate()
            .unwrap_or_else(|e| panic!("{url}: {e}"));
        }
    }

    #[test]
    fn validate_rejects_unknown_scheme() {
        let err = ProxySettings {
            url: Some("ftp://proxy.example.com".to_string()),
        }
        .validate()
        .unwrap_err();
        assert!(err.contains("scheme"), "got: {err}");
    }

    #[test]
    fn normalized_clears_blank_url() {
        let settings = ProxySettings {
            url: Some("   ".to_string()),
        };
        assert_eq!(settings.normalized(), ProxySettings::default());
    }
}
