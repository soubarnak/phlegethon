//! Integration tests for Download Core public API.
//!
//! These tests exercise the highest seam — Core commands → events — with a
//! fake HTTP backend (no live network).  Every acceptance criterion from
//! issue #14 is covered here.

use download_core::*;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

fn write_fetch_to_sink(
    sink: &mut dyn Write,
    data: &[u8],
    total: u64,
) -> Result<FetchResult, String> {
    let mut written = 0usize;
    while written < data.len() {
        let n = sink
            .write(&data[written..])
            .map_err(|e| format!("sink write: {e}"))?;
        if n == 0 {
            break;
        }
        written += n;
    }
    Ok(FetchResult {
        total_length: total,
        bytes_written: written as u64,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A fake HTTP backend that returns canned content.
///
/// Supports Range headers: when `range` is `None`, returns full content.
/// When `range` is `Some(start..)`, returns bytes from `start` to end.
#[derive(Clone)]
struct FakeHttp {
    responses: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
}

/// Process pending jobs and accumulate completion state across calls.
/// Returns the set of job IDs that have completed (across all calls).
fn process_and_accumulate_completions(
    core: &mut Core,
    rx: &crossbeam_channel::Receiver<CoreEvent>,
    done: &mut std::collections::HashSet<String>,
) {
    core.process_pending().unwrap();
    let events = collect_events(rx);
    for e in &events {
        if let CoreEvent::JobCompleted { job_id, .. } = e {
            done.insert(job_id.clone());
        }
    }
}

/// Process until a specific job ID completes, accumulating all completions.
fn process_until_job_done(
    core: &mut Core,
    rx: &crossbeam_channel::Receiver<CoreEvent>,
    target: &str,
) -> std::collections::HashSet<String> {
    let mut done = std::collections::HashSet::new();
    while !done.contains(target) {
        process_and_accumulate_completions(core, rx, &mut done);
    }
    done
}

impl HttpBackend for FakeHttp {
    fn fetch(
        &self,
        url: &str,
        range: Option<std::ops::Range<u64>>,
        sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        let responses = self.responses.lock().unwrap();
        let full_body = responses
            .get(url)
            .ok_or_else(|| format!("no fake response for {url}"))?;
        let total = full_body.len() as u64;

        let data = match range {
            Some(r) => {
                let start = r.start.min(total);
                let end = r.end.min(total).max(start);
                &full_body[start as usize..end as usize]
            }
            None => full_body.as_slice(),
        };

        write_fetch_to_sink(sink, data, total)
    }

    fn supports_range(&self, _url: &str) -> Result<bool, String> {
        Ok(true)
    }

    fn file_size(&self, url: &str) -> Result<u64, String> {
        let responses = self.responses.lock().unwrap();
        let full_body = responses
            .get(url)
            .ok_or_else(|| format!("no fake response for {url}"))?;
        Ok(full_body.len() as u64)
    }
}

fn fake_http() -> FakeHttp {
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/file.zip".into(),
        b"hello world this is a test file".to_vec(),
    );
    FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    }
}

/// A fake HTTP backend that always fails for any URL.
#[derive(Clone)]
struct FailingHttp;

impl HttpBackend for FailingHttp {
    fn fetch(
        &self,
        url: &str,
        _range: Option<std::ops::Range<u64>>,
        _sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        Err(format!("simulated failure for {url}"))
    }

    fn supports_range(&self, url: &str) -> Result<bool, String> {
        Err(format!("simulated failure for {url}"))
    }

    fn file_size(&self, url: &str) -> Result<u64, String> {
        Err(format!("simulated failure for {url}"))
    }
}

fn fake_toolchain() -> FakeExtractorToolchain {
    FakeExtractorToolchain::new()
}

fn enqueue_direct_job_id(core: &mut Core, url: &str) -> String {
    let result = core.enqueue(url, None).unwrap();
    match result {
        EnqueueOutcome::EnqueueDirect { job_id, .. } => job_id,
        _ => panic!("expected enqueue_direct, got probe_then_confirm"),
    }
}

fn collect_events(rx: &crossbeam_channel::Receiver<CoreEvent>) -> Vec<CoreEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

fn make_core(http: Box<dyn HttpBackend>) -> (Core, crossbeam_channel::Receiver<CoreEvent>) {
    Core::new(http, Box::new(fake_toolchain()))
}

// ---------------------------------------------------------------------------
// Acceptance: create/destroy and Library Root
// ---------------------------------------------------------------------------

#[test]
fn can_create_destroy_and_set_library_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(root.clone()).unwrap();
    assert!(root.exists());
    drop(core); // destroy — no panic
}

// ---------------------------------------------------------------------------
// Acceptance: enqueue_direct classification
// ---------------------------------------------------------------------------

#[test]
fn enqueue_clear_http_returns_enqueue_direct() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let result = core.enqueue("https://example.com/file.zip", None);
    assert!(matches!(
        result,
        Ok(EnqueueOutcome::EnqueueDirect {
            job_id: _,
            destination: _
        })
    ));
}

// ---------------------------------------------------------------------------
// Acceptance: job writes bytes under Library Root / Job Destination
// ---------------------------------------------------------------------------

#[test]
fn enqueue_direct_job_writes_bytes_to_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(fake_http()));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let result = core.enqueue("https://example.com/file.zip", None).unwrap();
    let (_job_id, destination) = match result {
        EnqueueOutcome::EnqueueDirect {
            job_id,
            destination,
        } => (job_id, destination),
        _ => panic!("expected enqueue_direct"),
    };

    // Process until complete
    core.process_pending().unwrap();

    // Check job directory exists
    assert!(destination.exists(), "job directory should exist");

    // Check the file was written
    let entries: Vec<_> = std::fs::read_dir(&destination)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(!entries.is_empty(), "job directory should contain files");

    // Check a completed event was emitted
    let events = collect_events(&rx);
    let has_completed = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(has_completed, "should emit JobCompleted event");

    // Check progress events were emitted
    let has_progress = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobProgress { .. }));
    assert!(has_progress, "should emit JobProgress event");
}

/// HTTP fake that forces the single-connection path (no Range support).
#[derive(Clone)]
struct NoRangeFakeHttp(FakeHttp);

impl HttpBackend for NoRangeFakeHttp {
    fn fetch(
        &self,
        url: &str,
        range: Option<std::ops::Range<u64>>,
        sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        self.0.fetch(url, range, sink)
    }

    fn supports_range(&self, _url: &str) -> Result<bool, String> {
        Ok(false)
    }

    fn file_size(&self, url: &str) -> Result<u64, String> {
        self.0.file_size(url)
    }
}

#[test]
fn single_connection_enqueue_writes_matching_bytes_via_part_file() {
    let expected = b"hello world this is a test file";
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let result = core.enqueue("https://example.com/file.zip", None).unwrap();
    let (job_id, destination) = match result {
        EnqueueOutcome::EnqueueDirect {
            job_id,
            destination,
        } => (job_id, destination),
        _ => panic!("expected enqueue_direct"),
    };

    core.process_pending().unwrap();
    let events = collect_events(&rx);

    let final_path = destination.join("file.zip");
    assert!(final_path.exists(), "final file should exist after rename");
    assert!(
        !destination.join("file.zip.part").exists(),
        "partial file should be renamed away on success"
    );

    let on_disk = std::fs::read(&final_path).unwrap();
    assert_eq!(on_disk, expected, "on-disk bytes should match source");

    let progress_bytes = events.iter().rev().find_map(|e| match e {
        CoreEvent::JobProgress {
            bytes_downloaded,
            total_bytes: Some(total),
            ..
        } => Some((*bytes_downloaded, *total)),
        _ => None,
    });
    assert!(
        progress_bytes.is_some(),
        "should emit progress with total bytes"
    );
    let (downloaded, total) = progress_bytes.unwrap();
    assert_eq!(downloaded, expected.len() as u64);
    assert_eq!(total, expected.len() as u64);
    assert_eq!(
        downloaded,
        on_disk.len() as u64,
        "progress bytes should match on-disk size"
    );

    let completed_path = events.iter().find_map(|e| match e {
        CoreEvent::JobCompleted { path, .. } => Some(path.clone()),
        _ => None,
    });
    assert_eq!(completed_path, Some(final_path));

    // Sanity: job destination is under library root
    assert!(destination.starts_with(root.join("jobs")));
    let _ = job_id;
}

// ---------------------------------------------------------------------------
// Acceptance: enqueue non-HTTP(S) URL returns probe_then_confirm
// ---------------------------------------------------------------------------

#[test]
fn enqueue_launch_site_url_returns_probe_then_confirm() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    // A YouTube URL is not a clear single-file HTTP URL
    let result = core.enqueue("https://www.youtube.com/watch?v=abc123", None);
    assert!(
        matches!(result, Ok(EnqueueOutcome::ProbeThenConfirm { .. })),
        "Launch Site URL should be probe_then_confirm, got {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// Acceptance: Pause / Resume / Failed / Retry / Discard lifecycle
// ---------------------------------------------------------------------------

#[test]
fn pause_pending_job_emits_paused_event() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.pause_job(&job_id).unwrap();

    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobPaused { job_id: id } if id == &job_id)),
        "should emit JobPaused event, got {:?}",
        events
    );
}

#[test]
fn paused_job_is_skipped_by_process_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.pause_job(&job_id).unwrap();
    collect_events(&rx); // drain pause event

    // process_pending should NOT process the paused job
    core.process_pending().unwrap();

    let events = collect_events(&rx);
    let has_progress = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobProgress { .. }));
    assert!(
        !has_progress,
        "paused job should not produce progress events"
    );
    let has_completed = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(
        !has_completed,
        "paused job should not produce completed events"
    );
}

#[test]
fn resume_paused_job_emits_resumed_event_and_allows_processing() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.pause_job(&job_id).unwrap();
    collect_events(&rx); // drain pause

    core.resume_job(&job_id).unwrap();

    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobResumed { job_id: id } if id == &job_id)),
        "should emit JobResumed event, got {:?}",
        events
    );

    // Now process_pending should complete it
    core.process_pending().unwrap();
    let events2 = collect_events(&rx);
    assert!(
        events2
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "resumed job should complete after process_pending"
    );
}

#[test]
fn failed_job_emits_failed_event_and_remains_visible() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(FailingHttp));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/fail.zip");

    // Process — should fail
    let result = core.process_pending();
    assert!(
        result.is_ok(),
        "process_pending should not propagate HTTP errors"
    );

    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobFailed { job_id: id, .. } if id == &job_id)),
        "should emit JobFailed event, got {:?}",
        events
    );

    // Retry the failed job
    core.retry_job(&job_id).unwrap();
    let events2 = collect_events(&rx);
    assert!(
        events2
            .iter()
            .any(|e| matches!(e, CoreEvent::JobRetried { job_id: id } if id == &job_id)),
        "should emit JobRetried event, got {:?}",
        events2
    );
}

#[test]
fn discard_removes_job_from_queue_and_emits_discarded_event() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");

    core.discard_job(&job_id).unwrap();

    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobDiscarded { job_id: id } if id == &job_id)),
        "should emit JobDiscarded event, got {:?}",
        events
    );

    // Discarding a non-existent job should error
    let err = core.discard_job(&job_id);
    assert!(err.is_err(), "discarding a removed job should fail");
}

#[test]
fn discard_unfinished_job_deletes_job_destination_files() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/partial.zip");
    let job_dir = tmp.path().join("library").join("jobs").join(&job_id);
    std::fs::create_dir_all(&job_dir).unwrap();
    let partial = job_dir.join("partial.zip.part");
    std::fs::write(&partial, b"orphan-bytes").unwrap();
    assert!(partial.exists());

    core.pause_job(&job_id).unwrap();
    core.discard_job(&job_id).unwrap();

    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobDiscarded { job_id: id } if id == &job_id)),
        "should emit JobDiscarded, got {events:?}"
    );
    assert!(
        core.list_job_snapshots().iter().all(|j| j.job_id != job_id),
        "discarded job must leave the queue"
    );
    assert!(
        !job_dir.exists(),
        "unfinished Discard must delete Job Destination including partials"
    );
}

#[test]
fn discard_failed_job_deletes_job_destination_files() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(FailingHttp));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/fail.zip");
    core.process_pending().unwrap();

    let job_dir = tmp.path().join("library").join("jobs").join(&job_id);
    std::fs::create_dir_all(&job_dir).unwrap();
    std::fs::write(job_dir.join("fail.zip.part"), b"partial").unwrap();

    collect_events(&rx);
    core.discard_job(&job_id).unwrap();

    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobDiscarded { job_id: id } if id == &job_id)),
        "should emit JobDiscarded, got {events:?}"
    );
    assert!(
        !job_dir.exists(),
        "Failed Discard must delete Job Destination including partials"
    );
}

#[test]
fn discard_completed_job_keeps_files_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.process_pending().unwrap();

    let job_dir = tmp.path().join("library").join("jobs").join(&job_id);
    let final_path = job_dir.join("file.zip");
    assert!(
        final_path.exists(),
        "completed job should have written the file"
    );
    let bytes_before = std::fs::read(&final_path).unwrap();

    collect_events(&rx); // drain completion events
    core.discard_job(&job_id).unwrap();

    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobDiscarded { job_id: id } if id == &job_id)),
        "should emit JobDiscarded, got {events:?}"
    );
    assert!(
        core.list_job_snapshots().iter().all(|j| j.job_id != job_id),
        "completed Discard must still remove the queue row"
    );
    assert!(
        final_path.exists(),
        "Completed Discard must keep files on disk"
    );
    assert_eq!(
        std::fs::read(&final_path).unwrap(),
        bytes_before,
        "Completed Discard must not alter finished file contents"
    );
}

#[test]
fn discard_during_in_flight_transfer_stops_and_deletes_partials() {
    #[derive(Clone)]
    struct SlowNoRangeFakeHttp {
        responses: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
        bytes_written: Arc<Mutex<u64>>,
    }

    impl HttpBackend for SlowNoRangeFakeHttp {
        fn fetch(
            &self,
            url: &str,
            range: Option<std::ops::Range<u64>>,
            sink: &mut dyn Write,
        ) -> Result<FetchResult, String> {
            let responses = self.responses.lock().unwrap();
            let full_body = responses
                .get(url)
                .ok_or_else(|| format!("no fake response for {url}"))?;
            let total = full_body.len() as u64;
            let data = match range {
                Some(r) => {
                    let start = r.start.min(total);
                    let end = r.end.min(total).max(start);
                    &full_body[start as usize..end as usize]
                }
                None => full_body.as_slice(),
            };

            let mut written = 0u64;
            for byte in data {
                let n = sink
                    .write(std::slice::from_ref(byte))
                    .map_err(|e| format!("sink write: {e}"))?;
                if n == 0 {
                    break;
                }
                written += n as u64;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            *self.bytes_written.lock().unwrap() = written;
            Ok(FetchResult {
                total_length: total,
                bytes_written: written,
            })
        }

        fn supports_range(&self, _url: &str) -> Result<bool, String> {
            Ok(false)
        }

        fn file_size(&self, url: &str) -> Result<u64, String> {
            let responses = self.responses.lock().unwrap();
            Ok(responses
                .get(url)
                .map(|b| b.len() as u64)
                .ok_or_else(|| format!("no fake response for {url}"))?)
        }
    }

    let content = vec![b'D'; 256];
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/discard-mid.zip".into(),
        content.clone(),
    );
    let bytes_written = Arc::new(Mutex::new(0u64));
    let http = SlowNoRangeFakeHttp {
        responses: Arc::new(Mutex::new(map)),
        bytes_written: bytes_written.clone(),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx, _clock) = make_capped_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/discard-mid.zip");
    let signals = core.job_signals();
    let job_dir = tmp.path().join("library").join("jobs").join(&job_id);

    let core = Arc::new(Mutex::new(core));
    let rx = Arc::new(rx);
    let job_for_discard = job_id.clone();

    let worker = {
        let core = core.clone();
        let rx = rx.clone();
        std::thread::spawn(move || {
            let mut guard = core.lock().unwrap();
            guard.process_pending().unwrap();
            collect_events(&rx)
        })
    };

    std::thread::sleep(std::time::Duration::from_millis(50));
    // Mimic shell Discard: signal stop without waiting for the Core mutex.
    assert!(
        signals.signal_discard_stop(&job_for_discard),
        "in-flight Discard should see an active transfer"
    );

    let events = worker.join().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobDiscarded { job_id: id } if id == &job_id)),
        "discard during transfer should emit JobDiscarded, got: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "discarded transfer must not claim success"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobPaused { .. })),
        "discarded transfer must not emit JobPaused"
    );

    let snapshots = core.lock().unwrap().list_job_snapshots();
    assert!(
        snapshots.iter().all(|j| j.job_id != job_id),
        "discarded in-flight job must leave the queue"
    );
    assert!(
        !job_dir.exists(),
        "in-flight Discard must delete Job Destination including partials"
    );
    let written = *bytes_written.lock().unwrap();
    assert!(
        written > 0,
        "some bytes should have been written before Discard stopped the transfer"
    );
    assert!(
        written < content.len() as u64,
        "Discard must stop promptly before the full body finishes, got {written}"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: interruptible Extractor Toolchain Pause/Discard (issue #38)
// ---------------------------------------------------------------------------

#[test]
fn discard_during_in_flight_extractor_download_stops_and_deletes_partials() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=discard-ext",
        "Discard Extractor Video",
        "https://cdn.youtube.com/discard-ext.mp4",
        vec![("720p", "720p")],
    );
    let chunks_written = toolchain.enable_slow_download(64, std::time::Duration::from_millis(20));

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    core.probe("https://www.youtube.com/watch?v=discard-ext")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=discard-ext".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    let job_id = job_ids[0].clone();
    let signals = core.job_signals();
    let job_dir = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .map(|j| PathBuf::from(j.destination))
        .expect("job destination");

    let core = Arc::new(Mutex::new(core));
    let rx = Arc::new(rx);
    let job_for_discard = job_id.clone();

    let worker = {
        let core = core.clone();
        let rx = rx.clone();
        std::thread::spawn(move || {
            let mut guard = core.lock().unwrap();
            guard.process_pending().unwrap();
            collect_events(&rx)
        })
    };

    // Wait until the slow extractor has written at least one chunk.
    let started = std::time::Instant::now();
    while *chunks_written.lock().unwrap() == 0 {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "extractor download never became active"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert!(
        signals.signal_discard_stop(&job_for_discard),
        "in-flight extractor Discard should see an active transfer"
    );

    let events = worker.join().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobDiscarded { job_id: id } if id == &job_id)),
        "discard during extractor download should emit JobDiscarded, got: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "discarded extractor transfer must not claim success"
    );

    let snapshots = core.lock().unwrap().list_job_snapshots();
    assert!(
        snapshots.iter().all(|j| j.job_id != job_id),
        "discarded in-flight extractor job must leave the queue"
    );
    assert!(
        !job_dir.exists(),
        "in-flight extractor Discard must delete Job Destination including partials"
    );
    let written = *chunks_written.lock().unwrap();
    assert!(
        written > 0,
        "some chunks should have been written before Discard stopped the transfer"
    );
    assert!(
        written < 64,
        "Discard must stop promptly before the fake extractor finishes, got {written}"
    );
}

#[test]
fn pause_during_in_flight_extractor_download_stops_promptly() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=pause-ext",
        "Pause Extractor Video",
        "https://cdn.youtube.com/pause-ext.mp4",
        vec![("720p", "720p")],
    );
    let chunks_written = toolchain.enable_slow_download(64, std::time::Duration::from_millis(20));

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    core.probe("https://www.youtube.com/watch?v=pause-ext")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=pause-ext".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    let job_id = job_ids[0].clone();
    let signals = core.job_signals();
    let job_dir = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .map(|j| PathBuf::from(j.destination))
        .expect("job destination");

    let core = Arc::new(Mutex::new(core));
    let rx = Arc::new(rx);
    let job_for_pause = job_id.clone();

    let worker = {
        let core = core.clone();
        let rx = rx.clone();
        std::thread::spawn(move || {
            let mut guard = core.lock().unwrap();
            guard.process_pending().unwrap();
            collect_events(&rx)
        })
    };

    let started = std::time::Instant::now();
    while *chunks_written.lock().unwrap() == 0 {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "extractor download never became active"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    signals
        .request_pause(&job_for_pause)
        .expect("pause flag registered")
        .store(true, std::sync::atomic::Ordering::Release);

    let events = worker.join().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobPaused { job_id: id } if id == &job_id)),
        "pause during extractor download should emit JobPaused, got: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "paused extractor transfer must not claim success"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobDiscarded { .. })),
        "pause must not discard the job"
    );

    let snapshots = core.lock().unwrap().list_job_snapshots();
    let snap = snapshots
        .iter()
        .find(|j| j.job_id == job_id)
        .expect("paused job stays in queue");
    assert_eq!(snap.status, "paused");
    assert!(
        job_dir.exists(),
        "Pause keeps Job Destination (partials allowed)"
    );
    let written = *chunks_written.lock().unwrap();
    assert!(
        written > 0 && written < 64,
        "Pause must stop promptly mid-download, got {written}"
    );
}

#[test]
fn resume_after_pause_reruns_a_resumable_extractor_in_the_same_destination() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=resume-ext",
        "Resume Extractor Video",
        "https://cdn.youtube.com/resume-ext.mp4",
        vec![("720p", "720p")],
    );
    toolchain.enable_resume();
    let total_chunks = 40;
    let chunks_written =
        toolchain.enable_slow_download(total_chunks, std::time::Duration::from_millis(10));
    let calls = toolchain.clone();

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();
    core.probe("https://www.youtube.com/watch?v=resume-ext")
        .unwrap();
    collect_events(&rx);
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=resume-ext".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    let job_id = core.confirm_probe_selection(&selection, None).unwrap()[0].clone();
    let signals = core.job_signals();
    let job_dir = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .map(|j| PathBuf::from(j.destination))
        .expect("job destination");

    let core = Arc::new(Mutex::new(core));
    let rx = Arc::new(rx);
    let worker = {
        let (core, rx) = (core.clone(), rx.clone());
        std::thread::spawn(move || {
            core.lock().unwrap().process_pending().unwrap();
            collect_events(&rx)
        })
    };
    let started = std::time::Instant::now();
    while *chunks_written.lock().unwrap() < 5 {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "extractor download never became active"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    signals
        .request_pause(&job_id)
        .expect("pause flag registered")
        .store(true, std::sync::atomic::Ordering::Release);
    let events = worker.join().unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobPaused { job_id: id } if id == &job_id)));
    let paused_at = *chunks_written.lock().unwrap();
    assert!(paused_at < total_chunks, "pause came too late");

    let mut guard = core.lock().unwrap();
    guard.resume_job(&job_id).unwrap();
    guard.process_pending().unwrap();
    let events = collect_events(&rx);
    assert!(
        !events.iter().any(|e| matches!(
            e,
            CoreEvent::JobFailed { error, .. } if error.contains("cannot resume")
        )),
        "Resume must not fail an extractor job: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { job_id: id, .. } if id == &job_id)),
        "{events:?}"
    );
    let dests = calls.download_dests();
    assert_eq!(dests.len(), 2, "{dests:?}");
    assert_eq!(dests[0], dests[1], "Resume reuses the Job Destination");
    assert_eq!(dests[0], job_dir);
    let partial = std::fs::read_to_string(job_dir.join("extractor-partial.bin")).unwrap();
    let chunk_lines: Vec<&str> = partial.lines().collect();
    assert_eq!(
        chunk_lines.len(),
        total_chunks as usize,
        "no restart from zero"
    );
    assert!(chunk_lines[0].starts_with("chunk:0:"));
}

#[test]
fn pause_on_completed_job_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.process_pending().unwrap();

    let err = core.pause_job(&job_id);
    assert!(err.is_err(), "cannot pause a completed job");
}

#[test]
fn retry_failed_job_goes_back_to_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(FailingHttp));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/fail.zip");
    core.process_pending().unwrap();
    collect_events(&rx); // drain failure events

    // Retry — then process again
    core.retry_job(&job_id).unwrap();
    collect_events(&rx); // drain retry event

    // Even with FailingHttp, process_pending should try (and fail again)
    core.process_pending().unwrap();
    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobFailed { .. })),
        "retried job should be re-processed and fail again"
    );
}

#[test]
fn resume_non_paused_job_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");

    // A pending job is not paused
    let err = core.resume_job(&job_id);
    assert!(err.is_err(), "cannot resume a non-paused job");
}

#[test]
fn retry_non_failed_job_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");

    let err = core.retry_job(&job_id);
    assert!(err.is_err(), "cannot retry a non-failed job");
}

// ---------------------------------------------------------------------------
// Acceptance: Resumable Transfer (issue #16)
// ---------------------------------------------------------------------------

/// A fake HTTP backend that returns content in stages to simulate an
/// interruptible download.
///
/// - First call (no range): returns only the first chunk of the content.
///   This simulates a mid-transfer interrupt.
/// - Subsequent calls (with range): return the remaining bytes.
#[derive(Clone)]
struct StagedFakeHttp {
    responses: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    /// Tracks how many bytes have been "sent" per URL.
    sent: Arc<Mutex<std::collections::HashMap<String, u64>>>,
}

impl StagedFakeHttp {
    fn new(responses: std::collections::HashMap<String, Vec<u8>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            sent: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }
}

