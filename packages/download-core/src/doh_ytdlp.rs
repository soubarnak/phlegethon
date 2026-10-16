//! When ISP DNS sinkholes a host, route yt-dlp through a local CONNECT proxy
//! that dials DoH-resolved IPs (preserves TLS SNI). Falls back to a Python
//! getaddrinfo patch when the proxy cannot start.

use crate::doh_proxy::{doh_override_map_for_url_with, start_doh_connect_proxy, DohProxyGuard};
use crate::proxy_settings::ProxySettings;
use crate::toolchain_discovery::YtDlpLauncher;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Keeps ephemeral DoH resources alive for the duration of a yt-dlp run.
pub struct YtdlpDohRunGuard {
    /// Private directory that holds the Python fallback script and its data.
    script_dir: Option<PathBuf>,
    /// Held so the proxy stays up for the run.
    _proxy_guard: Option<DohProxyGuard>,
}

impl YtdlpDohRunGuard {
    fn plain() -> Self {
        Self {
            script_dir: None,
            _proxy_guard: None,
        }
    }
}

impl Drop for YtdlpDohRunGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.script_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Build a [`Command`] for yt-dlp, wrapping with DoH routing when needed.
///
/// A configured proxy and the DoH proxy are both handed to yt-dlp through the
/// environment, not `--proxy`: the command line is readable by every local user, and the
/// proxy URL can hold credentials (M-24).
pub fn ytdlp_command_with_optional_doh_dns(
    launcher: &YtDlpLauncher,
    args: &[String],
    url: &str,
    proxy: &ProxySettings,
) -> Result<(Command, YtdlpDohRunGuard), String> {
    if let Some(proxy_url) = proxy.normalized().url {
        let mut cmd = launcher.command();
        cmd.args(args);
        apply_proxy_env(&mut cmd, &proxy_url);
        return Ok((cmd, YtdlpDohRunGuard::plain()));
    }

    let map = match doh_override_map_for_url_with(url, proxy) {
        Ok(Some(m)) => m,
        Ok(None) => return Ok(plain_command(launcher, args)),
        Err(e) => {
            // Not silent: a failed DoH check means the run uses the system resolver.
            eprintln!("DoH check for {url} failed ({e}); yt-dlp uses system DNS");
            return Ok(plain_command(launcher, args));
        }
    };

    // Prefer CONNECT proxy: works with yt-dlp.exe and preserves SNI (curl respects --proxy).
    match start_doh_connect_proxy(map.clone()) {
        Ok(proxy_guard) => {
            let mut cmd = launcher.command();
            cmd.args(args);
            apply_proxy_env(&mut cmd, &proxy_guard.proxy_url);
            return Ok((
                cmd,
                YtdlpDohRunGuard {
                    script_dir: None,
                    _proxy_guard: Some(proxy_guard),
                },
            ));
        }
        Err(e) => eprintln!("DoH proxy did not start ({e}); trying the Python fallback"),
    }

    if let Some(python) = find_python() {
        let (script, script_dir) = write_doh_patch_script(&map, args)?;
        let mut cmd = Command::new(&python.program);
        cmd.args(&python.prefix_args);
        // -I: isolated mode. No script directory, PYTHON* variables or user site on sys.path.
        cmd.arg("-I").arg(&script.script).arg(&script.payload);
        return Ok((
            cmd,
            YtdlpDohRunGuard {
                script_dir: Some(script_dir),
                _proxy_guard: None,
            },
        ));
    }

    eprintln!("DoH proxy and Python fallback unavailable; yt-dlp uses system DNS");
    Ok(plain_command(launcher, args))
}

fn plain_command(launcher: &YtDlpLauncher, args: &[String]) -> (Command, YtdlpDohRunGuard) {
    let mut cmd = launcher.command();
    cmd.args(args);
    (cmd, YtdlpDohRunGuard::plain())
}

/// Point yt-dlp at `proxy_url` through the environment.
///
/// `NO_PROXY` is emptied: `--proxy` ignored it, and an inherited entry must not send a
/// request around the proxy.
fn apply_proxy_env(cmd: &mut Command, proxy_url: &str) {
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
    cmd.env("NO_PROXY", "").env("no_proxy", "");
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
        // Same isolated mode as the real run, so the probe finds what the run can import.
        cmd.args(["-I", "-c", "import yt_dlp"]);
        if cmd.status().map(|s| s.success()).unwrap_or(false) {
            return Some(PythonLaunch {
                program: (*program).to_string(),
                prefix_args: prefix.iter().map(|s| (*s).to_string()).collect(),
            });
        }
    }
    None
}

/// Fixed script: all variable data arrives in the JSON payload, never as source text.
const PATCH_SCRIPT: &str = r#"import json, socket, sys
with open(sys.argv[1], encoding="utf-8") as f:
    _data = json.load(f)
_MAP = _data["map"]
_real = socket.getaddrinfo
def _fake(host, port, family=0, type=0, proto=0, flags=0):
    key = host.lower() if isinstance(host, str) else host
    if isinstance(key, str) and key in _MAP:
        host = _MAP[key]
    return _real(host, port, family, type, proto, flags)
socket.getaddrinfo = _fake
from yt_dlp import main
sys.argv = ["yt-dlp"] + _data["argv"]
raise SystemExit(main())
"#;

struct PatchFiles {
    script: PathBuf,
    payload: PathBuf,
}

