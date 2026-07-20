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
#[derive(Clone)]
struct FakeHttp {
    responses: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
}

impl HttpBackend for FakeHttp {
    fn fetch(
        &self,
        url: &str,
        _range: Option<std::ops::Range<u64>>,
    ) -> Result<(Vec<u8>, u64), String> {
        let responses = self.responses.lock().unwrap();
        match responses.get(url) {
            Some(body) => Ok((body.clone(), body.len() as u64)),
            None => Err(format!("no fake response for {url}")),
        }
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
