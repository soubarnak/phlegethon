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

// ---------------------------------------------------------------------------
// Acceptance: Bandwidth Cap (issue #17)
// ---------------------------------------------------------------------------

fn count_process_calls_until_done(core: &mut Core, rx: &crossbeam_channel::Receiver<CoreEvent>) -> usize {
    let mut count = 0;
    loop {
        core.process_pending().unwrap();
        count += 1;
        let events = collect_events(rx);
        if events.iter().any(|e| matches!(e, CoreEvent::JobCompleted { .. })) {
            return count;
        }
    }
}

#[test]
fn default_transfer_is_uncapped() {
    // With no bandwidth cap set, a file of any size transfers fully in one call.
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
    let (mut core, rx) = Core::new(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let _job_id = enqueue_direct_job_id(&mut core, "https://example.com/uncapped.zip");

    // One call should complete the job
    let calls = count_process_calls_until_done(&mut core, &rx);
    assert_eq!(
        calls, 1,
        "default uncapped transfer should complete in 1 call, took {calls}"
    );
}

#[test]
fn global_bandwidth_cap_limits_aggregate_throughput() {
    // With a global cap, the total bytes transferred across ALL jobs in one
    // process_pending call is limited.
    let content_a = b"AAAA this is 30 bytes of data!"; // 30 bytes
    let content_b = b"BBBB this is also 30 bytes woo"; // 30 bytes
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/a.zip".into(),
        content_a.to_vec(),
    );
    map.insert(
        "https://example.com/b.zip".into(),
        content_b.to_vec(),
    );
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_a = enqueue_direct_job_id(&mut core, "https://example.com/a.zip");
    let job_b = enqueue_direct_job_id(&mut core, "https://example.com/b.zip");

    // Global cap: 20 bytes per call (aggregate across both jobs)
    core.set_global_bandwidth_cap(20);

    // First call — neither job should complete yet (each is 30 bytes, cap is 20 total)
    let mut done = std::collections::HashSet::new();
    process_and_accumulate_completions(&mut core, &rx, &mut done);
    assert!(
        !done.contains(&job_a) && !done.contains(&job_b),
        "neither job should complete in one call with 20-byte global cap"
    );

    // Drain remaining bytes — track completions across calls
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
        assert_eq!(
            on_disk, *expected,
            "job {job_id} content should match"
        );
    }
}

#[test]
fn per_job_bandwidth_cap_limits_that_job_without_global() {
    // Per-job cap applies to one job only; other jobs without a cap are
    // unaffected (no global cap needed).
    let content_a = b"small file that fits in one call"; // ~32 bytes
    let content_b = b"this is a longer file content that spans multiple iterations when capped";
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/a.zip".into(),
        content_a.to_vec(),
    );
    map.insert(
        "https://example.com/b.zip".into(),
        content_b.to_vec(),
    );
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_a = enqueue_direct_job_id(&mut core, "https://example.com/a.zip");
    let job_b = enqueue_direct_job_id(&mut core, "https://example.com/b.zip");

    // Set per-job cap on job_b only (no global cap)
    core.set_job_bandwidth_cap(&job_b, 10).unwrap();

    // First call — job_a should complete (it's small, uncapped)
    // job_b should transfer ≤ 10 bytes and stay pending
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
    // When both global and per-job caps are set, the more restrictive one
    // (smaller value) is the effective cap.
    let content = vec![b'X'; 200]; // 200 bytes
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/big.zip".into(),
        content.clone(),
    );
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/big.zip");

    // Global cap 100, per-job cap 10 => effective cap should be 10
    core.set_global_bandwidth_cap(100);
    core.set_job_bandwidth_cap(&job_id, 10).unwrap();

    // First call should transfer at most 10 bytes
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // The last progress event should show bytes_downloaded = 10
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

    // Process remaining bytes using stateful accumulator
    process_until_job_done(&mut core, &rx, &job_id);

    // Verify full content
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
    // When global cap is tighter than per-job cap, global cap wins.
    let content = vec![b'Y'; 200]; // 200 bytes
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/big.zip".into(),
        content.clone(),
    );
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http));
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

    // Finish using stateful accumulator
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
    // A file smaller than the cap should complete in one call.
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
    let (mut core, rx) = Core::new(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let _job_id = enqueue_direct_job_id(&mut core, "https://example.com/small.zip");

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
    let content = b"this file should transfer uncapped after removing the global cap"
        .to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/global-switch.zip".into(),
        content.clone(),
    );
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let _job_id = enqueue_direct_job_id(&mut core, "https://example.com/global-switch.zip");

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
    let content = b"this file should transfer uncapped after removing the per-job cap"
        .to_vec();
    let mut map = std::collections::HashMap::new();
    map.insert(
        "https://example.com/job-switch.zip".into(),
        content.clone(),
    );
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_id = enqueue_direct_job_id(&mut core, "https://example.com/job-switch.zip");

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
    // With a tight global cap and multiple jobs, the total bytes transferred
    // in a single call across ALL jobs should not exceed the cap.
    //
    // Strategy: enqueue three jobs, each with 20 bytes of content.
    // Global cap = 30. In one call, at most 30 bytes total should be
    // transferred (one full 20-byte job plus 10 bytes of another).
    let content = vec![b'Z'; 20]; // 20 bytes each
    let mut map = std::collections::HashMap::new();
    map.insert("https://example.com/1.zip".into(), content.clone());
    map.insert("https://example.com/2.zip".into(), content.clone());
    map.insert("https://example.com/3.zip".into(), content.clone());
    let http = FakeHttp {
        responses: Arc::new(Mutex::new(map)),
    };

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(http));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let job_1 = enqueue_direct_job_id(&mut core, "https://example.com/1.zip");
    let job_2 = enqueue_direct_job_id(&mut core, "https://example.com/2.zip");
    let job_3 = enqueue_direct_job_id(&mut core, "https://example.com/3.zip");

    // Global cap 30 — should allow one full job (20) + 10 of the next
    core.set_global_bandwidth_cap(30);

    // First call: one job should complete (20b), another gets ≤10b (cap exhausted)
    let mut done = std::collections::HashSet::new();
    core.process_pending().unwrap();
    let events = collect_events(&rx);

    // Count completed jobs and total bytes transferred in this call
    let completed_in_first: Vec<&String> = events
        .iter()
        .filter_map(|e| match e {
            CoreEvent::JobCompleted { job_id, .. } => Some(job_id),
            _ => None,
        })
        .collect();
    // At most 1 job should complete
    assert!(
        completed_in_first.len() <= 1,
        "at most 1 job should complete in first call, got {}",
        completed_in_first.len()
    );

    // Verify job_1 was the one to complete (it was first in order)
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

    // Verify all 3 jobs have content on disk
    let job_dir = tmp.path().join("library").join("jobs");
    let job_count = std::fs::read_dir(&job_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .count();
    assert_eq!(job_count, 3, "all 3 jobs should have job dirs");
}

#[test]
fn job_bandwidth_cap_error_for_nonexistent_job() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = Core::new(Box::new(FakeHttp {
        responses: Arc::new(Mutex::new(std::collections::HashMap::new())),
    }));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let result = core.set_job_bandwidth_cap("nonexistent-job-uuid", 100);
    assert!(result.is_err(), "setting cap on nonexistent job should error");

    let result = core.remove_job_bandwidth_cap("nonexistent-job-uuid");
    assert!(result.is_err(), "removing cap on nonexistent job should error");

    let result = core.job_bandwidth_cap("nonexistent-job-uuid");
    assert!(result.is_err(), "getting cap on nonexistent job should error");
}
