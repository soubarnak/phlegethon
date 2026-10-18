//! Durable file-write helpers (temp file, fsync, rename, directory fsync).
//!
//! Without the syncs, a power loss after a rename can leave an empty or
//! truncated target, because the data blocks or the directory entry were
//! never flushed.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Flush a directory's entries to disk. Needed after a rename or create so the
/// new name survives power loss.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Rename `from` to `to`, then fsync the parent directory of `to`.
pub(crate) fn rename_durable(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)?;
    sync_dir(parent_of(to))
}

/// Replace `path` with `data` atomically and durably.
///
/// Writes a uniquely named temp file next to `path`, fsyncs it, renames it over
/// `path`, then fsyncs the parent directory. The temp file is removed on any
/// error, and concurrent writers never share a temp name.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    write_atomic_with_mode(path, data, None)
}

/// [`write_atomic`] for a file that holds secrets. The temp file is created with mode
/// 0600 before any data is written, and the renamed file keeps that mode, so no other
/// user can read it, not even briefly.
pub(crate) fn write_atomic_private(path: &Path, data: &[u8]) -> io::Result<()> {
    write_atomic_with_mode(path, data, Some(0o600))
}

fn write_atomic_with_mode(path: &Path, data: &[u8], mode: Option<u32>) -> io::Result<()> {
    let tmp = temp_sibling(path);
    let result = write_synced(&tmp, data, mode).and_then(|()| rename_durable(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Whether `name` is one ordinary path component: not empty, not `.` or `..`,
/// no separator, no NUL. Use it before joining a name that came from the network
/// or from a file on disk into a path that is later deleted or written.
pub(crate) fn is_single_component(name: &str) -> bool {
    use std::path::Component;
    if name.is_empty() || name.contains(['/', '\\', '\0']) {
        return false;
    }
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    )
}

/// Name `path` as `<path>.corrupt-<unix seconds>` (with a `-N` suffix when that
/// name is taken), so a bad file can be kept for inspection.
fn corrupt_sibling(path: &Path) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let base = path.as_os_str().to_string_lossy().into_owned();
    let mut candidate = PathBuf::from(format!("{base}.corrupt-{secs}"));
    let mut n = 1u32;
    while candidate.exists() {
        candidate = PathBuf::from(format!("{base}.corrupt-{secs}-{n}"));
        n += 1;
    }
    candidate
}

/// Move an unreadable file to `<path>.corrupt-<timestamp>` and return the new path.
pub(crate) fn move_aside_corrupt(path: &Path) -> io::Result<PathBuf> {
    let target = corrupt_sibling(path);
    fs::rename(path, &target)?;
    sync_dir(parent_of(path))?;
    Ok(target)
}

/// Copy a file to `<path>.corrupt-<timestamp>` and return the new path.
pub(crate) fn copy_aside_corrupt(path: &Path) -> io::Result<PathBuf> {
    let target = corrupt_sibling(path);
    fs::copy(path, &target)?;
    Ok(target)
}

fn write_synced(tmp: &Path, data: &[u8], mode: Option<u32>) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    if let Some(mode) = mode {
        set_create_mode(&mut options, mode);
    }
    let mut file = options.open(tmp)?;
    file.write_all(data)?;
    file.sync_all()
}

#[cfg(unix)]
fn set_create_mode(options: &mut OpenOptions, mode: u32) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(mode);
}

#[cfg(not(unix))]
fn set_create_mode(_options: &mut OpenOptions, _mode: u32) {}

pub(crate) fn temp_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp_name = format!(".{name}.tmp-{}-{}", std::process::id(), Uuid::new_v4());
    parent_of(path).join(tmp_name)
}

fn parent_of(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn writes_content_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        write_atomic(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
        assert_eq!(names(dir.path()), vec!["a.json"]);
    }

    #[test]
    fn overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        write_atomic(&path, b"first, longer payload").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(names(dir.path()), vec!["a.json"]);
    }

    #[test]
    fn failure_cleans_up_temp() {
        let dir = tempfile::tempdir().unwrap();
        // Target is a non-empty directory, so the rename fails.
        let path = dir.path().join("target");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("x"), b"x").unwrap();
        assert!(write_atomic(&path, b"data").is_err());
        assert_eq!(names(dir.path()), vec!["target"]);
    }

    #[test]
    fn concurrent_writers_both_succeed_with_full_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("archive.json"));
        let payloads: Vec<Vec<u8>> = vec![vec![b'a'; 256 * 1024], vec![b'b'; 300 * 1024]];
        let handles: Vec<_> = payloads
            .iter()
            .cloned()
            .map(|p| {
                let path = Arc::clone(&path);
                thread::spawn(move || {
                    for _ in 0..20 {
                        write_atomic(&path, &p).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let got = fs::read(&*path).unwrap();
        assert!(payloads.contains(&got), "result is not one full payload");
        assert_eq!(names(dir.path()), vec!["archive.json"]);
    }

    #[test]
    fn single_component_accepts_only_plain_names() {
        for ok in [
            "2026.10.01",
            "yt-dlp_linux",
            "phlegethon-launch-sites",
            ".hidden",
        ] {
            assert!(is_single_component(ok), "{ok}");
        }
        for bad in [
            "", ".", "..", "../x", "a/b", "/abs", "a\\b", "x\0y", "..\\..", "a/", "/",
        ] {
            assert!(!is_single_component(bad), "{bad:?}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn private_write_is_owner_only_even_over_a_wider_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        write_atomic(&path, b"public first").unwrap();
        write_atomic_private(&path, b"secret").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"secret");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(names(dir.path()), vec!["secrets.json"]);
    }

    #[test]
    fn rename_durable_moves_file() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::write(&from, b"x").unwrap();
        rename_durable(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(fs::read(&to).unwrap(), b"x");
    }
}
