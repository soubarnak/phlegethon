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

const FILE_CHOOSER_INTERFACE: &str = "org.freedesktop.portal.FileChooser";

/// Arguments for `dbus-send` that read the `version` property of the FileChooser
/// interface on the portal. The call only succeeds when a backend implements it.
pub fn file_chooser_version_args() -> Vec<String> {
    [
        "--session",
        "--print-reply",
        "--dest=org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.DBus.Properties.Get",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([
        format!("string:{FILE_CHOOSER_INTERFACE}"),
        "string:version".to_string(),
    ])
    .collect()
}

/// Parse the `variant uint32 N` reply of the FileChooser `version` property.
pub fn parse_file_chooser_version(stdout: &str) -> bool {
    stdout.lines().any(|line| {
        let line = line.trim();
        line.starts_with("variant") && line.contains("uint32")
    })
}

/// Run `dbus-send` with a hard timeout and return stdout when it exits successfully.
fn run_dbus_send(tool: &std::path::Path, args: &[String], timeout: Duration) -> Option<String> {
    let mut child = Command::new(tool)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    use std::io::Read;
                    let _ = stdout.read_to_string(&mut out);
                }
                return Some(out);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Ask the session bus using the `dbus-send` found in `dirs`.
pub fn portal_available_with(dbus_send_dirs: &[PathBuf], timeout: Duration) -> bool {
    let Some(tool) = crate::find_in_dirs("dbus-send", dbus_send_dirs) else {
        return false;
    };
    run_dbus_send(&tool, &name_has_owner_args(), timeout)
        .is_some_and(|out| parse_name_has_owner(&out))
}

/// Is a FileChooser backend available? The portal bus name having an owner is not
/// enough: `xdg-desktop-portal` runs without `xdg-desktop-portal-gtk` or `-gnome` on
/// some setups, and then every FileChooser call fails (H-20).
pub fn file_chooser_available_with(dbus_send_dirs: &[PathBuf], timeout: Duration) -> bool {
    let Some(tool) = crate::find_in_dirs("dbus-send", dbus_send_dirs) else {
        return false;
    };
    run_dbus_send(&tool, &name_has_owner_args(), timeout)
        .is_some_and(|out| parse_name_has_owner(&out))
        && run_dbus_send(&tool, &file_chooser_version_args(), timeout)
            .is_some_and(|out| parse_file_chooser_version(&out))
}

/// Convenience wrapper using the real `PATH` and a 2 s timeout.
pub fn portal_available() -> bool {
    let dirs = crate::path_dirs_from_env(std::env::var_os("PATH").as_deref());
    portal_available_with(&dirs, Duration::from_secs(2))
}

/// [`file_chooser_available_with`] using the real `PATH` and a 2 s timeout per call.
pub fn file_chooser_available() -> bool {
    let dirs = crate::path_dirs_from_env(std::env::var_os("PATH").as_deref());
    file_chooser_available_with(&dirs, Duration::from_secs(2))
}

/// What a finished portal folder dialog means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalPick {
    Picked(PathBuf),
    /// The user dismissed the dialog.
    Cancelled,
    /// The portal returned nothing the user could have caused: continue with another picker.
    Failed,
}

/// The dialog crate reports a failed portal call and a cancelled dialog the same way
/// (`None`). A person needs a moment to see a dialog and dismiss it, so an answer that
/// arrives faster than `human_floor` came from a failing portal call, not from the user.
pub fn classify_portal_pick(
    picked: Option<PathBuf>,
    elapsed: Duration,
    human_floor: Duration,
) -> PortalPick {
    match picked {
        Some(path) => PortalPick::Picked(path),
        None if elapsed < human_floor => PortalPick::Failed,
        None => PortalPick::Cancelled,
    }
}