impl HttpBackend for StagedFakeHttp {
    fn fetch(
        &self,
        url: &str,
        range: Option<std::ops::Range<u64>>,
        sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        let (full_body, total) = {
            let responses = self.responses.lock().unwrap();
            let body = responses
                .get(url)
                .ok_or_else(|| format!("no fake response for {url}"))?;
            (body.clone(), body.len() as u64)
        };

        let mut sent = self.sent.lock().unwrap();

        match range {
            None => {
                // First call: return only a portion (simulate interrupt)
                let chunk_size = (total / 2).max(1);
                let chunk = &full_body[..chunk_size as usize];
                sent.insert(url.to_string(), chunk_size);
                write_fetch_to_sink(sink, chunk, total)
            }
            Some(r) => {
                let start = r.start.min(total);
                let end = r.end.min(total).max(start);
                let remaining = &full_body[start as usize..end as usize];
                sent.insert(url.to_string(), total);
                write_fetch_to_sink(sink, remaining, total)
            }
        }
    }

    fn supports_range(&self, _url: &str) -> Result<bool, String> {
        // Return false so new jobs use the single-connection path.
        // StagedFakeHttp is designed for resumable-transfer tests:
        // first call (no range) returns half, second call (with range)
        // returns the rest.  Segmented transfer would bypass the staging.
        Ok(false)
    }

    fn file_size(&self, url: &str) -> Result<u64, String> {
        let responses = self.responses.lock().unwrap();
        let body = responses
            .get(url)
            .ok_or_else(|| format!("no fake response for {url}"))?;
        Ok(body.len() as u64)
    }
}

#[test]
fn interrupt_then_resume_continues_from_last_byte() {
    // Given a staged (interruptible) HTTP backend
    let content = b"hello world this is a resumable test file with enough data to split".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/resumable.zip".into(), content.clone());
    let http = StagedFakeHttp::new(map);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(http));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    // Enqueue
    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/resumable.zip");

    // First process_pending — simulates interrupt (StagedFakeHttp returns half)
    core.process_pending().unwrap();
    let events1 = collect_events(&rx);

    // Should have progress events but NOT a completed event
    let has_completed = events1
        .iter()
        .any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(
        !has_completed,
        "interrupted job should NOT emit completed, got events: {events1:?}"
    );

    // Should have progress event showing partial bytes
    let partial_progress = events1.iter().find_map(|e| match e {
        CoreEvent::JobProgress {
            bytes_downloaded,
            total_bytes: Some(total),
            ..
        } => Some((*bytes_downloaded, *total)),
        _ => None,
    });
    assert!(
        partial_progress.is_some(),
        "interrupted job should emit progress, got {events1:?}"
    );
    let (partial_bytes, total_bytes) = partial_progress.unwrap();
    assert!(
        partial_bytes < total_bytes,
        "interrupted job should have partial bytes ({partial_bytes}) < total ({total_bytes})"
    );

    // Second process_pending — should resume from where we left off via Range
    core.process_pending().unwrap();
    let events2 = collect_events(&rx);

    // Should now have completed event
    let has_completed2 = events2
        .iter()
        .any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(
        has_completed2,
        "resumed job should emit completed, got events: {events2:?}"
    );

    // The final progress should show all bytes
    let final_progress = events2.iter().rev().find_map(|e| match e {
        CoreEvent::JobProgress {
            bytes_downloaded,
            total_bytes: Some(t),
            ..
        } => Some((*bytes_downloaded, *t)),
        _ => None,
    });
    assert!(
        final_progress.is_some(),
        "resumed job should have final progress"
    );
    let (final_bytes, final_total) = final_progress.unwrap();
    assert_eq!(
        final_bytes, final_total,
        "resumed job should have complete bytes ({final_bytes} == {final_total})"
    );
    assert_eq!(
        final_bytes,
        content.len() as u64,
        "should match original content length"
    );

    // Verify file on disk has the full content
    let entries: Vec<_> = std::fs::read_dir(root.join("jobs").join(&job_id))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(!entries.is_empty(), "job directory should contain files");
    let file_content = std::fs::read(entries[0].path()).unwrap();
    assert_eq!(
        file_content, content,
        "file on disk should have full content after resume"
    );
}

#[test]
fn restart_restores_jobs_and_resumes_from_part() {
    let content = b"restart persistence test file with enough bytes to split cleanly".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/restart.zip".into(), content.clone());
    let http = StagedFakeHttp::new(map);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let job_id;

    {
        let (mut core, rx) = make_core(Box::new(http.clone()));
        core.set_library_root(root.clone()).unwrap();
        job_id = enqueue_direct_job_id(&mut core, "https://example.com/restart.zip");

        core.process_pending().unwrap();
        let events = collect_events(&rx);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
            "first pass should leave job incomplete"
        );
    }

    let queue_path = root.join(".phlegethon").join("jobs.json");
    assert!(
        queue_path.exists(),
        "queue index should be persisted under Library Root"
    );
    let queue_raw = std::fs::read_to_string(&queue_path).unwrap();
    assert!(
        queue_raw.contains(&job_id),
        "persisted queue should contain job id"
    );

    let part_path = root.join("jobs").join(&job_id).join("restart.zip.part");
    assert!(
        part_path.exists(),
        "partial file should exist after first pass"
    );
    let partial_len = std::fs::metadata(&part_path).unwrap().len();
    assert!(partial_len > 0 && partial_len < content.len() as u64);

    let (mut core2, rx2) = make_core(Box::new(http));
    core2.set_library_root(root.clone()).unwrap();

    core2.process_pending().unwrap();
    let events2 = collect_events(&rx2);
    assert!(
        events2
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "restarted core should complete the job, got {events2:?}"
    );

    let final_path = root.join("jobs").join(&job_id).join("restart.zip");
    assert!(
        final_path.exists(),
        "final file should exist after restart resume"
    );
    let on_disk = std::fs::read(&final_path).unwrap();
    assert_eq!(on_disk, content);
}

// ---------------------------------------------------------------------------
// Relaunch rules (issue #49)
// ---------------------------------------------------------------------------

#[test]
fn relaunch_in_progress_http_job_auto_resumes() {
    let content = b"relaunch downloading test file with enough bytes to split cleanly".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/relaunch-downloading.zip".into(),
        content.clone(),
    );
    let http = StagedFakeHttp::new(map);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let job_id;

    {
        let (mut core, rx) = make_core(Box::new(http.clone()));
        core.set_library_root(root.clone()).unwrap();
        job_id = enqueue_direct_job_id(&mut core, "https://example.com/relaunch-downloading.zip");
        core.process_pending().unwrap();
        collect_events(&rx);
    }

    let queue_raw = std::fs::read_to_string(root.join(".phlegethon").join("jobs.json")).unwrap();
    assert!(
        queue_raw.contains("\"downloading\""),
        "in-progress HTTP job should persist as downloading, got: {queue_raw}"
    );

    let (mut core2, rx2) = make_core(Box::new(http));
    core2.set_library_root(root.clone()).unwrap();

    let snap = core2
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .expect("job restored after relaunch");
    assert_eq!(snap.status, "pending");

    core2.process_pending().unwrap();
    let events = collect_events(&rx2);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "downloading job should auto-resume and complete after relaunch, got {events:?}"
    );
}

#[test]
fn relaunch_paused_job_stays_paused() {
    let content = b"relaunch paused test file with enough bytes to split cleanly".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/relaunch-paused.zip".into(), content);
    let http = StagedFakeHttp::new(map);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let job_id;

    {
        let (mut core, rx) = make_core(Box::new(http.clone()));
        core.set_library_root(root.clone()).unwrap();
        job_id = enqueue_direct_job_id(&mut core, "https://example.com/relaunch-paused.zip");
        core.process_pending().unwrap();
        collect_events(&rx);
        core.pause_job(&job_id).unwrap();
        collect_events(&rx);
    }

    let (mut core2, rx2) = make_core(Box::new(http));
    core2.set_library_root(root).unwrap();

    let snap = core2
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .expect("job restored after relaunch");
    assert_eq!(snap.status, "paused");

    core2.process_pending().unwrap();
    let events = collect_events(&rx2);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "paused job must not complete after relaunch, got {events:?}"
    );
    assert_eq!(
        core2
            .list_job_snapshots()
            .into_iter()
            .find(|j| j.job_id == job_id)
            .expect("job still queued")
            .status,
        "paused"
    );
}

#[test]
fn relaunch_failed_job_stays_failed_until_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let job_id;

    {
        let (mut core, rx) = make_core(Box::new(FailingHttp));
        core.set_library_root(root.clone()).unwrap();
        job_id = enqueue_direct_job_id(&mut core, "https://example.com/relaunch-failed.zip");
        core.process_pending().unwrap();
        collect_events(&rx);
    }

    let (mut core2, rx2) = make_core(Box::new(FailingHttp));
    core2.set_library_root(root).unwrap();

    let snap = core2
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .expect("job restored after relaunch");
    assert_eq!(snap.status, "failed");

    core2.process_pending().unwrap();
    let events = collect_events(&rx2);
    assert!(
        events.is_empty(),
        "failed job must not be processed until retry, got {events:?}"
    );
    assert_eq!(
        core2
            .list_job_snapshots()
            .into_iter()
            .find(|j| j.job_id == job_id)
            .expect("job still queued")
            .status,
        "failed"
    );

    core2.retry_job(&job_id).unwrap();
    core2.process_pending().unwrap();
    let retry_events = collect_events(&rx2);
    assert!(
        retry_events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobFailed { .. })),
        "explicit retry should process the job again"
    );
}

#[test]
fn relaunch_extractor_partial_without_resume_marks_failed() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=relaunch-ext",
        "Relaunch Extractor",
        "https://cdn.example.com/relaunch-ext.mp4",
        vec![("720p", "720p")],
    );

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let job_id;
    let job_dir;

    {
        let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
        core.set_library_root(root.clone()).unwrap();
        core.probe("https://www.youtube.com/watch?v=relaunch-ext")
            .unwrap();
        collect_events(&rx);

        let selection = ProbeSelection {
            url: "https://www.youtube.com/watch?v=relaunch-ext".to_string(),
            items: vec![ItemSelection {
                item_id: "item-1".to_string(),
                quality_id: "720p".to_string(),
            }],
            media_extras: None,
        };
        let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
        job_id = job_ids[0].clone();
        job_dir = core
            .list_job_snapshots()
            .into_iter()
            .find(|j| j.job_id == job_id)
            .map(|j| PathBuf::from(j.destination))
            .expect("job destination");
    }

    std::fs::create_dir_all(&job_dir).unwrap();
    std::fs::write(
        job_dir.join("extractor-partial.bin"),
        b"partial extractor bytes from interrupted transfer",
    )
    .unwrap();

    let (mut core2, rx2) = Core::new(Box::new(fake_http()), Box::new(fake_toolchain()));
    core2.set_library_root(root).unwrap();

    let snap = core2
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .expect("job restored after relaunch");
    assert_eq!(snap.status, "pending");
    assert!(
        snap.bytes_downloaded > 0,
        "reconcile should sync extractor partial bytes on load"
    );

    core2.process_pending().unwrap();
    let events = collect_events(&rx2);
    assert!(
        events.iter().any(|e| matches!(
            e,
            CoreEvent::JobFailed {
                job_id: id,
                error,
            } if id == &job_id && error.contains("cannot resume")
        )),
        "extractor partial without resume support must surface Failed, got {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "must not silently restart extractor download from zero"
    );
    assert_eq!(
        core2
            .list_job_snapshots()
            .into_iter()
            .find(|j| j.job_id == job_id)
            .expect("job still queued")
            .status,
        "failed"
    );
}

#[test]
fn resume_starts_from_last_byte_not_zero() {
    // This test verifies that restart-from-zero is NOT the default path
    // when Range is available.
    let full_content = b"exact content for resume test".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/exact.zip".into(), full_content.clone());

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(StagedFakeHttp::new(map)));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/exact.zip");

    // First pass — gets partial
    core.process_pending().unwrap();
    collect_events(&rx); // drain

    // Second pass — should use Range, not start from zero
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // Verify completed with expected total
    let completed = events.iter().find_map(|e| match e {
        CoreEvent::JobCompleted { bytes_written, .. } => Some(*bytes_written),
        _ => None,
    });
    assert_eq!(
        completed,
        Some(full_content.len() as u64),
        "should complete with full content bytes, not double-counted"
    );

    // File on disk should be exactly the content, not doubled
    let file_path = root.join("jobs").join(&job_id);
    let files: Vec<_> = std::fs::read_dir(&file_path)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(files.len(), 1, "should have one file");
    let on_disk = std::fs::read(files[0].path()).unwrap();
    assert_eq!(
        on_disk, full_content,
        "file should be exact content, not duplicated"
    );
    assert_eq!(
        on_disk.len(),
        full_content.len(),
        "file length should match original, not double"
    );
}

#[test]
fn progress_events_show_cumulative_bytes_on_resume() {
    let content = b"cumulative progress tracking for resumable transfer test".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/progress.zip".into(), content.clone());

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(StagedFakeHttp::new(map)));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let _job_id = enqueue_direct_job_id(&mut core, "https://example.com/progress.zip");

    // First pass — partial
    core.process_pending().unwrap();
    let events1 = collect_events(&rx);

    // Get the last progress from first pass
    let partial_downloaded = events1
        .iter()
        .filter_map(|e| match e {
            CoreEvent::JobProgress {
                bytes_downloaded, ..
            } => Some(*bytes_downloaded),
            _ => None,
        })
        .last()
        .unwrap_or(0);
    assert!(
        partial_downloaded > 0,
        "partial progress should show > 0 bytes"
    );
    assert!(
        partial_downloaded < content.len() as u64,
        "partial progress should be < total"
    );

    // Second pass — resume
    core.process_pending().unwrap();
    let events2 = collect_events(&rx);

    // The first progress event in the second pass should report cumulative bytes
    let resumed_progress = events2.iter().find_map(|e| match e {
        CoreEvent::JobProgress {
            bytes_downloaded, ..
        } => Some(*bytes_downloaded),
        _ => None,
    });
    let resumed_bytes = resumed_progress.unwrap_or(0);
    assert!(
        resumed_bytes >= partial_downloaded,
        "resumed progress ({resumed_bytes}) should be >= previous progress ({partial_downloaded})"
    );
    assert!(
        resumed_bytes <= content.len() as u64,
        "resumed progress ({resumed_bytes}) should not exceed total ({})",
        content.len()
    );
}

#[test]
fn paused_then_resumed_job_uses_range_not_full_redownload() {
    let content = b"pause then resume with range transfer test content".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/pause-resume.zip".into(),
        content.clone(),
    );

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(StagedFakeHttp::new(map)));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/pause-resume.zip");

    // First pass — partial
    core.process_pending().unwrap();
    collect_events(&rx);

    // Pause
    core.pause_job(&job_id).unwrap();
    collect_events(&rx);

    // Resume
    core.resume_job(&job_id).unwrap();
    collect_events(&rx);

    // Process — should resume from last byte via Range
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // Verify completed
    let completed = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(completed, "paused-then-resumed job should complete");

    // Verify file has full content (not duplicated)
    let file_path = root.join("jobs").join(&job_id);
    let files: Vec<_> = std::fs::read_dir(&file_path)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(files.len(), 1, "should have one file");
    let on_disk = std::fs::read(files[0].path()).unwrap();
    assert_eq!(
        on_disk, content,
        "file should have exact content, not duplicated"
    );
    assert_eq!(
        on_disk.len(),
        content.len(),
        "file length should match original"
    );
}

#[test]
fn retry_resets_bytes_received_and_starts_from_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(FailingHttp));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/fail-retry.zip");

    // First attempt fails
    core.process_pending().unwrap();
    collect_events(&rx); // drain failure

    // Retry — this resets bytes_received to 0
    core.retry_job(&job_id).unwrap();

    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobRetried { .. })),
        "retry should emit JobRetried event"
    );

    // Process again — should fail again with FailingHttp
    core.process_pending().unwrap();
    let events2 = collect_events(&rx);
    assert!(
        events2
            .iter()
            .any(|e| matches!(e, CoreEvent::JobFailed { .. })),
        "retried job should attempt processing again"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: Bandwidth Cap (issue #31 — bytes/sec token bucket)
// ---------------------------------------------------------------------------

fn make_capped_core(
    http: Box<dyn HttpBackend>,
) -> (
    Core,
    crossbeam_channel::Receiver<CoreEvent>,
    Arc<ManualClock>,
) {
    let clock = Arc::new(ManualClock::new());
    let (core, rx) = Core::new_with_clock(http, Box::new(fake_toolchain()), clock.clone());
    (core, rx, clock)
}

fn no_range_http(map: std::collections::HashMap<String, Vec<u8>>) -> NoRangeFakeHttp {
    NoRangeFakeHttp(FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    })
}

fn process_with_clock_until_done(
    core: &mut Core,
    rx: &crossbeam_channel::Receiver<CoreEvent>,
    clock: &ManualClock,
) {
    loop {
        core.process_pending().unwrap();
        let events = collect_events(rx);
        if events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. }))
        {
            return;
        }
        clock.advance(std::time::Duration::from_secs(1));
    }
}

fn process_until_job_done_with_clock(
    core: &mut Core,
    rx: &crossbeam_channel::Receiver<CoreEvent>,
    clock: &ManualClock,
    target: &str,
) {
    loop {
        core.process_pending().unwrap();
        let events = collect_events(rx);
        if events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { job_id, .. } if job_id == target))
        {
            return;
        }
        clock.advance(std::time::Duration::from_secs(1));
    }
}

#[test]
fn default_transfer_is_uncapped() {
    let content = b"this file is long enough to be split by a cap if one were set"
        .repeat(1)
        .to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/uncapped.zip".into(), content.clone());
    let http = no_range_http(map);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx, _clock) = make_capped_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let _job_id = enqueue_direct_job_id(&mut core, "https://example.com/uncapped.zip");

    core.process_pending().unwrap();
    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "default uncapped transfer should complete in one call"
    );
}

#[test]
fn global_bandwidth_cap_limits_aggregate_throughput() {
    let content_a = b"AAAA this is 30 bytes of data!"; // 30 bytes
    let content_b = b"BBBB this is also 30 bytes woo"; // 30 bytes
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/a.zip".into(), content_a.to_vec());
    map.insert("https://example.com/b.zip".into(), content_b.to_vec());
    let http = no_range_http(map);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx, clock) = make_capped_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_a = enqueue_direct_job_id(&mut core, "https://example.com/a.zip");
    let job_b = enqueue_direct_job_id(&mut core, "https://example.com/b.zip");

    // Global cap: 20 bytes/sec
    core.set_global_bandwidth_cap(20);

    // First call — neither job should complete yet
    let mut done = std::collections::HashSet::new();
    process_and_accumulate_completions(&mut core, &rx, &mut done);
    assert!(
        !done.contains(&job_a) && !done.contains(&job_b),
        "neither job should complete in one call with 20-byte/sec global cap"
    );

    // Drain remaining bytes, advancing the injectable clock between rounds
    while !done.contains(&job_a) || !done.contains(&job_b) {
        clock.advance(std::time::Duration::from_secs(1));
        process_and_accumulate_completions(&mut core, &rx, &mut done);
    }

    assert!(done.contains(&job_a), "job_a should complete");
    assert!(done.contains(&job_b), "job_b should complete");

    // Verify file contents on disk
    for (job_id, expected) in [(&job_a, content_a), (&job_b, content_b)] {
        let dir = tmp.path().join("library").join("jobs").join(job_id);
        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(!files.is_empty(), "job {job_id} should have files");
        let on_disk = std::fs::read(files[0].path()).unwrap();
        assert_eq!(on_disk, *expected, "job {job_id} content should match");
    }
}

#[test]
fn per_job_bandwidth_cap_limits_that_job_without_global() {
    let content_a = b"small file that fits in one call"; // ~32 bytes
    let content_b = b"this is a longer file content that spans multiple iterations when capped";
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/a.zip".into(), content_a.to_vec());
    map.insert("https://example.com/b.zip".into(), content_b.to_vec());
    let http = no_range_http(map);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx, clock) = make_capped_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_a = enqueue_direct_job_id(&mut core, "https://example.com/a.zip");
    let job_b = enqueue_direct_job_id(&mut core, "https://example.com/b.zip");

    core.set_job_bandwidth_cap(&job_b, 10).unwrap();

    let mut done = std::collections::HashSet::new();
    process_and_accumulate_completions(&mut core, &rx, &mut done);
    assert!(
        done.contains(&job_a),
        "uncapped job should complete in one call"
    );
    assert!(
        !done.contains(&job_b),
        "capped job should NOT complete in one call"
    );

    process_until_job_done_with_clock(&mut core, &rx, &clock, &job_b);

    // Verify full content on disk
    let dir = tmp.path().join("library").join("jobs").join(&job_b);
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(!files.is_empty(), "job_b should have files");
    let on_disk = std::fs::read(files[0].path()).unwrap();
    assert_eq!(on_disk, content_b, "job_b full content should match");
}

#[test]
fn per_job_cap_more_restrictive_than_global_is_effective() {
    let content = vec![b'X'; 200]; // 200 bytes
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/big.zip".into(), content.clone());
    let http = no_range_http(map);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx, clock) = make_capped_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/big.zip");

    core.set_global_bandwidth_cap(100);
    core.set_job_bandwidth_cap(&job_id, 10).unwrap();

    core.process_pending().unwrap();
    let events = collect_events(&rx);

    let last_progress = events
        .iter()
        .filter_map(|e| match e {
            CoreEvent::JobProgress {
                bytes_downloaded, ..
            } => Some(*bytes_downloaded),
            _ => None,
        })
        .last()
        .unwrap_or(0);
    assert_eq!(
        last_progress, 10,
        "should transfer exactly 10 bytes (per-job cap is more restrictive)"
    );

    process_until_job_done_with_clock(&mut core, &rx, &clock, &job_id);

    let dir = tmp.path().join("library").join("jobs").join(&job_id);
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(files.len(), 1, "should have one file");
    let on_disk = std::fs::read(files[0].path()).unwrap();
    assert_eq!(on_disk, content, "full content should match after cap");
    assert_eq!(
        on_disk.len(),
        content.len(),
        "file length should match original"
    );
}

#[test]
fn global_cap_more_restrictive_than_per_job_is_effective() {
    let content = vec![b'Y'; 200]; // 200 bytes
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/big.zip".into(), content.clone());
    let http = no_range_http(map);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx, clock) = make_capped_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/big.zip");

    core.set_global_bandwidth_cap(5);
    core.set_job_bandwidth_cap(&job_id, 100).unwrap();

    core.process_pending().unwrap();
    let events = collect_events(&rx);

    let last_progress = events
        .iter()
        .filter_map(|e| match e {
            CoreEvent::JobProgress {
                bytes_downloaded, ..
            } => Some(*bytes_downloaded),
            _ => None,
        })
        .last()
        .unwrap_or(0);
    assert_eq!(
        last_progress, 5,
        "should transfer exactly 5 bytes (global cap is more restrictive)"
    );

    process_until_job_done_with_clock(&mut core, &rx, &clock, &job_id);

    let dir = tmp.path().join("library").join("jobs").join(&job_id);
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(files.len(), 1, "should have one file");
    let on_disk = std::fs::read(files[0].path()).unwrap();
    assert_eq!(on_disk, content, "full content should match after cap");
}

#[test]
fn bandwidth_cap_does_not_affect_small_transfers() {
    let content = b"small".to_vec(); // 5 bytes
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/small.zip".into(), content.clone());
    let http = no_range_http(map);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx, _clock) = make_capped_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let _job_id = enqueue_direct_job_id(&mut core, "https://example.com/small.zip");

    core.set_global_bandwidth_cap(100);

    core.process_pending().unwrap();
    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "small file should complete in one call even with cap"
    );
}

#[test]
fn set_and_remove_global_bandwidth_cap_restores_uncapped() {
    let content = b"this file should transfer uncapped after removing the global cap".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/global-switch.zip".into(),
        content.clone(),
    );
    let http = no_range_http(map);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx, clock) = make_capped_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let _job_id = enqueue_direct_job_id(&mut core, "https://example.com/global-switch.zip");

    core.set_global_bandwidth_cap(5);
    assert_eq!(core.global_bandwidth_cap(), Some(5));

    core.remove_global_bandwidth_cap();
    assert_eq!(core.global_bandwidth_cap(), None);

    process_with_clock_until_done(&mut core, &rx, &clock);

    // Verify content
    let dir = tmp.path().join("library").join("jobs");
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1, "should have exactly one job dir");
    let files: Vec<_> = std::fs::read_dir(entries[0].path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    let on_disk = std::fs::read(files[0].path()).unwrap();
    assert_eq!(on_disk, content);
}

#[test]
fn set_and_remove_job_bandwidth_cap_restores_uncapped() {
    let content = b"this file should transfer uncapped after removing the per-job cap".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/job-switch.zip".into(), content.clone());
    let http = no_range_http(map);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx, clock) = make_capped_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/job-switch.zip");

    core.set_job_bandwidth_cap(&job_id, 5).unwrap();
    assert_eq!(core.job_bandwidth_cap(&job_id).unwrap(), Some(5));

    core.remove_job_bandwidth_cap(&job_id).unwrap();
    assert_eq!(core.job_bandwidth_cap(&job_id).unwrap(), None);

    process_with_clock_until_done(&mut core, &rx, &clock);

    let dir = tmp.path().join("library").join("jobs").join(&job_id);
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    let on_disk = std::fs::read(files[0].path()).unwrap();
    assert_eq!(on_disk, content);
}

