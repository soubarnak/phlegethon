//! Ephemeral HTTP CONNECT proxy that dials DoH-resolved IPs.
//!
//! yt-dlp always uses the system resolver. When ISP DNS is poisoned, route
//! extractor traffic through this localhost proxy so CONNECT targets hit the
//! Cloudflare (or other DoH) addresses browsers use.
//!
//! The proxy listens on loopback, so any local process can reach it. Each run
//! therefore has a random credential (`Proxy-Authorization: Basic`), refuses
//! loopback and link-local targets, times out idle and half-open clients, and
//! caps concurrent connections.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Largest request head accepted from a client.
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// How often a blocked tunnel read wakes up to check for idleness and shutdown.
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const RELAY_BUFFER_BYTES: usize = 16 * 1024;
const PROXY_USER: &str = "phlegethon";

/// Limits for one proxy run. [`Default`] is what production uses.
#[derive(Debug, Clone)]
pub(crate) struct ProxyOptions {
    /// Dial loopback, unspecified, link-local and multicast addresses. Tests only.
    pub allow_blocked_targets: bool,
    /// Concurrent client connections; further ones get `503`.
    pub max_connections: usize,
    /// Total time a client has to send its request head.
    pub header_timeout: Duration,
    /// A tunnel with no bytes in either direction for this long is closed.
    pub idle_timeout: Duration,
    /// Time a single write may block on a stalled peer.
    pub write_timeout: Duration,
    pub connect_timeout: Duration,
}

impl Default for ProxyOptions {
    fn default() -> Self {
        Self {
            allow_blocked_targets: false,
            max_connections: 64,
            header_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(120),
            write_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(20),
        }
    }
}

/// State shared by every client thread of one proxy run.
struct Shared {
    overrides: HashMap<String, IpAddr>,
    resolver: HostResolver,
    options: ProxyOptions,
    /// Expected `Proxy-Authorization` header value.
    expected_auth: String,
    active: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
}

/// Counts one live client connection and releases it on drop.
struct ConnectionSlot(Arc<AtomicUsize>);

impl ConnectionSlot {
    fn acquire(active: &Arc<AtomicUsize>, max: usize) -> Option<Self> {
        let prev = active.fetch_add(1, Ordering::SeqCst);
        if prev >= max {
            active.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(Arc::clone(active)))
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Guard that keeps the proxy thread alive until dropped.
pub struct DohProxyGuard {
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    /// `http://phlegethon:<token>@127.0.0.1:<port>`. The token is random per run, so
    /// keep the URL out of the process list and logs.
    pub proxy_url: String,
}

/// Resolves a CONNECT host name that has no fixed override.
pub(crate) type HostResolver = Arc<dyn Fn(&str) -> Option<IpAddr> + Send + Sync>;
type DohLookup = Box<dyn Fn(&str) -> Result<Vec<IpAddr>, String> + Send + Sync>;
type SystemLookup = Box<dyn Fn(&str) -> Option<IpAddr> + Send + Sync>;
type Report = Box<dyn Fn(&str) + Send + Sync>;

/// Resolves host names through DoH, remembers answers, and falls back to the system
/// resolver when DoH fails. Every fallback is passed to `report`.
pub(crate) struct DohResolver {
    lookup: DohLookup,
    fallback: SystemLookup,
    cache: Mutex<HashMap<String, (IpAddr, Instant)>>,
    report: Report,
}

const DOH_CACHE_TTL: Duration = Duration::from_secs(300);

impl DohResolver {
    pub(crate) fn new(
        lookup: impl Fn(&str) -> Result<Vec<IpAddr>, String> + Send + Sync + 'static,
        fallback: impl Fn(&str) -> Option<IpAddr> + Send + Sync + 'static,
        report: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        Self {
            lookup: Box::new(lookup),
            fallback: Box::new(fallback),
            cache: Mutex::new(HashMap::new()),
            report: Box::new(report),
        }
    }

    pub(crate) fn resolve(&self, host: &str) -> Option<IpAddr> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Some(ip);
        }
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((ip, at)) = cache.get(host) {
                if at.elapsed() < DOH_CACHE_TTL {
                    return Some(*ip);
                }
            }
        }
        match (self.lookup)(host) {
            Ok(ips) if !ips.is_empty() => {
                self.cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(host.to_string(), (ips[0], Instant::now()));
                Some(ips[0])
            }
            outcome => {
                let why = outcome.err().unwrap_or_else(|| "no A records".into());
                (self.report)(&format!(
                    "DoH lookup for {host} failed ({why}); using system DNS"
                ));
                (self.fallback)(host)
            }
        }
    }
}

