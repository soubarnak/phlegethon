//! The one table of Launch Sites.
//!
//! Classification, `ProbePlan` and the yt-dlp extractor check all ask this table, so a host
//! is a Launch Site everywhere or nowhere. A host matches a domain when it equals it or ends
//! with `.<domain>`, which covers `m.xhamster.com` and `music.youtube.com` and rejects
//! `notyoutube.com`.

/// A Launch Site: a domain family and the yt-dlp extractor key prefixes expected for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchSite {
    pub domain: &'static str,
    pub extractor_keys: &'static [&'static str],
}

const LAUNCH_SITES: &[LaunchSite] = &[
    LaunchSite {
        domain: "youtube.com",
        extractor_keys: &["Youtube"],
    },
    LaunchSite {
        domain: "youtu.be",
        extractor_keys: &["Youtube"],
    },
    LaunchSite {
        domain: "xhamster.com",
        extractor_keys: &["XHamster"],
    },
    LaunchSite {
        domain: "xhamster.desi",
        extractor_keys: &["XHamster"],
    },
    LaunchSite {
        domain: "xhamster2.com",
        extractor_keys: &["XHamster"],
    },
    LaunchSite {
        domain: "xhamster3.com",
        extractor_keys: &["XHamster"],
    },
    LaunchSite {
        domain: "luxuretv.com",
        extractor_keys: &["LuxureTV"],
    },
    LaunchSite {
        domain: "xxxbp.tv",
        extractor_keys: &["XxxbpTV"],
    },
];

/// Find the Launch Site for `host` (exact or dot-suffix match, case-insensitive).
pub fn find(host: &str) -> Option<&'static LaunchSite> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    LAUNCH_SITES.iter().find(|site| {
        host == site.domain
            || host
                .strip_suffix(site.domain)
                .is_some_and(|rest| rest.ends_with('.'))
    })
}

/// Is `host` a Launch Site host?
pub fn is_launch_site_host(host: &str) -> bool {
    find(host).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_exact_hosts_and_subdomains() {
        for host in [
            "youtube.com",
            "www.youtube.com",
            "music.youtube.com",
            "m.xhamster.com",
            "de.xhamster.com",
            "en.xxxbp.tv",
            "www.luxuretv.com",
            "a.b.xhamster2.com",
            "YouTube.COM",
            "youtu.be",
        ] {
            assert!(is_launch_site_host(host), "{host}");
        }
    }

    #[test]
    fn rejects_lookalike_hosts() {
        for host in [
            "notyoutube.com",
            "youtube.com.evil.example",
            "xhamster.com.evil.example",
            "example.com",
            "",
            "com",
        ] {
            assert!(!is_launch_site_host(host), "{host}");
        }
    }

    #[test]
    fn find_returns_the_extractor_keys() {
        assert_eq!(
            find("music.youtube.com").unwrap().extractor_keys,
            ["Youtube"]
        );
        assert_eq!(find("en.xxxbp.tv").unwrap().extractor_keys, ["XxxbpTV"]);
        assert_eq!(
            find("de.xhamster.com").unwrap().extractor_keys,
            ["XHamster"]
        );
    }
}