#[test]
fn global_bandwidth_cap_obeys_total_limit_across_multiple_jobs() {
    let content = vec![b'Z'; 20]; // 20 bytes each
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/1.zip".into(), content.clone());
    map.insert("https://example.com/2.zip".into(), content.clone());
    map.insert("https://example.com/3.zip".into(), content.clone());
    let http = no_range_http(map);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx, clock) = make_capped_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_1 = enqueue_direct_job_id(&mut core, "https://example.com/1.zip");
    let job_2 = enqueue_direct_job_id(&mut core, "https://example.com/2.zip");
    let job_3 = enqueue_direct_job_id(&mut core, "https://example.com/3.zip");

    core.set_global_bandwidth_cap(30);

    let mut done = std::collections::HashSet::new();
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    let completed_in_first: Vec<&String> = events
        .iter()
        .filter_map(|e| match e {
            CoreEvent::JobCompleted { job_id, .. } => Some(job_id),
            _ => None,
        })
        .collect();
    assert!(
        completed_in_first.len() <= 1,
        "at most 1 job should complete in first call, got {}",
        completed_in_first.len()
    );

    assert!(
        completed_in_first.contains(&&job_1),
        "first job should complete first"
    );
    for job_id in completed_in_first {
        done.insert(job_id.clone());
    }

    while done.len() < 3 {
        clock.advance(std::time::Duration::from_secs(1));
        process_and_accumulate_completions(&mut core, &rx, &mut done);
    }

    assert_eq!(done.len(), 3, "all 3 jobs should complete");
    assert!(done.contains(&job_1), "job_1 should complete");
    assert!(done.contains(&job_2), "job_2 should complete");
    assert!(done.contains(&job_3), "job_3 should complete");

    let job_dir = tmp.path().join("library").join("jobs");
    let job_count = std::fs::read_dir(&job_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .count();
    assert_eq!(job_count, 3, "all 3 jobs should have job dirs");
}

#[test]
fn job_bandwidth_cap_error_for_nonexistent_job() {
    let (mut core, _rx) = make_core(Box::new(FakeHttp {
        responses: Arc::new(Mutex::new(std::collections::HashMap::new())),
    }));
    let tmp = tempfile::tempdir().unwrap();
    core.set_library_root(tmp.path().join("library")).unwrap();

    let result = core.set_job_bandwidth_cap("nonexistent-job-uuid", 100);
    assert!(
        result.is_err(),
        "setting cap on nonexistent job should error"
    );

    let result = core.remove_job_bandwidth_cap("nonexistent-job-uuid");
    assert!(
        result.is_err(),
        "removing cap on nonexistent job should error"
    );

    let result = core.job_bandwidth_cap("nonexistent-job-uuid");
    assert!(
        result.is_err(),
        "getting cap on nonexistent job should error"
    );
}

#[test]
fn pause_during_in_flight_transfer_stops_without_full_body() {
    #[derive(Clone)]
    struct SlowNoRangeFakeHttp {
        responses: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    }

    impl HttpBackend for SlowNoRangeFakeHttp {
        fn fetch(
            &self,
            url: &str,
            range: Option<std::ops::Range<u64>>,
            sink: &mut dyn Write,
        ) -> Result<FetchResult, String> {
            let responses = self.responses.lock().unwrap();
            let full_body = responses
                .get(url)
                .ok_or_else(|| format!("no fake response for {url}"))?;
            let total = full_body.len() as u64;
            let data = match range {
                Some(r) => {
                    let start = r.start.min(total);
                    let end = r.end.min(total).max(start);
                    &full_body[start as usize..end as usize]
                }
                None => full_body.as_slice(),
            };

            let mut written = 0u64;
            for byte in data {
                let n = sink
                    .write(std::slice::from_ref(byte))
                    .map_err(|e| format!("sink write: {e}"))?;
                if n == 0 {
                    break;
                }
                written += n as u64;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(FetchResult {
                total_length: total,
                bytes_written: written,
            })
        }

        fn supports_range(&self, _url: &str) -> Result<bool, String> {
            Ok(false)
        }

        fn file_size(&self, url: &str) -> Result<u64, String> {
            let responses = self.responses.lock().unwrap();
            Ok(responses
                .get(url)
                .map(|b| b.len() as u64)
                .ok_or_else(|| format!("no fake response for {url}"))?)
        }
    }

    let content = vec![b'P'; 256];
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/pause-mid.zip".into(), content.clone());
    let http = SlowNoRangeFakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx, _clock) = make_capped_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/pause-mid.zip");
    let signals = core.job_signals();

    let core = Arc::new(Mutex::new(core));
    let rx = Arc::new(rx);
    let job_for_pause = job_id.clone();

    let worker = {
        let core = core.clone();
        let rx = rx.clone();
        std::thread::spawn(move || {
            let mut guard = core.lock().unwrap();
            guard.process_pending().unwrap();
            collect_events(&rx)
        })
    };

    std::thread::sleep(std::time::Duration::from_millis(50));
    signals
        .request_pause(&job_for_pause)
        .expect("pause flag registered")
        .store(true, std::sync::atomic::Ordering::Release);

    let events = worker.join().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobPaused { .. })),
        "pause during transfer should emit JobPaused, got: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "paused transfer must not claim success"
    );

    let job_dir = tmp.path().join("library").join("jobs").join(&job_id);
    let part_path = job_dir.join("pause-mid.zip.part");
    let final_path = job_dir.join("pause-mid.zip");
    assert!(
        !final_path.exists(),
        "paused transfer must not rename partial to final file"
    );
    assert!(part_path.exists(), "partial bytes should remain on disk");
    let partial_len = std::fs::metadata(&part_path).unwrap().len();
    assert!(
        partial_len > 0,
        "partial file should contain bytes written before pause"
    );
    assert!(
        partial_len < content.len() as u64,
        "partial file should be shorter than full content, got {partial_len}"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: Probe-then-Confirm (issue #18)
// ---------------------------------------------------------------------------

#[test]
fn probe_returns_items_and_qualities() {
    // Given a Core with a fake extractor that knows one URL
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=abc123",
        "Test Video",
        "https://cdn.youtube.com/video.mp4",
        vec![("1080p", "1080p (1920×1080)"), ("720p", "720p (1280×720)")],
    );

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    // Probe
    let result = core
        .probe("https://www.youtube.com/watch?v=abc123")
        .unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].title, "Test Video");
    assert_eq!(result.items[0].qualities.len(), 2);

    // Should emit a ProbeResult event
    let events = collect_events(&rx);
    let has_probe_event = events
        .iter()
        .any(|e| matches!(e, CoreEvent::ProbeResult { .. }));
    assert!(
        has_probe_event,
        "should emit ProbeResult event, got: {events:?}"
    );
}

/// Helper: create a FakeHttp that knows the probe item URLs.
fn fake_http_with_responses(pairs: Vec<(&str, &[u8])>) -> FakeHttp {
    let mut map = std::collections::HashMap::new();
    for (url, data) in pairs {
        map.insert(url.to_string(), data.to_vec());
    }
    FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    }
}

#[test]
fn probe_failure_emits_probe_failed_event() {
    // Given a Core with an empty fake extractor (all URLs fail)
    let toolchain = FakeExtractorToolchain::new();

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    // Probe unknown URL — should fail
    let err = core
        .probe("https://www.youtube.com/watch?v=unknown")
        .unwrap_err();
    assert!(
        err.contains("no response configured"),
        "error should mention missing config, got: {err}"
    );

    // Should emit a ProbeFailed event
    let events = collect_events(&rx);
    let has_fail_event = events
        .iter()
        .any(|e| matches!(e, CoreEvent::ProbeFailed { .. }));
    assert!(
        has_fail_event,
        "should emit ProbeFailed event, got: {events:?}"
    );
}

#[test]
fn confirm_probe_selection_creates_jobs_for_selected_items() {
    // Given a Core with a fake extractor that returns probe results
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=abc123",
        "Test Video",
        "https://cdn.youtube.com/video.mp4",
        vec![("1080p", "1080p"), ("720p", "720p")],
    );

    // Register the probe item URL in the HTTP backend
    let video_data = b"fake mp4 video content".to_vec();
    let http = fake_http_with_responses(vec![("https://cdn.youtube.com/video.mp4", &video_data)]);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    // First probe
    core.probe("https://www.youtube.com/watch?v=abc123")
        .unwrap();
    collect_events(&rx); // drain

    // Confirm selection — pick 720p
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=abc123".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    assert_eq!(job_ids.len(), 1, "should create exactly one job");

    // The job should be pending and processable
    core.process_pending().unwrap();
    let events = collect_events(&rx);
    let has_completed = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(
        has_completed,
        "confirmed job should complete, got: {events:?}"
    );
}

#[test]
fn confirm_probe_selection_creates_multiple_jobs_for_batch() {
    // Given a batch probe result (e.g. YouTube playlist)
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert_batch(
        "https://youtube.com/playlist?list=ABC",
        "My Playlist",
        vec![
            (
                "vid-1",
                "Video One",
                "https://youtube.com/watch?v=1",
                vec![("1080p", "1080p")],
            ),
            (
                "vid-2",
                "Video Two",
                "https://youtube.com/watch?v=2",
                vec![("720p", "720p")],
            ),
        ],
    );

    // Register both probe item URLs in the HTTP backend
    let http = fake_http_with_responses(vec![
        ("https://youtube.com/watch?v=1", b"video one content"),
        ("https://youtube.com/watch?v=2", b"video two content"),
    ]);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    // Probe
    core.probe("https://youtube.com/playlist?list=ABC").unwrap();
    collect_events(&rx); // drain

    // Confirm — select both items at different qualities
    let selection = ProbeSelection {
        url: "https://youtube.com/playlist?list=ABC".to_string(),
        items: vec![
            ItemSelection {
                item_id: "vid-1".to_string(),
                quality_id: "1080p".to_string(),
            },
            ItemSelection {
                item_id: "vid-2".to_string(),
                quality_id: "720p".to_string(),
            },
        ],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    assert_eq!(job_ids.len(), 2, "should create two jobs");

    // Both jobs should process and complete
    let mut all_done = std::collections::HashSet::new();
    while all_done.len() < 2 {
        process_and_accumulate_completions(&mut core, &rx, &mut all_done);
    }
    assert_eq!(all_done.len(), 2, "both jobs should complete");
}

#[test]
fn confirm_without_previous_probe_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    // Confirm without calling probe first
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=abc123".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "1080p".to_string(),
        }],
        media_extras: None,
    };
    let err = core.confirm_probe_selection(&selection, None).unwrap_err();
    assert!(
        err.contains("no pending probe"),
        "should mention no pending probe, got: {err}"
    );
}

#[test]
fn confirm_with_invalid_item_id_errors() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=abc123",
        "Test Video",
        "https://cdn.youtube.com/video.mp4",
        vec![("1080p", "1080p")],
    );

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    core.probe("https://www.youtube.com/watch?v=abc123")
        .unwrap();
    collect_events(&rx); // drain

    // Try to confirm a non-existent item
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=abc123".to_string(),
        items: vec![ItemSelection {
            item_id: "nonexistent-item".to_string(),
            quality_id: "1080p".to_string(),
        }],
        media_extras: None,
    };
    let err = core.confirm_probe_selection(&selection, None).unwrap_err();
    assert!(
        err.contains("not found"),
        "should mention item not found, got: {err}"
    );
}

#[test]
fn confirm_with_invalid_quality_id_errors() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=abc123",
        "Test Video",
        "https://cdn.youtube.com/video.mp4",
        vec![("1080p", "1080p")],
    );

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    core.probe("https://www.youtube.com/watch?v=abc123")
        .unwrap();
    collect_events(&rx); // drain

    // Try to confirm with a non-existent quality
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=abc123".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "8k".to_string(),
        }],
        media_extras: None,
    };
    let err = core.confirm_probe_selection(&selection, None).unwrap_err();
    assert!(
        err.contains("quality"),
        "should mention quality not found, got: {err}"
    );
}

#[test]
fn confirm_without_library_root_errors() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=abc123",
        "Test Video",
        "https://cdn.youtube.com/video.mp4",
        vec![("1080p", "1080p")],
    );

    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    // No library root set

    core.probe("https://www.youtube.com/watch?v=abc123")
        .unwrap();
    collect_events(&rx); // drain

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=abc123".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "1080p".to_string(),
        }],
        media_extras: None,
    };
    let err = core.confirm_probe_selection(&selection, None).unwrap_err();
    assert!(
        err.contains("Library Root"),
        "should mention Library Root not set, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: Batch/channel jobs use source-derived subfolder (issue #18)
// ---------------------------------------------------------------------------

#[test]
fn batch_job_destination_derives_from_source_name() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert_batch(
        "https://youtube.com/playlist?list=ABC",
        "My Awesome Playlist",
        vec![(
            "vid-1",
            "Video One",
            "https://youtube.com/watch?v=1",
            vec![("1080p", "1080p")],
        )],
    );

    let http = fake_http_with_responses(vec![(
        "https://youtube.com/watch?v=1",
        b"video one content",
    )]);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    core.probe("https://youtube.com/playlist?list=ABC").unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://youtube.com/playlist?list=ABC".to_string(),
        items: vec![ItemSelection {
            item_id: "vid-1".to_string(),
            quality_id: "1080p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    assert_eq!(job_ids.len(), 1);

    // Process to create directories on disk
    core.process_pending().unwrap();

    // The job directory should contain the source name prefix
    let job_dir = root.join("jobs");
    let entries: Vec<_> = std::fs::read_dir(&job_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1, "should have one job dir");

    let dir_name = entries[0].file_name().into_string().unwrap();
    assert!(
        dir_name.starts_with("My_Awesome_Playlist"),
        "job dir should start with source name, got: {dir_name}"
    );
}

#[test]
fn single_video_probe_without_source_name_uses_title_for_subfolder() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=abc123",
        "My Cool Video Title",
        "https://cdn.youtube.com/video.mp4",
        vec![("1080p", "1080p")],
    );

    let http = fake_http_with_responses(vec![(
        "https://cdn.youtube.com/video.mp4",
        b"fake mp4 content",
    )]);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    core.probe("https://www.youtube.com/watch?v=abc123")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=abc123".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "1080p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    assert_eq!(job_ids.len(), 1);

    // Process to create directories on disk
    core.process_pending().unwrap();

    let job_dir = root.join("jobs");
    let entries: Vec<_> = std::fs::read_dir(&job_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1);
    let dir_name = entries[0].file_name().into_string().unwrap();
    assert!(
        dir_name.starts_with("My_Cool_Video_Title"),
        "single-video job dir should start with video title, got: {dir_name}"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: Video and HTTP jobs share queue parity (issue #18)
// ---------------------------------------------------------------------------

#[test]
fn probe_created_jobs_share_same_lifecycle_as_direct_jobs() {
    // A job created via probe-then-confirm should support the same
    // lifecycle commands (pause, resume) as an enqueue_direct job.
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=abc123",
        "Video Title",
        "https://cdn.youtube.com/video.mp4",
        vec![("1080p", "1080p")],
    );

    let http = fake_http_with_responses(vec![(
        "https://cdn.youtube.com/video.mp4",
        b"fake video content",
    )]);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    // Create a probe-confirmed job
    core.probe("https://www.youtube.com/watch?v=abc123")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=abc123".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "1080p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    assert_eq!(job_ids.len(), 1);

    let video_job_id = &job_ids[0];

    // Pause the video job
    core.pause_job(video_job_id).unwrap();
    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobPaused { job_id } if job_id == video_job_id)),
        "probe-created job should support pause"
    );

    // Resume the video job
    core.resume_job(video_job_id).unwrap();
    let events2 = collect_events(&rx);
    assert!(
        events2
            .iter()
            .any(|e| matches!(e, CoreEvent::JobResumed { job_id } if job_id == video_job_id)),
        "probe-created job should support resume"
    );

    // Process — should complete
    core.process_pending().unwrap();
    let events3 = collect_events(&rx);
    assert!(
        events3
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "probe-created job should complete after resume"
    );
}

#[test]
fn probe_created_jobs_use_same_storage_rules() {
    // Jobs created via probe and via direct enqueue should both write
    // to the Library Root / Job Destination.
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=probe",
        "Probed Video",
        "https://cdn.youtube.com/probed.mp4",
        vec![("1080p", "1080p")],
    );

    // Register both URLs in the HTTP backend
    let http = fake_http_with_responses(vec![
        ("https://example.com/file.zip", b"hello this is a test file"),
        (
            "https://cdn.youtube.com/probed.mp4",
            b"probed video content",
        ),
    ]);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    // Create a direct job
    let direct_job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");

    // Create a probe-confirmed job
    core.probe("https://www.youtube.com/watch?v=probe").unwrap();
    collect_events(&rx);
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=probe".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "1080p".to_string(),
        }],
        media_extras: None,
    };
    let probe_job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    assert_eq!(probe_job_ids.len(), 1);

    // Process to create directories and complete both jobs
    let mut done = std::collections::HashSet::new();
    while done.len() < 2 {
        process_and_accumulate_completions(&mut core, &rx, &mut done);
    }

    // Both should be in the same jobs directory
    let job_dir = root.join("jobs");
    let entries: Vec<_> = std::fs::read_dir(&job_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        entries.len(),
        2,
        "should have 2 job dirs (direct + probe), got {}",
        entries.len()
    );
    assert_eq!(done.len(), 2, "both jobs should complete");
    assert!(done.contains(&direct_job_id), "direct job should complete");
    assert!(
        done.contains(&probe_job_ids[0]),
        "probe job should complete"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: Segmented Transfer (issue #19)
// ---------------------------------------------------------------------------

/// A fake HTTP backend that tracks all fetch calls for verification.
///
/// Supports exact byte ranges and lets tests inspect how many requests
/// were made and what ranges were used.
#[derive(Clone)]
struct TrackedFakeHttp {
    responses: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    /// (url, range) for every fetch call made.
    calls: Arc<Mutex<Vec<(String, Option<std::ops::Range<u64>>)>>>,
    range_supported: bool,
}

impl TrackedFakeHttp {
    fn new(responses: std::collections::HashMap<String, Vec<u8>>, range_supported: bool) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            calls: Arc::new(Mutex::new(Vec::new())),
            range_supported,
        }
    }

    /// Number of fetch calls made so far.
    #[allow(dead_code)]
    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    /// Return a copy of all recorded (url, range) calls.
    fn ranges(&self) -> Vec<(String, Option<std::ops::Range<u64>>)> {
        self.calls.lock().unwrap().clone()
    }
}

impl HttpBackend for TrackedFakeHttp {
    fn fetch(
        &self,
        url: &str,
        range: Option<std::ops::Range<u64>>,
        sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        let mut calls = self.calls.lock().unwrap();
        calls.push((url.to_string(), range.clone()));

        let responses = self.responses.lock().unwrap();
        let full_body = responses
            .get(url)
            .ok_or_else(|| format!("no fake response for {url}"))?;
        let total = full_body.len() as u64;

        let data = match range {
            Some(r) => {
                let start = r.start.min(total);
                let end = r.end.min(total).max(start);
                &full_body[start as usize..end as usize]
            }
            None => full_body.as_slice(),
        };

        write_fetch_to_sink(sink, data, total)
    }

    fn supports_range(&self, _url: &str) -> Result<bool, String> {
        Ok(self.range_supported)
    }

    fn file_size(&self, url: &str) -> Result<u64, String> {
        let responses = self.responses.lock().unwrap();
        let full_body = responses
            .get(url)
            .ok_or_else(|| format!("no fake response for {url}"))?;
        Ok(full_body.len() as u64)
    }
}

#[test]
fn segmented_transfer_writes_matching_bytes_via_part_file() {
    let content: Vec<u8> = (0u8..100).collect();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/segmented.bin".into(), content.clone());

    let http = TrackedFakeHttp::new(map, true);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(FakeExtractorToolchain::new()));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/segmented.bin");

    core.process_pending().unwrap();
    let events = collect_events(&rx);

    let job_dir = root.join("jobs").join(&job_id);
    let final_path = job_dir.join("segmented.bin");
    let part_path = job_dir.join("segmented.bin.part");

    assert!(final_path.exists(), "final file should exist after rename");
    assert!(
        !part_path.exists(),
        "partial file should be renamed away on success"
    );

    let on_disk = std::fs::read(&final_path).unwrap();
    assert_eq!(on_disk, content, "on-disk bytes should match source");

    let progress_bytes = events.iter().rev().find_map(|e| match e {
        CoreEvent::JobProgress {
            bytes_downloaded,
            total_bytes: Some(total),
            ..
        } => Some((*bytes_downloaded, *total)),
        _ => None,
    });
    assert!(
        progress_bytes.is_some(),
        "should emit progress with total bytes"
    );
    let (downloaded, total) = progress_bytes.unwrap();
    assert_eq!(downloaded, content.len() as u64);
    assert_eq!(total, content.len() as u64);
    assert_eq!(
        downloaded,
        on_disk.len() as u64,
        "progress bytes should match on-disk size"
    );

    let completed_path = events.iter().find_map(|e| match e {
        CoreEvent::JobCompleted { path, .. } => Some(path.clone()),
        _ => None,
    });
    assert_eq!(completed_path, Some(final_path));
}

#[test]
fn segmented_transfer_completes_to_correct_file() {
    // Given a 100-byte file hosted on a Range-supporting server
    let content: Vec<u8> = (0u8..100).collect();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/segmented.bin".into(), content.clone());

    let http = TrackedFakeHttp::new(map, true);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(
        Box::new(http.clone()),
        Box::new(FakeExtractorToolchain::new()),
    );
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/segmented.bin");

    // Process pending — should complete in one call via segmented transfer
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // Should have completed with the full content
    let completed = events.iter().find_map(|e| match e {
        CoreEvent::JobCompleted { bytes_written, .. } => Some(*bytes_written),
        _ => None,
    });
    assert_eq!(
        completed,
        Some(100),
        "segmented transfer should complete with 100 bytes, got events: {events:?}"
    );

    // Verify file on disk
    let file_path = root.join("jobs").join(&job_id);
    let files: Vec<_> = std::fs::read_dir(&file_path)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(files.len(), 1, "should have one file");
    let on_disk = std::fs::read(files[0].path()).unwrap();
    assert_eq!(on_disk, content, "file content should match original");
    assert_eq!(on_disk.len(), 100);
}

#[test]
fn segmented_transfer_makes_multiple_concurrent_range_requests() {
    // Given a 100-byte file on a Range-supporting server
    let content: Vec<u8> = (0u8..100).collect();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/segmented.bin".into(), content.clone());

    let http = TrackedFakeHttp::new(map, true);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(
        Box::new(http.clone()),
        Box::new(FakeExtractorToolchain::new()),
    );
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let _job_id = enqueue_direct_job_id(&mut core, "https://example.com/segmented.bin");

    // Process
    core.process_pending().unwrap();
    collect_events(&rx);

    // Inspect calls made to the HTTP backend
    let calls = http.ranges();

    // The first call should be the file_size probe (via file_size, not fetch)
    // Then segments: for 100 bytes with DEFAULT_SEGMENT_COUNT=4:
    //   seg_size = 25
    //   segment 0: 0..25
    //   segment 1: 25..50
    //   segment 2: 50..75
    //   segment 3: 75..100

    // We should have exactly 4 fetch calls (one per segment)
    assert_eq!(
        calls.len(),
        4,
        "should make exactly 4 Range requests, got {len}: {calls:?}",
        len = calls.len()
    );

    // All calls should be to the same URL
    for (url, _) in &calls {
        assert_eq!(url, "https://example.com/segmented.bin");
    }

    // Each call should have a specific range (not None)
    for (_, range) in &calls {
        assert!(
            range.is_some(),
            "each segmented fetch should have a Range header"
        );
    }

    // There should be a range starting at 0 (first segment)
    let starts_at_zero = calls
        .iter()
        .any(|(_, r)| matches!(r, Some(rng) if rng.start == 0));
    assert!(
        starts_at_zero,
        "should have a segment starting at 0: {calls:?}"
    );

    // The final segment should end at the total file size (100)
    let ends_at_100 = calls
        .iter()
        .any(|(_, r)| matches!(r, Some(rng) if rng.end == 100));
    assert!(
        ends_at_100,
        "should have a segment ending at 100: {calls:?}"
    );

    // Ranges should be non-overlapping and cover 0..100
    let mut ranges: Vec<_> = calls.iter().filter_map(|(_, r)| r.clone()).collect();
    ranges.sort_by_key(|r| r.start);

    // Check ranges are contiguous
    let mut prev_end = 0u64;
    for r in &ranges {
        assert_eq!(
            r.start, prev_end,
            "ranges should be contiguous: prev_end={prev_end}, next.start={}",
            r.start
        );
        prev_end = r.end;
    }
    assert_eq!(prev_end, 100, "ranges should cover exactly 100 bytes");
}

