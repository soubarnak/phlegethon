//! WS2: restoring the queue from `jobs.json` and switching the Library Root
//! (H-7, H-8, M-4, M-7, M-8, L-9, L-10, L-11).

use download_core::*;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

/// HTTP backend for tests that never reach the network.
struct NoHttp;

impl HttpBackend for NoHttp {
    fn fetch(
        &self,
        url: &str,
        _range: Option<std::ops::Range<u64>>,
        _sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        Err(format!("no network in this test: {url}"))
    }

    fn supports_range(&self, url: &str) -> Result<bool, String> {
        Err(format!("no network in this test: {url}"))
    }

    fn file_size(&self, url: &str) -> Result<u64, String> {
        Err(format!("no network in this test: {url}"))
    }
}

fn new_core() -> Core {
    Core::new(Box::new(NoHttp), Box::new(FakeExtractorToolchain::new())).0
}

fn queue_file(root: &Path) -> PathBuf {
    root.join(".phlegethon").join("jobs.json")
}

/// Create a Library Root holding `n` direct jobs and return their ids.
fn root_with_jobs(root: &Path, n: usize) -> Vec<String> {
    let mut core = new_core();
    core.set_library_root(root.to_path_buf()).unwrap();
    (0..n)
        .map(|i| {
            match core
                .enqueue(&format!("https://example.com/file{i}.zip"), None)
                .unwrap()
            {
                EnqueueOutcome::EnqueueDirect { job_id, .. } => job_id,
                other => panic!("expected a direct job, got {other:?}"),
            }
        })
        .collect()
}

fn edit_queue(root: &Path, edit: impl FnOnce(&mut Value)) {
    let path = queue_file(root);
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut value);
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

fn corrupt_copies(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("{prefix}.corrupt-"))
        })
        .collect()
}

fn job_ids(core: &Core) -> Vec<String> {
    core.list_job_snapshots()
        .into_iter()
        .map(|j| j.job_id)
        .collect()
}

#[test]
fn h8_unreadable_queue_file_is_moved_aside_and_the_root_stays_usable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    root_with_jobs(&root, 1);
    let truncated = b"{\"version\": 1, \"jobs\": [{\"id\": \"abc".to_vec();
    std::fs::write(queue_file(&root), &truncated).unwrap();

    let mut core = new_core();
    let report = core.set_library_root(root.clone()).unwrap();

    assert_eq!(core.library_root(), Some(root.as_path()));
    assert!(core.list_job_snapshots().is_empty());
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("jobs.json"));
    let moved = corrupt_copies(&root.join(".phlegethon"), "jobs.json");
    assert_eq!(moved.len(), 1);
    assert_eq!(std::fs::read(&moved[0]).unwrap(), truncated);

    // The root works again: a new job is queued and persisted.
    core.enqueue("https://example.com/after.zip", None).unwrap();
    let mut reloaded = new_core();
    let report = reloaded.set_library_root(root).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(reloaded.list_job_snapshots().len(), 1);
}

#[test]
fn h8_queue_file_from_a_newer_version_is_moved_aside_too() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    root_with_jobs(&root, 1);
    edit_queue(&root, |v| v["version"] = Value::from(99));

    let mut core = new_core();
    let report = core.set_library_root(root.clone()).unwrap();
    assert!(core.list_job_snapshots().is_empty());
    assert!(report.warnings[0].contains("unsupported queue version 99"));
    assert_eq!(
        corrupt_copies(&root.join(".phlegethon"), "jobs.json").len(),
        1
    );
}

#[test]
fn h8_one_bad_row_is_skipped_and_the_others_load() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let ids = root_with_jobs(&root, 2);
    edit_queue(&root, |v| v["jobs"][0] = serde_json::json!({ "id": 5 }));

    let mut core = new_core();
    let report = core.set_library_root(root.clone()).unwrap();

    assert_eq!(job_ids(&core), vec![ids[1].clone()]);
    assert!(
        report.warnings.iter().any(|w| w.contains("entry 1")),
        "{:?}",
        report.warnings
    );
    // The original file is kept, because the next save drops the bad row.
    assert_eq!(
        corrupt_copies(&root.join(".phlegethon"), "jobs.json").len(),
        1
    );
}

#[test]
fn m4_corrupt_archive_is_moved_aside_and_the_root_is_set() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let ids = root_with_jobs(&root, 1);
    std::fs::write(root.join(".phlegethon").join("archive.json"), b"{ not json").unwrap();

    let mut core = new_core();
    let report = core.set_library_root(root.clone()).unwrap();

    assert_eq!(core.library_root(), Some(root.as_path()));
    assert_eq!(job_ids(&core), ids);
    assert!(core.is_history_empty());
    assert!(report.warnings.iter().any(|w| w.contains("archive.json")));
    assert_eq!(
        corrupt_copies(&root.join(".phlegethon"), "archive.json").len(),
        1
    );
}

#[test]
fn m4_failed_root_switch_leaves_the_previous_root_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let root_a = tmp.path().join("a");
    let ids = root_with_jobs(&root_a, 1);
    let root_b = tmp.path().join("b");
    // An archive that cannot be read at all (a directory) is a hard error.
    std::fs::create_dir_all(root_b.join(".phlegethon").join("archive.json")).unwrap();

    let mut core = new_core();
    core.set_library_root(root_a.clone()).unwrap();
    assert!(core.set_library_root(root_b).is_err());

    assert_eq!(core.library_root(), Some(root_a.as_path()));
    assert_eq!(job_ids(&core), ids, "jobs must not be swapped on failure");
}

