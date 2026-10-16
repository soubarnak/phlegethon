//! WS2: Discard and persistence order (M-1, M-2, M-5, M-6).

use download_core::*;
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

const VIDEO: &str = "https://www.youtube.com/watch?v=ws2";

fn core_with(
    toolchain: FakeExtractorToolchain,
    root: &Path,
) -> (Core, crossbeam_channel::Receiver<CoreEvent>) {
    let (mut core, rx) = Core::new(Box::new(NoHttp), Box::new(toolchain));
    core.set_library_root(root.to_path_buf()).unwrap();
    (core, rx)
}

fn one_video() -> FakeExtractorToolchain {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        VIDEO,
        "Clip",
        "https://cdn.youtube.com/ws2.mp4",
        vec![("720p", "720p")],
    );
    toolchain
}

fn two_videos() -> FakeExtractorToolchain {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert_batch(
        VIDEO,
        "Pair",
        vec![
            (
                "a",
                "Clip A",
                "https://www.youtube.com/watch?v=a",
                vec![("720p", "720p")],
            ),
            (
                "b",
                "Clip B",
                "https://www.youtube.com/watch?v=b",
                vec![("720p", "720p")],
            ),
        ],
    );
    toolchain
}

fn select(items: &[(&str, &str)]) -> ProbeSelection {
    ProbeSelection {
        url: VIDEO.to_string(),
        items: items
            .iter()
            .map(|(item, quality)| ItemSelection {
                item_id: (*item).to_string(),
                quality_id: (*quality).to_string(),
            })
            .collect(),
        media_extras: None,
    }
}

fn drain(rx: &crossbeam_channel::Receiver<CoreEvent>) -> Vec<CoreEvent> {
    rx.try_iter().collect()
}

fn queue_file(root: &Path) -> PathBuf {
    root.join(".phlegethon").join("jobs.json")
}

fn snapshot_ids(core: &Core) -> Vec<String> {
    core.list_job_snapshots()
        .into_iter()
        .map(|j| j.job_id)
        .collect()
}

#[cfg(unix)]
#[test]
fn m1_failed_delete_keeps_the_job_so_discard_can_be_retried() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, rx) = core_with(FakeExtractorToolchain::new(), &root);
    let EnqueueOutcome::EnqueueDirect {
        job_id,
        destination,
    } = core.enqueue("https://example.com/file.zip", None).unwrap()
    else {
        panic!("expected a direct job");
    };
    let locked = destination.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("partial.bin"), b"partial").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
    let restore = |mode| std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(mode));
    if std::fs::write(locked.join("probe"), b"x").is_ok() {
        restore(0o700).unwrap();
        return; // permissions are not enforced (running as root)
    }
    drain(&rx);

    let err = core.discard_job(&job_id).unwrap_err();
    assert!(err.contains("delete Job Destination"), "got: {err}");
    assert_eq!(snapshot_ids(&core), vec![job_id.clone()]);
    assert!(
        !drain(&rx)
            .iter()
            .any(|e| matches!(e, CoreEvent::JobDiscarded { .. })),
        "a failed Discard must not announce success"
    );
    // A restart agrees with memory.
    let (reloaded, _rx2) = core_with(FakeExtractorToolchain::new(), &root);
    assert_eq!(snapshot_ids(&reloaded), vec![job_id.clone()]);

    restore(0o700).unwrap();
    core.discard_job(&job_id).unwrap();
    assert!(snapshot_ids(&core).is_empty());
    assert!(!destination.exists());
}

/// Raises a job's discard flag (and nothing else) while its download runs, so
/// the Discard "arrives as the transfer finishes": the transfer never sees a
/// stop signal and ends Completed.
struct DiscardDuringDownload {
    inner: FakeExtractorToolchain,
    discard: DiscardSlot,
}

type DiscardSlot =
    std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>>;

impl ExtractorToolchain for DiscardDuringDownload {
    fn probe(
        &self,
        url: &str,
        cookies: &CookieChannel,
        proxy: &ProxySettings,
    ) -> Result<ProbeResult, String> {
        self.inner.probe(url, cookies, proxy)
    }