impl Drop for DohProxyGuard {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// Start a CONNECT proxy on 127.0.0.1.
///
/// Hosts in `host_overrides` use their fixed IP. Every other CONNECT host is resolved
/// through DoH, so CDN hosts that the ISP sinkholes work too (M-22). A failed lookup falls
/// back to system DNS and is reported on stderr.
pub fn start_doh_connect_proxy(
    host_overrides: HashMap<String, IpAddr>,
) -> Result<DohProxyGuard, String> {
    let resolver = Arc::new(DohResolver::new(
        crate::doh_dns::resolve_a_records,
        system_resolve,
        |note| eprintln!("{note}"),
    ));
    start_with_resolver(
        host_overrides,
        ProxyOptions::default(),
        Arc::new(move |host| resolver.resolve(host)),
    )
}

#[cfg(test)]
pub(crate) fn start_with_options(
    host_overrides: HashMap<String, IpAddr>,
    options: ProxyOptions,
) -> Result<DohProxyGuard, String> {
    start_with_resolver(host_overrides, options, Arc::new(system_resolve))
}

pub(crate) fn start_with_resolver(
    host_overrides: HashMap<String, IpAddr>,
    options: ProxyOptions,
    resolver: HostResolver,
) -> Result<DohProxyGuard, String> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind DoH CONNECT proxy: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("DoH proxy set nonblocking: {e}"))?;

    let addr = listener
        .local_addr()
        .map_err(|e| format!("DoH proxy local_addr: {e}"))?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let proxy_url = format!("http://{PROXY_USER}:{token}@{addr}");
    let shutdown = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Shared {
        overrides: host_overrides,
        resolver,
        options,
        expected_auth: format!(
            "Basic {}",
            base64(format!("{PROXY_USER}:{token}").as_bytes())
        ),
        active: Arc::new(AtomicUsize::new(0)),
        shutdown: Arc::clone(&shutdown),
    });

    let join = thread::spawn(move || {
        while !shared.shutdown.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let Some(slot) =
                        ConnectionSlot::acquire(&shared.active, shared.options.max_connections)
                    else {
                        reject(&stream, "503 Service Unavailable");
                        continue;
                    };
                    let shared = Arc::clone(&shared);
                    thread::spawn(move || {
                        let _slot = slot;
                        let _ = handle_client(stream, &shared);
                    });
                }
                Err(_) => thread::sleep(Duration::from_millis(10)),
            }
        }
    });

    Ok(DohProxyGuard {
        shutdown,
        join: Some(join),
        proxy_url,
    })
}

fn reject(mut stream: &TcpStream, status: &str) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let _ = stream.write_all(format!("HTTP/1.1 {status}\r\nConnection: close\r\n\r\n").as_bytes());
}

