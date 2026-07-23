//! DNS-over-HTTPS helpers for extractor connectivity.
//!
//! Some ISPs return sinkhole / blocked A records for adult Launch Sites while
//! browsers still work via Secure DNS (DoH). Resolve via Cloudflare DoH so
//! yt-dlp can reach the same endpoints Chrome uses.

use std::net::IpAddr;

/// Resolve A records for `host` via Cloudflare DNS-over-HTTPS.
pub fn resolve_a_records(host: &str) -> Result<Vec<IpAddr>, String> {
    if host.is_empty() || host == "localhost" {
        return Err("refusing DoH resolve for empty/localhost host".into());
    }

    let url = format!("https://cloudflare-dns.com/dns-query?name={host}&type=A");
    let response = ureq::get(&url)
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
