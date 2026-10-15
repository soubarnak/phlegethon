//! Is `xdg-desktop-portal` running on the session bus?
//!
//! Uses `dbus-send` (part of Void's `dbus` package) with a hard timeout, rather
//! than linking a D-Bus client. The native folder picker depends on the portal,
//! so when it is absent the shell falls back to zenity / kdialog / a typed path.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";

/// Arguments for `dbus-send` that ask the bus whether the portal owns its name.
pub fn name_has_owner_args() -> Vec<String> {
    [
        "--session",
        "--print-reply",
        "--dest=org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus.NameHasOwner",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(std::iter::once(format!("string:{PORTAL_NAME}")))
    .collect()
}

/// Parse `dbus-send --print-reply` output for a boolean reply.
pub fn parse_name_has_owner(stdout: &str) -> bool {
    stdout
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case("boolean true"))
}

/// Ask the session bus using the `dbus-send` found in `dirs`.
pub fn portal_available_with(dbus_send_dirs: &[PathBuf], timeout: Duration) -> bool {
    let Some(tool) = crate::find_in_dirs("dbus-send", dbus_send_dirs) else {
        return false;
    };
    let Ok(mut child) = Command::new(tool)
        .args(name_has_owner_args())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return false;
                }
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    use std::io::Read;
                    let _ = stdout.read_to_string(&mut out);
                }
                return parse_name_has_owner(&out);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Convenience wrapper using the real `PATH` and a 2 s timeout.
pub fn portal_available() -> bool {
    let dirs = crate::path_dirs_from_env(std::env::var_os("PATH").as_deref());
    portal_available_with(&dirs, Duration::from_secs(2))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::fake_tool;

    #[test]
    fn args_target_the_portal_name() {
        let args = name_has_owner_args();
        assert!(args.contains(&"--session".to_string()));
        assert_eq!(args.last().unwrap(), "string:org.freedesktop.portal.Desktop");
    }

    #[test]
    fn parses_replies() {
        assert!(parse_name_has_owner("method return time=1.2\n   boolean true\n"));
        assert!(!parse_name_has_owner("method return time=1.2\n   boolean false\n"));
        assert!(!parse_name_has_owner(""));
    }

    #[test]
    fn available_when_bus_says_true() {
        let dir = tempfile::tempdir().unwrap();
        fake_tool(dir.path(), "dbus-send", "echo '   boolean true'");
        assert!(portal_available_with(&[dir.path().to_path_buf()], Duration::from_secs(2)));
    }

    #[test]
    fn unavailable_on_false_failure_or_missing_tool() {
        let dir = tempfile::tempdir().unwrap();
        fake_tool(dir.path(), "dbus-send", "echo '   boolean false'");
        assert!(!portal_available_with(&[dir.path().to_path_buf()], Duration::from_secs(2)));

        let failing = tempfile::tempdir().unwrap();
        fake_tool(failing.path(), "dbus-send", "exit 1");
        assert!(!portal_available_with(&[failing.path().to_path_buf()], Duration::from_secs(2)));

        let empty = tempfile::tempdir().unwrap();
        assert!(!portal_available_with(&[empty.path().to_path_buf()], Duration::from_secs(1)));
    }

    #[test]
    fn hung_tool_times_out() {
        let dir = tempfile::tempdir().unwrap();
        fake_tool(dir.path(), "dbus-send", "sleep 30");
        let started = Instant::now();
        assert!(!portal_available_with(&[dir.path().to_path_buf()], Duration::from_millis(300)));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
