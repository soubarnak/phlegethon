//! Global HTTP(S)/SOCKS proxy settings for Segmented Transfer and Extractor Toolchain.

use serde::{Deserialize, Serialize};

/// Global proxy configuration. Empty / unset means direct connectivity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProxySettings {
    /// Proxy URL (`http://`, `socks4://`, `socks4a://`, `socks5://`).
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
            "http" | "socks4" | "socks4a" | "socks5" => {}
            other => {
                return Err(format!(
                    "proxy URL scheme must be http, socks4, socks4a, or socks5, got: {other}"
                ))
            }
        }
        // The HTTP client is the authority on what it can use, so a URL that passes here
        // can always be turned into an agent.
        ureq::Proxy::new(url).map_err(|e| format!("invalid proxy URL: {e}"))?;
        Ok(())
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

/// Hosts that bypass an environment proxy, parsed from `NO_PROXY` / `no_proxy`.
///
/// `ureq` 2 ignores `NO_PROXY`, so the HTTP backend consults this to pick a direct agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NoProxy {
    entries: Vec<NoProxyEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NoProxyEntry {
    All,
    Domain {
        name: String,
        port: Option<u16>,
    },
    Cidr {
        addr: std::net::IpAddr,
        prefix: u8,
        port: Option<u16>,
    },
}

impl NoProxy {
    /// Read `NO_PROXY`, falling back to `no_proxy`.
    pub(crate) fn from_env() -> Self {
        let raw = std::env::var("NO_PROXY")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| std::env::var("no_proxy").ok())
            .unwrap_or_default();
        Self::parse(&raw)
    }

    /// Parse a comma-separated list: `*`, domain suffixes (`example.com`, `.example.com`,
    /// `*.example.com`), IP literals and CIDR ranges, each with an optional `:port`.
    pub(crate) fn parse(list: &str) -> Self {
        let entries = list
            .split(',')
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .filter_map(|e| Self::parse_entry(&e))
            .collect();
        Self { entries }
    }

    fn parse_entry(entry: &str) -> Option<NoProxyEntry> {
        if entry == "*" {
            return Some(NoProxyEntry::All);
        }
        let (host, port) = split_entry_port(entry);
        if host.is_empty() {
            return None;
        }
        if let Some((ip, prefix)) = host.split_once('/') {
            let addr: std::net::IpAddr = ip.trim_matches(['[', ']']).parse().ok()?;
            let max = if addr.is_ipv4() { 32 } else { 128 };
            let prefix: u8 = prefix.parse().ok().filter(|p| *p <= max)?;
            return Some(NoProxyEntry::Cidr { addr, prefix, port });
        }
        if let Ok(addr) = host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
            let prefix = if addr.is_ipv4() { 32 } else { 128 };
            return Some(NoProxyEntry::Cidr { addr, prefix, port });
        }
        let name = host.trim_start_matches("*.").trim_start_matches('.');
        if name.is_empty() {
            return None;
        }
        Some(NoProxyEntry::Domain {
            name: name.to_string(),
            port,
        })
    }

    /// Whether the host of `url` is listed, so the request must not use an environment proxy.
    pub(crate) fn matches_url(&self, url: &str) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let Ok(parsed) = url::Url::parse(url) else {
            return false;
        };
        let port = parsed.port_or_known_default();
        match parsed.host() {
            Some(url::Host::Domain(d)) => self.matches_domain(&d.to_ascii_lowercase(), port),
            Some(url::Host::Ipv4(ip)) => self.matches_ip(std::net::IpAddr::V4(ip), port),
            Some(url::Host::Ipv6(ip)) => self.matches_ip(std::net::IpAddr::V6(ip), port),
            None => false,
        }
    }

    fn matches_domain(&self, host: &str, port: Option<u16>) -> bool {
        self.entries.iter().any(|e| match e {
            NoProxyEntry::All => true,
            NoProxyEntry::Domain { name, port: p } => {
                port_ok(*p, port)
                    && (host == name
                        || host
                            .strip_suffix(name.as_str())
                            .is_some_and(|rest| rest.ends_with('.')))
            }
            NoProxyEntry::Cidr { .. } => false,
        })
    }

    fn matches_ip(&self, ip: std::net::IpAddr, port: Option<u16>) -> bool {
        self.entries.iter().any(|e| match e {
            NoProxyEntry::All => true,
            NoProxyEntry::Domain { name, port: p } => port_ok(*p, port) && ip.to_string() == *name,
            NoProxyEntry::Cidr {
                addr,
                prefix,
                port: p,
            } => port_ok(*p, port) && cidr_contains(*addr, *prefix, ip),
        })
    }
}

