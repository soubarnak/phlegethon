//! Ephemeral HTTP CONNECT proxy that dials DoH-resolved IPs.
//!
//! yt-dlp always uses the system resolver. When ISP DNS is poisoned, route
//! extractor traffic through this localhost proxy so CONNECT targets hit the
//! Cloudflare (or other DoH) addresses browsers use.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Guard that keeps the proxy thread alive until dropped.
pub struct DohProxyGuard {
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    pub proxy_url: String,
}

impl Drop for DohProxyGuard {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Ok(addr) = self
            .proxy_url
            .trim_start_matches("http://")
            .parse::<SocketAddr>()
        {
            let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(200));
        }
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// Start a CONNECT proxy on 127.0.0.1 that maps hostnames to fixed IPs.
pub fn start_doh_connect_proxy(
    host_overrides: HashMap<String, IpAddr>,
) -> Result<DohProxyGuard, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("bind DoH CONNECT proxy: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("DoH proxy set nonblocking: {e}"))?;

    let addr = listener
        .local_addr()
        .map_err(|e| format!("DoH proxy local_addr: {e}"))?;
    let proxy_url = format!("http://{addr}");
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_flag = Arc::clone(&shutdown);
    let overrides = Arc::new(host_overrides);

    let join = thread::spawn(move || {
        while !shutdown_flag.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    if shutdown_flag.load(Ordering::SeqCst) {
                        break;
                    }
                    let map = Arc::clone(&overrides);
                    thread::spawn(move || {
                        let _ = handle_client(stream, &map);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => thread::sleep(Duration::from_millis(10)),
            }
        }
    });

    // Let accept loop start before clients connect.
    thread::sleep(Duration::from_millis(100));

    Ok(DohProxyGuard {
        shutdown,
        join: Some(join),
        proxy_url,
    })
}

fn handle_client(mut client: TcpStream, overrides: &HashMap<String, IpAddr>) -> std::io::Result<()> {
    let header_bytes = read_http_headers(&mut client)?;
    let req = String::from_utf8_lossy(&header_bytes);
    let first_line = req.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if !method.eq_ignore_ascii_case("CONNECT") {
        let _ = client.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\n\r\n");
        return Ok(());
    }

    let (host, port) = split_host_port(target);
    let host_key = host.to_ascii_lowercase();

    let ip = resolve_connect_ip(&host_key, overrides);
    let Some(ip) = ip else {
        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n");
        return Ok(());
    };

    let server_addr = SocketAddr::new(ip, port);
    let mut server = match TcpStream::connect_timeout(&server_addr, Duration::from_secs(20)) {
        Ok(s) => s,
        Err(_) => {
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n");
            return Ok(());
        }
    };

    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    let _ = client.flush();
    let _ = client.set_nodelay(true);
    let _ = server.set_nodelay(true);

    tunnel_poll(&mut client, &mut server)?;
    Ok(())
}

/// Bidirectional relay matching the select-loop pattern used by working Python proxies.
fn tunnel_poll(client: &mut TcpStream, server: &mut TcpStream) -> std::io::Result<()> {
    client.set_nonblocking(true)?;
    server.set_nonblocking(true)?;
    let mut buf = [0u8; 16384];
    let mut client_to_server_open = true;
    let mut server_to_client_open = true;

    while client_to_server_open || server_to_client_open {
        if client_to_server_open {
            match client.read(&mut buf) {
                Ok(0) => {
                    client_to_server_open = false;
                    let _ = server.shutdown(Shutdown::Write);
                }
                Ok(n) => server.write_all(&buf[..n])?,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
        }
        if server_to_client_open {
            match server.read(&mut buf) {
                Ok(0) => {
                    server_to_client_open = false;
                    let _ = client.shutdown(Shutdown::Write);
                }
                Ok(n) => client.write_all(&buf[..n])?,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
        }
        if client_to_server_open || server_to_client_open {
            thread::sleep(Duration::from_millis(1));
        }
    }
    Ok(())
}

fn resolve_connect_ip(host: &str, overrides: &HashMap<String, IpAddr>) -> Option<IpAddr> {
    if let Some(ip) = overrides.get(host) {
        return Some(*ip);
    }
    if let Some(rest) = host.strip_prefix("www.") {
        if let Some(ip) = overrides.get(rest) {
            return Some(*ip);
        }
    } else if let Some(ip) = overrides.get(&format!("www.{host}")) {
        return Some(*ip);
    }

    // CDN / redirect hosts: use system DNS (usually fine for those).
    use std::net::ToSocketAddrs;
    format!("{host}:443")
        .to_socket_addrs()
        .ok()?
        .map(|a| a.ip())
        .next()
}

fn read_http_headers(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    while buf.len() < 65536 {
        let n = stream.read(&mut byte)?;
        if n == 0 {
            break;
        }
        buf.push(byte[0]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    Ok(buf)
}

fn split_host_port(target: &str) -> (String, u16) {
    if let Some((h, p)) = target.rsplit_once(':') {
        if let Ok(port) = p.parse::<u16>() {
            return (
                h.trim_matches(|c| c == '[' || c == ']').to_string(),
                port,
            );
        }
    }
    (target.to_string(), 443)
}

/// Build host→IP map for a URL's host via DoH. Returns None when override not needed.
pub fn doh_override_map_for_url(url: &str) -> Result<Option<HashMap<String, IpAddr>>, String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid URL for DoH map: {e}"))?;
    let host = match parsed.host_str() {
        Some(h) => h.to_ascii_lowercase(),
        None => return Ok(None),
    };

    let doh_ips = crate::doh_dns::resolve_a_records(&host)?;
    if !crate::doh_dns::system_dns_diverges_from_doh(&host, &doh_ips) {
        return Ok(None);
    }

    let mut map = HashMap::new();
    map.insert(host.clone(), doh_ips[0]);
    if let Some(rest) = host.strip_prefix("www.") {
        map.insert(rest.to_string(), doh_ips[0]);
    } else {
        map.insert(format!("www.{host}"), doh_ips[0]);
    }
    Ok(Some(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    #[ignore = "live network + yt-dlp"]
    fn live_rust_connect_proxy_ytdlp_probe() {
        let map = doh_override_map_for_url("https://xhamster.com/creators/your-eshita")
            .expect("map")
            .expect("override needed");
        let guard = start_doh_connect_proxy(map).expect("proxy start");
        let output = std::process::Command::new("yt-dlp")
            .args([
                "--proxy",
                &guard.proxy_url,
                "--flat-playlist",
                "-J",
                "https://xhamster.com/creators/your-eshita",
            ])
            .output()
            .expect("spawn yt-dlp");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "yt-dlp via Rust CONNECT proxy failed: {stderr}"
        );
        let json: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("parse json");
        let count = json
            .get("entries")
            .and_then(|e| e.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert!(count > 1, "expected playlist entries, got {count}");
    }

    #[test]
    fn split_host_port_defaults_443() {
        assert_eq!(
            split_host_port("xhamster.com"),
            ("xhamster.com".into(), 443)
        );
        assert_eq!(
            split_host_port("xhamster.com:8443"),
            ("xhamster.com".into(), 8443)
        );
    }

    #[test]
    fn start_and_drop_proxy() {
        let mut map = HashMap::new();
        map.insert(
            "example.com".to_string(),
            IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
        );
        let guard = start_doh_connect_proxy(map).expect("start proxy");
        assert!(guard.proxy_url.starts_with("http://127.0.0.1:"));
        drop(guard);
    }
}