/// Write the fallback script and its JSON payload into a new private directory.
/// Returns the files and the directory to delete afterwards.
fn write_doh_patch_script(
    map: &HashMap<String, IpAddr>,
    args: &[String],
) -> Result<(PatchFiles, PathBuf), String> {
    let dir = private_run_dir()?;
    let files = PatchFiles {
        script: dir.join("doh_ytdlp.py"),
        payload: dir.join("payload.json"),
    };
    let payload = serde_json::json!({
        "map": map.iter().map(|(h, ip)| (h.clone(), ip.to_string())).collect::<HashMap<_, _>>(),
        "argv": args,
    });
    let written = write_private_file(&files.script, PATCH_SCRIPT.as_bytes()).and_then(|()| {
        write_private_file(
            &files.payload,
            serde_json::to_string(&payload)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        )
    });
    if let Err(e) = written {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!("write DoH yt-dlp script: {e}"));
    }
    Ok((files, dir))
}

/// New directory that only this user can enter. The name is random and creation fails if
/// it exists, so a pre-planted directory or symlink is never reused.
fn private_run_dir() -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt;
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("phlegethon-doh-{}", uuid::Uuid::new_v4().simple()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|e| format!("create private DoH directory {}: {e}", dir.display()))?;
    Ok(dir)
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(contents)
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::os::unix::fs::PermissionsExt;

    fn python3() -> Option<&'static str> {
        ["python3", "python"].into_iter().find(|p| {
            Command::new(p)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        })
    }

    #[test]
    fn script_is_fixed_and_data_travels_as_json() {
        // M-23: Rust {:?} escapes are not Python literals, so data must not be source text.
        let mut map = HashMap::new();
        map.insert(
            "xhamster.com".to_string(),
            IpAddr::V4(Ipv4Addr::new(104, 16, 3, 81)),
        );
        let args = vec![
            "-J".to_string(),
            "it's \"quoted\" \\ \u{1F600}\n{{braces}}".to_string(),
        ];
        let (files, dir) = write_doh_patch_script(&map, &args).unwrap();
        let src = std::fs::read_to_string(&files.script).unwrap();
        assert_eq!(src, PATCH_SCRIPT);
        assert!(!src.contains("xhamster.com") && !src.contains("quoted"));

        let payload: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&files.payload).unwrap()).unwrap();
        assert_eq!(payload["map"]["xhamster.com"], "104.16.3.81");
        assert_eq!(payload["argv"][1], args[1].as_str());

        if let Some(python) = python3() {
            // Python itself must read back the same strings.
            let out = Command::new(python)
                .args([
                    "-I",
                    "-c",
                    "import json,sys; print(json.load(open(sys.argv[1]))['argv'][1])",
                ])
                .arg(&files.payload)
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'),
                args[1]
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn run_directory_is_private_and_unique() {
        let (_, a) = write_doh_patch_script(&HashMap::new(), &[]).unwrap();
        let (_, b) = write_doh_patch_script(&HashMap::new(), &[]).unwrap();
        assert_ne!(a, b, "concurrent runs must not share a path");
        for dir in [&a, &b] {
            let mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", dir.display());
            for entry in std::fs::read_dir(dir).unwrap() {
                let mode = entry.unwrap().metadata().unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600);
            }
        }
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn guard_removes_the_run_directory() {
        let (_, dir) = write_doh_patch_script(&HashMap::new(), &[]).unwrap();
        assert!(dir.exists());
        drop(YtdlpDohRunGuard {
            script_dir: Some(dir.clone()),
            _proxy_guard: None,
        });
        assert!(!dir.exists());
    }

    #[test]
    fn isolated_python_ignores_planted_modules_next_to_the_script() {
        // M-23: `python script.py` puts the script's directory first on sys.path, so a
        // planted socket.py in a shared /tmp was imported.
        let Some(python) = python3() else {
            return;
        };
        let (files, dir) = write_doh_patch_script(&HashMap::new(), &[]).unwrap();
        let marker = dir.join("planted-ran");
        std::fs::write(
            dir.join("socket.py"),
            format!("open({:?}, 'w').close()\n", marker.display().to_string()),
        )
        .unwrap();

        // Without -I the planted module runs (shows the check can fail).
        let _ = Command::new(python)
            .arg(&files.script)
            .arg(&files.payload)
            .output();
        assert!(
            marker.exists(),
            "control run did not import the planted module"
        );
        std::fs::remove_file(&marker).unwrap();

        let _ = Command::new(python)
            .arg("-I")
            .arg(&files.script)
            .arg(&files.payload)
            .output();
        assert!(!marker.exists(), "isolated run imported the planted module");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn proxy_env_keeps_the_url_off_the_command_line() {
        let url = "http://phlegethon:secret@127.0.0.1:9";
        let mut cmd = Command::new("yt-dlp");
        cmd.args(["-J", "https://example.com/video"]);
        apply_proxy_env(&mut cmd, url);

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

    fn launcher() -> YtDlpLauncher {
        YtDlpLauncher::binary("yt-dlp")
    }

    #[test]
    fn configured_proxy_credentials_go_through_the_environment() {
        // M-24: the password used to be in argv, readable from /proc/<pid>/cmdline.
        let proxy = ProxySettings {
            url: Some("socks5://alice:hunter2@proxy.example:1080".to_string()),
        };
        let args = vec!["-J".to_string(), "https://example.com/v".to_string()];
        let (cmd, _guard) = ytdlp_command_with_optional_doh_dns(
            &launcher(),
            &args,
            "https://example.com/v",
            &proxy,
        )
        .unwrap();

        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv, args);
        assert!(!argv.iter().any(|a| a.contains("hunter2") || a == "--proxy"));
        let all_proxy = cmd
            .get_envs()
            .find(|(k, _)| *k == "ALL_PROXY")
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().into_owned());
        assert_eq!(
            all_proxy.as_deref(),
            Some("socks5://alice:hunter2@proxy.example:1080")
        );
    }
}
