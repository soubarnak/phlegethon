//! Desktop integration helpers that do not depend on Tauri, so they can be unit
//! tested anywhere and reused by the packaging scripts.
//!
//! Everything here shells out to small, widely available tools instead of
//! linking D-Bus or GTK, which keeps the xbps package light and avoids
//! systemd-only assumptions (Void uses runit).

pub mod nmh;
pub mod notify;
pub mod picker;
pub mod platform;
pub mod portal;

use std::path::{Path, PathBuf};

/// Directories on `PATH`, in order. Empty entries are skipped.
pub fn path_dirs_from_env(path_var: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    match path_var {
        Some(raw) => std::env::split_paths(raw)
            .filter(|p| !p.as_os_str().is_empty())
            .collect(),
        None => Vec::new(),
    }
}

/// First executable called `name` in `dirs`.
pub fn find_in_dirs(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.join(name))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
pub(crate) mod test_util {
    use std::path::{Path, PathBuf};

    /// Create an executable shell script `name` in `dir` with the given body.
    #[cfg(unix)]
    pub fn fake_tool(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }
}
