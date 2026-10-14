//! Disk-space pre-check before starting transfers with a known total size.

use std::path::Path;

/// Fixed safety margin when the download size is small.
pub const DISK_SPACE_MARGIN_BYTES: u64 = 64 * 1024 * 1024;
/// Percentage safety margin applied to the remaining download size.
pub const DISK_SPACE_MARGIN_PERCENT: u64 = 5;

/// Extra bytes reserved beyond the remaining download size.
pub fn disk_space_margin_bytes(remaining_bytes: u64) -> u64 {
    let percent_margin = remaining_bytes.saturating_mul(DISK_SPACE_MARGIN_PERCENT) / 100;
    DISK_SPACE_MARGIN_BYTES.max(percent_margin)
}

/// Free bytes required to start (or resume) writing `remaining_bytes`.
pub fn required_free_bytes(remaining_bytes: u64) -> u64 {
    remaining_bytes.saturating_add(disk_space_margin_bytes(remaining_bytes))
}

/// Whether `free_bytes` can accommodate `remaining_bytes` plus the safety margin.
pub fn has_sufficient_free_space(free_bytes: u64, remaining_bytes: u64) -> bool {
    free_bytes >= required_free_bytes(remaining_bytes)
}

/// User-facing error when a known-size job cannot start for lack of disk space.
pub fn insufficient_disk_space_error(
    destination: &Path,
    free_bytes: u64,
    remaining_bytes: u64,
) -> String {
    let margin = disk_space_margin_bytes(remaining_bytes);
    let required = required_free_bytes(remaining_bytes);
    format!(
        "insufficient disk space at {}: need {required} bytes ({remaining_bytes} remaining + {margin} margin) but {free_bytes} available",
        destination.display()
    )
}

/// Lookup free space for a path (typically the Job Destination parent volume).
pub trait FreeSpaceLookup: Send + Sync {
    fn free_bytes(&self, path: &Path) -> Result<u64, String>;
}

/// Production lookup via the host filesystem.
pub struct SystemFreeSpace;

impl FreeSpaceLookup for SystemFreeSpace {
    fn free_bytes(&self, path: &Path) -> Result<u64, String> {
        // The Job Destination usually does not exist yet when the guard runs;
        // `statvfs` needs an existing path, so query the nearest existing ancestor.
        let probe = nearest_existing_ancestor(path);
        fs2::available_space(probe).map_err(|e| {
            format!(
                "query free disk space at {}: {e}",
                probe.display()
            )
        })
    }
}

/// Walk up from `path` to the first component that exists on disk.
///
/// Falls back to `path` itself when nothing exists (the caller then reports
/// the OS error for that path).
pub fn nearest_existing_ancestor(path: &Path) -> &Path {
    let mut current = path;
    loop {
        if current.exists() {
            return current;
        }
        match current.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => current = parent,
            _ => return path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_existing_ancestor_walks_up_to_existing_dir() {
        let tmp = std::env::temp_dir();
        let missing = tmp.join("phlegethon-missing-a").join("missing-b");
        assert_eq!(nearest_existing_ancestor(&missing), tmp.as_path());
        assert_eq!(nearest_existing_ancestor(&tmp), tmp.as_path());
    }

    #[test]
    fn system_free_space_works_for_not_yet_created_destination() {
        let tmp = std::env::temp_dir();
        let missing = tmp.join("phlegethon-missing-job-dir").join("job-123");
        let free = SystemFreeSpace.free_bytes(&missing).unwrap();
        assert!(free > 0);
    }

    #[test]
    fn margin_uses_sixty_four_mb_floor_for_small_downloads() {
        assert_eq!(disk_space_margin_bytes(100), DISK_SPACE_MARGIN_BYTES);
    }

    #[test]
    fn margin_uses_five_percent_for_large_downloads() {
        let two_gib = 2 * 1024 * 1024 * 1024;
        assert_eq!(disk_space_margin_bytes(two_gib), two_gib / 20);
    }

    #[test]
    fn required_free_includes_margin() {
        let remaining = 1_000_000;
        assert_eq!(
            required_free_bytes(remaining),
            remaining + disk_space_margin_bytes(remaining)
        );
    }

    #[test]
    fn has_sufficient_free_space_compares_against_required() {
        let remaining = 512;
        let required = required_free_bytes(remaining);
        assert!(!has_sufficient_free_space(required - 1, remaining));
        assert!(has_sufficient_free_space(required, remaining));
    }
}