fn port_ok(wanted: Option<u16>, actual: Option<u16>) -> bool {
    wanted.is_none_or(|w| Some(w) == actual)
}

fn split_entry_port(entry: &str) -> (&str, Option<u16>) {
    if let Some(rest) = entry.strip_prefix('[') {
        // `[::1]:8080` or `[::1]`
        if let Some((addr, tail)) = rest.split_once(']') {
            let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
            return (addr, port);
        }
    }
    // A single colon is `host:port`; several are a bare IPv6 literal.
    if entry.matches(':').count() == 1 {
        if let Some((host, port)) = entry.split_once(':') {
            if let Ok(port) = port.parse() {
                return (host, Some(port));
            }
        }
    }
    (entry, None)
}

fn cidr_contains(net: std::net::IpAddr, prefix: u8, ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    let (a, b, bits) = match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(i)) => (u128::from(u32::from(n)), u128::from(u32::from(i)), 32),
        (IpAddr::V6(n), IpAddr::V6(i)) => (u128::from(n), u128::from(i), 128),
        _ => return false,
    };
    if prefix == 0 {
        return true;
    }
    let shift = u32::from(bits - prefix);
    (a >> shift) == (b >> shift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_http_and_socks_schemes() {
        for url in [
            "http://proxy.example.com:8080",
            "socks5://user:pass@proxy:1080",
            "socks4://127.0.0.1:1080",
            "socks4a://127.0.0.1:1080",
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
    fn validate_rejects_schemes_the_http_client_cannot_use() {
        // H-16: ureq 2 has no https or socks5h proxy support; accepting them let the
        // Download Core fall back to a direct connection while yt-dlp used the proxy.
        for url in [
            "https://corp-proxy:8443",
            "socks5h://proxy:1080",
            "ftp://proxy:21",
        ] {
            let err = ProxySettings {
                url: Some(url.to_string()),
            }
            .validate()
            .unwrap_err();
            assert!(err.contains("scheme"), "{url}: {err}");
        }
    }

    #[test]
    fn validate_rejects_url_the_http_client_cannot_parse() {
        let err = ProxySettings {
            url: Some("http://user@proxy:8080".to_string()),
        }
        .validate()
        .unwrap_err();
        assert!(err.contains("invalid proxy URL"), "got: {err}");
    }

    #[test]
    fn no_proxy_matches_hosts_suffixes_and_ranges() {
        let np = NoProxy::parse("localhost, .corp.example, *.lan, 10.0.0.0/8, [::1], intra:8080");
        assert!(np.matches_url("http://localhost/x"));
        assert!(np.matches_url("https://git.corp.example/x"));
        assert!(np.matches_url("https://corp.example/x"));
        assert!(np.matches_url("http://nas.lan/x"));
        assert!(np.matches_url("http://10.2.3.4/x"));
        assert!(np.matches_url("http://[::1]:3000/x"));
        assert!(np.matches_url("http://intra:8080/x"));
        assert!(!np.matches_url("http://intra:9090/x"));
        assert!(!np.matches_url("http://notcorp.example/x"));
        assert!(!np.matches_url("http://11.0.0.1/x"));
        assert!(!np.matches_url("https://example.com/x"));
    }

    #[test]
    fn no_proxy_star_matches_everything_and_empty_nothing() {
        assert!(NoProxy::parse("*").matches_url("https://example.com/"));
        assert!(!NoProxy::parse("").matches_url("https://example.com/"));
        assert!(!NoProxy::parse(" , ").matches_url("https://example.com/"));
    }

    #[test]
    fn normalized_clears_blank_url() {
        let settings = ProxySettings {
            url: Some("   ".to_string()),
        };
        assert_eq!(settings.normalized(), ProxySettings::default());
    }
}