#[test]
fn segmented_transfer_falls_back_to_single_connection_when_range_not_supported() {
    // Given a 100-byte file on a server that does NOT support Range
    let content: Vec<u8> = (0u8..100).collect();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/fallback.bin".into(), content.clone());

    let http = TrackedFakeHttp::new(map, false); // range NOT supported

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(
        Box::new(http.clone()),
        Box::new(FakeExtractorToolchain::new()),
    );
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/fallback.bin");

    // Process pending
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // Should complete (via single-connection fallback)
    let completed = events.iter().find_map(|e| match e {
        CoreEvent::JobCompleted { bytes_written, .. } => Some(*bytes_written),
        _ => None,
    });
    assert_eq!(
        completed,
        Some(100),
        "fallback should complete with 100 bytes, got events: {events:?}"
    );

    // Verify only ONE fetch call was made (single connection, no Range)
    let calls = http.ranges();
    assert_eq!(
        calls.len(),
        1,
        "fallback should make exactly 1 request, got {len}: {calls:?}",
        len = calls.len()
    );
    assert!(
        calls[0].1.is_none(),
        "fallback request should NOT have a Range header, got {:?}",
        calls[0].1
    );

    // Verify file on disk
    let file_path = root.join("jobs").join(&job_id);
    let files: Vec<_> = std::fs::read_dir(&file_path)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(files.len(), 1, "should have one file");
    let on_disk = std::fs::read(files[0].path()).unwrap();
    assert_eq!(on_disk, content, "file content should match original");
    assert_eq!(on_disk.len(), 100);
}

#[test]
fn segmented_transfer_handles_small_files_with_single_segment() {
    // A file smaller than DEFAULT_SEGMENT_COUNT bytes should still work
    // (seg_count will be min(4, 3) = 3, each segment 1 byte)
    let content: Vec<u8> = b"abc".to_vec(); // 3 bytes
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/small.bin".into(), content.clone());

    let http = TrackedFakeHttp::new(map, true);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(
        Box::new(http.clone()),
        Box::new(FakeExtractorToolchain::new()),
    );
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/small.bin");

    // Process
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // Should complete
    let completed = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(
        completed,
        "small file should complete via segmented transfer"
    );

    // Should make seg_count (3) fetch calls, each getting 1 byte
    let calls = http.ranges();
    assert_eq!(
        calls.len(),
        3,
        "3-byte file should make 3 segment requests, got {len}: {calls:?}",
        len = calls.len()
    );

    // Verify file on disk
    let file_path = root.join("jobs").join(&job_id);
    let files: Vec<_> = std::fs::read_dir(&file_path)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    let on_disk = std::fs::read(files[0].path()).unwrap();
    assert_eq!(on_disk, content, "small file content should match");
    assert_eq!(on_disk.len(), 3);
}

#[test]
fn segmented_transfer_zero_byte_file_fails_gracefully() {
    // Zero-length file — segmented transfer should error, not crash
    let content: Vec<u8> = vec![];
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/empty.bin".into(), content.clone());

    let http = TrackedFakeHttp::new(map, true);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(FakeExtractorToolchain::new()));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let _job_id = enqueue_direct_job_id(&mut core, "https://example.com/empty.bin");

    // Processing an empty file should not panic
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // Should emit a failed event (empty file is an error)
    let has_failed = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobFailed { .. }));
    assert!(has_failed, "empty file should fail, got events: {events:?}");
}

// ---------------------------------------------------------------------------
// Acceptance: Mirror Sets (issue #20)
// ---------------------------------------------------------------------------

/// A fake HTTP backend that fails for specific URLs, succeeds for others.
///
/// Used to test mirror failover: primary URL fails, secondary succeeds.
#[derive(Clone)]
struct FailoverFakeHttp {
    responses: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    /// URLs that should simulate failure (e.g. primary mirror is down).
    failing_urls: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl FailoverFakeHttp {
    fn new(
        responses: std::collections::HashMap<String, Vec<u8>>,
        failing_urls: std::collections::HashSet<String>,
    ) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            failing_urls: Arc::new(Mutex::new(failing_urls)),
        }
    }
}

impl HttpBackend for FailoverFakeHttp {
    fn fetch(
        &self,
        url: &str,
        range: Option<std::ops::Range<u64>>,
        sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        // Check if this URL should fail
        {
            let failing = self.failing_urls.lock().unwrap();
            if failing.contains(url) {
                return Err(format!("simulated failure for mirror {url}"));
            }
        }

        let responses = self.responses.lock().unwrap();
        let full_body = responses
            .get(url)
            .ok_or_else(|| format!("no fake response for {url}"))?;
        let total = full_body.len() as u64;

        let data = match range {
            Some(r) => {
                let start = r.start.min(total);
                let end = r.end.min(total).max(start);
                &full_body[start as usize..end as usize]
            }
            None => full_body.as_slice(),
        };

        write_fetch_to_sink(sink, data, total)
    }

    fn supports_range(&self, _url: &str) -> Result<bool, String> {
        Ok(false) // disable segmented for failover tests (simpler)
    }

    fn file_size(&self, url: &str) -> Result<u64, String> {
        // Also fail for size check
        {
            let failing = self.failing_urls.lock().unwrap();
            if failing.contains(url) {
                return Err(format!("simulated failure for mirror {url}"));
            }
        }
        let responses = self.responses.lock().unwrap();
        let full_body = responses
            .get(url)
            .ok_or_else(|| format!("no fake response for {url}"))?;
        Ok(full_body.len() as u64)
    }
}

// Register a job with an additional mirror via Core API, then verify
// the mirror set contains both URLs.
#[test]
fn add_mirror_to_job_extends_mirror_set() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://primary.example.com/file.zip");

    // Add a mirror via Core API
    core.add_job_mirror(&job_id, "https://mirror.example.com/file.zip".into())
        .unwrap();

    // Verify mirror set has both URLs
    let mirrors = core.job_mirrors(&job_id).unwrap();
    assert_eq!(mirrors.len(), 2);
    assert_eq!(mirrors[0], "https://primary.example.com/file.zip");
    assert_eq!(mirrors[1], "https://mirror.example.com/file.zip");
}

// Verify Metalink XML can populate mirrors for a job.
#[test]
fn add_metalink_to_job_adds_mirrors() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://primary.example.com/file.zip");

    let metalink_xml = r#"
        <?xml version="1.0" encoding="UTF-8"?>
        <metalink version="3.0">
            <file name="file.zip">
                <url>https://mirror1.example.com/file.zip</url>
                <url>https://mirror2.example.com/file.zip</url>
            </file>
        </metalink>
    "#;

    let count = core.add_metalink_to_job(&job_id, metalink_xml).unwrap();
    assert_eq!(count, 2, "should discover 2 mirrors from Metalink");

    let mirrors = core.job_mirrors(&job_id).unwrap();
    assert_eq!(mirrors.len(), 3); // primary + 2 mirrors
    assert!(mirrors.contains(&"https://mirror1.example.com/file.zip".into()));
    assert!(mirrors.contains(&"https://mirror2.example.com/file.zip".into()));
}

// Primary mirror fails → core tries next mirror → job completes.
#[test]
fn mirror_failover_proceeds_to_next_mirror() {
    let content = b"content from secondary mirror".to_vec();
    let mut responses = std::collections::HashMap::new();
    responses.insert(
        "https://primary.example.com/file.zip".into(),
        b"should not be read".to_vec(),
    );
    responses.insert(
        "https://secondary.example.com/file.zip".into(),
        content.clone(),
    );

    let mut failing = std::collections::HashSet::new();
    failing.insert("https://primary.example.com/file.zip".into());

    let http = FailoverFakeHttp::new(responses, failing);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(FakeExtractorToolchain::new()));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    // Enqueue with primary URL, then add secondary mirror
    let job_id = enqueue_direct_job_id(&mut core, "https://primary.example.com/file.zip");
    core.add_job_mirror(&job_id, "https://secondary.example.com/file.zip".into())
        .unwrap();

    // Process — should failover and complete via secondary
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // Should have a failover event
    let has_failover = events.iter().any(|e| {
        matches!(e, CoreEvent::JobMirrorFailover {
            previous_url,
            next_url,
            ..
        } if previous_url == "https://primary.example.com/file.zip"
          && next_url == "https://secondary.example.com/file.zip")
    });
    assert!(
        has_failover,
        "should emit JobMirrorFailover when primary fails, got: {events:?}"
    );

    // Should NOT emit JobFailed
    let has_job_failed = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobFailed { .. }));
    assert!(
        !has_job_failed,
        "should NOT fail if secondary mirror succeeds, got: {events:?}"
    );

    // Should complete via secondary
    let has_completed = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(
        has_completed,
        "should complete after failover, got: {events:?}"
    );

    // File on disk should have secondary content
    let job_dir = root.join("jobs").join(&job_id);
    let files: Vec<_> = std::fs::read_dir(&job_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(!files.is_empty(), "job directory should have files");
    let on_disk = std::fs::read(files[0].path()).unwrap();
    assert_eq!(
        on_disk, content,
        "should contain secondary mirror content, not primary"
    );
}

// All mirrors fail → job transitions to Failed state.
#[test]
fn all_mirrors_exhausted_emits_job_failed() {
    let mut responses = std::collections::HashMap::new();
    responses.insert(
        "https://primary.example.com/file.zip".into(),
        b"content".to_vec(),
    );
    responses.insert(
        "https://secondary.example.com/file.zip".into(),
        b"content".to_vec(),
    );

    let mut failing = std::collections::HashSet::new();
    failing.insert("https://primary.example.com/file.zip".into());
    failing.insert("https://secondary.example.com/file.zip".into());

    let http = FailoverFakeHttp::new(responses, failing);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(FakeExtractorToolchain::new()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://primary.example.com/file.zip");
    core.add_job_mirror(&job_id, "https://secondary.example.com/file.zip".into())
        .unwrap();

    // Process — both mirrors should fail in one call (retry loop)
    // Events: JobProgress → JobMirrorFailover (primary→secondary) → JobFailed (all exhausted)
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // Should have a failover event
    let has_failover = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobMirrorFailover { .. }));
    assert!(
        has_failover,
        "should emit failover when primary fails, got: {events:?}"
    );

    // Should have a JobFailed event (all mirrors exhausted in same call)
    let has_failed = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobFailed { .. }));
    assert!(
        has_failed,
        "all mirrors exhausted should emit JobFailed, got: {events:?}"
    );

    // Should have exactly 1 failover (primary→secondary, no more to try after that)
    let failover_count = events
        .iter()
        .filter(|e| matches!(e, CoreEvent::JobMirrorFailover { .. }))
        .count();
    assert_eq!(
        failover_count, 1,
        "should emit exactly 1 failover, got {failover_count}: {events:?}"
    );

    // Second call should produce nothing (job already failed)
    core.process_pending().unwrap();
    let events2 = collect_events(&rx);
    assert!(
        events2.is_empty(),
        "second process_pending should have no events (job already failed), got: {events2:?}"
    );
}

// Primary mirror works fine → no failover, completes directly.
#[test]
fn mirror_set_primary_succeeds_no_failover() {
    let content = b"content from primary".to_vec();
    let mut responses = std::collections::HashMap::new();
    responses.insert(
        "https://primary.example.com/file.zip".into(),
        content.clone(),
    );
    responses.insert(
        "https://secondary.example.com/file.zip".into(),
        b"should not be needed".to_vec(),
    );

    // Nothing in failing set — all URLs work
    let http = FailoverFakeHttp::new(responses, std::collections::HashSet::new());

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(FakeExtractorToolchain::new()));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://primary.example.com/file.zip");
    core.add_job_mirror(&job_id, "https://secondary.example.com/file.zip".into())
        .unwrap();

    // Process — primary works, should complete immediately
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    let has_completed = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(has_completed, "primary should complete, got: {events:?}");

    let has_failover = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobMirrorFailover { .. }));
    assert!(
        !has_failover,
        "should NOT emit failover when primary succeeds, got: {events:?}"
    );

    let on_disk = {
        let job_dir = root.join("jobs").join(&job_id);
        let files: Vec<_> = std::fs::read_dir(&job_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        std::fs::read(files[0].path()).unwrap()
    };
    assert_eq!(on_disk, content, "should have primary mirror content");
}

// Page-local mirror lists can extend a job's mirror set.
#[test]
fn page_local_mirrors_extend_mirror_set() {
    let content = b"content from page-local mirror".to_vec();
    let mut responses = std::collections::HashMap::new();
    responses.insert(
        "https://page-mirror.example.com/file.zip".into(),
        content.clone(),
    );

    // Primary fails, page-local mirror works
    let mut failing = std::collections::HashSet::new();
    failing.insert("https://primary.example.com/file.zip".into());

    let http = FailoverFakeHttp::new(responses, failing);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(FakeExtractorToolchain::new()));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://primary.example.com/file.zip");

    // Simulate page-local mirror discovery (e.g. from HTML page)
    let page_local_mirrors = vec![
        "https://page-mirror.example.com/file.zip".to_string(),
        "https://another-mirror.example.com/file.zip".to_string(),
    ];
    for url in page_local_mirrors {
        core.add_job_mirror(&job_id, url).unwrap();
    }

    let mirrors = core.job_mirrors(&job_id).unwrap();
    assert_eq!(mirrors.len(), 3, "primary + 2 page-local mirrors");

    // Process — should failover to page-local mirror and succeed
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    let has_completed = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(
        has_completed,
        "should complete via page-local mirror, got: {events:?}"
    );
}

// User-added URLs extend mirror set and are tried after primary.
#[test]
fn user_added_urls_extend_mirror_set() {
    let content = b"content from user mirror".to_vec();
    let mut responses = std::collections::HashMap::new();
    responses.insert(
        "https://user-mirror.example.com/file.zip".into(),
        content.clone(),
    );

    let mut failing = std::collections::HashSet::new();
    failing.insert("https://primary.example.com/file.zip".into());

    let http = FailoverFakeHttp::new(responses, failing);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(FakeExtractorToolchain::new()));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://primary.example.com/file.zip");

    // User adds a mirror URL (e.g. via UI "Add mirror" button)
    core.add_job_mirror(&job_id, "https://user-mirror.example.com/file.zip".into())
        .unwrap();

    let mirrors = core.job_mirrors(&job_id).unwrap();
    assert_eq!(mirrors.len(), 2, "primary + user mirror");
    assert_eq!(mirrors[1], "https://user-mirror.example.com/file.zip");

    // Process — should failover to user mirror
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    let has_completed = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(
        has_completed,
        "should complete via user-added mirror, got: {events:?}"
    );
}

// After a retry, the mirror index resets and primary is tried first.
struct UnfailFakeHttp {
    responses: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    failures_remaining: Arc<Mutex<std::collections::HashMap<String, u32>>>,
}

impl UnfailFakeHttp {
    fn new(
        responses: std::collections::HashMap<String, Vec<u8>>,
        fail_urls: Vec<(&str, u32)>,
    ) -> Self {
        let mut remaining = std::collections::HashMap::new();
        for (url, count) in fail_urls {
            remaining.insert(url.to_string(), count);
        }
        Self {
            responses: Arc::new(Mutex::new(responses)),
            failures_remaining: Arc::new(Mutex::new(remaining)),
        }
    }
}

impl HttpBackend for UnfailFakeHttp {
    fn fetch(
        &self,
        url: &str,
        range: Option<std::ops::Range<u64>>,
        sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        // Check if this URL should fail some number of times
        {
            let mut remaining = self.failures_remaining.lock().unwrap();
            if let Some(count) = remaining.get_mut(url) {
                if *count > 0 {
                    *count -= 1;
                    return Err(format!("simulated failure ({count} left)"));
                }
            }
        }

        let responses = self.responses.lock().unwrap();
        let full_body = responses
            .get(url)
            .ok_or_else(|| format!("no fake response for {url}"))?;
        let total = full_body.len() as u64;

        let data = match range {
            Some(r) => {
                let start = r.start.min(total);
                let end = r.end.min(total).max(start);
                &full_body[start as usize..end as usize]
            }
            None => full_body.as_slice(),
        };

        write_fetch_to_sink(sink, data, total)
    }

    fn supports_range(&self, _url: &str) -> Result<bool, String> {
        Ok(false)
    }

    fn file_size(&self, url: &str) -> Result<u64, String> {
        // Don't fail for file_size check (HEAD request approximation)
        let responses = self.responses.lock().unwrap();
        let full_body = responses
            .get(url)
            .ok_or_else(|| format!("no fake response for {url}"))?;
        Ok(full_body.len() as u64)
    }
}

#[test]
fn retry_resets_mirror_index_after_all_exhausted() {
    // Primary fails 1 time, secondary always works
    let content = b"retry-mirror content".to_vec();
    let mut responses = std::collections::HashMap::new();
    responses.insert(
        "https://primary.example.com/file.zip".into(),
        content.clone(),
    );
    responses.insert(
        "https://secondary.example.com/file.zip".into(),
        b"secondary content that should win on first try".to_vec(),
    );

    // Primary: fails 2 times (once per attempt cycle)
    let http = UnfailFakeHttp::new(
        responses,
        vec![
            ("https://primary.example.com/file.zip", 2), // fail twice (1st and retry attempt)
            ("https://secondary.example.com/file.zip", 0), // never fail
        ],
    );

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(FakeExtractorToolchain::new()));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://primary.example.com/file.zip");
    core.add_job_mirror(&job_id, "https://secondary.example.com/file.zip".into())
        .unwrap();

    // First process call: primary fails -> failover to secondary -> succeeds
    core.process_pending().unwrap();
    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "should complete via secondary on first attempt, got: {events:?}"
    );

    // Verify correct content (from secondary, since primary failed)
    let job_dir = root.join("jobs").join(&job_id);
    let files: Vec<_> = std::fs::read_dir(&job_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    let on_disk = std::fs::read(files[0].path()).unwrap();
    assert_eq!(
        on_disk,
        b"secondary content that should win on first try".to_vec(),
        "should have secondary content"
    );
}

// A job can be created by directly constructing a MirrorSet from URLs.
#[test]
fn mirror_set_is_accessible_from_job() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://primary.example.com/file.zip");

    // Add mirrors
    core.add_job_mirror(&job_id, "https://m1.example.com/file.zip".into())
        .unwrap();
    core.add_job_mirror(&job_id, "https://m2.example.com/file.zip".into())
        .unwrap();

    let mirrors = core.job_mirrors(&job_id).unwrap();
    assert_eq!(mirrors.len(), 3);
    assert_eq!(mirrors[0], "https://primary.example.com/file.zip");
    assert_eq!(mirrors[1], "https://m1.example.com/file.zip");
    assert_eq!(mirrors[2], "https://m2.example.com/file.zip");
}

// Metalink with no URLs returns 0, doesn't error.
#[test]
fn add_empty_metalink_returns_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");

    let count = core
        .add_metalink_to_job(&job_id, "<metalink><file name=\"x\"></file></metalink>")
        .unwrap();
    assert_eq!(count, 0, "empty Metalink should add 0 mirrors");

    // Mirrors should still be just the primary
    let mirrors = core.job_mirrors(&job_id).unwrap();
    assert_eq!(mirrors.len(), 1);
}

// Error on adding mirrors to nonexistent job.
#[test]
fn add_mirror_to_nonexistent_job_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let err = core.add_job_mirror("nonexistent", "https://example.com/file.zip".into());
    assert!(err.is_err(), "should error on nonexistent job");

    let err = core.job_mirrors("nonexistent");
    assert!(err.is_err(), "should error on nonexistent job");
}