/// Shorter than any plausible human dismissal of a dialog.
pub const HUMAN_DISMISS_FLOOR: Duration = Duration::from_millis(400);

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::fake_tool;

    #[test]
    fn args_target_the_portal_name() {
        let args = name_has_owner_args();
        assert!(args.contains(&"--session".to_string()));
        assert_eq!(
            args.last().unwrap(),
            "string:org.freedesktop.portal.Desktop"
        );
    }

    #[test]
    fn parses_replies() {
        assert!(parse_name_has_owner(
            "method return time=1.2\n   boolean true\n"
        ));
        assert!(!parse_name_has_owner(
            "method return time=1.2\n   boolean false\n"
        ));
        assert!(!parse_name_has_owner(""));
    }

    #[test]
    fn available_when_bus_says_true() {
        let dir = tempfile::tempdir().unwrap();
        fake_tool(dir.path(), "dbus-send", "echo '   boolean true'");
        assert!(portal_available_with(
            &[dir.path().to_path_buf()],
            Duration::from_secs(2)
        ));
    }

    #[test]
    fn unavailable_on_false_failure_or_missing_tool() {
        let dir = tempfile::tempdir().unwrap();
        fake_tool(dir.path(), "dbus-send", "echo '   boolean false'");
        assert!(!portal_available_with(
            &[dir.path().to_path_buf()],
            Duration::from_secs(2)
        ));

        let failing = tempfile::tempdir().unwrap();
        fake_tool(failing.path(), "dbus-send", "exit 1");
        assert!(!portal_available_with(
            &[failing.path().to_path_buf()],
            Duration::from_secs(2)
        ));

        let empty = tempfile::tempdir().unwrap();
        assert!(!portal_available_with(
            &[empty.path().to_path_buf()],
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn hung_tool_times_out() {
        let dir = tempfile::tempdir().unwrap();
        fake_tool(dir.path(), "dbus-send", "sleep 30");
        let started = Instant::now();
        assert!(!portal_available_with(
            &[dir.path().to_path_buf()],
            Duration::from_millis(300)
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn file_chooser_args_ask_for_the_interface_version() {
        let args = file_chooser_version_args();
        assert!(args.contains(&"--dest=org.freedesktop.portal.Desktop".to_string()));
        assert!(args.contains(&"string:org.freedesktop.portal.FileChooser".to_string()));
        assert_eq!(args.last().unwrap(), "string:version");
    }

    #[test]
    fn parses_the_file_chooser_version_reply() {
        assert!(parse_file_chooser_version(
            "method return time=1.2 sender=:1.5 -> destination=:1.99 serial=3 reply_serial=2\n   variant       uint32 4\n"
        ));
        assert!(!parse_file_chooser_version(""));
        assert!(!parse_file_chooser_version("   boolean true\n"));
    }

    /// A bus where the portal name has an owner, but the FileChooser interface is
    /// missing: `Properties.Get` fails, as it does with no backend installed.
    #[test]
    fn portal_without_a_file_chooser_backend_is_not_available() {
        let dir = tempfile::tempdir().unwrap();
        fake_tool(
            dir.path(),
            "dbus-send",
            "case \"$*\" in *Properties.Get*) exit 1;; *) echo '   boolean true';; esac",
        );
        let dirs = [dir.path().to_path_buf()];
        assert!(portal_available_with(&dirs, Duration::from_secs(2)));
        assert!(!file_chooser_available_with(&dirs, Duration::from_secs(2)));
    }

    #[test]
    fn portal_with_a_file_chooser_backend_is_available() {
        let dir = tempfile::tempdir().unwrap();
        fake_tool(
            dir.path(),
            "dbus-send",
            "case \"$*\" in *Properties.Get*) echo '   variant       uint32 4';; *) echo '   boolean true';; esac",
        );
        assert!(file_chooser_available_with(
            &[dir.path().to_path_buf()],
            Duration::from_secs(2)
        ));
    }

    #[test]
    fn a_fast_empty_portal_answer_is_a_failure_and_a_slow_one_a_cancel() {
        let floor = HUMAN_DISMISS_FLOOR;
        let path = PathBuf::from("/home/u/dl");
        assert_eq!(
            classify_portal_pick(Some(path.clone()), Duration::from_millis(5), floor),
            PortalPick::Picked(path)
        );
        assert_eq!(
            classify_portal_pick(None, Duration::from_millis(30), floor),
            PortalPick::Failed
        );
        assert_eq!(
            classify_portal_pick(None, Duration::from_secs(4), floor),
            PortalPick::Cancelled
        );
    }
}
