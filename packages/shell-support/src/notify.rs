//! `notify-send` fallback for desktops where the notification plugin has no daemon.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `notify-send` binary, if installed (package `libnotify` on Void).
pub fn detect(dirs: &[PathBuf]) -> Option<PathBuf> {
    crate::find_in_dirs("notify-send", dirs)
}

pub fn args_for(summary: &str, body: &str) -> Vec<String> {
    vec![
        "--app-name=Phlegethon".to_string(),
        "--icon=phlegethon".to_string(),
        "--".to_string(),
        summary.to_string(),
        body.to_string(),
    ]
}

/// Fire-and-forget. Returns an error only when the tool cannot be started.
pub fn send(program: &Path, summary: &str, body: &str) -> Result<(), String> {
    Command::new(program)
        .args(args_for(summary, body))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|mut child| {
            // Reap in the background so we never leave zombies.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        })
        .map_err(|e| format!("failed to start notify-send: {e}"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::fake_tool;

    #[test]
    fn summary_is_never_parsed_as_an_option() {
        let args = args_for("--hint=evil", "body");
        let dashdash = args.iter().position(|a| a == "--").unwrap();
        assert!(args.iter().position(|a| a == "--hint=evil").unwrap() > dashdash);
    }

    #[test]
    fn detect_and_send() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect(&[dir.path().to_path_buf()]).is_none());
        let log = dir.path().join("log");
        let tool = fake_tool(
            dir.path(),
            "notify-send",
            &format!("printf '%s\\n' \"$@\" > {}", log.display()),
        );
        assert_eq!(detect(&[dir.path().to_path_buf()]), Some(tool.clone()));
        send(&tool, "Done", "file.mp4").unwrap();
        for _ in 0..50 {
            if log.exists() && std::fs::read_to_string(&log).unwrap().contains("file.mp4") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(logged.contains("Done") && logged.contains("file.mp4"), "{logged}");
    }

    #[test]
    fn missing_program_is_an_error() {
        assert!(send(Path::new("/nonexistent/notify-send"), "a", "b").is_err());
    }
}