// Mirror failover for segmented transfer path.
#[test]
fn mirror_failover_in_segmented_transfer() {
    // A segmented transfer that fails on primary and succeeds on secondary
    let content = b"segmented failover content 1234567890".to_vec();
    let mut responses = std::collections::HashMap::new();
    responses.insert(
        "https://primary.example.com/file.zip".into(),
        b"should fail".to_vec(),
    );
    responses.insert(
        "https://secondary.example.com/file.zip".into(),
        content.clone(),
    );

    let mut failing = std::collections::HashSet::new();
    failing.insert("https://primary.example.com/file.zip".into());

    // We need a backend that supports range so segmented path is taken
    // But our FailoverFakeHttp has supports_range=false.
    // Let's make one that supports range.
    #[derive(Clone)]
    struct SegmentedFailoverHttp {
        responses: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
        failing_urls: Arc<Mutex<std::collections::HashSet<String>>>,
    }

    impl HttpBackend for SegmentedFailoverHttp {
        fn fetch(
            &self,
            url: &str,
            range: Option<std::ops::Range<u64>>,
            sink: &mut dyn Write,
        ) -> Result<FetchResult, String> {
            {
                let failing = self.failing_urls.lock().unwrap();
                if failing.contains(url) {
                    return Err(format!("simulated failure for mirror {url}"));
                }
            }
            let responses = self.responses.lock().unwrap();
            let full_body = responses
                .get(url)
                .ok_or_else(|| format!("no fake response for {url}"))?;
            let total = full_body.len() as u64;
            let data = match range {
                Some(r) => {
                    let start = r.start.min(total);
                    let end = r.end.min(total).max(start);
                    &full_body[start as usize..end as usize]
                }
                None => full_body.as_slice(),
            };
            write_fetch_to_sink(sink, data, total)
        }

        fn supports_range(&self, _url: &str) -> Result<bool, String> {
            Ok(true) // enable segmented
        }

        fn file_size(&self, url: &str) -> Result<u64, String> {
            {
                let failing = self.failing_urls.lock().unwrap();
                if failing.contains(url) {
                    return Err(format!("simulated failure for mirror {url}"));
                }
            }
            let responses = self.responses.lock().unwrap();
            let full_body = responses
                .get(url)
                .ok_or_else(|| format!("no fake response for {url}"))?;
            Ok(full_body.len() as u64)
        }
    }

    let http = SegmentedFailoverHttp {
        responses: Arc::new(Mutex::new(responses)),
        failing_urls: Arc::new(Mutex::new(failing)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(FakeExtractorToolchain::new()));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://primary.example.com/file.zip");
    core.add_job_mirror(&job_id, "https://secondary.example.com/file.zip".into())
        .unwrap();

    // Process — should failover and complete via secondary
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    let has_failover = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobMirrorFailover { .. }));
    assert!(
        has_failover,
        "segmented transfer should failover on primary failure, got: {events:?}"
    );

    let has_completed = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(
        has_completed,
        "segmented transfer should complete after failover, got: {events:?}"
    );

    let job_dir = root.join("jobs").join(&job_id);
    let files: Vec<_> = std::fs::read_dir(&job_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(!files.is_empty(), "job dir should contain file");
    let on_disk = std::fs::read(files[0].path()).unwrap();
    assert_eq!(
        on_disk, content,
        "segmented failover should use secondary content"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: Live HTTP JobProgress (issue #36)
// ---------------------------------------------------------------------------

/// Slow single-connection fake: writes one byte at a time so mid-transfer
/// JobProgress can be observed from another thread.
#[derive(Clone)]
struct SlowKnownSizeHttp {
    body: Arc<Vec<u8>>,
    supports_range: bool,
}

impl HttpBackend for SlowKnownSizeHttp {
    fn fetch(
        &self,
        _url: &str,
        range: Option<std::ops::Range<u64>>,
        sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        let total = self.body.len() as u64;
        let data = match range {
            Some(r) => {
                let start = r.start.min(total) as usize;
                let end = r.end.min(total).max(r.start.min(total)) as usize;
                &self.body[start..end]
            }
            None => self.body.as_slice(),
        };
        let mut written = 0u64;
        for byte in data {
            let n = sink
                .write(std::slice::from_ref(byte))
                .map_err(|e| format!("sink write: {e}"))?;
            if n == 0 {
                break;
            }
            written += n as u64;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Ok(FetchResult {
            total_length: total,
            bytes_written: written,
        })
    }

    fn supports_range(&self, _url: &str) -> Result<bool, String> {
        Ok(self.supports_range)
    }

    fn file_size(&self, _url: &str) -> Result<u64, String> {
        Ok(self.body.len() as u64)
    }
}

/// Slow fake with unknown total (no Content-Length / HEAD size).
#[derive(Clone)]
struct SlowUnknownSizeHttp {
    body: Arc<Vec<u8>>,
}

impl HttpBackend for SlowUnknownSizeHttp {
    fn fetch(
        &self,
        _url: &str,
        _range: Option<std::ops::Range<u64>>,
        sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        let mut written = 0u64;
        for byte in self.body.iter() {
            let n = sink
                .write(std::slice::from_ref(byte))
                .map_err(|e| format!("sink write: {e}"))?;
            if n == 0 {
                break;
            }
            written += n as u64;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // Unknown size: report 0 as "no Content-Length" until Core maps this to None.
        Ok(FetchResult {
            total_length: 0,
            bytes_written: written,
        })
    }

    fn supports_range(&self, _url: &str) -> Result<bool, String> {
        Ok(false)
    }

    fn file_size(&self, _url: &str) -> Result<u64, String> {
        Err("no Content-Length".into())
    }
}

#[test]
fn http_emits_mid_transfer_job_progress_with_known_total() {
    let body = vec![b'K'; 64];
    let http = SlowKnownSizeHttp {
        body: Arc::new(body.clone()),
        supports_range: false,
    };
    let tmp = tempfile::tempdir().unwrap();
    let (core, rx) = make_core(Box::new(http));
    let mut core = core;
    core.set_library_root(tmp.path().join("library")).unwrap();
    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/known.bin");

    let core = Arc::new(Mutex::new(core));
    let worker = {
        let core = core.clone();
        std::thread::spawn(move || {
            core.lock().unwrap().process_pending().unwrap();
        })
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut saw_mid = false;
    let mut mid_total = None;
    while std::time::Instant::now() < deadline {
        for e in collect_events(&rx) {
            if let CoreEvent::JobProgress {
                job_id: id,
                bytes_downloaded,
                total_bytes,
            } = e
            {
                assert_eq!(id, job_id);
                if bytes_downloaded > 0 && bytes_downloaded < body.len() as u64 {
                    saw_mid = true;
                    mid_total = total_bytes;
                    break;
                }
            }
        }
        if saw_mid {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    worker.join().unwrap();

    assert!(
        saw_mid,
        "expected mid-transfer JobProgress with 0 < bytes < total"
    );
    assert_eq!(
        mid_total,
        Some(body.len() as u64),
        "known size must expose total_bytes: Some"
    );

    let snap = core
        .lock()
        .unwrap()
        .list_job_snapshots()
        .into_iter()
        .find(|s| s.job_id == job_id)
        .expect("job snapshot");
    assert_eq!(snap.total_bytes, Some(body.len() as u64));
    assert_eq!(snap.bytes_downloaded, body.len() as u64);
}

#[test]
fn http_emits_mid_transfer_job_progress_with_unknown_total() {
    let body = vec![b'U'; 64];
    let http = SlowUnknownSizeHttp {
        body: Arc::new(body.clone()),
    };
    let tmp = tempfile::tempdir().unwrap();
    let (core, rx) = make_core(Box::new(http));
    let mut core = core;
    core.set_library_root(tmp.path().join("library")).unwrap();
    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/unknown.bin");

    let core = Arc::new(Mutex::new(core));
    let worker = {
        let core = core.clone();
        std::thread::spawn(move || {
            core.lock().unwrap().process_pending().unwrap();
        })
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut saw_mid = false;
    let mut mid_total = Some(999u64); // sentinel — should become None
    while std::time::Instant::now() < deadline {
        for e in collect_events(&rx) {
            if let CoreEvent::JobProgress {
                job_id: id,
                bytes_downloaded,
                total_bytes,
            } = e
            {
                assert_eq!(id, job_id);
                if bytes_downloaded > 0 && bytes_downloaded < body.len() as u64 {
                    saw_mid = true;
                    mid_total = total_bytes;
                    break;
                }
            }
        }
        if saw_mid {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    worker.join().unwrap();

    assert!(
        saw_mid,
        "expected mid-transfer JobProgress with bytes-so-far"
    );
    assert_eq!(
        mid_total, None,
        "unknown size must expose total_bytes: None"
    );

    let snap = core
        .lock()
        .unwrap()
        .list_job_snapshots()
        .into_iter()
        .find(|s| s.job_id == job_id)
        .expect("job snapshot");
    assert_eq!(snap.total_bytes, None);
    assert!(
        snap.bytes_downloaded > 0,
        "snapshot should expose bytes downloaded"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: Extractor progress via on-disk poll (issue #39)
// ---------------------------------------------------------------------------

#[test]
fn silent_extractor_download_emits_mid_transfer_job_progress_from_disk_growth() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=progress-ext",
        "Progress Extractor Video",
        "https://cdn.youtube.com/progress-ext.mp4",
        vec![("720p", "720p")],
    );
    // Silent toolchain: grows a file on disk, never reports progress itself.
    let _chunks = toolchain.enable_slow_download(48, std::time::Duration::from_millis(15));

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    core.probe("https://www.youtube.com/watch?v=progress-ext")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=progress-ext".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    let job_id = job_ids[0].clone();

    let core = Arc::new(Mutex::new(core));
    let worker = {
        let core = core.clone();
        std::thread::spawn(move || {
            core.lock().unwrap().process_pending().unwrap();
        })
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut saw_mid = false;
    let mut mid_total = Some(999u64); // sentinel — silent extractor has no known total
    let mut progress_events = 0u32;
    let mut last_bytes = 0u64;
    let mut saw_growth = false;

    while std::time::Instant::now() < deadline {
        for e in collect_events(&rx) {
            if let CoreEvent::JobProgress {
                job_id: id,
                bytes_downloaded,
                total_bytes,
            } = e
            {
                assert_eq!(id, job_id);
                progress_events += 1;
                if bytes_downloaded > last_bytes {
                    saw_growth = true;
                    last_bytes = bytes_downloaded;
                }
                if bytes_downloaded > 0 {
                    saw_mid = true;
                    mid_total = total_bytes;
                }
            }
        }
        if saw_mid && saw_growth && progress_events >= 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    worker.join().unwrap();

    assert!(
        saw_mid,
        "expected mid-transfer JobProgress with bytes > 0 from on-disk growth"
    );
    assert!(
        saw_growth && progress_events >= 2,
        "expected repeated JobProgress as Job Destination files grow, got {progress_events} events (last_bytes={last_bytes})"
    );
    assert_eq!(
        mid_total, None,
        "silent extractor without known size must expose total_bytes: None"
    );

    let snap = core
        .lock()
        .unwrap()
        .list_job_snapshots()
        .into_iter()
        .find(|s| s.job_id == job_id)
        .expect("job snapshot");
    assert!(
        snap.bytes_downloaded > 0,
        "snapshot should expose bytes downloaded after extractor job"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: Extractor known size → determinate JobProgress (issue #43)
// ---------------------------------------------------------------------------

#[test]
fn extractor_quality_with_known_size_emits_job_progress_total_bytes() {
    const KNOWN_TOTAL: u64 = 12_345_678;

    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert_with_sizes(
        "https://www.youtube.com/watch?v=sized-ext",
        "Sized Extractor Video",
        "https://cdn.youtube.com/sized-ext.mp4",
        vec![("720p", "720p", Some(KNOWN_TOTAL))],
    );
    let _chunks = toolchain.enable_slow_download(48, std::time::Duration::from_millis(15));

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let probe = core
        .probe("https://www.youtube.com/watch?v=sized-ext")
        .unwrap();
    assert_eq!(
        probe.items[0].qualities[0].size_bytes,
        Some(KNOWN_TOTAL),
        "probe Quality must carry toolchain size"
    );
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=sized-ext".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    let job_id = job_ids[0].clone();

    let snap_before = core
        .list_job_snapshots()
        .into_iter()
        .find(|s| s.job_id == job_id)
        .expect("job snapshot");
    assert_eq!(
        snap_before.total_bytes,
        Some(KNOWN_TOTAL),
        "confirm must set job total from selected quality size"
    );

    let core = Arc::new(Mutex::new(core));
    let worker = {
        let core = core.clone();
        std::thread::spawn(move || {
            core.lock().unwrap().process_pending().unwrap();
        })
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut saw_mid = false;
    let mut mid_total = None;
    let mut progress_events = 0u32;
    let mut last_bytes = 0u64;
    let mut saw_growth = false;

    while std::time::Instant::now() < deadline {
        for e in collect_events(&rx) {
            if let CoreEvent::JobProgress {
                job_id: id,
                bytes_downloaded,
                total_bytes,
            } = e
            {
                assert_eq!(id, job_id);
                progress_events += 1;
                if bytes_downloaded > last_bytes {
                    saw_growth = true;
                    last_bytes = bytes_downloaded;
                }
                if bytes_downloaded > 0 {
                    saw_mid = true;
                    mid_total = total_bytes;
                }
            }
        }
        if saw_mid && saw_growth && progress_events >= 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    worker.join().unwrap();

    assert!(
        saw_mid,
        "expected mid-transfer JobProgress with bytes > 0 from on-disk growth"
    );
    assert_eq!(
        mid_total,
        Some(KNOWN_TOTAL),
        "known quality size must expose total_bytes: Some on JobProgress"
    );

    let snap = core
        .lock()
        .unwrap()
        .list_job_snapshots()
        .into_iter()
        .find(|s| s.job_id == job_id)
        .expect("job snapshot");
    assert_eq!(
        snap.total_bytes,
        Some(KNOWN_TOTAL),
        "job total must remain set through transfer"
    );
    assert!(
        snap.bytes_downloaded > 0,
        "completed/partial extractor job must report bytes"
    );
}

#[test]
fn extractor_quality_without_size_keeps_job_progress_total_none() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert_with_sizes(
        "https://www.youtube.com/watch?v=unsized-ext",
        "Unsized Extractor Video",
        "https://cdn.youtube.com/unsized-ext.mp4",
        vec![("720p", "720p", None)],
    );
    let _chunks = toolchain.enable_slow_download(24, std::time::Duration::from_millis(15));

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    core.probe("https://www.youtube.com/watch?v=unsized-ext")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=unsized-ext".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    let job_id = job_ids[0].clone();

    let core = Arc::new(Mutex::new(core));
    let worker = {
        let core = core.clone();
        std::thread::spawn(move || {
            core.lock().unwrap().process_pending().unwrap();
        })
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut saw_mid = false;
    let mut mid_total = Some(999u64);

    while std::time::Instant::now() < deadline {
        for e in collect_events(&rx) {
            if let CoreEvent::JobProgress {
                job_id: id,
                bytes_downloaded,
                total_bytes,
            } = e
            {
                assert_eq!(id, job_id);
                if bytes_downloaded > 0 {
                    saw_mid = true;
                    mid_total = total_bytes;
                }
            }
        }
        if saw_mid {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    worker.join().unwrap();

    assert!(saw_mid, "expected mid-transfer JobProgress");
    assert_eq!(
        mid_total, None,
        "quality without size must keep total_bytes: None"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: Library Root + absolute Job Destination override (issue #37)
// ---------------------------------------------------------------------------

#[test]
fn library_root_getter_returns_set_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    assert!(core.library_root().is_none());
    core.set_library_root(root.clone()).unwrap();
    assert_eq!(core.library_root(), Some(root.as_path()));
}

#[test]
fn changing_library_root_affects_default_destination_for_new_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let root_a = tmp.path().join("library-a");
    let root_b = tmp.path().join("library-b");
    let (mut core, _rx) = make_core(Box::new(fake_http()));

    core.set_library_root(root_a.clone()).unwrap();
    let dest_a = match core.enqueue("https://example.com/a.zip", None).unwrap() {
        EnqueueOutcome::EnqueueDirect { destination, .. } => destination,
        _ => panic!("expected enqueue_direct"),
    };
    assert!(dest_a.starts_with(root_a.join("jobs")));

    core.set_library_root(root_b.clone()).unwrap();
    let dest_b = match core.enqueue("https://example.com/b.zip", None).unwrap() {
        EnqueueOutcome::EnqueueDirect { destination, .. } => destination,
        _ => panic!("expected enqueue_direct"),
    };
    assert!(dest_b.starts_with(root_b.join("jobs")));
    assert!(!dest_b.starts_with(&root_a));
}

#[test]
fn enqueue_accepts_absolute_destination_outside_library_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let off_root = tmp.path().join("elsewhere").join("save-here");
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(root).unwrap();

    let (job_id, destination) = match core
        .enqueue("https://example.com/offroot.zip", Some(off_root.clone()))
        .unwrap()
    {
        EnqueueOutcome::EnqueueDirect {
            job_id,
            destination,
        } => (job_id, destination),
        _ => panic!("expected enqueue_direct"),
    };

    assert!(
        destination.starts_with(&off_root),
        "Job Destination must nest under Save-to parent"
    );
    assert_ne!(
        destination, off_root,
        "Job Destination must be unique job-owned subfolder, not shared parent"
    );
    assert_eq!(destination.parent(), Some(off_root.as_path()));
    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|s| s.job_id == job_id)
        .expect("job snapshot");
    assert_eq!(snap.destination, destination.display().to_string());
}

#[test]
fn enqueue_rejects_relative_destination_override() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let err = core
        .enqueue(
            "https://example.com/file.zip",
            Some(PathBuf::from("relative/path")),
        )
        .unwrap_err();
    assert!(
        err.to_lowercase().contains("absolute"),
        "expected absolute-path error, got: {err}"
    );
}

#[test]
fn off_root_destination_persists_across_queue_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let off_root = tmp.path().join("off-root-dest");
    let job_id;

    {
        let (mut core, _rx) = make_core(Box::new(fake_http()));
        core.set_library_root(root.clone()).unwrap();
        job_id = match core
            .enqueue(
                "https://example.com/persist-off.zip",
                Some(off_root.clone()),
            )
            .unwrap()
        {
            EnqueueOutcome::EnqueueDirect { job_id, .. } => job_id,
            _ => panic!("expected enqueue_direct"),
        };
    }

    let (mut core2, _rx2) = make_core(Box::new(fake_http()));
    core2.set_library_root(root).unwrap();
    let snap = core2
        .list_job_snapshots()
        .into_iter()
        .find(|s| s.job_id == job_id)
        .expect("reloaded job");
    let dest = PathBuf::from(&snap.destination);
    assert!(
        dest.starts_with(&off_root) && dest != off_root,
        "persisted Job Destination must remain nested under Save-to parent"
    );
}

#[test]
fn confirm_probe_selection_accepts_absolute_destination_override() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=offroot",
        "Off Root Video",
        "https://cdn.example.com/off.mp4",
        vec![("720p", "720p")],
    );

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let off_root = tmp.path().join("probe-save-to");
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(root.clone()).unwrap();

    core.probe("https://www.youtube.com/watch?v=offroot")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=offroot".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core
        .confirm_probe_selection(&selection, Some(off_root.clone()))
        .unwrap();
    assert_eq!(job_ids.len(), 1);

    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|s| s.job_id == job_ids[0])
        .expect("confirmed job");
    let dest = PathBuf::from(&snap.destination);
    assert!(
        dest.starts_with(&off_root) && dest != off_root,
        "single-item Save-to must nest unique Job Destination under parent"
    );
    assert!(!dest.starts_with(&root));
    let name = dest.file_name().unwrap().to_string_lossy();
    assert!(
        name.starts_with("Off_Root_Video-"),
        "nested folder should use title prefix, got {name}"
    );
}

#[test]
fn confirm_off_root_destination_persists_across_queue_reload() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=persist-off",
        "Persist Off Root",
        "https://cdn.example.com/persist-off.mp4",
        vec![("720p", "720p")],
    );

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let off_root = tmp.path().join("probe-persist-dest");
    let job_id;

    {
        let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
        core.set_library_root(root.clone()).unwrap();
        core.probe("https://www.youtube.com/watch?v=persist-off")
            .unwrap();
        collect_events(&rx);

        let selection = ProbeSelection {
            url: "https://www.youtube.com/watch?v=persist-off".to_string(),
            items: vec![ItemSelection {
                item_id: "item-1".to_string(),
                quality_id: "720p".to_string(),
            }],
            media_extras: None,
        };
        let job_ids = core
            .confirm_probe_selection(&selection, Some(off_root.clone()))
            .unwrap();
        assert_eq!(job_ids.len(), 1);
        job_id = job_ids[0].clone();
    }

    let (mut core2, _rx2) = make_core(Box::new(fake_http()));
    core2.set_library_root(root).unwrap();
    let snap = core2
        .list_job_snapshots()
        .into_iter()
        .find(|s| s.job_id == job_id)
        .expect("reloaded confirm job");
    let dest = PathBuf::from(&snap.destination);
    assert!(
        dest.starts_with(&off_root) && dest != off_root,
        "persisted confirm Job Destination must remain nested under Save-to parent"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: resolved Job Destination preview before confirm (issue #34 US#27)
// ---------------------------------------------------------------------------

#[test]
fn preview_probe_destination_shows_batch_subfolder_under_library_root() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert_batch(
        "https://youtube.com/playlist?list=PREV",
        "My Awesome Playlist",
        vec![
            (
                "vid-1",
                "Video One",
                "https://youtube.com/watch?v=prev1",
                vec![("1080p", "1080p")],
            ),
            (
                "vid-2",
                "Video Two",
                "https://youtube.com/watch?v=prev2",
                vec![("1080p", "1080p")],
            ),
        ],
    );

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    core.probe("https://youtube.com/playlist?list=PREV")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://youtube.com/playlist?list=PREV".to_string(),
        items: vec![
            ItemSelection {
                item_id: "vid-1".to_string(),
                quality_id: "1080p".to_string(),
            },
            ItemSelection {
                item_id: "vid-2".to_string(),
                quality_id: "1080p".to_string(),
            },
        ],
        media_extras: None,
    };

    let preview = core
        .preview_probe_destination(&selection, None)
        .expect("preview should resolve");
    let expected = root.join("jobs").join("My_Awesome_Playlist-\u{2026}");
    assert_eq!(
        preview.path_display,
        expected.display().to_string(),
        "batch preview must show Library Root jobs subfolder pattern"
    );

    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    assert_eq!(job_ids.len(), 2);
    for id in &job_ids {
        let dest = core
            .list_job_snapshots()
            .into_iter()
            .find(|s| &s.job_id == id)
            .expect("job")
            .destination;
        let name = PathBuf::from(&dest)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("My_Awesome_Playlist-"),
            "confirmed dest should match preview prefix, got {name}"
        );
    }
}

#[test]
fn preview_probe_destination_shows_nested_pattern_for_single_save_to_override() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=prev-exact",
        "Exact Preview Video",
        "https://cdn.example.com/prev-exact.mp4",
        vec![("720p", "720p")],
    );

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let off_root = tmp.path().join("exact-save-to");
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(root).unwrap();

    core.probe("https://www.youtube.com/watch?v=prev-exact")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=prev-exact".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };

    let preview = core
        .preview_probe_destination(&selection, Some(off_root.clone()))
        .expect("preview");
    let expected = off_root.join("Exact_Preview_Video-\u{2026}");
    assert_eq!(
        preview.path_display,
        expected.display().to_string(),
        "single Save to… override must preview nested job-owned Job Destination"
    );
}

#[test]
fn preview_probe_destination_shows_multi_item_pattern_under_save_to_override() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert_batch(
        "https://youtube.com/playlist?list=PREV-MULTI",
        "Channel Batch",
        vec![
            (
                "vid-a",
                "A",
                "https://youtube.com/watch?v=preva",
                vec![("1080p", "1080p")],
            ),
            (
                "vid-b",
                "B",
                "https://youtube.com/watch?v=prevb",
                vec![("1080p", "1080p")],
            ),
        ],
    );

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let off_root = tmp.path().join("batch-save-to");
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(root).unwrap();

    core.probe("https://youtube.com/playlist?list=PREV-MULTI")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://youtube.com/playlist?list=PREV-MULTI".to_string(),
        items: vec![
            ItemSelection {
                item_id: "vid-a".to_string(),
                quality_id: "1080p".to_string(),
            },
            ItemSelection {
                item_id: "vid-b".to_string(),
                quality_id: "1080p".to_string(),
            },
        ],
        media_extras: None,
    };

    let preview = core
        .preview_probe_destination(&selection, Some(off_root.clone()))
        .expect("preview");
    let expected = off_root.join("Channel_Batch-\u{2026}");
    assert_eq!(
        preview.path_display,
        expected.display().to_string(),
        "multi-item Save to… must preview subfolders under the override parent"
    );

    let job_ids = core
        .confirm_probe_selection(&selection, Some(off_root.clone()))
        .unwrap();
    assert_eq!(job_ids.len(), 2);
    for id in &job_ids {
        let dest = PathBuf::from(
            core.list_job_snapshots()
                .into_iter()
                .find(|s| &s.job_id == id)
                .expect("job")
                .destination,
        );
        assert!(
            dest.starts_with(&off_root),
            "confirmed dest must stay under override parent"
        );
        let name = dest.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("Channel_Batch-"),
            "confirmed dest should match preview prefix, got {name}"
        );
    }
}

// ---------------------------------------------------------------------------
// Acceptance: Extractor Completed cleans orphan partials (issue #42)
// ---------------------------------------------------------------------------

#[test]
fn extractor_completed_removes_orphan_temp_siblings_under_job_destination() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=orphan-temps",
        "Orphan Temps Video",
        "https://cdn.youtube.com/orphan-temps.mp4",
        vec![("720p", "720p")],
    );
    toolchain.leave_orphan_temps(&[
        "Orphan Temps Video [orphan-temps].mp4.part",
        "Orphan Temps Video [orphan-temps].mp4.ytdl",
    ]);

    let tmp = tempfile::tempdir().unwrap();
    let save_to = tmp.path().join("home").join("xavier");
    std::fs::create_dir_all(&save_to).unwrap();

    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    core.probe("https://www.youtube.com/watch?v=orphan-temps")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=orphan-temps".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core
        .confirm_probe_selection(&selection, Some(save_to.clone()))
        .unwrap();
    let job_id = job_ids[0].clone();
    let job_dir = PathBuf::from(
        core.list_job_snapshots()
            .into_iter()
            .find(|j| j.job_id == job_id)
            .expect("job")
            .destination,
    );

    core.process_pending().unwrap();
    let events = collect_events(&rx);

    let completed_path = events.iter().find_map(|e| match e {
        CoreEvent::JobCompleted {
            job_id: id, path, ..
        } if id == &job_id => Some(path.clone()),
        _ => None,
    });
    let final_path = completed_path.expect("JobCompleted with path");
    assert!(
        final_path.exists(),
        "final Extractor output must remain at {:?}",
        final_path
    );
    assert_eq!(
        final_path.parent(),
        Some(job_dir.as_path()),
        "Completed path must stay under Job Destination"
    );
    assert!(
        !job_dir
            .join("Orphan Temps Video [orphan-temps].mp4.part")
            .exists(),
        ".part orphan must be removed after Extractor Completed"
    );
    assert!(
        !job_dir
            .join("Orphan Temps Video [orphan-temps].mp4.ytdl")
            .exists(),
        ".ytdl orphan must be removed after Extractor Completed"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: nest Job Destination + safe Discard (issue #46)
// ---------------------------------------------------------------------------

#[test]
fn single_item_save_to_nests_unique_job_destination_under_parent() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=nest-single",
        "Nest Single Video",
        "https://cdn.example.com/nest-single.mp4",
        vec![("720p", "720p")],
    );

    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("Downloads");
    std::fs::create_dir_all(&shared).unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    core.probe("https://www.youtube.com/watch?v=nest-single")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=nest-single".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core
        .confirm_probe_selection(&selection, Some(shared.clone()))
        .unwrap();
    let dest = PathBuf::from(
        core.list_job_snapshots()
            .into_iter()
            .find(|s| s.job_id == job_ids[0])
            .expect("job")
            .destination,
    );

    assert_eq!(dest.parent(), Some(shared.as_path()));
    assert_ne!(dest, shared);
    let name = dest.file_name().unwrap().to_string_lossy();
    assert!(
        name.starts_with("Nest_Single_Video-"),
        "expected title-derived nested folder, got {name}"
    );
}

#[test]
fn discard_save_to_override_deletes_only_job_owned_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("shared-downloads");
    std::fs::create_dir_all(&shared).unwrap();
    let sibling = shared.join("keep-me.txt");
    let foreign_part = shared.join("browser-download.part");
    std::fs::write(&sibling, b"user file").unwrap();
    std::fs::write(&foreign_part, b"browser partial").unwrap();

    let (mut core, rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let (job_id, job_dir) = match core
        .enqueue("https://example.com/override.zip", Some(shared.clone()))
        .unwrap()
    {
        EnqueueOutcome::EnqueueDirect {
            job_id,
            destination,
        } => (job_id, destination),
        _ => panic!("expected enqueue_direct"),
    };

    assert_eq!(job_dir.parent(), Some(shared.as_path()));
    assert_ne!(job_dir, shared);

    std::fs::create_dir_all(&job_dir).unwrap();
    std::fs::write(job_dir.join("override.zip.part"), b"job partial").unwrap();

    core.pause_job(&job_id).unwrap();
    core.discard_job(&job_id).unwrap();

    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobDiscarded { job_id: id } if id == &job_id)),
        "should emit JobDiscarded, got {events:?}"
    );
    assert!(
        !job_dir.exists(),
        "Discard must remove job-owned destination tree"
    );
    assert!(
        sibling.exists(),
        "Discard must not wipe sibling files in shared Save-to parent"
    );
    assert!(
        foreign_part.exists(),
        "Discard must not wipe foreign .part files in shared parent"
    );
    assert!(
        shared.exists(),
        "shared Save-to parent must survive Discard"
    );
}

#[test]
fn extractor_cleanup_and_byte_totals_ignore_siblings_in_shared_parent() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=safe-siblings",
        "Safe Siblings Video",
        "https://cdn.youtube.com/safe-siblings.mp4",
        vec![("720p", "720p")],
    );
    toolchain.leave_orphan_temps(&[
        "Safe Siblings Video [safe-siblings].mp4.part",
        "Safe Siblings Video [safe-siblings].mp4.ytdl",
    ]);
    let _chunks = toolchain.enable_slow_download(24, std::time::Duration::from_millis(10));

    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("home").join("downloads");
    std::fs::create_dir_all(&shared).unwrap();
    let foreign_part = shared.join("chrome-other.part");
    let foreign_keep = shared.join("photo.jpg");
    // Large enough that inflated destination_byte_total would blow mid-progress.
    let foreign_blob = vec![0u8; 2 * 1024 * 1024];
    std::fs::write(&foreign_part, b"browser in-progress").unwrap();
    std::fs::write(&foreign_keep, &foreign_blob).unwrap();

    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    core.probe("https://www.youtube.com/watch?v=safe-siblings")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=safe-siblings".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core
        .confirm_probe_selection(&selection, Some(shared.clone()))
        .unwrap();
    let job_id = job_ids[0].clone();
    let job_dir = PathBuf::from(
        core.list_job_snapshots()
            .into_iter()
            .find(|j| j.job_id == job_id)
            .expect("job")
            .destination,
    );
    assert_ne!(job_dir, shared);

    let core = Arc::new(Mutex::new(core));
    let worker = {
        let core = core.clone();
        std::thread::spawn(move || {
            core.lock().unwrap().process_pending().unwrap();
        })
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut max_progress = 0u64;
    let mut saw_completed = false;
    let mut completed_bytes = 0u64;

    while std::time::Instant::now() < deadline {
        for e in collect_events(&rx) {
            match e {
                CoreEvent::JobProgress {
                    job_id: id,
                    bytes_downloaded,
                    ..
                } if id == job_id => {
                    max_progress = max_progress.max(bytes_downloaded);
                    assert!(
                        bytes_downloaded < foreign_blob.len() as u64,
                        "destination_byte_total must ignore siblings in shared parent \
                         (progress {bytes_downloaded} >= foreign {})",
                        foreign_blob.len()
                    );
                }
                CoreEvent::JobCompleted {
                    job_id: id,
                    bytes_written,
                    ..
                } if id == job_id => {
                    saw_completed = true;
                    completed_bytes = bytes_written;
                }
                _ => {}
            }
        }
        if saw_completed {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    worker.join().unwrap();

    assert!(saw_completed, "extractor job should complete");
    assert!(
        max_progress > 0 && max_progress < foreign_blob.len() as u64,
        "mid-transfer progress must reflect job tree only, got max={max_progress}"
    );
    assert!(
        completed_bytes < foreign_blob.len() as u64,
        "Completed byte total must exclude shared-parent siblings, got {completed_bytes}"
    );
    assert!(
        foreign_part.exists(),
        "cleanup must not delete foreign .part in shared parent"
    );
    assert!(
        foreign_keep.exists(),
        "cleanup must not touch sibling files in shared parent"
    );
    assert!(
        !job_dir
            .join("Safe Siblings Video [safe-siblings].mp4.part")
            .exists(),
        "job-owned .part orphans must still be cleaned"
    );
    assert!(
        !job_dir
            .join("Safe Siblings Video [safe-siblings].mp4.ytdl")
            .exists(),
        "job-owned .ytdl orphans must still be cleaned"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: Active Job Limit + queue reorder (issue #50)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct SlowTrackingHttp {
    responses: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    in_flight: Arc<std::sync::atomic::AtomicU32>,
    max_concurrent: Arc<std::sync::atomic::AtomicU32>,
    delay: std::time::Duration,
    start_order: Arc<Mutex<Vec<String>>>,
}

impl HttpBackend for SlowTrackingHttp {
    fn fetch(
        &self,
        url: &str,
        range: Option<std::ops::Range<u64>>,
        sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        self.start_order.lock().unwrap().push(url.to_string());
        let cur = self
            .in_flight
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        loop {
            let max = self
                .max_concurrent
                .load(std::sync::atomic::Ordering::SeqCst);
            if cur > max {
                if self
                    .max_concurrent
                    .compare_exchange(
                        max,
                        cur,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    )
                    .is_ok()
                {
                    break;
                }
            } else {
                break;
            }
        }

        std::thread::sleep(self.delay);

        let responses = self.responses.lock().unwrap();
        let full_body = responses
            .get(url)
            .ok_or_else(|| format!("no fake response for {url}"))?;
        let total = full_body.len() as u64;
        let data = match range {
            Some(r) => {
                let start = r.start.min(total);
                let end = r.end.min(total).max(start);
                &full_body[start as usize..end as usize]
            }
            None => full_body.as_slice(),
        };
        let result = write_fetch_to_sink(sink, data, total);
        self.in_flight
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        result
    }

    fn supports_range(&self, _url: &str) -> Result<bool, String> {
        Ok(false)
    }

    fn file_size(&self, url: &str) -> Result<u64, String> {
        let responses = self.responses.lock().unwrap();
        Ok(responses.get(url).map(|b| b.len() as u64).unwrap_or(0))
    }
}

fn slow_tracking_http(urls: &[(&str, usize)]) -> SlowTrackingHttp {
    slow_tracking_http_with_delay(urls, std::time::Duration::from_millis(80))
}

fn slow_tracking_http_with_delay(
    urls: &[(&str, usize)],
    delay: std::time::Duration,
) -> SlowTrackingHttp {
    let mut map = std::collections::HashMap::new();
    for (url, size) in urls {
        map.insert((*url).to_string(), vec![b'x'; *size]);
    }
    SlowTrackingHttp {
        responses: Arc::new(Mutex::new(map)),
        in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        max_concurrent: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        delay,
        start_order: Arc::new(Mutex::new(Vec::new())),
    }
}

#[test]
fn active_job_limit_defaults_to_three() {
    let (core, _rx) = make_core(Box::new(fake_http()));
    assert_eq!(core.active_job_limit(), DEFAULT_ACTIVE_JOB_LIMIT);
}

#[test]
fn set_active_job_limit_rejects_zero() {
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    let err = core.set_active_job_limit(0).unwrap_err();
    assert!(
        err.contains("at least 1"),
        "expected validation error, got: {err}"
    );
}

#[test]
fn active_job_limit_caps_concurrent_transfers() {
    let urls: Vec<_> = (1..=5)
        .map(|n| (format!("https://example.com/{n}.zip"), 64))
        .collect();
    let url_refs: Vec<(&str, usize)> = urls.iter().map(|(u, s)| (u.as_str(), *s)).collect();
    let http = slow_tracking_http(&url_refs);

    let tmp = tempfile::tempdir().unwrap();
    let (core, rx) = Core::new(Box::new(http.clone()), Box::new(fake_toolchain()));
    let core = Arc::new(Mutex::new(core));
    core.lock()
        .unwrap()
        .set_library_root(tmp.path().join("library"))
        .unwrap();

    let mut job_ids = Vec::new();
    for (url, _) in &urls {
        let id = {
            let mut guard = core.lock().unwrap();
            match guard.enqueue(url, None).unwrap() {
                EnqueueOutcome::EnqueueDirect { job_id, .. } => job_id,
                _ => panic!("expected enqueue_direct"),
            }
        };
        job_ids.push(id);
    }

    {
        let mut guard = core.lock().unwrap();
        guard.set_active_job_limit(3).unwrap();
    }

    Core::dispatch_pending(Arc::clone(&core)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert!(
        http.max_concurrent
            .load(std::sync::atomic::Ordering::SeqCst)
            <= 3,
        "at most 3 jobs should transfer concurrently"
    );

    let mut done = std::collections::HashSet::new();
    for _ in 0..40 {
        for ev in collect_events(&rx) {
            if let CoreEvent::JobCompleted { job_id, .. } = ev {
                done.insert(job_id);
            }
        }
        if done.len() == job_ids.len() {
            break;
        }
        Core::dispatch_pending(Arc::clone(&core)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(done.len(), job_ids.len(), "all jobs should complete");
}

#[test]
fn reorder_moves_job_in_queue() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let id_a = enqueue_direct_job_id(&mut core, "https://example.com/a.zip");
    let id_b = enqueue_direct_job_id(&mut core, "https://example.com/b.zip");
    let id_c = enqueue_direct_job_id(&mut core, "https://example.com/c.zip");

    core.reorder_job(2, 0).unwrap();

    let ids: Vec<String> = core
        .list_job_snapshots()
        .into_iter()
        .map(|j| j.job_id)
        .collect();
    assert_eq!(ids, vec![id_c, id_a, id_b]);
}

#[test]
fn reorder_does_not_preempt_active_transfer() {
    let urls: Vec<String> = (1..=3)
        .map(|n| format!("https://example.com/job{n}.zip"))
        .collect();
    let url_refs: Vec<(&str, usize)> = urls.iter().map(|u| (u.as_str(), 256)).collect();
    let http = slow_tracking_http_with_delay(&url_refs, std::time::Duration::from_millis(400));

    let tmp = tempfile::tempdir().unwrap();
    let (core, _rx) = Core::new(Box::new(http), Box::new(fake_toolchain()));
    let core = Arc::new(Mutex::new(core));
    core.lock()
        .unwrap()
        .set_library_root(tmp.path().join("library"))
        .unwrap();

    let mut job_ids = Vec::new();
    for url in &urls {
        let id = {
            let mut guard = core.lock().unwrap();
            match guard.enqueue(url, None).unwrap() {
                EnqueueOutcome::EnqueueDirect { job_id, .. } => job_id,
                _ => panic!("expected enqueue_direct"),
            }
        };
        job_ids.push(id);
    }

    {
        let mut guard = core.lock().unwrap();
        guard.set_active_job_limit(1).unwrap();
    }

    Core::dispatch_pending(Arc::clone(&core)).unwrap();

    for _ in 0..40 {
        if core
            .lock()
            .unwrap()
            .job_signals()
            .is_transfer_active(&job_ids[0])
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        Core::dispatch_pending(Arc::clone(&core)).unwrap();
    }

    {
        let mut guard = core.lock().unwrap();
        assert!(
            guard.job_signals().is_transfer_active(&job_ids[0]),
            "job1 should be actively transferring"
        );
        guard.reorder_job(2, 0).unwrap();
        assert!(
            guard.job_signals().is_transfer_active(&job_ids[0]),
            "reorder must not preempt an active transfer"
        );
    }
}

// ---------------------------------------------------------------------------
// Regression: queue changes during a transfer (B-1)
// ---------------------------------------------------------------------------

/// Queue [paused A, B], then start B so its transfer runs while the test
/// edits the queue. Returns the shared Core, A's id, B's id and the receiver.
type TransferBehindPausedJob = (
    Arc<Mutex<Core>>,
    String,
    String,
    crossbeam_channel::Receiver<CoreEvent>,
);

fn start_transfer_behind_paused_job(library: &std::path::Path) -> TransferBehindPausedJob {
    let http = slow_tracking_http_with_delay(
        &[
            ("https://example.com/a.zip", 64),
            ("https://example.com/b.zip", 64),
        ],
        std::time::Duration::from_millis(400),
    );
    let (core, rx) = Core::new(Box::new(http), Box::new(fake_toolchain()));
    let core = Arc::new(Mutex::new(core));
    let (a_id, b_id) = {
        let mut guard = core.lock().unwrap();
        guard.set_library_root(library.to_path_buf()).unwrap();
        let a = enqueue_direct_job_id(&mut guard, "https://example.com/a.zip");
        let b = enqueue_direct_job_id(&mut guard, "https://example.com/b.zip");
        guard.pause_job(&a).unwrap();
        (a, b)
    };
    Core::dispatch_pending(Arc::clone(&core)).unwrap();
    for _ in 0..100 {
        if core.lock().unwrap().job_signals().is_transfer_active(&b_id) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        core.lock().unwrap().job_signals().is_transfer_active(&b_id),
        "job B should be transferring"
    );
    (core, a_id, b_id, rx)
}

fn wait_for_completion(rx: &crossbeam_channel::Receiver<CoreEvent>, job_id: &str) -> bool {
    for _ in 0..200 {
        for ev in collect_events(rx) {
            if matches!(&ev, CoreEvent::JobCompleted { job_id: id, .. } if id == job_id) {
                return true;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    false
}

#[test]
fn discarding_an_earlier_job_during_a_transfer_does_not_break_the_transfer() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, a_id, b_id, rx) = start_transfer_behind_paused_job(&tmp.path().join("library"));

    core.lock().unwrap().discard_job(&a_id).unwrap();

    assert!(
        wait_for_completion(&rx, &b_id),
        "job B should complete after job A is discarded"
    );
    let guard = core.lock().expect("Core mutex must not be poisoned");
    let snapshots = guard.list_job_snapshots();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].job_id, b_id);
    assert_eq!(snapshots[0].status, "completed");
}

#[test]
fn reordering_during_a_transfer_completes_the_job_that_transferred() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, a_id, b_id, rx) = start_transfer_behind_paused_job(&tmp.path().join("library"));

    core.lock().unwrap().reorder_job(0, 1).unwrap();

    assert!(
        wait_for_completion(&rx, &b_id),
        "job B should complete after the queue is reordered"
    );
    let guard = core.lock().expect("Core mutex must not be poisoned");
    let snapshots = guard.list_job_snapshots();
    let a = snapshots.iter().find(|s| s.job_id == a_id).unwrap();
    let b = snapshots.iter().find(|s| s.job_id == b_id).unwrap();
    assert_eq!(a.status, "paused", "job A must not be completed by B");
    assert_eq!(b.status, "completed");
    let b_path = b.path.as_deref().expect("completed job has a path");
    assert!(
        b_path.ends_with("b.zip"),
        "B should keep its own filename, got {b_path}"
    );
    assert!(std::path::Path::new(b_path).is_file());
}

// ---------------------------------------------------------------------------
// Acceptance: Integrity Check (issue #51)
// ---------------------------------------------------------------------------

const HELLO_FILE_SHA256: &str = "f69af9e91e9e5d93dac3c583e5e896dc71b756ef1a42ecbc2a0028393b487868";

#[test]
fn integrity_check_pass_completes_when_hash_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.set_job_expected_hash(&job_id, "sha-256", HELLO_FILE_SHA256)
        .unwrap();

    core.process_pending().unwrap();
    let events = collect_events(&rx);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "matching hash should complete, got: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobFailed { .. })),
        "matching hash must not fail"
    );

    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .expect("job still queued");
    assert_eq!(snap.status, "completed");
}

#[test]
fn integrity_check_mismatch_fails_and_keeps_file_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.set_job_expected_hash(&job_id, "sha-256", "0".repeat(64).as_str())
        .unwrap();

    core.process_pending().unwrap();
    let events = collect_events(&rx);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobFailed { job_id: id, .. } if id == &job_id)),
        "mismatch should emit JobFailed, got: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "mismatch must not complete"
    );

    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .expect("job still queued");
    assert_eq!(snap.status, "failed");

    let final_path = snap.destination.clone() + "/file.zip";
    let final_path = std::path::PathBuf::from(final_path);
    assert!(final_path.exists(), "mismatched file must remain on disk");
    assert_eq!(
        std::fs::read(&final_path).unwrap(),
        b"hello world this is a test file"
    );
}

