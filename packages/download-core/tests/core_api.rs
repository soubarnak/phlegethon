//! Integration tests for Download Core public API.
//!
//! These tests exercise the highest seam — Core commands → events — with a
//! fake HTTP backend (no live network).  Every acceptance criterion from
//! issue #14 is covered here.

use download_core::*;
use std::io::Write;
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
    let result = core.enqueue(url).unwrap();
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

    let result = core.enqueue("https://example.com/file.zip");
    assert!(
        matches!(result, Ok(EnqueueOutcome::EnqueueDirect { job_id: _, destination: _ }))
    );
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

    let result = core.enqueue("https://example.com/file.zip").unwrap();
    let (_job_id, destination) = match result {
        EnqueueOutcome::EnqueueDirect { job_id, destination } => (job_id, destination),
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

    let result = core.enqueue("https://example.com/file.zip").unwrap();
    let (job_id, destination) = match result {
        EnqueueOutcome::EnqueueDirect { job_id, destination } => (job_id, destination),
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
    let result = core.enqueue("https://www.youtube.com/watch?v=abc123");
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
        events.iter().any(|e| matches!(e, CoreEvent::JobPaused { job_id: id } if id == &job_id)),
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
    let content =
        b"hello world this is a resumable test file with enough data to split".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/resumable.zip".into(),
        content.clone(),
    );
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
    let final_progress = events2.iter().find_map(|e| match e {
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
fn resume_starts_from_last_byte_not_zero() {
    // This test verifies that restart-from-zero is NOT the default path
    // when Range is available.
    let full_content = b"exact content for resume test".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/exact.zip".into(),
        full_content.clone(),
    );

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
    let completed = events
        .iter()
        .find_map(|e| match e {
            CoreEvent::JobCompleted {
                bytes_written, ..
            } => Some(*bytes_written),
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
    let content =
        b"cumulative progress tracking for resumable transfer test".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/progress.zip".into(),
        content.clone(),
    );

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(StagedFakeHttp::new(map)));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let _job_id =
        enqueue_direct_job_id(&mut core, "https://example.com/progress.zip");

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

    let job_id =
        enqueue_direct_job_id(&mut core, "https://example.com/pause-resume.zip");

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

    let job_id =
        enqueue_direct_job_id(&mut core, "https://example.com/fail-retry.zip");

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
// Acceptance: Bandwidth Cap (issue #17)
// ---------------------------------------------------------------------------

fn count_process_calls_until_done(
    core: &mut Core,
    rx: &crossbeam_channel::Receiver<CoreEvent>,
) -> usize {
    let mut count = 0;
    loop {
        core.process_pending().unwrap();
        count += 1;
        let events = collect_events(rx);
        if events
            .iter()
            .any(|e| matches!(e, CoreEvent::JobCompleted { .. }))
        {
            return count;
        }
    }
}

#[test]
fn default_transfer_is_uncapped() {
    let content = b"this file is long enough to be split by a cap if one were set"
        .repeat(1)
        .to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/uncapped.zip".into(),
        content.clone(),
    );
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let _job_id =
        enqueue_direct_job_id(&mut core, "https://example.com/uncapped.zip");

    // One call should complete the job
    let calls = count_process_calls_until_done(&mut core, &rx);
    assert_eq!(
        calls, 1,
        "default uncapped transfer should complete in 1 call, took {calls}"
    );
}

#[test]
fn global_bandwidth_cap_limits_aggregate_throughput() {
    let content_a = b"AAAA this is 30 bytes of data!"; // 30 bytes
    let content_b = b"BBBB this is also 30 bytes woo"; // 30 bytes
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/a.zip".into(), content_a.to_vec());
    map.insert("https://example.com/b.zip".into(), content_b.to_vec());
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_a = enqueue_direct_job_id(&mut core, "https://example.com/a.zip");
    let job_b = enqueue_direct_job_id(&mut core, "https://example.com/b.zip");

    // Global cap: 20 bytes per call (aggregate across both jobs)
    core.set_global_bandwidth_cap(20);

    // First call — neither job should complete yet
    let mut done = std::collections::HashSet::new();
    process_and_accumulate_completions(&mut core, &rx, &mut done);
    assert!(
        !done.contains(&job_a) && !done.contains(&job_b),
        "neither job should complete in one call with 20-byte global cap"
    );

    // Drain remaining bytes
    while !done.contains(&job_a) || !done.contains(&job_b) {
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
    let content_b =
        b"this is a longer file content that spans multiple iterations when capped";
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/a.zip".into(), content_a.to_vec());
    map.insert("https://example.com/b.zip".into(), content_b.to_vec());
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_a = enqueue_direct_job_id(&mut core, "https://example.com/a.zip");
    let job_b = enqueue_direct_job_id(&mut core, "https://example.com/b.zip");

    // Set per-job cap on job_b only (no global cap)
    core.set_job_bandwidth_cap(&job_b, 10).unwrap();

    // First call — job_a should complete; job_b should stay pending
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

    // Finish job_b in subsequent calls
    process_until_job_done(&mut core, &rx, &job_b);

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
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/big.zip");

    // Global cap 100, per-job cap 10 => effective cap should be 10
    core.set_global_bandwidth_cap(100);
    core.set_job_bandwidth_cap(&job_id, 10).unwrap();

    // First call should transfer at most 10 bytes
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

    process_until_job_done(&mut core, &rx, &job_id);

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
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/big.zip");

    // Global cap 5, per-job cap 100 => effective cap should be 5
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

    process_until_job_done(&mut core, &rx, &job_id);

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
    map.insert(
        "https://example.com/small.zip".into(),
        content.clone(),
    );
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let _job_id =
        enqueue_direct_job_id(&mut core, "https://example.com/small.zip");

    // Set cap larger than content
    core.set_global_bandwidth_cap(100);

    let calls = count_process_calls_until_done(&mut core, &rx);
    assert_eq!(
        calls, 1,
        "small file should complete in 1 call even with cap, took {calls}"
    );
}

#[test]
fn set_and_remove_global_bandwidth_cap_restores_uncapped() {
    let content =
        b"this file should transfer uncapped after removing the global cap".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/global-switch.zip".into(),
        content.clone(),
    );
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let _job_id =
        enqueue_direct_job_id(&mut core, "https://example.com/global-switch.zip");

    // Set cap, verify it's active
    core.set_global_bandwidth_cap(5);
    assert_eq!(core.global_bandwidth_cap(), Some(5));

    // Remove cap
    core.remove_global_bandwidth_cap();
    assert_eq!(core.global_bandwidth_cap(), None);

    // Now it should complete in one call
    let calls = count_process_calls_until_done(&mut core, &rx);
    assert_eq!(
        calls, 1,
        "after removing global cap, transfer completes in 1 call, took {calls}"
    );

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
    let content =
        b"this file should transfer uncapped after removing the per-job cap".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/job-switch.zip".into(),
        content.clone(),
    );
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id =
        enqueue_direct_job_id(&mut core, "https://example.com/job-switch.zip");

    // Set per-job cap, verify
    core.set_job_bandwidth_cap(&job_id, 5).unwrap();
    assert_eq!(core.job_bandwidth_cap(&job_id).unwrap(), Some(5));

    // Remove per-job cap
    core.remove_job_bandwidth_cap(&job_id).unwrap();
    assert_eq!(core.job_bandwidth_cap(&job_id).unwrap(), None);

    // Should complete in one call
    let calls = count_process_calls_until_done(&mut core, &rx);
    assert_eq!(
        calls, 1,
        "after removing per-job cap, transfer completes in 1 call, took {calls}"
    );

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
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = make_core(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_1 = enqueue_direct_job_id(&mut core, "https://example.com/1.zip");
    let job_2 = enqueue_direct_job_id(&mut core, "https://example.com/2.zip");
    let job_3 = enqueue_direct_job_id(&mut core, "https://example.com/3.zip");

    // Global cap 30 — should allow one full job (20) + 10 of the next
    core.set_global_bandwidth_cap(30);

    // First call: one job should complete (20b), another gets ≤10b
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

    // Process remaining until all 3 are done
    while done.len() < 3 {
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
    let result = core.probe("https://www.youtube.com/watch?v=abc123").unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].title, "Test Video");
    assert_eq!(result.items[0].qualities.len(), 2);

    // Should emit a ProbeResult event
    let events = collect_events(&rx);
    let has_probe_event = events.iter().any(|e| matches!(e, CoreEvent::ProbeResult { .. }));
    assert!(has_probe_event, "should emit ProbeResult event, got: {events:?}");
}

/// Helper: create a FakeHttp that knows the probe item URLs.
fn fake_http_with_responses(
    pairs: Vec<(&str, &[u8])>,
) -> FakeHttp {
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
    let err = core.probe("https://www.youtube.com/watch?v=unknown").unwrap_err();
    assert!(err.contains("no response configured"), "error should mention missing config, got: {err}");

    // Should emit a ProbeFailed event
    let events = collect_events(&rx);
    let has_fail_event = events.iter().any(|e| matches!(e, CoreEvent::ProbeFailed { .. }));
    assert!(has_fail_event, "should emit ProbeFailed event, got: {events:?}");
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
    let http = fake_http_with_responses(vec![
        ("https://cdn.youtube.com/video.mp4", &video_data),
    ]);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    // First probe
    core.probe("https://www.youtube.com/watch?v=abc123").unwrap();
    collect_events(&rx); // drain

    // Confirm selection — pick 720p
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=abc123".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "720p".to_string(),
        }],
    };
    let job_ids = core.confirm_probe_selection(&selection).unwrap();
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
    };
    let job_ids = core.confirm_probe_selection(&selection).unwrap();
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
    };
    let err = core.confirm_probe_selection(&selection).unwrap_err();
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

    core.probe("https://www.youtube.com/watch?v=abc123").unwrap();
    collect_events(&rx); // drain

    // Try to confirm a non-existent item
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=abc123".to_string(),
        items: vec![ItemSelection {
            item_id: "nonexistent-item".to_string(),
            quality_id: "1080p".to_string(),
        }],
    };
    let err = core.confirm_probe_selection(&selection).unwrap_err();
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

    core.probe("https://www.youtube.com/watch?v=abc123").unwrap();
    collect_events(&rx); // drain

    // Try to confirm with a non-existent quality
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=abc123".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "8k".to_string(),
        }],
    };
    let err = core.confirm_probe_selection(&selection).unwrap_err();
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

    core.probe("https://www.youtube.com/watch?v=abc123").unwrap();
    collect_events(&rx); // drain

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=abc123".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "1080p".to_string(),
        }],
    };
    let err = core.confirm_probe_selection(&selection).unwrap_err();
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
        vec![
            (
                "vid-1",
                "Video One",
                "https://youtube.com/watch?v=1",
                vec![("1080p", "1080p")],
            ),
        ],
    );

    let http = fake_http_with_responses(vec![
        ("https://youtube.com/watch?v=1", b"video one content"),
    ]);

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
    };
    let job_ids = core.confirm_probe_selection(&selection).unwrap();
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

    let http = fake_http_with_responses(vec![
        ("https://cdn.youtube.com/video.mp4", b"fake mp4 content"),
    ]);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    core.probe("https://www.youtube.com/watch?v=abc123").unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=abc123".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "1080p".to_string(),
        }],
    };
    let job_ids = core.confirm_probe_selection(&selection).unwrap();
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

    let http = fake_http_with_responses(vec![
        ("https://cdn.youtube.com/video.mp4", b"fake video content"),
    ]);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    core.set_library_root(tmp.path().join("library")).unwrap();

    // Create a probe-confirmed job
    core.probe("https://www.youtube.com/watch?v=abc123").unwrap();
    collect_events(&rx);

    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=abc123".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "1080p".to_string(),
        }],
    };
    let job_ids = core.confirm_probe_selection(&selection).unwrap();
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
        ("https://cdn.youtube.com/probed.mp4", b"probed video content"),
    ]);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http), Box::new(toolchain));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    // Create a direct job
    let direct_job_id =
        enqueue_direct_job_id(&mut core, "https://example.com/file.zip");

    // Create a probe-confirmed job
    core.probe("https://www.youtube.com/watch?v=probe").unwrap();
    collect_events(&rx);
    let selection = ProbeSelection {
        url: "https://www.youtube.com/watch?v=probe".to_string(),
        items: vec![ItemSelection {
            item_id: "item-1".to_string(),
            quality_id: "1080p".to_string(),
        }],
    };
    let probe_job_ids = core.confirm_probe_selection(&selection).unwrap();
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
    fn new(
        responses: std::collections::HashMap<String, Vec<u8>>,
        range_supported: bool,
    ) -> Self {
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
fn segmented_transfer_completes_to_correct_file() {
    // Given a 100-byte file hosted on a Range-supporting server
    let content: Vec<u8> = (0u8..100).collect();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/segmented.bin".into(), content.clone());

    let http = TrackedFakeHttp::new(map, true);

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http.clone()), Box::new(FakeExtractorToolchain::new()));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id =
        enqueue_direct_job_id(&mut core, "https://example.com/segmented.bin");

    // Process pending — should complete in one call via segmented transfer
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // Should have completed with the full content
    let completed = events.iter().find_map(|e| match e {
        CoreEvent::JobCompleted {
            bytes_written, ..
        } => Some(*bytes_written),
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
    let (mut core, rx) = Core::new(Box::new(http.clone()), Box::new(FakeExtractorToolchain::new()));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let _job_id =
        enqueue_direct_job_id(&mut core, "https://example.com/segmented.bin");

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
    let starts_at_zero = calls.iter().any(|(_, r)| matches!(r, Some(rng) if rng.start == 0));
    assert!(
        starts_at_zero,
        "should have a segment starting at 0: {calls:?}"
    );

    // The final segment should end at the total file size (100)
    let ends_at_100 = calls.iter().any(|(_, r)| matches!(r, Some(rng) if rng.end == 100));
    assert!(
        ends_at_100,
        "should have a segment ending at 100: {calls:?}"
    );

    // Ranges should be non-overlapping and cover 0..100
    let mut ranges: Vec<_> = calls
        .iter()
        .filter_map(|(_, r)| r.clone())
        .collect();
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
    let (mut core, rx) = Core::new(Box::new(http.clone()), Box::new(FakeExtractorToolchain::new()));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id =
        enqueue_direct_job_id(&mut core, "https://example.com/fallback.bin");

    // Process pending
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // Should complete (via single-connection fallback)
    let completed = events.iter().find_map(|e| match e {
        CoreEvent::JobCompleted {
            bytes_written, ..
        } => Some(*bytes_written),
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
    let (mut core, rx) = Core::new(Box::new(http.clone()), Box::new(FakeExtractorToolchain::new()));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id =
        enqueue_direct_job_id(&mut core, "https://example.com/small.bin");

    // Process
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // Should complete
    let completed = events.iter().any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(completed, "small file should complete via segmented transfer");

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

    let _job_id =
        enqueue_direct_job_id(&mut core, "https://example.com/empty.bin");

    // Processing an empty file should not panic
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // Should emit a failed event (empty file is an error)
    let has_failed = events
        .iter()
        .any(|e| matches!(e, CoreEvent::JobFailed { .. }));
    assert!(
        has_failed,
        "empty file should fail, got events: {events:?}"
    );
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
    assert_eq!(
        on_disk, content,
        "should have primary mirror content"
    );
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
    core.add_job_mirror(&job_id, "https://m1.example.com/file.zip".into()).unwrap();
    core.add_job_mirror(&job_id, "https://m2.example.com/file.zip".into()).unwrap();

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
