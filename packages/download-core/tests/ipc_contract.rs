//! Pins the serde shape of every type that crosses the IPC boundary to the shared sample file.
//!
//! The frontend reads the same file (`src/tauri/contract.test.ts`) and checks it against the
//! TypeScript types that ts-rs generates from these Rust types, so a change on either side
//! fails a test (M-71). Change a type: update `samples.json` and regenerate with
//! `scripts/gen-ts-types.sh`.

use download_core::{
    ActiveJobMatch, ExtractionTier, JobSnapshot, MediaFetchKind, MediaItem,
    ProbeDestinationPreview, ProbeResult, Quality,
};
use serde_json::Value;

fn samples() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tauri-shell/frontend/src/tauri/contract/samples.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("read samples.json"))
        .expect("parse samples.json")
}

#[test]
fn job_snapshot_matches_the_sample() {
    let snapshot = JobSnapshot {
        job_id: "job-1".into(),
        url: "https://example.com/a.iso".into(),
        destination: "/lib/jobs/job-1".into(),
        status: "downloading".into(),
        bytes_downloaded: 1024,
        total_bytes: Some(4096),
        title: "a.iso".into(),
        path: None,
        is_extractor: false,
        extraction_tier: Some(ExtractionTier::DirectFile),
        last_error: None,
        queue_index: 2,
    };
    assert_eq!(
        serde_json::to_value(snapshot).unwrap(),
        samples()["JobSnapshot"]
    );
}

#[test]
fn active_job_match_matches_the_samples() {
    let matches = vec![
        ActiveJobMatch {
            job_id: "job-1".into(),
            url: "https://example.com/a.iso".into(),
            status: "paused".into(),
            title: "a.iso".into(),
        },
        ActiveJobMatch {
            job_id: String::new(),
            url: "https://example.com/v".into(),
            status: "probing".into(),
            title: "https://example.com/v".into(),
        },
    ];
    assert_eq!(
        serde_json::to_value(matches).unwrap(),
        samples()["ActiveJobMatch"]
    );
}

#[test]
fn enums_serialise_as_the_sampled_snake_case_strings() {
    let tiers = [
        ExtractionTier::Dedicated,
        ExtractionTier::GenericExtractor,
        ExtractionTier::PageSniff,
        ExtractionTier::DirectFile,
    ];
    assert_eq!(
        serde_json::to_value(tiers).unwrap(),
        samples()["ExtractionTier"]
    );
    let kinds = [MediaFetchKind::Extractor, MediaFetchKind::DirectHttp];
    assert_eq!(
        serde_json::to_value(kinds).unwrap(),
        samples()["MediaFetchKind"]
    );
}

#[test]
fn probe_types_match_the_samples() {
    let preview = ProbeDestinationPreview {
        path_display: "/lib/jobs/\u{2026}".into(),
    };
    assert_eq!(
        serde_json::to_value(preview).unwrap(),
        samples()["ProbeDestinationPreview"]
    );

    let result = ProbeResult {
        url: "https://example.com/v".into(),
        items: vec![MediaItem {
            id: "v1".into(),
            title: "Video".into(),
            url: "https://example.com/v?id=1".into(),
            qualities: vec![Quality {
                id: "1080p".into(),
                label: "1080p".into(),
                size_bytes: Some(1000),
                container: Some("mp4".into()),
            }],
            archived: false,
            uploader: None,
            upload_date: Some("20260101".into()),
            fetch_kind: MediaFetchKind::Extractor,
            qualities_are_presets: false,
            referer: None,
        }],
        source_name: None,
        tier: ExtractionTier::Dedicated,
        extractor_key: Some("Example".into()),
        warnings: vec!["note".into()],
    };
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        samples()["ProbeResult"]
    );
    // Probe results also come back from the extractor plugins as JSON.
    let parsed: ProbeResult = serde_json::from_value(samples()["ProbeResult"].clone()).unwrap();
    assert_eq!(parsed, result);
}
