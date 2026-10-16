//! Folder picker fallbacks for desktops without `xdg-desktop-portal`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerBackend {
    Zenity,
    Kdialog,
}

impl PickerBackend {
    pub fn program(self) -> &'static str {
        match self {
            PickerBackend::Zenity => "zenity",
            PickerBackend::Kdialog => "kdialog",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickOutcome {
    Picked(PathBuf),
    Cancelled,
}

/// Prefer zenity (GTK desktops), then kdialog (KDE).
pub fn detect(dirs: &[PathBuf]) -> Option<(PickerBackend, PathBuf)> {
    [PickerBackend::Zenity, PickerBackend::Kdialog]
        .into_iter()
        .find_map(|backend| crate::find_in_dirs(backend.program(), dirs).map(|p| (backend, p)))
}

/// Command line arguments (without the program) for a directory chooser.
pub fn args_for(backend: PickerBackend, title: &str, start_dir: Option<&Path>) -> Vec<String> {
    let start = start_dir.map(|p| {
        let mut s = p.display().to_string();
        if !s.ends_with('/') {
            s.push('/');
        }
        s
    });
    match backend {
        PickerBackend::Zenity => {
            let mut args = vec![
                "--file-selection".to_string(),
                "--directory".to_string(),
                format!("--title={title}"),
            ];
            if let Some(start) = start {
                args.push(format!("--filename={start}"));
            }
            args
        }
        PickerBackend::Kdialog => {
            let mut args = vec!["--getexistingdirectory".to_string()];
            args.push(start.unwrap_or_else(|| ".".to_string()));
            args.push("--title".to_string());
            args.push(title.to_string());
            args
        }
    }
}

/// Interpret the exit status and stdout of a chooser run.
///
/// Both tools exit 0 with the path on stdout when something was chosen, and
/// non-zero with empty output when the dialog was cancelled.
pub fn parse_outcome(success: bool, stdout: &str) -> PickOutcome {
    let trimmed = stdout.trim_end_matches(['\n', '\r']);
    if success && !trimmed.is_empty() {
        PickOutcome::Picked(PathBuf::from(trimmed))
    } else {
        PickOutcome::Cancelled
    }
}

/// Run the chooser and wait for it. Blocking: call from a worker thread.
pub fn run(
    backend: PickerBackend,
    program: &Path,
    title: &str,
    start_dir: Option<&Path>,
) -> Result<PickOutcome, String> {
    let output = Command::new(program)
        .args(args_for(backend, title, start_dir))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("failed to start {}: {e}", backend.program()))?;
    Ok(parse_outcome(
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::fake_tool;

    #[test]
    fn zenity_is_preferred_over_kdialog() {
        let dir = tempfile::tempdir().unwrap();
        fake_tool(dir.path(), "kdialog", "true");
        assert_eq!(
            detect(&[dir.path().to_path_buf()]).unwrap().0,
            PickerBackend::Kdialog
        );
        fake_tool(dir.path(), "zenity", "true");
        assert_eq!(
            detect(&[dir.path().to_path_buf()]).unwrap().0,
            PickerBackend::Zenity
        );
        assert!(detect(&[]).is_none());
    }

    #[test]
    fn builds_arguments() {
        let z = args_for(PickerBackend::Zenity, "Pick", Some(Path::new("/home/u")));
        assert_eq!(z[0], "--file-selection");
        assert!(z.contains(&"--directory".to_string()));
        assert!(z.contains(&"--title=Pick".to_string()));
        assert!(z.contains(&"--filename=/home/u/".to_string()));

        let k = args_for(PickerBackend::Kdialog, "Pick", None);
        assert_eq!(k, vec!["--getexistingdirectory", ".", "--title", "Pick"]);
    }

    #[test]
    fn parses_outcomes() {
        assert_eq!(
            parse_outcome(true, "/home/u/Videos\n"),
            PickOutcome::Picked(PathBuf::from("/home/u/Videos"))
        );
        assert_eq!(parse_outcome(false, ""), PickOutcome::Cancelled);
        assert_eq!(parse_outcome(true, "\n"), PickOutcome::Cancelled);
    }

    #[test]
    fn run_returns_picked_path_and_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let ok = fake_tool(dir.path(), "zenity", "echo /srv/media");
        assert_eq!(
            run(PickerBackend::Zenity, &ok, "t", None).unwrap(),
            PickOutcome::Picked(PathBuf::from("/srv/media"))
        );
        let cancel = fake_tool(dir.path(), "kdialog", "exit 1");
        assert_eq!(
            run(PickerBackend::Kdialog, &cancel, "t", None).unwrap(),
            PickOutcome::Cancelled
        );
    }
}