#[test]
fn integrity_check_skipped_when_no_expected_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");

    core.process_pending().unwrap();
    let events = collect_events(&rx);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "no hash should complete normally, got: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobFailed { .. })),
        "no hash must not fail verification"
    );

    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .expect("job still queued");
    assert_eq!(snap.status, "completed");
}

#[test]
fn add_metalink_to_job_sets_expected_hash_from_xml() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    let metalink_xml = format!(
        r#"
        <metalink>
            <file name="file.zip">
                <hash type="sha-256">{HELLO_FILE_SHA256}</hash>
                <url>https://example.com/file.zip</url>
            </file>
        </metalink>
    "#
    );
    core.add_metalink_to_job(&job_id, &metalink_xml).unwrap();

    core.process_pending().unwrap();
    let events = collect_events(&rx);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "metalink hash match should complete, got: {events:?}"
    );
}

#[test]
fn set_job_expected_hash_from_sidecar_loads_checksum_file() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    let sidecar = tmp.path().join("file.zip.sha256");
    std::fs::write(&sidecar, format!("{HELLO_FILE_SHA256}  file.zip\n")).unwrap();
    core.set_job_expected_hash_from_sidecar(&job_id, &sidecar)
        .unwrap();

    core.process_pending().unwrap();
    let events = collect_events(&rx);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "sidecar hash match should complete, got: {events:?}"
    );
}

#[test]
fn set_job_expected_hash_from_sidecar_uses_the_row_for_the_job_filename() {
    // M-17: a multi-file SHA256SUMS must be matched by filename column.
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    let sidecar = tmp.path().join("SHA256SUMS");
    std::fs::write(
        &sidecar,
        format!(
            "{}  other.zip\n{HELLO_FILE_SHA256} *file.zip\n",
            "a".repeat(64)
        ),
    )
    .unwrap();
    core.set_job_expected_hash_from_sidecar(&job_id, &sidecar)
        .unwrap();

    core.process_pending().unwrap();
    let events = collect_events(&rx);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "row for file.zip should be used, got: {events:?}"
    );
}

#[test]
fn set_job_expected_hash_from_sidecar_errors_when_no_row_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    let sidecar = tmp.path().join("SHA256SUMS");
    std::fs::write(&sidecar, format!("{HELLO_FILE_SHA256}  other.zip\n")).unwrap();
    let err = core
        .set_job_expected_hash_from_sidecar(&job_id, &sidecar)
        .unwrap_err();
    assert!(err.contains("file.zip"), "got: {err}");
}

// ---------------------------------------------------------------------------
// Acceptance: Disk-space pre-check (issue #52)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FakeFreeSpace {
    free_bytes: u64,
}

impl FreeSpaceLookup for FakeFreeSpace {
    fn free_bytes(&self, _path: &std::path::Path) -> Result<u64, String> {
        Ok(self.free_bytes)
    }
}

fn make_core_with_free_space(
    http: Box<dyn HttpBackend>,
    free_bytes: u64,
) -> (Core, crossbeam_channel::Receiver<CoreEvent>) {
    Core::new_with_free_space(
        http,
        Box::new(fake_toolchain()),
        Arc::new(FakeFreeSpace { free_bytes }),
    )
}

#[test]
fn disk_precheck_refuses_when_known_size_insufficient() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core_with_free_space(Box::new(NoRangeFakeHttp(fake_http())), 1_000);
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    assert!(
        events.iter().any(|e| match e {
            CoreEvent::JobFailed { job_id: id, error } => {
                id == &job_id && error.contains("insufficient disk space")
            }
            _ => false,
        }),
        "known size with too little free space should fail clearly, got: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "insufficient disk must not complete"
    );

    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .expect("job still queued");
    assert_eq!(snap.status, "failed");
}

#[test]
fn disk_precheck_allows_when_known_size_fits_with_margin() {
    let tmp = tempfile::tempdir().unwrap();
    let body_len = b"hello world this is a test file".len() as u64;
    let free = required_free_bytes(body_len);
    let (mut core, rx) = make_core_with_free_space(Box::new(NoRangeFakeHttp(fake_http())), free);
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "known size with enough free space should complete, got: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobFailed { .. })),
        "sufficient disk must not fail"
    );

    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .expect("job still queued");
    assert_eq!(snap.status, "completed");
}

#[test]
fn disk_precheck_allows_when_size_unknown() {
    let body = vec![b'U'; 64];
    let http = SlowUnknownSizeHttp {
        body: Arc::new(body.clone()),
    };
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core_with_free_space(Box::new(http), 0);
    core.set_library_root(tmp.path().join("library")).unwrap();

    let url = "https://example.com/unknown.bin";
    let job_id = {
        let outcome = core.enqueue(url, None).expect("enqueue should succeed");
        match outcome {
            EnqueueOutcome::EnqueueDirect { job_id, .. } => job_id,
            _ => panic!("expected direct enqueue"),
        }
    };

    core.process_pending().unwrap();
    let events = collect_events(&rx);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "unknown size should start even with zero reported free space, got: {events:?}"
    );
    assert!(
        !events.iter().any(|e| match e {
            CoreEvent::JobFailed { job_id: id, .. } => id == &job_id,
            _ => false,
        }),
        "unknown size must not fail disk pre-check"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: Default Quality Policy + audio-only (issue #53)
// ---------------------------------------------------------------------------

fn sample_video_qualities() -> Vec<Quality> {
    vec![
        Quality {
            id: "2160p".into(),
            label: "2160p".into(),
            size_bytes: None,
            container: None,
        },
        Quality {
            id: "1080p".into(),
            label: "1080p".into(),
            size_bytes: None,
            container: Some("mp4".into()),
        },
        Quality {
            id: "720p".into(),
            label: "720p".into(),
            size_bytes: None,
            container: None,
        },
    ]
}

#[test]
fn default_quality_policy_selects_best_at_or_below_1080p() {
    let (core, _) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    let selected = core
        .select_default_quality(&sample_video_qualities())
        .expect("should select a quality");
    assert_eq!(selected, "1080p");
}

#[test]
fn audio_only_mode_overrides_video_default_quality_policy() {
    let (mut core, _) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    core.set_audio_only_mode(true).unwrap();

    let qualities = vec![
        Quality {
            id: "1080p".into(),
            label: "1080p".into(),
            size_bytes: None,
            container: None,
        },
        Quality {
            id: "256k-audio".into(),
            label: "high (audio only)".into(),
            size_bytes: None,
            container: None,
        },
        Quality {
            id: "130k-audio".into(),
            label: "medium (audio only)".into(),
            size_bytes: None,
            container: None,
        },
    ];

    let selected = core
        .select_default_quality(&qualities)
        .expect("should select audio");
    assert_eq!(selected, "256k-audio");
}

#[test]
fn audio_only_mode_setting_persists_across_library_root_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, _) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    core.set_library_root(root.clone()).unwrap();
    core.set_audio_only_mode(true).unwrap();

    let (mut core2, _) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    core2.set_library_root(root).unwrap();
    assert!(core2.audio_only_mode());
}

#[test]
fn default_quality_policy_settings_change_reflected_on_next_selection() {
    let (mut core, _) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    let qualities = sample_video_qualities();

    assert_eq!(
        core.select_default_quality(&qualities).as_deref(),
        Some("1080p")
    );

    core.set_audio_only_mode(true).unwrap();
    let audio_qualities = vec![
        qualities[1].clone(),
        Quality {
            id: "192k-audio".into(),
            label: "audio".into(),
            size_bytes: None,
            container: None,
        },
    ];
    assert_eq!(
        core.select_default_quality(&audio_qualities).as_deref(),
        Some("192k-audio")
    );
}

// ---------------------------------------------------------------------------
// Acceptance: Media extras + remux/embed (issue #54)
// ---------------------------------------------------------------------------

#[test]
fn media_extras_defaults_persist_and_round_trip_on_confirm() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, _) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    core.set_library_root(root.clone()).unwrap();

    let defaults = MediaExtrasOptions {
        write_subs: true,
        write_thumbnail: true,
        ..Default::default()
    };
    core.set_default_media_extras(defaults).unwrap();

    let mut fake = fake_toolchain();
    fake.insert(
        "https://www.youtube.com/watch?v=media-extras",
        "Extras Video",
        "https://cdn.example.com/video.webm",
        vec![("720p", "720p")],
    );
    let (mut core, _rx) = Core::new(Box::new(NoRangeFakeHttp(fake_http())), Box::new(fake));
    core.set_library_root(root.clone()).unwrap();
    assert_eq!(core.default_media_extras(), defaults);

    core.probe("https://www.youtube.com/watch?v=media-extras")
        .unwrap();

    let override_opts = MediaExtrasOptions {
        write_subs: false,
        write_thumbnail: true,
        embed_thumbnail: true,
        ..Default::default()
    };
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=media-extras".into(),
        items: vec![ItemSelection {
            item_id: "item-1".into(),
            quality_id: "720p".into(),
        }],
        media_extras: Some(override_opts),
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    assert_eq!(job_ids.len(), 1);

    let stored = core.job_media_extras(&job_ids[0]).unwrap();
    assert!(!stored.write_subs, "override should disable write_subs");
    assert!(stored.write_thumbnail);
    if download_core::ffmpeg_available(None) {
        assert!(stored.embed_thumbnail, "embed kept when ffmpeg is on PATH");
    } else {
        assert!(
            !stored.embed_thumbnail,
            "embed stripped when ffmpeg unavailable"
        );
    }
}

#[test]
fn media_extras_capabilities_report_ffmpeg_unavailable_by_default() {
    if download_core::ffmpeg_available(None) {
        eprintln!("skipping: ffmpeg is on PATH in this environment");
        return;
    }
    let (core, _) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    let caps = core.media_extras_capabilities();
    assert!(!caps.ffmpeg_available);
    assert!(caps.embed_disabled_reason.is_some());
}

#[cfg(unix)]
#[test]
fn remux_runs_ffmpeg_and_completes_with_the_remuxed_file() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let ffmpeg_dir = tmp.path().join("ffmpeg-bin");
    std::fs::create_dir_all(&ffmpeg_dir).unwrap();
    let ffmpeg = ffmpeg_dir.join("ffmpeg");
    std::fs::write(
        &ffmpeg,
        "#!/bin/sh\nfor a; do last=$a; done\necho remuxed > \"$last\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut fake = fake_toolchain();
    fake.insert(
        "https://www.youtube.com/watch?v=remux-run",
        "Remux Run",
        "https://cdn.example.com/clip.webm",
        vec![("best", "best")],
    );
    fake.set_ffmpeg_dir(ffmpeg_dir);
    let (mut core, rx) = Core::new(Box::new(NoRangeFakeHttp(fake_http())), Box::new(fake));
    core.set_library_root(tmp.path().join("library")).unwrap();
    core.set_default_media_extras(MediaExtrasOptions {
        remux: true,
        ..Default::default()
    })
    .unwrap();
    core.probe("https://www.youtube.com/watch?v=remux-run")
        .unwrap();
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=remux-run".into(),
        items: vec![ItemSelection {
            item_id: "item-1".into(),
            quality_id: "best".into(),
        }],
        media_extras: None,
    };
    let job_id = core.confirm_probe_selection(&selection, None).unwrap()[0].clone();
    assert!(core.job_media_extras(&job_id).unwrap().remux);

    core.process_pending().unwrap();
    let events = collect_events(&rx);
    assert!(
        events.iter().any(|e| matches!(
            e,
            CoreEvent::JobCompleted { job_id: id, .. } if id == &job_id
        )),
        "{events:?}"
    );
    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .unwrap();
    assert!(PathBuf::from(&snap.destination)
        .join("clip.remux.mp4")
        .is_file());
}