fn handle_client(mut client: TcpStream, shared: &Shared) -> io::Result<()> {
    let options = &shared.options;
    // Accepted sockets must block: the relay uses one thread per direction.
    client.set_nonblocking(false)?;
    client.set_write_timeout(Some(options.write_timeout))?;

    let header_bytes = read_http_headers(&mut client, options.header_timeout)?;
    let req = String::from_utf8_lossy(&header_bytes);
    let mut lines = req.lines();
    let first_line = lines.next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    let authorized = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("proxy-authorization"))
        .is_some_and(|(_, value)| constant_time_eq(value.trim(), &shared.expected_auth));
    if !authorized {
        let _ = client.write_all(
            b"HTTP/1.1 407 Proxy Authentication Required\r\n\
              Proxy-Authenticate: Basic realm=\"phlegethon\"\r\n\
              Connection: close\r\n\r\n",
        );
        return Ok(());
    }

    if !method.eq_ignore_ascii_case("CONNECT") {
        reject(&client, "405 Method Not Allowed");
        return Ok(());
    }

    let (host, port) = split_host_port(target);
    let host_key = host.to_ascii_lowercase();
    if port == 0 || (!options.allow_blocked_targets && is_localhost_name(&host_key)) {
        reject(&client, "403 Forbidden");
        return Ok(());
    }

    let Some(ip) = resolve_connect_ip(&host_key, &shared.overrides, &shared.resolver) else {
        reject(&client, "502 Bad Gateway");
        return Ok(());
    };
    if !options.allow_blocked_targets && is_blocked_target(ip) {
        reject(&client, "403 Forbidden");
        return Ok(());
    }

    let server_addr = SocketAddr::new(ip, port);
    let Ok(server) = TcpStream::connect_timeout(&server_addr, options.connect_timeout) else {
        reject(&client, "502 Bad Gateway");
        return Ok(());
    };

    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    let _ = client.flush();
    let _ = client.set_nodelay(true);
    let _ = server.set_nodelay(true);
    server.set_write_timeout(Some(options.write_timeout))?;

    relay(client, server, shared)
}

/// Bidirectional relay: blocking sockets, one thread per direction.
///
/// A full send buffer blocks the writing thread, which stops it reading from the
/// faster peer. That is the back-pressure a slow reader needs.
fn relay(client: TcpStream, server: TcpStream, shared: &Shared) -> io::Result<()> {
    client.set_read_timeout(Some(POLL_INTERVAL))?;
    server.set_read_timeout(Some(POLL_INTERVAL))?;
    let clock = Arc::new(Activity::new());
    let idle = shared.options.idle_timeout;
    let upstream = {
        let from = client.try_clone()?;
        let to = server.try_clone()?;
        let clock = Arc::clone(&clock);
        let shutdown = Arc::clone(&shared.shutdown);
        thread::spawn(move || pump(from, to, &clock, &shutdown, idle))
    };
    let downstream = pump(server, client, &clock, &shared.shutdown, idle);
    let upstream = upstream
        .join()
        .unwrap_or_else(|_| Err(io::ErrorKind::Other.into()));
    downstream.and(upstream)
}

/// Time of the last byte moved in either direction, so one busy direction keeps an
/// otherwise quiet one open.
struct Activity {
    start: Instant,
    last_ms: AtomicU64,
}

impl Activity {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            last_ms: AtomicU64::new(0),
        }
    }

    fn touch(&self) {
        self.last_ms
            .store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    fn idle_for(&self) -> Duration {
        let now = self.start.elapsed().as_millis() as u64;
        Duration::from_millis(now.saturating_sub(self.last_ms.load(Ordering::Relaxed)))
    }
}

/// Copy `from` to `to` until EOF, then half-close `to`. Any error closes both sockets so
/// the opposite direction wakes up.
fn pump(
    mut from: TcpStream,
    mut to: TcpStream,
    clock: &Activity,
    shutdown: &AtomicBool,
    idle_timeout: Duration,
) -> io::Result<()> {
    let mut buf = vec![0u8; RELAY_BUFFER_BYTES];
    let result = loop {
        match from.read(&mut buf) {
            Ok(0) => {
                let _ = to.shutdown(Shutdown::Write);
                break Ok(());
            }
            Ok(n) => {
                clock.touch();
                if let Err(e) = to.write_all(&buf[..n]) {
                    break Err(e);
                }
                clock.touch();
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if shutdown.load(Ordering::SeqCst) {
                    break Err(io::ErrorKind::ConnectionAborted.into());
                }
                if clock.idle_for() >= idle_timeout {
                    break Err(io::ErrorKind::TimedOut.into());
                }
            }
            Err(e) => break Err(e),
        }
    };
    if result.is_err() {
        let _ = from.shutdown(Shutdown::Both);
        let _ = to.shutdown(Shutdown::Both);
    }
    result
}

