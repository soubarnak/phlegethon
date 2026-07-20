//! Integration tests for YtDlpExtractorToolchain with canned JSON fixtures.
//!
//! These tests use FakeYtDlpExtractor (no live subprocess) to verify
//! JSON parsing, extractor key validation, and quality mapping.

use download_core::{
    ExtractorToolchain, FakeYtDlpExtractor,
};

const CANNED_YOUTUBE_JSON: &str = r#"{
  "id": "dQw4w9WgXcQ",
  "title": "Rick Astley - Never Gonna Give You Up (Official Music Video)",
  "extractor": "youtube",
  "extractor_key": "Youtube",
  "webpage_url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
  "original_url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
  "duration": 212.0,
  "formats": [
    {"format_id":"18","format_note":"360p","ext":"mp4","height":360,"width":640,"tbr":250000.0,"vcodec":"avc1.42001E","acodec":"mp4a.40.2","resolution":"640x360"},
    {"format_id":"22","format_note":"720p","ext":"mp4","height":720,"width":1280,"tbr":1150000.0,"vcodec":"avc1.64001F","acodec":"mp4a.40.2","resolution":"1280x720"},
    {"format_id":"137","format_note":"1080p","ext":"mp4","height":1080,"width":1920,"tbr":4500000.0,"vcodec":"avc1.640028","acodec":"none","resolution":"1920x1080"},
    {"format_id":"140","format_note":"medium","ext":"m4a","tbr":130000.0,"vcodec":"none","acodec":"mp4a.40.2"},
    {"format_id":"247","format_note":"720p (60fps)","ext":"webm","height":720,"width":1280,"tbr":2000000.0,"vcodec":"vp9","acodec":"none","resolution":"1280x720","fps":60.0},
    {"format_id":"251","format_note":"medium","ext":"webm","tbr":160000.0,"vcodec":"none","acodec":"opus"}
  ]
}"#;

const CANNED_GENERIC_JSON: &str = r#"{
  "id": "unknown",
  "title": "Generic Fallback",
  "extractor": "generic",
  "extractor_key": "Generic",
  "webpage_url": "https://luxuretv.com/video/12345",
  "original_url": "https://luxuretv.com/video/12345",
  "formats": [
    {"format_id":"default","format_note":"default","ext":"mp4","height":720,"width":1280,"tbr":1000000.0,"vcodec":"avc1","acodec":"mp4a","resolution":"1280x720"}
  ]
}"#;

#[test]
fn fake_ytdlp_extractor_probe_youtube_single_video() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert(
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        CANNED_YOUTUBE_JSON,
    );

    let result = fake
        .probe("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
        .unwrap();

    assert_eq!(result.items.len(), 1);
    assert_eq!(
        result.items[0].title,
        "Rick Astley - Never Gonna Give You Up (Official Music Video)"
    );
    assert_eq!(result.source_name, None);
}

#[test]
fn fake_ytdlp_extractor_qualities_include_resolutions() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert(
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        CANNED_YOUTUBE_JSON,
    );

    let result = fake
        .probe("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
        .unwrap();
    let qualities = &result.items[0].qualities;

    let has_360p = qualities.iter().any(|q| q.id == "360p");
    let has_720p = qualities.iter().any(|q| q.id == "720p");
    assert!(has_360p, "should have 360p, got: {qualities:?}");
    assert!(has_720p, "should have 720p, got: {qualities:?}");

    let has_1080p_videoonly = qualities.iter().any(|q| q.id == "1080p-videoonly");
    assert!(has_1080p_videoonly, "should have 1080p video-only");

    let has_audio = qualities.iter().any(|q| q.id.ends_with("-audio"));
    assert!(has_audio, "should have audio-only quality");
}

#[test]
fn generic_extractor_hard_fails() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert(
        "https://luxuretv.com/video/12345",
        CANNED_GENERIC_JSON,
    );

    let err = fake
        .probe("https://luxuretv.com/video/12345")
        .unwrap_err();
    assert!(
        err.contains("generic extractor") || err.contains("unsupported"),
        "generic extractor should hard-fail, got: {err}"
    );
}

#[test]
fn fake_extractor_fails_on_unknown_url() {
    let fake = FakeYtDlpExtractor::new();
    let err = fake.probe("https://www.youtube.com/watch?v=unknown").unwrap_err();
    assert!(err.contains("no canned response"), "got: {err}");
}

#[test]
fn fake_extractor_fails_on_configured_failure() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert_failure("https://www.youtube.com/watch?v=fail");
    let err = fake.probe("https://www.youtube.com/watch?v=fail").unwrap_err();
    assert!(err.contains("process error"), "got: {err}");
}

#[test]
fn quality_ids_are_unique() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert(
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        CANNED_YOUTUBE_JSON,
    );

    let result = fake
        .probe("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
        .unwrap();
    let qualities = &result.items[0].qualities;

    let mut ids: Vec<&str> = qualities.iter().map(|q| q.id.as_str()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), qualities.len(), "quality IDs must be unique");
}
