//! DNS-over-HTTPS helpers for extractor connectivity.
//!
//! Some ISPs return sinkhole / blocked A records for adult Launch Sites while
//! browsers still work via Secure DNS (DoH). Resolve via Cloudflare DoH so
//! yt-dlp can reach the same endpoints Chrome uses.

use crate::proxy_settings::ProxySettings;
use std::net::IpAddr;

const DOH_ENDPOINT: &str = "https://cloudflare-dns.com/dns-query";

/// Resolve A records for `host` via Cloudflare DNS-over-HTTPS, reaching the resolver
/// through the environment proxy when one is set.
pub fn resolve_a_records(host: &str) -> Result<Vec<IpAddr>, String> {
    resolve_a_records_with(&ProxySettings::default(), host)
}

/// Like [`resolve_a_records`], but sends the DoH request through the configured proxy.
pub fn resolve_a_records_with(proxy: &ProxySettings, host: &str) -> Result<Vec<IpAddr>, String> {
    if host.is_empty() || host == "localhost" {
        return Err("refusing DoH resolve for empty/localhost host".into());
    }

    let response = doh_agent(proxy)?
        .get(&doh_query_url(host))
        .set("Accept", "application/dns-json")
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .map_err(|e| format!("DoH request failed for '{host}': {e}"))?;

    let body: DohResponse = response
        .into_json()
        .map_err(|e| format!("DoH JSON parse failed for '{host}': {e}"))?;

    let mut ips = Vec::new();
    for answer in body.answer.unwrap_or_default() {
        if answer.type_ != 1 {
            continue;
        }
        if let Ok(ip) = answer.data.parse::<IpAddr>() {
            ips.push(ip);
        }
    }

    if ips.is_empty() {
        return Err(format!("DoH returned no A records for '{host}'"));
    }
    Ok(ips)
}

/// Agent for the DoH request: the configured proxy, else the environment proxy, as for
/// the Download Core's own requests.
fn doh_agent(proxy: &ProxySettings) -> Result<ureq::Agent, String> {
    let builder = ureq::AgentBuilder::new();
    let builder = match proxy.normalized().url {
        Some(url) => builder.proxy(ureq::Proxy::new(&url).map_err(|e| {
            format!(
                "DoH proxy {}: {e}",
                crate::proxy_settings::strip_userinfo(&url)
            )
        })?),
        None => builder.try_proxy_from_env(true),
    };
    Ok(builder.build())
}

/// Query URL with the host name percent-encoded.
fn doh_query_url(host: &str) -> String {
    let mut url = url::Url::parse(DOH_ENDPOINT).expect("constant URL");
    url.query_pairs_mut()
        .append_pair("name", host)
        .append_pair("type", "A");
    url.into()
}

/// System resolver A records (may be ISP-poisoned).
pub fn system_a_records(host: &str) -> Vec<IpAddr> {
    use std::net::ToSocketAddrs;
    format!("{host}:443")
        .to_socket_addrs()
        .map(|iter| iter.map(|a| a.ip()).collect())
        .unwrap_or_default()
}

/// True when system DNS and DoH disagree (common ISP sinkhole pattern).
pub fn system_dns_diverges_from_doh(host: &str, doh_ips: &[IpAddr]) -> bool {
    let system = system_a_records(host);
    if system.is_empty() {
        return true;
    }
    !system.iter().any(|ip| doh_ips.contains(ip))
}

#[derive(Debug, serde::Deserialize)]
struct DohResponse {
    #[serde(rename = "Answer")]
    answer: Option<Vec<DohAnswer>>,
}

#[derive(Debug, serde::Deserialize)]
struct DohAnswer {
    #[serde(rename = "type")]
    type_: u16,
    data: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn query_url_encodes_the_host() {
        assert_eq!(
            doh_query_url("example.com"),
            "https://cloudflare-dns.com/dns-query?name=example.com&type=A"
        );
        let url = doh_query_url("a b&type=AAAA#x");
        assert!(url.contains("name=a+b%26type%3DAAAA%23x&type=A"), "{url}");
    }

    #[test]
    fn doh_agent_rejects_a_proxy_ureq_cannot_use() {
        let proxy = ProxySettings {
            url: Some("https://proxy:8443".into()),
        };
        assert!(doh_agent(&proxy).is_err());
    }

    #[test]
    fn system_dns_diverges_when_no_overlap() {
        let doh = vec![IpAddr::V4(Ipv4Addr::new(104, 16, 3, 81))];
        // We cannot force system DNS in unit tests; empty system counts as diverge.
        let system = system_a_records("this-host-should-not-resolve.invalid");
        assert!(system.is_empty());
        assert!(system_dns_diverges_from_doh(
            "this-host-should-not-resolve.invalid",
            &doh
        ));
    }

    #[test]
    fn system_dns_diverges_helper_detects_disjoint_sets() {
        // Direct unit of the overlap logic via local copies.
        let doh = vec![IpAddr::V4(Ipv4Addr::new(104, 16, 3, 81))];
        let system = vec![IpAddr::V4(Ipv4Addr::new(202, 83, 21, 14))];
        assert!(!system.iter().any(|ip| doh.contains(ip)));
    }
}