fn is_localhost_name(host: &str) -> bool {
    host == "localhost" || host.ends_with(".localhost")
}

/// Addresses a client must not reach through this proxy.
fn is_blocked_target(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_blocked_target(IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

fn resolve_connect_ip(
    host: &str,
    overrides: &HashMap<String, IpAddr>,
    resolver: &HostResolver,
) -> Option<IpAddr> {
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
    resolver(host)
}

fn system_resolve(host: &str) -> Option<IpAddr> {
    use std::net::ToSocketAddrs;
    format!("{host}:443")
        .to_socket_addrs()
        .ok()?
        .map(|a| a.ip())
        .next()
}

/// Read the request head. Fails when the client is too slow, sends too much, or hangs up
/// before the blank line.
fn read_http_headers(stream: &mut TcpStream, timeout: Duration) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    read_until_blank_line(stream, MAX_HEADER_BYTES, |stream| {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        stream.set_read_timeout(Some(left))
    })
}

/// Read one byte at a time (so no tunnel bytes are consumed) up to `\r\n\r\n`.
/// `before_read` runs ahead of each read to enforce a deadline.
///
/// Only the end of the buffer is checked after each byte, so the cost is linear in the
/// head size (L-15: it used to rescan the whole buffer for every byte).
fn read_until_blank_line<R: Read>(
    reader: &mut R,
    max: usize,
    mut before_read: impl FnMut(&mut R) -> io::Result<()>,
) -> io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    while buf.len() < max {
        before_read(reader)?;
        if reader.read(&mut byte)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            return Ok(buf);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "request head too large",
    ))
}

