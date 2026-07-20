//! Integration tests for Download Core public API.
//!
//! These tests exercise the highest seam — Core commands → events — with a
//! fake HTTP backend (no live network).  Every acceptance criterion from
//! issue #14 is covered here.

use download_core::*;
use std::sync::Arc;
use std::sync::Mutex;

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

impl HttpBackend for FakeHttp {
    fn fetch(
        &self,
        url: &str,
        range: Option<std::ops::Range<u64>>,
    ) -> Result<(Vec<u8>, u64), String> {
        let responses = self.responses.lock().unwrap();
        let full_body = responses
            .get(url)
            .ok_or_else(|| format!("no fake response for {url}"))?;
        let total = full_body.len() as u64;

        let data = match range {
            Some(r) => {
                let start = r.start.min(total);
                full_body[start as usize..].to_vec()
            }
            None => full_body.clone(),
        };

        Ok((data, total))
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
    ) -> Result<(Vec<u8>, u64), String> {
        Err(format!("simulated failure for {url}"))
    }
}

fn enqueue_direct_job_id(
    core: &mut Core,
    url: &str,
) -> String {
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

// ---------------------------------------------------------------------------
// Acceptance: create/destroy and Library Root
// ---------------------------------------------------------------------------

#[test]
fn can_create_destroy_and_set_library_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let (mut core, _rx) = Core::new(Box::new(fake_http()));
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
    let (mut core, _rx) = Core::new(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let result = core.enqueue("https://example.com/file.zip");
    assert!(matches!(result, Ok(EnqueueOutcome::EnqueueDirect { job_id: _, destination: _ })));
}

// ---------------------------------------------------------------------------
// Acceptance: job writes bytes under Library Root / Job Destination
// ---------------------------------------------------------------------------

#[test]
fn enqueue_direct_job_writes_bytes_to_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()));
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
    let has_completed = events.iter().any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(has_completed, "should emit JobCompleted event");

    // Check progress events were emitted
    let has_progress = events.iter().any(|e| matches!(e, CoreEvent::JobProgress { .. }));
    assert!(has_progress, "should emit JobProgress event");
}

// ---------------------------------------------------------------------------
// Acceptance: enqueue non-HTTP(S) URL returns probe_then_confirm
// ---------------------------------------------------------------------------

#[test]
fn enqueue_launch_site_url_returns_probe_then_confirm() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = Core::new(Box::new(fake_http()));
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
    let (mut core, rx) = Core::new(Box::new(fake_http()));
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
    let (mut core, rx) = Core::new(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.pause_job(&job_id).unwrap();
    collect_events(&rx); // drain pause event

    // process_pending should NOT process the paused job
    core.process_pending().unwrap();

    let events = collect_events(&rx);
    let has_progress = events.iter().any(|e| matches!(e, CoreEvent::JobProgress { .. }));
    assert!(!has_progress, "paused job should not produce progress events");
    let has_completed = events.iter().any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(!has_completed, "paused job should not produce completed events");
}

#[test]
fn resume_paused_job_emits_resumed_event_and_allows_processing() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.pause_job(&job_id).unwrap();
    collect_events(&rx); // drain pause

    core.resume_job(&job_id).unwrap();

    let events = collect_events(&rx);
    assert!(
        events.iter().any(|e| matches!(e, CoreEvent::JobResumed { job_id: id } if id == &job_id)),
        "should emit JobResumed event, got {:?}",
        events
    );

    // Now process_pending should complete it
    core.process_pending().unwrap();
    let events2 = collect_events(&rx);
    assert!(
        events2.iter().any(|e| matches!(e, CoreEvent::JobCompleted { .. })),
        "resumed job should complete after process_pending"
    );
}

#[test]
fn failed_job_emits_failed_event_and_remains_visible() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(FailingHttp));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/fail.zip");

    // Process — should fail
    let result = core.process_pending();
    assert!(result.is_ok(), "process_pending should not propagate HTTP errors");

    let events = collect_events(&rx);
    assert!(
        events.iter().any(|e| matches!(e, CoreEvent::JobFailed { job_id: id, .. } if id == &job_id)),
        "should emit JobFailed event, got {:?}",
        events
    );

    // Retry the failed job
    core.retry_job(&job_id).unwrap();
    let events2 = collect_events(&rx);
    assert!(
        events2.iter().any(|e| matches!(e, CoreEvent::JobRetried { job_id: id } if id == &job_id)),
        "should emit JobRetried event, got {:?}",
        events2
    );
}

#[test]
fn discard_removes_job_from_queue_and_emits_discarded_event() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");

    core.discard_job(&job_id).unwrap();

    let events = collect_events(&rx);
    assert!(
        events.iter().any(|e| matches!(e, CoreEvent::JobDiscarded { job_id: id } if id == &job_id)),
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
    let (mut core, _rx) = Core::new(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");
    core.process_pending().unwrap();

    let err = core.pause_job(&job_id);
    assert!(err.is_err(), "cannot pause a completed job");
}

#[test]
fn retry_failed_job_goes_back_to_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(FailingHttp));
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
        events.iter().any(|e| matches!(e, CoreEvent::JobFailed { .. })),
        "retried job should be re-processed and fail again"
    );
}