#[test]
fn remux_stripped_when_ffmpeg_unavailable_and_job_completes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");

    let mut fake = fake_toolchain();
    fake.insert(
        "https://www.youtube.com/watch?v=remux-skip",
        "Remux Skip",
        "https://cdn.example.com/clip.webm",
        vec![("best", "best")],
    );
    fake.set_ffmpeg_dir(tmp.path().join("no-ffmpeg"));
    let (mut core, rx) = Core::new(Box::new(NoRangeFakeHttp(fake_http())), Box::new(fake));
    core.set_library_root(root).unwrap();
    core.set_default_media_extras(MediaExtrasOptions {
        remux: true,
        ..Default::default()
    })
    .unwrap();

    core.probe("https://www.youtube.com/watch?v=remux-skip")
        .unwrap();
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=remux-skip".into(),
        items: vec![ItemSelection {
            item_id: "item-1".into(),
            quality_id: "best".into(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    let job_id = job_ids[0].clone();
    assert!(
        !core.job_media_extras(&job_id).unwrap().remux,
        "remux stripped when ffmpeg unavailable"
    );

    core.process_pending().unwrap();
    let events = collect_events(&rx);
    assert!(
        events.iter().any(|e| matches!(
            e,
            CoreEvent::JobCompleted { job_id: id, .. } if id == &job_id
        )),
        "job should complete without remux when ffmpeg missing"
    );

    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .unwrap();
    let dest = PathBuf::from(&snap.destination);
    let webm = dest.join("clip.webm");
    assert!(
        webm.is_file(),
        "download output retained at {}",
        webm.display()
    );
}

#[test]
fn default_media_extras_settings_persist_across_library_root_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, _) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    core.set_library_root(root.clone()).unwrap();
    let opts = MediaExtrasOptions {
        write_subs: true,
        write_metadata: true,
        ..Default::default()
    };
    core.set_default_media_extras(opts).unwrap();

    let (mut core2, _) = make_core(Box::new(NoRangeFakeHttp(fake_http())));
    core2.set_library_root(root).unwrap();
    assert_eq!(core2.default_media_extras(), opts);
}

// ---------------------------------------------------------------------------
// Download Archive (issue #55)
// ---------------------------------------------------------------------------

fn archive_path_for(root: &std::path::Path) -> std::path::PathBuf {
    root.join(".phlegethon").join("archive.json")
}

fn complete_probe_job(
    core: &mut Core,
    rx: &crossbeam_channel::Receiver<CoreEvent>,
    probe_url: &str,
    item_id: &str,
    quality_id: &str,
) -> String {
    core.probe(probe_url).unwrap();
    collect_events(rx);

    let selection = ProbeSelection {
        url: probe_url.to_string(),
        items: vec![ItemSelection {
            item_id: item_id.to_string(),
            quality_id: quality_id.to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    assert_eq!(job_ids.len(), 1);
    core.process_pending().unwrap();
    let events = collect_events(rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "job should complete, got: {events:?}"
    );
    job_ids[0].clone()
}

#[test]
fn completed_media_job_records_identity_in_download_archive() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=archive1",
        "Archived Video",
        "https://cdn.youtube.com/archive1.mp4",
        vec![("1080p", "1080p")],
    );
    let http = fake_http_with_responses(vec![(
        "https://cdn.youtube.com/archive1.mp4",
        b"archive video content",
    )]);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    core.set_library_root(root.clone()).unwrap();

    let job_id = complete_probe_job(
        &mut core,
        &rx,
        "https://www.youtube.com/watch?v=archive1",
        "item-1",
        "1080p",
    );

    let raw = std::fs::read_to_string(archive_path_for(&root)).unwrap();
    assert!(
        raw.contains("item-1"),
        "archive should contain media id, got: {raw}"
    );
    assert!(raw.contains("Archived Video"));
    assert!(raw.contains(&job_id));
}

#[test]
fn probe_marks_archived_items() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert_batch(
        "https://youtube.com/playlist?list=ARCH",
        "Archive Playlist",
        vec![
            (
                "vid-done",
                "Done Video",
                "https://youtube.com/watch?v=done",
                vec![("1080p", "1080p")],
            ),
            (
                "vid-new",
                "New Video",
                "https://youtube.com/watch?v=new",
                vec![("720p", "720p")],
            ),
        ],
    );
    let http = fake_http_with_responses(vec![(
        "https://youtube.com/watch?v=done",
        b"done video content",
    )]);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    core.set_library_root(root).unwrap();

    complete_probe_job(
        &mut core,
        &rx,
        "https://youtube.com/playlist?list=ARCH",
        "vid-done",
        "1080p",
    );

    let result = core
        .probe("https://youtube.com/playlist?list=ARCH")
        .unwrap();
    let done = result
        .items
        .iter()
        .find(|item| item.id == "vid-done")
        .expect("vid-done in probe");
    let fresh = result
        .items
        .iter()
        .find(|item| item.id == "vid-new")
        .expect("vid-new in probe");

    assert!(done.archived, "completed item should be marked archived");
    assert!(!fresh.archived, "new item should not be archived");
}

#[test]
fn confirm_skips_archived_items_when_shell_omits_them() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert_batch(
        "https://youtube.com/playlist?list=SKIP",
        "Skip Playlist",
        vec![
            (
                "vid-archived",
                "Archived",
                "https://youtube.com/watch?v=archived",
                vec![("1080p", "1080p")],
            ),
            (
                "vid-fresh",
                "Fresh",
                "https://youtube.com/watch?v=fresh",
                vec![("720p", "720p")],
            ),
        ],
    );
    let http = fake_http_with_responses(vec![
        ("https://youtube.com/watch?v=archived", b"archived content"),
        ("https://youtube.com/watch?v=fresh", b"fresh content"),
    ]);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    core.set_library_root(root).unwrap();

    complete_probe_job(
        &mut core,
        &rx,
        "https://youtube.com/playlist?list=SKIP",
        "vid-archived",
        "1080p",
    );

    core.probe("https://youtube.com/playlist?list=SKIP")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://youtube.com/playlist?list=SKIP".to_string(),
        items: vec![ItemSelection {
            item_id: "vid-fresh".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    assert_eq!(
        job_ids.len(),
        1,
        "only non-archived selection should enqueue"
    );
}

#[test]
fn confirm_reselect_archived_item_creates_job() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=reselect",
        "Reselect Video",
        "https://cdn.youtube.com/reselect.mp4",
        vec![("1080p", "1080p")],
    );
    let http = fake_http_with_responses(vec![(
        "https://cdn.youtube.com/reselect.mp4",
        b"reselect video content",
    )]);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    core.set_library_root(root).unwrap();

    complete_probe_job(
        &mut core,
        &rx,
        "https://www.youtube.com/watch?v=reselect",
        "item-1",
        "1080p",
    );

    core.probe("https://www.youtube.com/watch?v=reselect")
        .unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=reselect".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "1080p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    assert_eq!(
        job_ids.len(),
        1,
        "explicit re-select of archived item should create a job"
    );
}

#[test]
fn list_history_returns_completed_entries_newest_first() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert_batch(
        "https://youtube.com/playlist?list=hist",
        "History Playlist",
        vec![
            (
                "hist-first",
                "First Video",
                "https://cdn.youtube.com/hist1.mp4",
                vec![("1080p", "1080p")],
            ),
            (
                "hist-second",
                "Second Video",
                "https://cdn.youtube.com/hist2.mp4",
                vec![("1080p", "1080p")],
            ),
        ],
    );
    let http = fake_http_with_responses(vec![
        ("https://cdn.youtube.com/hist1.mp4", b"first"),
        ("https://cdn.youtube.com/hist2.mp4", b"second"),
    ]);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    core.set_library_root(root).unwrap();

    core.probe("https://youtube.com/playlist?list=hist")
        .unwrap();
    collect_events(&rx);
    let selection = ProbeSelection {
        url: "https://youtube.com/playlist?list=hist".to_string(),
        items: vec![
            ItemSelection {
                item_id: "hist-first".to_string(),
                quality_id: "1080p".to_string(),
            },
            ItemSelection {
                item_id: "hist-second".to_string(),
                quality_id: "1080p".to_string(),
            },
        ],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    assert_eq!(job_ids.len(), 2);
    core.process_pending().unwrap();
    let events = collect_events(&rx);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, CoreEvent::JobCompleted { .. }))
            .count(),
        2
    );

    let history = core.list_history();
    assert_eq!(history.len(), 2);
    let ids: std::collections::HashSet<_> = history.iter().map(|e| e.media_id.as_str()).collect();
    assert_eq!(
        ids,
        ["hist-first", "hist-second"]
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
    );
    assert!(history
        .iter()
        .all(|e| e.outcome == ArchiveOutcome::Completed));
}

#[test]
fn list_history_by_outcome_filters_completed_and_failed() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert_batch(
        "https://www.youtube.com/watch?v=ok",
        "OK",
        vec![(
            "item-ok",
            "OK Video",
            "https://cdn.youtube.com/ok.mp4",
            vec![("1080p", "1080p")],
        )],
    );
    toolchain.insert_batch(
        "https://www.youtube.com/watch?v=bad",
        "Bad",
        vec![(
            "item-bad",
            "Bad Video",
            "https://cdn.youtube.com/bad.mp4",
            vec![("1080p", "1080p")],
        )],
    );
    let http = fake_http_with_responses(vec![
        ("https://cdn.youtube.com/ok.mp4", b"ok"),
        (
            "https://cdn.youtube.com/bad.mp4",
            b"bad content for hash test",
        ),
    ]);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    core.set_library_root(root).unwrap();

    complete_probe_job(
        &mut core,
        &rx,
        "https://www.youtube.com/watch?v=ok",
        "item-ok",
        "1080p",
    );

    core.probe("https://www.youtube.com/watch?v=bad").unwrap();
    collect_events(&rx);
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=bad".to_string(),
        items: vec![ItemSelection {
            item_id: "item-bad".to_string(),
            quality_id: "1080p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    assert_eq!(job_ids.len(), 1);
    core.set_job_expected_hash(&job_ids[0], "sha-256", "0".repeat(64).as_str())
        .unwrap();
    core.process_pending().unwrap();
    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobFailed { .. })),
        "integrity mismatch should fail job, got: {events:?}"
    );

    let completed = core.list_history_by_outcome(ArchiveOutcome::Completed);
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].media_id, "item-ok");

    let failed = core.list_history_by_outcome(ArchiveOutcome::Failed);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].media_id, "item-bad");
}

#[test]
fn probe_does_not_mark_failed_archive_entries_as_archived() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert_batch(
        "https://www.youtube.com/watch?v=bad",
        "Bad",
        vec![(
            "item-bad",
            "Bad Video",
            "https://cdn.youtube.com/bad.mp4",
            vec![("1080p", "1080p")],
        )],
    );
    let http = fake_http_with_responses(vec![(
        "https://cdn.youtube.com/bad.mp4",
        b"bad content for hash test",
    )]);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    core.set_library_root(root).unwrap();

    core.probe("https://www.youtube.com/watch?v=bad").unwrap();
    collect_events(&rx);
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=bad".to_string(),
        items: vec![ItemSelection {
            item_id: "item-bad".to_string(),
            quality_id: "1080p".to_string(),
        }],
        media_extras: None,
    };
    let job_ids = core.confirm_probe_selection(&selection, None).unwrap();
    core.set_job_expected_hash(&job_ids[0], "sha-256", "0".repeat(64).as_str())
        .unwrap();
    core.process_pending().unwrap();
    collect_events(&rx);

    let result = core.probe("https://www.youtube.com/watch?v=bad").unwrap();
    let item = result
        .items
        .iter()
        .find(|i| i.id == "item-bad")
        .expect("item in probe");
    assert!(
        !item.archived,
        "failed archive entry must not skip item on probe sheet"
    );
}

#[test]
fn remove_history_entry_removes_archive_only_and_keeps_files() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert_batch(
        "https://www.youtube.com/watch?v=remove1",
        "Remove",
        vec![(
            "item-remove",
            "Remove Me",
            "https://cdn.youtube.com/remove1.mp4",
            vec![("1080p", "1080p")],
        )],
    );
    let http = fake_http_with_responses(vec![(
        "https://cdn.youtube.com/remove1.mp4",
        b"keep this file",
    )]);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    core.set_library_root(root.clone()).unwrap();

    let _job_id = complete_probe_job(
        &mut core,
        &rx,
        "https://www.youtube.com/watch?v=remove1",
        "item-remove",
        "1080p",
    );

    let entry = core
        .list_history()
        .into_iter()
        .find(|e| e.media_id == "item-remove")
        .expect("history entry");
    let output_path = entry.destination.clone();
    assert!(
        output_path.exists(),
        "completed output should exist on disk"
    );

    let removed = core.remove_history_entry("item-remove").expect("removed");
    assert_eq!(removed.media_id, "item-remove");
    assert!(core.list_history().is_empty());
    assert!(
        output_path.exists(),
        "remove-from-history must not delete files"
    );
    assert!(
        std::fs::metadata(&output_path).unwrap().len() > 0,
        "output file should remain on disk with content"
    );
}

#[test]
fn cookie_channel_defaults_empty() {
    let (core, _rx) = make_core(Box::new(fake_http()));
    assert!(!core.cookie_channel().is_configured());
}

#[test]
fn cookie_channel_persists_across_library_root_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(root.clone()).unwrap();

    let channel = CookieChannel {
        cookies_from_browser: Some("firefox".to_string()),
        cookie_file: Some(root.join("cookies.txt")),
    };
    core.set_cookie_channel(channel.clone()).unwrap();

    let (mut core2, _rx2) = make_core(Box::new(fake_http()));
    core2.set_library_root(root).unwrap();
    assert_eq!(core2.cookie_channel(), channel);
}

#[test]
fn cookie_channel_rejects_relative_cookie_file() {
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    let tmp = tempfile::tempdir().unwrap();
    core.set_library_root(tmp.path().join("library")).unwrap();

    let err = core
        .set_cookie_channel(CookieChannel {
            cookies_from_browser: None,
            cookie_file: Some(PathBuf::from("cookies.txt")),
        })
        .unwrap_err();
    assert!(err.contains("absolute path"), "got: {err}");
}

#[test]
fn proxy_settings_defaults_empty() {
    let (core, _rx) = make_core(Box::new(fake_http()));
    assert!(!core.proxy_settings().is_configured());
}

#[test]
fn proxy_settings_persist_across_library_root_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(root.clone()).unwrap();

    let settings = ProxySettings {
        url: Some("socks5://127.0.0.1:1080".to_string()),
    };
    core.set_proxy_settings(settings.clone()).unwrap();

    let (mut core2, _rx2) = make_core(Box::new(fake_http()));
    core2.set_library_root(root).unwrap();
    assert_eq!(core2.proxy_settings(), settings);
}

#[test]
fn proxy_settings_clearing_restores_direct_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, _rx) = make_core(Box::new(ProdHttpBackend::new()));
    core.set_library_root(root).unwrap();

    core.set_proxy_settings(ProxySettings {
        url: Some("http://127.0.0.1:8080".to_string()),
    })
    .unwrap();
    core.set_proxy_settings(ProxySettings::default()).unwrap();
    assert!(!core.proxy_settings().is_configured());
}

#[test]
fn proxy_settings_rejects_invalid_scheme() {
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    let tmp = tempfile::tempdir().unwrap();
    core.set_library_root(tmp.path().join("library")).unwrap();

    let err = core
        .set_proxy_settings(ProxySettings {
            url: Some("ftp://proxy.example.com".to_string()),
        })
        .unwrap_err();
    assert!(err.contains("scheme"), "got: {err}");
}

// ---------------------------------------------------------------------------
// Sidecar update manager (#63)
// ---------------------------------------------------------------------------

fn write_toolchain_fixture(sidecar: &std::path::Path, version: &str, bytes: &[u8]) {
    let manifest = ToolchainManifest::stub(version);
    write_toolchain_manifest(sidecar, &manifest).unwrap();
    std::fs::write(toolchain_binary_path(sidecar, &manifest), bytes).unwrap();
}

fn toolchain_catalog(tmp: &tempfile::TempDir, version: &str, bytes: &[u8]) -> UpdateCatalog {
    let binary = tmp.path().join(format!("yt-dlp-{version}"));
    std::fs::write(&binary, bytes).unwrap();
    UpdateCatalog {
        toolchain_latest: Some(version.to_string()),
        toolchain_versions: std::collections::HashMap::from([(
            version.to_string(),
            CatalogVersionEntry::local(binary),
        )]),
        ..Default::default()
    }
}

fn setup_sidecar_core(
    tmp: &tempfile::TempDir,
    catalog: UpdateCatalog,
) -> (
    Core,
    crossbeam_channel::Receiver<CoreEvent>,
    PathBuf,
    Arc<dyn SidecarUpdateSource>,
) {
    let (mut core, rx) = make_core(Box::new(fake_http()));
    let sidecar = tmp.path().join("sidecar");
    let library = tmp.path().join("library");
    let source: Arc<dyn SidecarUpdateSource> = FakeSidecarUpdateSource::new(catalog);
    core.set_sidecar_update_source(Arc::clone(&source));
    core.set_sidecar_root(sidecar.clone()).unwrap();
    core.set_library_root(library).unwrap();
    (core, rx, sidecar, source)
}

#[test]
fn sidecar_update_settings_default_check_on_launch_off() {
    let (core, _rx) = make_core(Box::new(fake_http()));
    assert!(!core.sidecar_update_settings().check_on_launch);
}

#[test]
fn sidecar_update_settings_persist_across_library_root_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(root.clone()).unwrap();

    let settings = SidecarUpdateSettings {
        check_on_launch: true,
        toolchain_pin: Some("1.0.0".to_string()),
        plugin_pin: None,
    };
    core.set_sidecar_update_settings(settings.clone()).unwrap();

    let (mut core2, _rx2) = make_core(Box::new(fake_http()));
    core2.set_library_root(root).unwrap();
    assert_eq!(core2.sidecar_update_settings(), settings);
}

#[test]
fn check_sidecar_updates_reports_installed_and_available() {
    let tmp = tempfile::tempdir().unwrap();
    let catalog = toolchain_catalog(&tmp, "2.0.0", b"v2");
    let (core, _rx, sidecar, _source) = setup_sidecar_core(&tmp, catalog);
    write_toolchain_fixture(&sidecar, "1.0.0", b"v1");

    let result = core.check_sidecar_updates().unwrap();
    assert!(result.toolchain.update_available);
    assert_eq!(result.toolchain.installed_version.as_deref(), Some("1.0.0"));
    assert_eq!(result.toolchain.available_version.as_deref(), Some("2.0.0"));
}

#[test]
fn apply_sidecar_updates_when_idle_installs_new_version() {
    let tmp = tempfile::tempdir().unwrap();
    let catalog = toolchain_catalog(&tmp, "2.0.0", b"v2");
    let (mut core, _rx, sidecar, _source) = setup_sidecar_core(&tmp, catalog);
    write_toolchain_fixture(&sidecar, "1.0.0", b"v1");

    let result = core.apply_sidecar_updates().unwrap();
    assert_eq!(result.outcome, SidecarApplyOutcome::Applied);
    assert_eq!(result.applied, vec![SidecarComponent::Toolchain]);

    let installed = core.check_sidecar_updates().unwrap();
    assert_eq!(
        installed.toolchain.installed_version.as_deref(),
        Some("2.0.0")
    );
    assert!(!installed.toolchain.update_available);
}

#[test]
fn apply_sidecar_updates_blocked_mid_transfer() {
    let tmp = tempfile::tempdir().unwrap();
    let catalog = toolchain_catalog(&tmp, "2.0.0", b"v2");
    let (mut core, _rx, sidecar, _source) = setup_sidecar_core(&tmp, catalog);
    write_toolchain_fixture(&sidecar, "1.0.0", b"v1");

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.job_signals()
        .set_transfer_active_for_test(&job_id, true);

    let result = core.apply_sidecar_updates().unwrap();
    assert_eq!(result.outcome, SidecarApplyOutcome::BlockedMidTransfer);
    assert!(result.applied.is_empty());

    let installed = core.check_sidecar_updates().unwrap();
    assert_eq!(
        installed.toolchain.installed_version.as_deref(),
        Some("1.0.0"),
        "mid-transfer apply must not swap sidecar"
    );
}

#[test]
fn pin_prevents_update_available_when_installed_matches_pin() {
    let tmp = tempfile::tempdir().unwrap();
    let catalog = toolchain_catalog(&tmp, "2.0.0", b"v2");
    let (mut core, _rx, sidecar, _source) = setup_sidecar_core(&tmp, catalog);
    write_toolchain_fixture(&sidecar, "1.0.0", b"v1");
    core.pin_sidecar_version(SidecarComponent::Toolchain, "1.0.0".to_string())
        .unwrap();

    let result = core.check_sidecar_updates().unwrap();
    assert!(!result.toolchain.update_available);
}

#[test]
fn rollback_restores_previous_toolchain_version_when_idle() {
    let tmp = tempfile::tempdir().unwrap();
    let catalog = toolchain_catalog(&tmp, "2.0.0", b"v2");
    let (mut core, _rx, sidecar, _source) = setup_sidecar_core(&tmp, catalog);
    write_toolchain_fixture(&sidecar, "1.0.0", b"v1");

    core.apply_sidecar_updates().unwrap();
    core.rollback_sidecar(SidecarComponent::Toolchain).unwrap();

    let installed = core.check_sidecar_updates().unwrap();
    assert_eq!(
        installed.toolchain.installed_version.as_deref(),
        Some("1.0.0")
    );
}

#[test]
fn rollback_rebinds_extractor_via_apply_sidecar_root() {
    let tmp = tempfile::tempdir().unwrap();
    let catalog = toolchain_catalog(&tmp, "2.0.0", b"v2");
    let fake = FakeExtractorToolchain::new();
    let applies = fake.sidecar_root_applies_handle();
    let (mut core, _rx) = Core::new(Box::new(fake_http()), Box::new(fake));
    let sidecar = tmp.path().join("sidecar");
    let library = tmp.path().join("library");
    let source: Arc<dyn SidecarUpdateSource> = FakeSidecarUpdateSource::new(catalog);
    core.set_sidecar_update_source(Arc::clone(&source));
    core.set_sidecar_root(sidecar.clone()).unwrap();
    core.set_library_root(library).unwrap();
    write_toolchain_fixture(&sidecar, "1.0.0", b"v1");

    core.apply_sidecar_updates().unwrap();
    let before_rollback = *applies.lock().unwrap();
    core.rollback_sidecar(SidecarComponent::Toolchain).unwrap();
    let after_rollback = *applies.lock().unwrap();

    assert!(
        after_rollback > before_rollback,
        "rollback must call apply_sidecar_root (before={before_rollback}, after={after_rollback})"
    );
    assert_eq!(
        core.check_sidecar_updates()
            .unwrap()
            .toolchain
            .installed_version
            .as_deref(),
        Some("1.0.0")
    );
}

#[test]
fn deferred_updates_apply_on_launch_when_idle() {
    let tmp = tempfile::tempdir().unwrap();
    let catalog = toolchain_catalog(&tmp, "2.0.0", b"v2");
    let (mut core, _rx, sidecar, _source) = setup_sidecar_core(&tmp, catalog);
    write_toolchain_fixture(&sidecar, "1.0.0", b"v1");

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.job_signals()
        .set_transfer_active_for_test(&job_id, true);
    core.apply_sidecar_updates().unwrap();

    core.job_signals()
        .set_transfer_active_for_test(&job_id, false);
    let result = core.apply_sidecar_updates_on_launch().unwrap();
    assert_eq!(result.outcome, SidecarApplyOutcome::Applied);
    assert_eq!(
        core.check_sidecar_updates()
            .unwrap()
            .toolchain
            .installed_version
            .as_deref(),
        Some("2.0.0")
    );
}

#[test]
fn check_on_launch_applies_updates_when_idle() {
    let tmp = tempfile::tempdir().unwrap();
    let catalog = toolchain_catalog(&tmp, "2.0.0", b"v2");
    let (mut core, _rx, sidecar, _source) = setup_sidecar_core(&tmp, catalog);
    write_toolchain_fixture(&sidecar, "1.0.0", b"v1");
    core.set_sidecar_update_settings(SidecarUpdateSettings {
        check_on_launch: true,
        ..Default::default()
    })
    .unwrap();

    let result = core.apply_sidecar_updates_on_launch().unwrap();
    assert_eq!(result.outcome, SidecarApplyOutcome::Applied);
}

// ---------------------------------------------------------------------------
// URL input: normalization, multi-paste, duplicate detection (#57)
// ---------------------------------------------------------------------------

#[test]
fn normalize_url_strips_fragment_and_trailing_slash_on_enqueue() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let result = core
        .enqueue("https://example.com/file.zip/#skip", None)
        .unwrap();
    let job_id = match result {
        EnqueueOutcome::EnqueueDirect { job_id, .. } => job_id,
        _ => panic!("expected enqueue_direct"),
    };

    let snapshot = core
        .list_job_snapshots()
        .into_iter()
        .find(|s| s.job_id == job_id)
        .unwrap();
    assert_eq!(snapshot.url, "https://example.com/file.zip");
}

#[test]
fn split_urls_splits_multiline_paste() {
    use download_core::split_urls;
    let text = "https://a.example/one\nhttps://b.example/two";
    assert_eq!(
        split_urls(text),
        vec![
            "https://a.example/one".to_string(),
            "https://b.example/two".to_string(),
        ]
    );
}

#[test]
fn find_active_job_for_url_matches_pending_job() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    let matched = core
        .find_active_job_for_url("https://example.com/file.zip/#frag")
        .unwrap();
    assert_eq!(matched.job_id, job_id);
    assert_eq!(matched.status, "pending");
}

#[test]
fn find_active_job_for_url_ignores_completed_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.process_pending().unwrap();

    assert!(core
        .find_active_job_for_url("https://example.com/file.zip")
        .is_none());
}

// ---------------------------------------------------------------------------
// Filename Template (issue #58)
// ---------------------------------------------------------------------------