fn split_host_port(target: &str) -> (String, u16) {
    if let Some((h, p)) = target.rsplit_once(':') {
        if let Ok(port) = p.parse::<u16>() {
            return (h.trim_matches(|c| c == '[' || c == ']').to_string(), port);
        }
    }
    (target.to_string(), 443)
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Build host→IP map for a URL's host via DoH. Returns None when override not needed.
#[cfg(test)]
pub fn doh_override_map_for_url(url: &str) -> Result<Option<HashMap<String, IpAddr>>, String> {
    doh_override_map_for_url_with(url, &crate::proxy_settings::ProxySettings::default())
}

/// Like [`doh_override_map_for_url`], with the DoH request sent through `proxy`.
pub fn doh_override_map_for_url_with(
    url: &str,
    proxy: &crate::proxy_settings::ProxySettings,
) -> Result<Option<HashMap<String, IpAddr>>, String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid URL for DoH map: {e}"))?;
    let host = match parsed.host_str() {
        Some(h) => h.to_ascii_lowercase(),
        None => return Ok(None),
    };

    let doh_ips = crate::doh_dns::resolve_a_records_with(proxy, &host)?;
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
    use std::io::Cursor;
    use std::net::Ipv4Addr;

    const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

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
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("parse json");
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
        let url = url::Url::parse(&guard.proxy_url).unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.username(), PROXY_USER);
        assert_eq!(url.password().map(str::len), Some(32), "random token");
        drop(guard);
    }

    #[test]
    fn each_run_gets_a_new_token() {
        let a = start_doh_connect_proxy(HashMap::new()).unwrap();
        let b = start_doh_connect_proxy(HashMap::new()).unwrap();
        assert_ne!(a.proxy_url, b.proxy_url);
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), want, "{input:?}");
        }
    }

    #[test]
    fn header_reader_stops_at_blank_line_and_leaves_the_rest() {
        let mut input = Cursor::new(b"CONNECT a:443 HTTP/1.1\r\nHost: a\r\n\r\nTLS".to_vec());
        let mut reads = 0;
        let head = read_until_blank_line(&mut input, 1024, |_| {
            reads += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(head, b"CONNECT a:443 HTTP/1.1\r\nHost: a\r\n\r\n");
        assert_eq!(
            reads,
            head.len(),
            "one read per byte, no rescans or extra reads"
        );
        let mut rest = Vec::new();
        input.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"TLS");
    }

    #[test]
    fn header_reader_rejects_oversized_and_truncated_heads() {
        let big = vec![b'a'; 4096];
        let err = read_until_blank_line(&mut Cursor::new(big), 1024, |_| Ok(())).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let err =
            read_until_blank_line(&mut Cursor::new(b"CONNECT a:1\r\n".to_vec()), 1024, |_| {
                Ok(())
            })
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn blocked_targets_cover_loopback_link_local_and_mapped_addresses() {
        for ip in [
            "127.0.0.1",
            "127.8.8.8",
            "0.0.0.0",
            "169.254.169.254",
            "224.0.0.1",
            "::1",
            "::",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_blocked_target(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["93.184.216.34", "10.0.0.5", "2606:4700::1111"] {
            assert!(!is_blocked_target(ip.parse().unwrap()), "{ip}");
        }
    }

    // --- socket-level tests -------------------------------------------------

    fn local_options() -> ProxyOptions {
        ProxyOptions {
            allow_blocked_targets: true,
            ..ProxyOptions::default()
        }
    }

    fn origin_overrides() -> HashMap<String, IpAddr> {
        HashMap::from([("origin.test".to_string(), LOOPBACK)])
    }

    fn basic_auth(guard: &DohProxyGuard) -> String {
        let url = url::Url::parse(&guard.proxy_url).unwrap();
        let creds = format!("{}:{}", url.username(), url.password().unwrap());
        format!("Basic {}", base64(creds.as_bytes()))
    }

    fn proxy_addr(guard: &DohProxyGuard) -> SocketAddr {
        let url = url::Url::parse(&guard.proxy_url).unwrap();
        SocketAddr::new(LOOPBACK, url.port().unwrap())
    }

    /// Send a CONNECT request and return the socket and the response status line.
    fn connect_via(guard: &DohProxyGuard, target: &str, auth: Option<&str>) -> (TcpStream, String) {
        let mut s = TcpStream::connect(proxy_addr(guard)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let auth = auth
            .map(|a| format!("Proxy-Authorization: {a}\r\n"))
            .unwrap_or_default();
        write!(
            s,
            "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n{auth}\r\n"
        )
        .unwrap();
        let head = read_until_blank_line(&mut s, 4096, |_| Ok(())).unwrap();
        let status = String::from_utf8_lossy(&head)
            .lines()
            .next()
            .unwrap()
            .to_string();
        (s, status)
    }

    fn listener() -> (TcpListener, u16) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        (l, port)
    }

    #[test]
    fn connect_without_the_run_token_is_refused() {
        // M-21: any local process can reach the loopback listener.
        let (origin, port) = listener();
        let guard = start_with_options(origin_overrides(), local_options()).unwrap();
        let target = format!("origin.test:{port}");

        let (_, status) = connect_via(&guard, &target, None);
        assert!(status.contains("407"), "{status}");
        let (_, status) = connect_via(&guard, &target, Some("Basic d3Jvbmc6Y3JlZHM="));
        assert!(status.contains("407"), "{status}");

        origin.set_nonblocking(true).unwrap();
        assert!(
            origin.accept().is_err(),
            "origin was dialled without a token"
        );

        let auth = basic_auth(&guard);
        let (_, status) = connect_via(&guard, &target, Some(&auth));
        assert!(status.contains("200"), "{status}");
    }

    #[test]
    fn loopback_targets_are_refused() {
        // M-21: the proxy must not reach loopback-only services.
        let (origin, port) = listener();
        let guard = start_doh_connect_proxy(origin_overrides()).unwrap();
        let auth = basic_auth(&guard);

        for target in [
            format!("origin.test:{port}"), // override pointing at 127.0.0.1
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ] {
            let (_, status) = connect_via(&guard, &target, Some(&auth));
            assert!(status.contains("403"), "{target}: {status}");
        }
        origin.set_nonblocking(true).unwrap();
        assert!(origin.accept().is_err(), "loopback origin was dialled");
    }

    #[test]
    fn connection_cap_rejects_extra_clients_and_frees_slots() {
        let options = ProxyOptions {
            max_connections: 1,
            ..local_options()
        };
        let guard = start_with_options(origin_overrides(), options).unwrap();
        let first = TcpStream::connect(proxy_addr(&guard)).unwrap(); // holds the only slot

        let mut rejected = false;
        for _ in 0..40 {
            let mut second = TcpStream::connect(proxy_addr(&guard)).unwrap();
            second
                .set_read_timeout(Some(Duration::from_millis(250)))
                .unwrap();
            let mut buf = [0u8; 64];
            if let Ok(n) = second.read(&mut buf) {
                assert!(String::from_utf8_lossy(&buf[..n]).contains("503"));
                rejected = true;
                break;
            }
        }
        assert!(rejected, "second client was never rejected");

        drop(first);
        let mut accepted = false;
        for _ in 0..40 {
            let mut s = TcpStream::connect(proxy_addr(&guard)).unwrap();
            s.set_read_timeout(Some(Duration::from_millis(250)))
                .unwrap();
            s.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
            let mut buf = [0u8; 64];
            if let Ok(n) = s.read(&mut buf) {
                if String::from_utf8_lossy(&buf[..n]).contains("407") {
                    accepted = true;
                    break;
                }
            }
        }
        assert!(accepted, "slot was not released");
    }

    #[test]
    fn silent_client_is_dropped_after_the_header_timeout() {
        // M-21: idle clients used to hold a thread and a descriptor forever.
        let options = ProxyOptions {
            header_timeout: Duration::from_millis(200),
            ..local_options()
        };
        let guard = start_with_options(origin_overrides(), options).unwrap();
        let mut s = TcpStream::connect(proxy_addr(&guard)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let started = Instant::now();
        let mut buf = [0u8; 8];
        assert!(matches!(s.read(&mut buf), Ok(0) | Err(_)));
        assert!(started.elapsed() < Duration::from_secs(4), "not closed");
    }

    #[test]
    fn idle_tunnel_is_closed_after_the_idle_timeout() {
        let (origin, port) = listener();
        let held = thread::spawn(move || origin.accept().map(|(s, _)| s));
        let options = ProxyOptions {
            idle_timeout: Duration::from_millis(300),
            ..local_options()
        };
        let guard = start_with_options(origin_overrides(), options).unwrap();
        let auth = basic_auth(&guard);
        let (mut s, status) = connect_via(&guard, &format!("origin.test:{port}"), Some(&auth));
        assert!(status.contains("200"), "{status}");
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let started = Instant::now();
        let mut buf = [0u8; 8];
        assert!(matches!(s.read(&mut buf), Ok(0) | Err(_)));
        assert!(started.elapsed() < Duration::from_secs(8));
        drop(held);
    }

    const BODY_BYTES: usize = 16 * 1024 * 1024;

    fn pattern(i: usize) -> u8 {
        (i % 251) as u8
    }

    #[test]
    fn slow_reader_still_receives_the_whole_body() {
        // H-17: both sockets were non-blocking, so write_all hit WouldBlock as soon as the
        // client's receive buffer filled and the tunnel was torn down mid-body.
        let (origin, port) = listener();
        let server = thread::spawn(move || {
            let (mut s, _) = origin.accept().unwrap();
            let chunk: Vec<u8> = (0..64 * 1024).map(pattern).collect();
            let mut sent = 0;
            while sent < BODY_BYTES {
                s.write_all(&chunk).unwrap();
                sent += chunk.len();
            }
        });
        let guard = start_with_options(origin_overrides(), local_options()).unwrap();
        let auth = basic_auth(&guard);
        let (mut client, status) = connect_via(&guard, &format!("origin.test:{port}"), Some(&auth));
        assert!(status.contains("200"), "{status}");

        // Let the origin run ahead until every buffer between the two ends is full.
        thread::sleep(Duration::from_millis(700));
        let mut received = 0usize;
        let mut buf = vec![0u8; 32 * 1024];
        loop {
            let n = client.read(&mut buf).expect("tunnel dropped mid-body");
            if n == 0 {
                break;
            }
            for (i, b) in buf[..n].iter().enumerate() {
                assert_eq!(*b, pattern((received + i) % (64 * 1024)));
            }
            received += n;
        }
        assert_eq!(received, BODY_BYTES);
        server.join().unwrap();
    }

    #[test]
    fn slow_origin_still_receives_the_whole_upload() {
        let (origin, port) = listener();
        let server = thread::spawn(move || {
            let (mut s, _) = origin.accept().unwrap();
            thread::sleep(Duration::from_millis(700));
            let mut total = 0usize;
            let mut buf = vec![0u8; 32 * 1024];
            loop {
                let n = s.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                total += n;
            }
            total
        });
        let guard = start_with_options(origin_overrides(), local_options()).unwrap();
        let auth = basic_auth(&guard);
        let (mut client, status) = connect_via(&guard, &format!("origin.test:{port}"), Some(&auth));
        assert!(status.contains("200"), "{status}");
        let chunk = vec![7u8; 64 * 1024];
        let mut sent = 0;
        while sent < BODY_BYTES {
            client.write_all(&chunk).unwrap();
            sent += chunk.len();
        }
        client.shutdown(Shutdown::Write).unwrap();
        assert_eq!(server.join().unwrap(), BODY_BYTES);
    }

    #[test]
    fn hosts_without_an_override_go_through_the_resolver() {
        // M-22: only the page host used to be mapped; CDN hosts used system DNS.
        let (origin, port) = listener();
        let server = thread::spawn(move || origin.accept().map(|(_, _)| ()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_by_resolver = Arc::clone(&seen);
        let resolver: HostResolver = Arc::new(move |host| {
            seen_by_resolver.lock().unwrap().push(host.to_string());
            Some(LOOPBACK)
        });
        let guard = start_with_resolver(HashMap::new(), local_options(), resolver).unwrap();
        let auth = basic_auth(&guard);
        let (_, status) = connect_via(&guard, &format!("cdn.example:{port}"), Some(&auth));
        assert!(status.contains("200"), "{status}");
        assert_eq!(*seen.lock().unwrap(), vec!["cdn.example".to_string()]);
        server.join().unwrap().unwrap();
    }

    fn doh_resolver(
        lookup: impl Fn(&str) -> Result<Vec<IpAddr>, String> + Send + Sync + 'static,
        fallback: Option<IpAddr>,
    ) -> (DohResolver, Arc<Mutex<Vec<String>>>) {
        let notes = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&notes);
        (
            DohResolver::new(
                lookup,
                move |_| fallback,
                move |n| sink.lock().unwrap().push(n.to_string()),
            ),
            notes,
        )
    }

    #[test]
    fn doh_resolver_caches_answers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let ip = IpAddr::V4(Ipv4Addr::new(104, 16, 3, 81));
        let (resolver, notes) = doh_resolver(
            move |_| {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(vec![ip])
            },
            None,
        );
        assert_eq!(resolver.resolve("cdn.example"), Some(ip));
        assert_eq!(resolver.resolve("cdn.example"), Some(ip));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(notes.lock().unwrap().is_empty());
    }

    #[test]
    fn doh_failure_falls_back_to_system_dns_and_leaves_a_diagnostic() {
        // M-22: a DoH error used to fall back with no trace.
        let sys = IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34));
        let (resolver, notes) = doh_resolver(|_| Err("timed out".into()), Some(sys));
        assert_eq!(resolver.resolve("cdn.example"), Some(sys));
        let notes = notes.lock().unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("cdn.example") && notes[0].contains("timed out"));
    }

    #[test]
    fn doh_resolver_does_not_look_up_ip_literals() {
        let (resolver, _) = doh_resolver(|_| panic!("lookup for an IP literal"), None);
        assert_eq!(
            resolver.resolve("93.184.216.34"),
            Some("93.184.216.34".parse().unwrap())
        );
    }
}