#[test]
fn h7_library_root_cannot_change_while_a_transfer_is_active() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let ids = root_with_jobs(&root, 1);
    let mut core = new_core();
    core.set_library_root(root.clone()).unwrap();
    let signals = core.job_signals();

    signals.set_transfer_active_for_test(&ids[0], true);
    for target in [root.clone(), tmp.path().join("other")] {
        let err = core.set_library_root(target).unwrap_err();
        assert!(err.contains("transfers are active"), "got: {err}");
    }
    assert_eq!(core.library_root(), Some(root.as_path()));
    assert_eq!(
        job_ids(&core),
        ids,
        "the running job must stay in the queue"
    );

    signals.set_transfer_active_for_test(&ids[0], false);
    core.set_library_root(root).unwrap();
}

#[test]
fn l9_old_job_signals_are_unregistered_on_a_root_change() {
    let tmp = tempfile::tempdir().unwrap();
    let root_a = tmp.path().join("a");
    let ids = root_with_jobs(&root_a, 1);
    let mut core = new_core();
    core.set_library_root(root_a).unwrap();
    let signals = core.job_signals();
    assert!(signals.request_pause(&ids[0]).is_some());

    core.set_library_root(tmp.path().join("b")).unwrap();

    assert!(signals.request_pause(&ids[0]).is_none());
    assert!(signals.request_discard(&ids[0]).is_none());
}

#[test]
fn m7_paused_job_with_a_missing_part_stays_paused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let ids = root_with_jobs(&root, 1);
    edit_queue(&root, |v| {
        v["jobs"][0]["state"] = Value::from("paused");
        v["jobs"][0]["bytes_received"] = Value::from(500);
    });

    let mut core = new_core();
    core.set_library_root(root).unwrap();

    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == ids[0])
        .unwrap();
    assert_eq!(snap.status, "paused");
    assert_eq!(snap.bytes_downloaded, 0);
    assert!(snap.last_error.is_some(), "the user needs an explanation");
}

#[test]
fn m7_paused_segmented_job_with_a_missing_part_stays_paused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let ids = root_with_jobs(&root, 1);
    edit_queue(&root, |v| {
        v["jobs"][0]["state"] = Value::from("paused");
        v["jobs"][0]["bytes_received"] = Value::from(500);
        v["jobs"][0]["segments"] = serde_json::json!([
            {"start": 0, "end": 1000, "written": 500}
        ]);
    });

    let mut core = new_core();
    core.set_library_root(root).unwrap();

    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == ids[0])
        .unwrap();
    assert_eq!(snap.status, "paused");
    assert_eq!(snap.bytes_downloaded, 0);
    assert!(snap.last_error.is_some(), "the user needs an explanation");
}

#[test]
fn m7_downloading_job_with_a_missing_part_fails_with_a_message() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let ids = root_with_jobs(&root, 1);
    edit_queue(&root, |v| {
        v["jobs"][0]["state"] = Value::from("downloading");
        v["jobs"][0]["bytes_received"] = Value::from(500);
    });

    let mut core = new_core();
    core.set_library_root(root).unwrap();

    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == ids[0])
        .unwrap();
    assert_eq!(snap.status, "failed");
    assert!(snap.last_error.is_some());
}

#[test]
fn m8_stored_active_job_limit_of_zero_falls_back_to_the_default() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    root_with_jobs(&root, 1);
    edit_queue(&root, |v| v["active_job_limit"] = Value::from(0));

    let mut core = new_core();
    let report = core.set_library_root(root).unwrap();
    assert_eq!(core.active_job_limit(), DEFAULT_ACTIVE_JOB_LIMIT);
    assert!(report
        .warnings
        .iter()
        .any(|w| w.contains("Active Job Limit")));
}

#[test]
fn m8_rows_with_unsafe_destination_or_filename_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let ids = root_with_jobs(&root, 6);
    let shared = tmp.path().join("Downloads");
    edit_queue(&root, |v| {
        let rows = &mut v["jobs"];
        rows[0]["destination"] = Value::from("relative/dir");
        rows[1]["filename"] = Value::from("../escape.bin");
        rows[2]["filename"] = Value::from("sub/dir.bin");
        rows[3]["destination"] = Value::from(root.display().to_string());
        rows[4]["destination"] = Value::from(shared.display().to_string());
        // rows[5] stays valid
    });

    let mut core = new_core();
    let report = core.set_library_root(root).unwrap();

    assert_eq!(job_ids(&core), vec![ids[5].clone()]);
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|w| w.starts_with("Skipped queue entry"))
            .count(),
        5,
        "{:?}",
        report.warnings
    );
}

#[test]
fn l10_duplicate_job_ids_keep_only_the_first_row() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let ids = root_with_jobs(&root, 2);
    edit_queue(&root, |v| {
        let first = v["jobs"][0].clone();
        v["jobs"][1] = first;
    });

    let mut core = new_core();
    let report = core.set_library_root(root).unwrap();

    assert_eq!(job_ids(&core), vec![ids[0].clone()]);
    assert!(report
        .warnings
        .iter()
        .any(|w| w.contains("duplicate job id")));
}

#[test]
fn l11_relative_cookie_files_in_the_queue_file_are_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let ids = root_with_jobs(&root, 1);
    edit_queue(&root, |v| {
        v["cookie_channel"] = serde_json::json!({ "cookie_file": "cookies.txt" });
        v["jobs"][0]["cookie_session"] = serde_json::json!({ "cookie_file": "session.txt" });
    });

    let mut core = new_core();
    let report = core.set_library_root(root).unwrap();

    assert!(!core.cookie_channel().is_configured());
    assert_eq!(core.job_cookie_session(&ids[0]).unwrap(), None);
    assert!(report.warnings.iter().any(|w| w.contains("cookie")));
}