#[test]
fn filename_template_setting_persists_in_queue_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(root.clone()).unwrap();

    let custom = "%(uploader)s - %(title)s.%(ext)s".to_string();
    core.set_filename_template(custom.clone()).unwrap();
    assert_eq!(core.filename_template(), custom.as_str());

    let (mut core2, _rx2) = make_core(Box::new(fake_http()));
    core2.set_library_root(root).unwrap();
    assert_eq!(core2.filename_template(), custom.as_str());
}

#[test]
fn extractor_confirm_applies_filename_template() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=abc123",
        "My Video",
        "https://cdn.youtube.com/video.mp4",
        vec![("720p", "720p")],
    );

    let http = fake_http_with_responses(vec![(
        "https://cdn.youtube.com/video.mp4",
        b"video".as_slice(),
    )]);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, _rx) = Core::new(Box::new(http), Box::new(toolchain));
    core.set_library_root(root.clone()).unwrap();
    core.set_filename_template("clip-%(id)s.%(ext)s".to_string())
        .unwrap();

    core.probe("https://www.youtube.com/watch?v=abc123")
        .unwrap();
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=abc123".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    core.confirm_probe_selection(&selection, None).unwrap();

    let queue_raw = std::fs::read_to_string(root.join(".phlegethon").join("jobs.json")).unwrap();
    assert!(
        queue_raw.contains("clip-1.mp4"),
        "expected templated filename in persisted job, got: {queue_raw}"
    );
}

#[test]
fn http_enqueue_keeps_url_derived_filename() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();
    core.set_filename_template("ignored-%(title)s.%(ext)s".to_string())
        .unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/archive.zip");
    let job = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .unwrap();
    assert_eq!(job.title, "archive.zip");
}

#[test]
fn filename_template_collision_suffix_in_core_module() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("clip.mp4"), b"done").unwrap();

    let vars = TemplateVars {
        title: "Clip",
        id: "abc",
        uploader: None,
        upload_date: None,
        ext: "mp4",
    };
    let (ytdlp, basename) = prepare_extractor_output(&dest, "%(title)s.%(ext)s", &vars);
    assert_eq!(ytdlp, basename);
    assert_eq!(basename, "Clip (1).mp4");
}

#[test]
fn dispatch_pending_emits_job_failed_on_transfer_thread_error() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, rx) = make_core(Box::new(FailingHttp));
    let core = Arc::new(Mutex::new(core));
    core.lock()
        .unwrap()
        .set_library_root(tmp.path().join("library"))
        .unwrap();

    let job_id = {
        let mut guard = core.lock().unwrap();
        enqueue_direct_job_id(&mut guard, "https://example.com/fail.zip")
    };

    Core::dispatch_pending(Arc::clone(&core)).unwrap();

    let mut failed = false;
    for _ in 0..50 {
        for ev in collect_events(&rx) {
            if let CoreEvent::JobFailed { job_id: id, .. } = ev {
                if id == job_id {
                    failed = true;
                }
            }
        }
        if failed {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        failed,
        "dispatch_pending transfer thread failure should emit JobFailed"
    );

    let guard = core.lock().unwrap();
    let snap = guard
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .expect("job should remain visible");
    assert_eq!(snap.status, "failed");
}

#[test]
fn clear_pending_probe_allows_re_probe_after_cancel() {
    let tmp = tempfile::tempdir().unwrap();
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=cancel-probe",
        "Cancel Probe Video",
        "https://cdn.youtube.com/cancel-probe.mp4",
        vec![("720p", "720p")],
    );
    let (mut core, _rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let url = "https://www.youtube.com/watch?v=cancel-probe";
    core.probe(url).unwrap();
    assert!(core.find_active_job_for_url(url).is_some());

    core.clear_pending_probe(url).unwrap();
    assert!(core.find_active_job_for_url(url).is_none());
}

// ---------------------------------------------------------------------------
// Transfers run outside the Core mutex (extractor + segmented)
// ---------------------------------------------------------------------------

/// Range-capable fake that streams slowly so a transfer can be paused mid-way.
#[derive(Clone)]
struct SlowRangeFakeHttp {
    responses: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    delay_per_byte: std::time::Duration,
}

impl HttpBackend for SlowRangeFakeHttp {
    fn fetch(
        &self,
        url: &str,
        range: Option<std::ops::Range<u64>>,
        sink: &mut dyn Write,
    ) -> Result<FetchResult, String> {
        let full_body = {
            let responses = self.responses.lock().unwrap();
            responses
                .get(url)
                .cloned()
                .ok_or_else(|| format!("no fake response for {url}"))?
        };
        let total = full_body.len() as u64;
        let data = match range {
            Some(r) => {
                let start = r.start.min(total);
                let end = r.end.min(total).max(start);
                &full_body[start as usize..end as usize]
            }
            None => full_body.as_slice(),
        };
        let mut written = 0u64;
        for byte in data {
            let n = sink
                .write(std::slice::from_ref(byte))
                .map_err(|e| format!("sink write: {e}"))?;
            if n == 0 {
                break;
            }
            written += n as u64;
            std::thread::sleep(self.delay_per_byte);
        }
        Ok(FetchResult {
            total_length: total,
            bytes_written: written,
        })
    }

    fn supports_range(&self, _url: &str) -> Result<bool, String> {
        Ok(true)
    }

    fn file_size(&self, url: &str) -> Result<u64, String> {
        let responses = self.responses.lock().unwrap();
        responses
            .get(url)
            .map(|b| b.len() as u64)
            .ok_or_else(|| format!("no fake response for {url}"))
    }
}

fn wait_for_status(
    core: &Arc<Mutex<Core>>,
    job_id: &str,
    status: &str,
    timeout: std::time::Duration,
) {
    let started = std::time::Instant::now();
    loop {
        let snap = core
            .lock()
            .unwrap()
            .list_job_snapshots()
            .into_iter()
            .find(|j| j.job_id == job_id);
        match snap {
            Some(s) if s.status == status => return,
            Some(s) => {
                assert!(
                    started.elapsed() < timeout,
                    "job {job_id} did not reach status {status:?} (now {:?})",
                    s.status
                );
            }
            None => {
                assert!(started.elapsed() < timeout, "job {job_id} vanished");
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn extractor_download_does_not_hold_core_mutex() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=unlocked-ext",
        "Unlocked Extractor Video",
        "https://cdn.youtube.com/unlocked-ext.mp4",
        vec![("720p", "720p")],
    );
    let chunks_written = toolchain.enable_slow_download(40, std::time::Duration::from_millis(20));

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();
    core.probe("https://www.youtube.com/watch?v=unlocked-ext")
        .unwrap();
    collect_events(&rx);
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=unlocked-ext".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    let job_id = core.confirm_probe_selection(&selection, None).unwrap()[0].clone();

    let core = Arc::new(Mutex::new(core));
    Core::dispatch_pending(core.clone()).unwrap();

    let started = std::time::Instant::now();
    while *chunks_written.lock().unwrap() == 0 {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "extractor download never became active"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    // While yt-dlp (the fake) is mid-download, queue reads must not block.
    let t0 = std::time::Instant::now();
    let snapshots = core.lock().unwrap().list_job_snapshots();
    let waited = t0.elapsed();
    assert!(
        waited < std::time::Duration::from_millis(300),
        "list_job_snapshots blocked for {waited:?} during an extractor download"
    );
    assert_eq!(snapshots.len(), 1);
    assert_eq!(
        snapshots[0].status, "downloading",
        "active transfers report downloading"
    );
    assert!(
        *chunks_written.lock().unwrap() < 40,
        "snapshot should have been taken mid-download"
    );

    wait_for_status(
        &core,
        &job_id,
        "completed",
        std::time::Duration::from_secs(10),
    );
    let events = collect_events(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { job_id: id, .. } if id == &job_id)),
        "expected JobCompleted, got {events:?}"
    );
}

#[test]
fn segmented_pause_then_resume_produces_intact_file() {
    let content: Vec<u8> = (0..400u32).map(|i| (i % 251) as u8).collect();
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/seg-pause.bin".to_string(),
        content.clone(),
    );
    let http = SlowRangeFakeHttp {
        responses: Arc::new(Mutex::new(map)),
        delay_per_byte: std::time::Duration::from_millis(2),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();
    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/seg-pause.bin");
    let signals = core.job_signals();

    let core = Arc::new(Mutex::new(core));
    Core::dispatch_pending(core.clone()).unwrap();

    // Let the four segments write a little, then pause.
    std::thread::sleep(std::time::Duration::from_millis(120));
    signals
        .request_pause(&job_id)
        .expect("pause flag registered")
        .store(true, std::sync::atomic::Ordering::Release);
    wait_for_status(&core, &job_id, "paused", std::time::Duration::from_secs(5));

    let job_dir = tmp.path().join("library").join("jobs").join(&job_id);
    let part_path = job_dir.join("seg-pause.bin.part");
    assert!(
        !part_path.exists(),
        "interrupted segmented partial has holes and must be dropped, not resumed"
    );
    let paused_snap = core
        .lock()
        .unwrap()
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .unwrap();
    assert_eq!(
        paused_snap.bytes_downloaded, 0,
        "no resumable bytes after segmented pause"
    );
    collect_events(&rx);

    // Resume: the transfer restarts and must produce byte-identical output.
    {
        let mut guard = core.lock().unwrap();
        guard.resume_job(&job_id).unwrap();
    }
    Core::dispatch_pending(core.clone()).unwrap();
    wait_for_status(
        &core,
        &job_id,
        "completed",
        std::time::Duration::from_secs(20),
    );

    let final_path = job_dir.join("seg-pause.bin");
    let on_disk = std::fs::read(&final_path).expect("final file written");
    assert_eq!(
        on_disk, content,
        "resumed segmented download must be intact"
    );
}

// ---------------------------------------------------------------------------
// Tiered extraction for non-Launch-Site URLs (yt-dlp → generic → page sniff → file)
// ---------------------------------------------------------------------------

fn fake_http_map(entries: &[(&str, &[u8])]) -> FakeHttp {
    let mut map = std::collections::HashMap::new();
    for (url, body) in entries {
        map.insert((*url).to_string(), body.to_vec());
    }
    FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    }
}

fn sniffed(url: &str, kind: SniffedKind, label: Option<&str>) -> SniffedMedia {
    SniffedMedia {
        url: url.to_string(),
        kind,
        label_hint: label.map(str::to_string),
        source: SniffSource::SourceTag,
    }
}

#[test]
fn launch_site_probe_still_hard_fails_on_generic_extractor() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=generic-yt",
        "Looks Generic",
        "https://cdn.example.com/g.mp4",
        vec![("720p", "720p")],
    );
    toolchain.mark_generic("https://www.youtube.com/watch?v=generic-yt");
    let probe_calls_handle = toolchain.clone();
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let err = core
        .probe("https://www.youtube.com/watch?v=generic-yt")
        .unwrap_err();
    assert!(err.contains("generic"), "got: {err}");
    let calls = probe_calls_handle.probe_calls();
    assert!(
        calls.is_empty() || calls.iter().all(|(_, allow)| !allow),
        "Launch Sites never allow generic: {calls:?}"
    );
    let events = collect_events(&rx);
    assert!(events
        .iter()
        .any(|e| matches!(e, CoreEvent::ProbeFailed { .. })));
}

#[test]
fn unknown_site_accepts_generic_extractor_with_warning() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://videos.example.org/watch/42",
        "Some Clip",
        "https://videos.example.org/watch/42",
        vec![("720p", "720p"), ("360p", "360p")],
    );
    toolchain.mark_generic("https://videos.example.org/watch/42");
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();
    core.set_page_sniffer(Arc::new(FakePageSniffer::new()));

    let result = core.probe("https://videos.example.org/watch/42").unwrap();
    assert_eq!(result.tier, ExtractionTier::GenericExtractor);
    assert_eq!(result.extractor_key.as_deref(), Some("Generic"));
    assert!(
        !result.warnings.is_empty(),
        "generic results carry a warning"
    );
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].fetch_kind, MediaFetchKind::Extractor);

    // Confirm still creates an extractor-backed job.
    let selection = ProbeSelection {
        url: "https://videos.example.org/watch/42".into(),
        items: vec![ItemSelection {
            item_id: "item-1".into(),
            quality_id: "720p".into(),
        }],
        media_extras: None,
    };
    let ids = core.confirm_probe_selection(&selection, None).unwrap();
    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == ids[0])
        .unwrap();
    assert!(snap.is_extractor);
    assert_eq!(snap.extraction_tier, Some(ExtractionTier::GenericExtractor));
}

#[test]
fn generic_disabled_in_settings_makes_unknown_site_fail_through_to_sniff() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://videos.example.org/watch/43",
        "Clip",
        "https://videos.example.org/watch/43",
        vec![("720p", "720p")],
    );
    toolchain.mark_generic("https://videos.example.org/watch/43");
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();
    let sniffer = Arc::new(FakePageSniffer::new());
    core.set_page_sniffer(sniffer.clone());
    core.set_probe_tier_settings(ProbeTierSettings {
        allow_generic: false,
        ..ProbeTierSettings::default()
    })
    .unwrap();

    let err = core
        .probe("https://videos.example.org/watch/43")
        .unwrap_err();
    assert!(
        err.contains("yt-dlp"),
        "error names the failing tier: {err}"
    );
    assert!(
        sniffer.calls().iter().any(|(_, kind)| *kind == "sniff"),
        "page sniff ran after yt-dlp was rejected: {:?}",
        sniffer.calls()
    );
}

#[test]
fn unknown_site_falls_back_to_page_sniff_and_downloads_plain_file() {
    let page = "https://blog.example.net/posts/clip";
    let body_720 = b"seven-twenty-bytes-of-video!!".to_vec();
    let http = fake_http_map(&[
        ("https://cdn.example.net/media/clip_720.mp4", &body_720),
        ("https://cdn.example.net/media/clip_480.mp4", b"small"),
    ]);
    let tmp = tempfile::tempdir().unwrap();
    // No yt-dlp response configured → toolchain fails → sniffer takes over.
    let (mut core, rx) = Core::new(Box::new(http), Box::new(FakeExtractorToolchain::new()));
    core.set_library_root(tmp.path().join("library")).unwrap();
    let sniffer = Arc::new(FakePageSniffer::new());
    sniffer.insert_page(
        page,
        SniffOutcome::Page {
            title: Some("A clip".into()),
            media: vec![
                sniffed(
                    "https://cdn.example.net/media/clip_480.mp4",
                    SniffedKind::DirectFile { ext: "mp4".into() },
                    Some("480p"),
                ),
                sniffed(
                    "https://cdn.example.net/media/clip_720.mp4",
                    SniffedKind::DirectFile { ext: "mp4".into() },
                    None,
                ),
                sniffed(
                    "https://cdn.example.net/hls/master.m3u8",
                    SniffedKind::Hls,
                    None,
                ),
            ],
        },
    );
    core.set_page_sniffer(sniffer.clone());

    let result = core.probe(page).unwrap();
    assert_eq!(result.tier, ExtractionTier::PageSniff);
    assert!(result.warnings[0].contains("yt-dlp"));
    assert_eq!(result.items.len(), 2, "one progressive item + one HLS item");
    let file_item = result
        .items
        .iter()
        .find(|i| i.fetch_kind == MediaFetchKind::DirectHttp)
        .expect("plain-file item");
    assert_eq!(file_item.qualities[0].id, "720p");
    assert_eq!(file_item.referer.as_deref(), Some(page));
    let hls_item = result
        .items
        .iter()
        .find(|i| i.fetch_kind == MediaFetchKind::Extractor)
        .expect("hls item");
    assert_eq!(hls_item.qualities[0].id, "best");
    assert_eq!(
        sniffer.calls(),
        vec![(page.to_string(), "head"), (page.to_string(), "sniff")],
        "HEAD first, full sniff only after yt-dlp failed"
    );

    // Confirm the plain-file item at 720p: a normal HTTP job, not yt-dlp.
    let selection = ProbeSelection {
        url: page.into(),
        items: vec![ItemSelection {
            item_id: file_item.id.clone(),
            quality_id: file_item.qualities[0].id.clone(),
        }],
        media_extras: None,
    };
    let ids = core.confirm_probe_selection(&selection, None).unwrap();
    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == ids[0])
        .unwrap();
    assert!(
        !snap.is_extractor,
        "sniffed plain files use the HTTP backend"
    );
    assert_eq!(snap.url, "https://cdn.example.net/media/clip_720.mp4");
    assert_eq!(snap.extraction_tier, Some(ExtractionTier::PageSniff));

    process_until_job_done(&mut core, &rx, &ids[0]);
    let done = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == ids[0])
        .unwrap();
    assert_eq!(done.status, "completed");
    let path = PathBuf::from(done.path.expect("completed jobs expose their path"));
    assert_eq!(path.file_name().unwrap(), "clip_720.mp4");
    assert_eq!(std::fs::read(&path).unwrap(), body_720);
}

#[test]
fn sniffed_variant_quality_selects_its_own_url() {
    let page = "https://blog.example.net/posts/variants";
    let http = fake_http_map(&[
        ("https://cdn.example.net/v/x_1080.mp4", b"big"),
        ("https://cdn.example.net/v/x_480.mp4", b"small-file"),
    ]);
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(FakeExtractorToolchain::new()));
    core.set_library_root(tmp.path().join("library")).unwrap();
    let sniffer = Arc::new(FakePageSniffer::new());
    sniffer.insert_page(
        page,
        SniffOutcome::Page {
            title: None,
            media: vec![
                sniffed(
                    "https://cdn.example.net/v/x_1080.mp4",
                    SniffedKind::DirectFile { ext: "mp4".into() },
                    None,
                ),
                sniffed(
                    "https://cdn.example.net/v/x_480.mp4",
                    SniffedKind::DirectFile { ext: "mp4".into() },
                    None,
                ),
            ],
        },
    );
    core.set_page_sniffer(sniffer);
    let result = core.probe(page).unwrap();
    let item = &result.items[0];
    let q480 = item
        .qualities
        .iter()
        .find(|q| q.id.starts_with("480p"))
        .unwrap();
    let ids = core
        .confirm_probe_selection(
            &ProbeSelection {
                url: page.into(),
                items: vec![ItemSelection {
                    item_id: item.id.clone(),
                    quality_id: q480.id.clone(),
                }],
                media_extras: None,
            },
            None,
        )
        .unwrap();
    process_until_job_done(&mut core, &rx, &ids[0]);
    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == ids[0])
        .unwrap();
    assert_eq!(snap.url, "https://cdn.example.net/v/x_480.mp4");
    assert_eq!(std::fs::read(snap.path.unwrap()).unwrap(), b"small-file");
}

#[test]
fn head_binary_short_circuits_to_direct_file_without_spawning_ytdlp() {
    let url = "https://mirror.example.org/dl?id=77";
    let body = b"ISO-9660-ish-bytes".to_vec();
    let http = fake_http_map(&[(url, &body)]);
    let toolchain = FakeExtractorToolchain::new();
    let calls = toolchain.clone();
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();
    let sniffer = Arc::new(FakePageSniffer::new());
    sniffer.insert_head(
        url,
        SniffOutcome::Binary {
            content_type: "application/x-iso9660-image".into(),
            content_length: Some(body.len() as u64),
            final_url: url.into(),
            filename: Some("void-live.iso".into()),
        },
    );
    core.set_page_sniffer(sniffer);

    // The URL has no file extension, so classification wants a probe...
    match core.enqueue(url, None).unwrap() {
        EnqueueOutcome::ProbeThenConfirm { .. } => {}
        other => panic!("expected probe, got {other:?}"),
    }
    // ...and the probe resolves it as a plain file from the HEAD alone.
    let result = core.probe(url).unwrap();
    assert_eq!(result.tier, ExtractionTier::DirectFile);
    assert!(
        calls.probe_calls().is_empty(),
        "yt-dlp must not run for binary bodies"
    );
    let item = &result.items[0];
    assert_eq!(item.title, "void-live.iso");
    assert_eq!(item.fetch_kind, MediaFetchKind::DirectHttp);
    assert_eq!(item.qualities[0].id, "file");
    assert_eq!(item.qualities[0].size_bytes, Some(body.len() as u64));

    let ids = core
        .confirm_probe_selection(
            &ProbeSelection {
                url: url.into(),
                items: vec![ItemSelection {
                    item_id: item.id.clone(),
                    quality_id: "file".into(),
                }],
                media_extras: None,
            },
            None,
        )
        .unwrap();
    process_until_job_done(&mut core, &rx, &ids[0]);
    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == ids[0])
        .unwrap();
    assert_eq!(snap.status, "completed");
    let path = PathBuf::from(snap.path.unwrap());
    assert_eq!(path.file_name().unwrap(), "void-live.iso");
    assert_eq!(std::fs::read(&path).unwrap(), body);
}

#[test]
fn probe_with_nothing_found_reports_both_tiers() {
    let url = "https://nothing.example.org/page";
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = Core::new(
        Box::new(fake_http()),
        Box::new(FakeExtractorToolchain::new()),
    );
    core.set_library_root(tmp.path().join("library")).unwrap();
    let sniffer = Arc::new(FakePageSniffer::new());
    sniffer.insert_page(
        url,
        SniffOutcome::Page {
            title: Some("x".into()),
            media: vec![],
        },
    );
    core.set_page_sniffer(sniffer);
    let err = core.probe(url).unwrap_err();
    assert!(
        err.contains("yt-dlp") && err.contains("page sniff"),
        "got: {err}"
    );
}

#[test]
fn cancel_probe_aborts_between_tiers() {
    let url = "https://slow.example.org/page";
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = Core::new(
        Box::new(fake_http()),
        Box::new(FakeExtractorToolchain::new()),
    );
    core.set_library_root(tmp.path().join("library")).unwrap();
    core.set_page_sniffer(Arc::new(FakePageSniffer::new()));
    let plan = core.plan_probe(url).unwrap();
    assert!(core.cancel_probe(url), "in-flight probe is cancellable");
    let err = plan.run().unwrap_err();
    assert!(err.contains("cancelled"), "got: {err}");
    let err = core.finish_probe(plan.url(), Err(err)).unwrap_err();
    assert!(err.contains("cancelled"));
    assert!(!core.cancel_probe(url), "finished probes are forgotten");
}

#[test]
fn probe_tier_settings_persist_across_library_root_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let custom = ProbeTierSettings {
        allow_generic: false,
        allow_page_sniff: true,
        allow_direct_fallback: false,
        probe_timeout_secs: 90,
    };
    {
        let (mut core, _rx) = make_core(Box::new(fake_http()));
        core.set_library_root(root.clone()).unwrap();
        assert_eq!(core.probe_tier_settings(), ProbeTierSettings::default());
        core.set_probe_tier_settings(custom).unwrap();
        let err = core
            .set_probe_tier_settings(ProbeTierSettings {
                probe_timeout_secs: 1,
                ..custom
            })
            .unwrap_err();
        assert!(err.contains("timeout"));
    }
    let (mut core, _rx) = make_core(Box::new(fake_http()));
    core.set_library_root(root).unwrap();
    assert_eq!(core.probe_tier_settings(), custom);
}

#[test]
fn enqueue_force_direct_skips_probe_classification() {
    let tmp = tempfile::tempdir().unwrap();
    let url = "https://www.youtube.com/watch?v=plain-file";
    let body = b"not really a video".to_vec();
    let http = fake_http_map(&[(url, &body)]);
    let (mut core, rx) = make_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();
    let outcome = core
        .enqueue_with_options(url, None, EnqueueOptions { force_direct: true })
        .unwrap();
    let EnqueueOutcome::EnqueueDirect { job_id, .. } = outcome else {
        panic!("force_direct must enqueue a plain job");
    };
    process_until_job_done(&mut core, &rx, &job_id);
    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .unwrap();
    assert_eq!(snap.status, "completed");
    assert_eq!(snap.extraction_tier, Some(ExtractionTier::DirectFile));
    assert_eq!(snap.queue_index, 0);
}

#[test]
fn failed_job_snapshot_carries_last_error_until_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = make_core(Box::new(FailingHttp));
    core.set_library_root(tmp.path().join("library")).unwrap();
    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.process_pending().unwrap();
    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .unwrap();
    assert_eq!(snap.status, "failed");
    assert!(
        snap.last_error.is_some(),
        "failed jobs expose the error text"
    );
    core.retry_job(&job_id).unwrap();
    let snap = core
        .list_job_snapshots()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .unwrap();
    assert_eq!(snap.last_error, None);
}

#[test]
fn clear_history_removes_archive_entries() {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        "https://www.youtube.com/watch?v=hist-clear",
        "History Clear",
        "https://cdn.youtube.com/hist-clear.mp4",
        vec![("720p", "720p")],
    );
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();
    core.probe("https://www.youtube.com/watch?v=hist-clear")
        .unwrap();
    let ids = core
        .confirm_probe_selection(
            &ProbeSelection {
                url: "https://www.youtube.com/watch?v=hist-clear".into(),
                items: vec![ItemSelection {
                    item_id: "item-1".into(),
                    quality_id: "720p".into(),
                }],
                media_extras: None,
            },
            None,
        )
        .unwrap();
    process_until_job_done(&mut core, &rx, &ids[0]);
    assert_eq!(core.history_len(), 1);
    assert_eq!(core.clear_history(Some(ArchiveOutcome::Failed)).unwrap(), 0);
    assert_eq!(core.clear_history(None).unwrap(), 1);
    assert_eq!(core.history_len(), 0);
}