    fn download(
        &self,
        url: &str,
        format: &str,
        dest_dir: &Path,
        stop: &std::sync::atomic::AtomicBool,
        media_extras: &MediaExtrasOptions,
        cookies: &CookieChannel,
        proxy: &ProxySettings,
        output_template: &str,
    ) -> Result<ExtractorDownloadOutcome, String> {
        if let Some(flag) = self.discard.lock().unwrap().as_ref() {
            flag.store(true, std::sync::atomic::Ordering::Release);
        }
        self.inner.download(
            url,
            format,
            dest_dir,
            stop,
            media_extras,
            cookies,
            proxy,
            output_template,
        )
    }
}

#[test]
fn m2_discard_that_arrives_as_the_transfer_finishes_is_honoured() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let slot = DiscardSlot::default();
    let toolchain = DiscardDuringDownload {
        inner: one_video(),
        discard: slot.clone(),
    };
    let (mut core, rx) = Core::new(Box::new(NoHttp), Box::new(toolchain));
    core.set_library_root(root.clone()).unwrap();
    core.probe(VIDEO).unwrap();
    let ids = core
        .confirm_probe_selection(&select(&[("item-1", "720p")]), None)
        .unwrap();
    let destination = PathBuf::from(core.list_job_snapshots()[0].destination.clone());
    *slot.lock().unwrap() = core.job_signals().request_discard(&ids[0]);
    drain(&rx);

    core.process_pending().unwrap();

    assert!(
        snapshot_ids(&core).is_empty(),
        "the Discard must not be lost"
    );
    assert!(
        drain(&rx)
            .iter()
            .any(|e| matches!(e, CoreEvent::JobDiscarded { job_id } if job_id == &ids[0])),
        "JobDiscarded must be announced"
    );
    // Completed-job semantics: the row goes, the finished file stays.
    let files: Vec<_> = std::fs::read_dir(&destination).unwrap().collect();
    assert_eq!(files.len(), 1, "the completed file stays on disk");
}

#[test]
fn m5_failed_archive_save_does_not_undo_a_completed_job() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, rx) = core_with(one_video(), &root);
    core.probe(VIDEO).unwrap();
    let ids = core
        .confirm_probe_selection(&select(&[("item-1", "720p")]), None)
        .unwrap();
    // A directory where archive.json belongs makes every archive save fail.
    std::fs::create_dir_all(root.join(".phlegethon").join("archive.json").join("x")).unwrap();
    drain(&rx);

    core.process_pending().unwrap();

    assert!(drain(&rx)
        .iter()
        .any(|e| matches!(e, CoreEvent::JobCompleted { .. })));
    let raw = std::fs::read_to_string(queue_file(&root)).unwrap();
    let queue: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(queue["jobs"][0]["id"], ids[0].as_str());
    assert_eq!(
        queue["jobs"][0]["state"], "completed",
        "jobs.json must agree with the JobCompleted event"
    );
}

#[test]
fn m6_confirm_validates_the_whole_selection_before_queueing_anything() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = core_with(two_videos(), &tmp.path().join("library"));
    core.probe(VIDEO).unwrap();

    let err = core
        .confirm_probe_selection(&select(&[("a", "720p"), ("b", "no-such-quality")]), None)
        .unwrap_err();
    assert!(err.contains("quality 'no-such-quality' not found"), "{err}");
    assert!(
        snapshot_ids(&core).is_empty(),
        "no job may be queued from a rejected selection"
    );

    // The probe is still pending, so a corrected selection works once.
    let ids = core
        .confirm_probe_selection(&select(&[("a", "720p"), ("b", "720p")]), None)
        .unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(snapshot_ids(&core).len(), 2);
}

#[test]
fn m6_failed_save_rolls_back_enqueue_and_confirm() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, _rx) = core_with(two_videos(), &root);
    core.probe(VIDEO).unwrap();
    // A directory where jobs.json belongs makes every queue save fail.
    let blocker = queue_file(&root);
    std::fs::create_dir_all(blocker.join("x")).unwrap();

    assert!(core.enqueue("https://example.com/file.zip", None).is_err());
    assert!(snapshot_ids(&core).is_empty(), "enqueue must roll back");
    assert!(core
        .confirm_probe_selection(&select(&[("a", "720p"), ("b", "720p")]), None)
        .is_err());
    assert!(snapshot_ids(&core).is_empty(), "confirm must roll back");

    // Once saving works again, confirming creates each job exactly once.
    std::fs::remove_dir_all(&blocker).unwrap();
    let ids = core
        .confirm_probe_selection(&select(&[("a", "720p"), ("b", "720p")]), None)
        .unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(snapshot_ids(&core).len(), 2);
}