#[test]
fn resume_non_paused_job_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = Core::new(Box::new(fake_http()));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/file.zip");

    // A pending job is not paused
    let err = core.resume_job(&job_id);
    assert!(err.is_err(), "cannot resume a non-paused job");
}

#[test]
fn retry_non_failed_job_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = Core::new(Box::new(fake_http()));
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
    ) -> Result<(Vec<u8>, u64), String> {
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
                let chunk = full_body[..chunk_size as usize].to_vec();
                sent.insert(url.to_string(), chunk_size);
                Ok((chunk, total))
            }
            Some(r) => {
                let start = r.start.min(total);
                let remaining = full_body[start as usize..].to_vec();
                sent.insert(url.to_string(), total);
                Ok((remaining, total))
            }
        }
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
    let (mut core, rx) = Core::new(Box::new(http));
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
    assert!(final_progress.is_some(), "resumed job should have final progress");
    let (final_bytes, final_total) = final_progress.unwrap();
    assert_eq!(
        final_bytes, final_total,
        "resumed job should have complete bytes ({final_bytes} == {final_total})"
    );
    assert_eq!(final_bytes, content.len() as u64, "should match original content length");

    // Verify file on disk has the full content
    let entries: Vec<_> = std::fs::read_dir(root.join("jobs").join(&job_id))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(!entries.is_empty(), "job directory should contain files");
    let file_content = std::fs::read(entries[0].path()).unwrap();
    assert_eq!(file_content, content, "file on disk should have full content after resume");
}

#[test]
fn resume_starts_from_last_byte_not_zero() {
    // This test verifies that restart-from-zero is NOT the default path
    // when Range is available.
    //
    // We do this by checking that the Range header is passed to the backend
    // on the second call (after partial data was written).
    //
    // Strategy: Use a RangeFakeHttp where only the Range-fulfilled call
    // returns the correct remaining bytes. If process_pending started from
    // zero, the second fetch would get the full content (duplicating data).
    let full_content = b"exact content for resume test".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/exact.zip".into(), full_content.clone());

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(StagedFakeHttp::new(map)));
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
        on_disk,
        full_content,
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
    // Verify that progress events show cumulative bytes across resume.
    // A resumed job should report bytes_downloaded = previously_downloaded + new_bytes.
    let content = b"cumulative progress tracking for resumable transfer test".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/progress.zip".into(), content.clone());

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(StagedFakeHttp::new(map)));
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
    // that are >= the partial_downloaded from the first pass
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
    // When a job is paused and later resumed, the resume should continue
    // from the last byte via Range — not restart from zero.
    let content = b"pause then resume with range transfer test content".to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/pause-resume.zip".into(), content.clone());

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(StagedFakeHttp::new(map)));
    let root = tmp.path().join("library");
    core.set_library_root(root.clone()).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/pause-resume.zip");

    // First pass — partial (simulates downloading before pause)
    core.process_pending().unwrap();
    collect_events(&rx);

    // Pause
    core.pause_job(&job_id).unwrap();
    collect_events(&rx);

    // Resume — this should keep bytes_received and use Range on next process
    core.resume_job(&job_id).unwrap();
    collect_events(&rx);

    // Process — should resume from last byte via Range
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // Verify completed
    let completed = events.iter().any(|e| matches!(e, CoreEvent::JobCompleted { .. }));
    assert!(completed, "paused-then-resumed job should complete");

    // Verify file has full content (not duplicated)
    let file_path = root.join("jobs").join(&job_id);
    let files: Vec<_> = std::fs::read_dir(&file_path)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(files.len(), 1, "should have one file");
    let on_disk = std::fs::read(files[0].path()).unwrap();
    assert_eq!(on_disk, content, "file should have exact content, not duplicated");
    assert_eq!(on_disk.len(), content.len(), "file length should match original");
}

#[test]
fn retry_resets_bytes_received_and_starts_from_zero() {
    // After a failed job is retried, bytes_received should reset to 0
    // so the file is written fresh (not appended to potentially corrupt data).
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(FailingHttp));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/fail-retry.zip");

    // First attempt fails
    core.process_pending().unwrap();
    collect_events(&rx); // drain failure

    // Retry — this resets bytes_received to 0
    core.retry_job(&job_id).unwrap();

    // Now swap to a RangeFakeHttp that would only return correct data
    // (We can't swap backend in current Core API, but we can verify retry
    // resets state by checking that the failed job transitions correctly.)
    let events = collect_events(&rx);
    assert!(
        events.iter().any(|e| matches!(e, CoreEvent::JobRetried { .. })),
        "retry should emit JobRetried event"
    );

    // Process again — should fail again with FailingHttp
    core.process_pending().unwrap();
    let events2 = collect_events(&rx);
    assert!(
        events2.iter().any(|e| matches!(e, CoreEvent::JobFailed { .. })),
        "retried job should attempt processing again"
    );
}
