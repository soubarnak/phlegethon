//! When ISP DNS sinkholes a host, route yt-dlp through a local CONNECT proxy
//! that dials DoH-resolved IPs (preserves TLS SNI). Falls back to a Python
//! getaddrinfo patch when the proxy cannot start.

use crate::doh_proxy::{doh_override_map_for_url, start_doh_connect_proxy, DohProxyGuard};
use crate::proxy_settings::ProxySettings;
use crate::toolchain_discovery::YtDlpLauncher;
use std::collections::HashMap;
use std::io::Write;
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::Command;

/// Keeps ephemeral DoH resources alive for the duration of a yt-dlp run.
pub struct YtdlpDohRunGuard {
    script: Option<PathBuf>,
    proxy_guard: Option<DohProxyGuard>,
}

impl Drop for YtdlpDohRunGuard {
    fn drop(&mut self) {
        if let Some(path) = self.script.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Build a [`Command`] for yt-dlp, wrapping with DoH routing when needed.
pub fn ytdlp_command_with_optional_doh_dns(
    launcher: &YtDlpLauncher,
    args: &[String],
    url: &str,
    proxy: &ProxySettings,
) -> Result<(Command, YtdlpDohRunGuard), String> {
    if proxy.url.as_ref().is_some_and(|u| !u.trim().is_empty()) {
        let mut cmd = launcher.command();
        cmd.args(args);
        return Ok((
            cmd,
            YtdlpDohRunGuard {
                script: None,
                proxy_guard: None,
            },
        ));
    }

    let map = match doh_override_map_for_url(url) {
        Ok(Some(m)) => m,
        Ok(None) | Err(_) => {
            let mut cmd = launcher.command();
            cmd.args(args);
            return Ok((
                cmd,
                YtdlpDohRunGuard {
                    script: None,
                    proxy_guard: None,
                },
            ));
        }
    };

    // Prefer CONNECT proxy: works with yt-dlp.exe and preserves SNI (curl respects --proxy).
    if let Ok(proxy_guard) = start_doh_connect_proxy(map.clone()) {
        let mut cmd = launcher.command();
        cmd.args(args);
        // The proxy URL carries the per-run token. Keep it off the command line, which any
        // local user can read from /proc/<pid>/cmdline.
        apply_proxy_env(&mut cmd, &proxy_guard.proxy_url, true);
        return Ok((
            cmd,
            YtdlpDohRunGuard {
                script: None,
                proxy_guard: Some(proxy_guard),
            },
        ));
    }

    if let Some(python) = find_python() {
        let script = write_doh_patch_script(&map, args)?;
        let mut cmd = Command::new(&python.program);
        for a in &python.prefix_args {
            cmd.arg(a);
        }
        cmd.arg(&script);
        return Ok((
            cmd,
            YtdlpDohRunGuard {
                script: Some(script),
                proxy_guard: None,
            },
        ));
    }

    let mut cmd = launcher.command();
    cmd.args(args);
    Ok((
        cmd,
        YtdlpDohRunGuard {
            script: None,
            proxy_guard: None,
        },
    ))
}

/// Point yt-dlp at `proxy_url` through the environment instead of `--proxy`.
///
/// `clear_no_proxy` also empties `NO_PROXY`, so an inherited entry cannot send a poisoned
/// host around the DoH proxy.
fn apply_proxy_env(cmd: &mut Command, proxy_url: &str, clear_no_proxy: bool) {
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        cmd.env(key, proxy_url);
    }
    if clear_no_proxy {
        cmd.env("NO_PROXY", "").env("no_proxy", "");
    }
}

struct PythonLaunch {
    program: String,
    prefix_args: Vec<String>,
}

fn find_python() -> Option<PythonLaunch> {
    let probes: &[(&str, &[&str])] = &[("python", &[]), ("python3", &[]), ("py", &["-3"])];
    for (program, prefix) in probes {
        let mut cmd = Command::new(program);
        for a in *prefix {
            cmd.arg(a);
        }
        cmd.args(["-c", "import yt_dlp"]);
        if cmd.status().map(|s| s.success()).unwrap_or(false) {
            return Some(PythonLaunch {
                program: (*program).to_string(),
                prefix_args: prefix.iter().map(|s| (*s).to_string()).collect(),
            });
        }
    }
    None
}

fn write_doh_patch_script(
    map: &HashMap<String, IpAddr>,
    args: &[String],
) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("phlegethon-doh-ytdlp-{}.py", std::process::id()));

    let mut map_literal = String::from("{");
    for (host, ip) in map {
        map_literal.push_str(&format!("{:?}: {:?},", host, ip.to_string()));
    }
    map_literal.push('}');

    let mut argv_literal = String::from("[");
    for a in args {
        argv_literal.push_str(&format!("{:?},", a));
    }
    argv_literal.push(']');

    let source = format!(
        r#"import socket, sys
_MAP = {map_literal}
_real = socket.getaddrinfo
def _fake(host, port, family=0, type=0, proto=0, flags=0):
    key = host.lower() if isinstance(host, str) else host
    if isinstance(key, str) and key in _MAP:
        host = _MAP[key]
    return _real(host, port, family, type, proto, flags)
socket.getaddrinfo = _fake
from yt_dlp import main
sys.argv = ["yt-dlp"] + {argv_literal}
raise SystemExit(main())
"#
    );

    let mut file =
        std::fs::File::create(&path).map_err(|e| format!("create DoH yt-dlp script: {e}"))?;
    file.write_all(source.as_bytes())
        .map_err(|e| format!("write DoH yt-dlp script: {e}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn write_script_contains_host_map() {
        let mut map = HashMap::new();
        map.insert(
            "xhamster.com".to_string(),
            IpAddr::V4(Ipv4Addr::new(104, 16, 3, 81)),
        );
        let args = vec![
            "-J".into(),
            "https://xhamster.com/creators/your-eshita".into(),
        ];
        let path = write_doh_patch_script(&map, &args).unwrap();
        let src = std::fs::read_to_string(&path).unwrap();
        assert!(src.contains("xhamster.com"));
        assert!(src.contains("104.16.3.81"));
        assert!(src.contains("from yt_dlp"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn proxy_env_keeps_the_url_off_the_command_line() {
        let url = "http://phlegethon:secret@127.0.0.1:9";
        let mut cmd = Command::new("yt-dlp");
        cmd.args(["-J", "https://example.com/video"]);
        apply_proxy_env(&mut cmd, url, true);

        assert!(cmd
            .get_args()
            .all(|a| !a.to_string_lossy().contains("secret")));
        let envs: HashMap<_, _> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "https_proxy"] {
            assert_eq!(envs[key].as_deref(), Some(url), "{key}");
        }
        assert_eq!(envs["NO_PROXY"].as_deref(), Some(""));
    }
}
