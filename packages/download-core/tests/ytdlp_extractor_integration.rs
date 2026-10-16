//! Integration tests for YtDlpExtractorToolchain with canned JSON fixtures.
//!
//! These tests use FakeYtDlpExtractor (no live subprocess) to verify
//! JSON parsing, extractor key validation, and quality mapping.
//! CI defaults to fixtures — never live Launch Site network.

use download_core::{Core, ExtractorToolchain, FakeYtDlpExtractor, ItemSelection, ProbeSelection};

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

const CANNED_YOUTUBE_PLAYLIST_JSON: &str = r#"{
  "_type": "playlist",
  "id": "PLabc123",
  "title": "My Awesome Playlist",
  "extractor": "youtube:playlist",
  "extractor_key": "YoutubePlaylist",
  "webpage_url": "https://www.youtube.com/playlist?list=PLabc123",
  "original_url": "https://www.youtube.com/playlist?list=PLabc123",
  "entries": [
    {"_type":"url","id":"vidOne111","title":"First Playlist Video","url":"https://www.youtube.com/watch?v=vidOne111","ie_key":"Youtube"},
    {"_type":"url","id":"vidTwo222","title":"Second Playlist Video","url":"https://www.youtube.com/watch?v=vidTwo222","ie_key":"Youtube"},
    null
  ]
}"#;

const CANNED_XHAMSTER_JSON: &str = r#"{
  "id": "xh-video-12345",
  "title": "Example xHamster Video",
  "extractor": "xhamster",
  "extractor_key": "XHamster",
  "webpage_url": "https://xhamster.com/videos/example-xh-video-12345",
  "original_url": "https://xhamster.com/videos/example-xh-video-12345",
  "formats": [
    {"format_id":"240p","format_note":"240p","ext":"mp4","height":240,"width":426,"tbr":300000.0,"vcodec":"avc1","acodec":"mp4a.40.2","resolution":"426x240"},
    {"format_id":"720p","format_note":"720p","ext":"mp4","height":720,"width":1280,"tbr":1500000.0,"vcodec":"avc1","acodec":"mp4a.40.2","resolution":"1280x720"},
    {"format_id":"hls-1080","format_note":"1080p","ext":"mp4","height":1080,"width":1920,"tbr":4000000.0,"vcodec":"avc1","acodec":"mp4a.40.2","resolution":"1920x1080"}
  ]
}"#;

const CANNED_XHAMSTER_USER_JSON: &str = r#"{
  "_type": "playlist",
  "id": "example-creator",
  "title": "example-creator",
  "extractor": "xhamster:user",
  "extractor_key": "XHamsterUser",
  "webpage_url": "https://xhamster.com/creators/example-creator",
  "original_url": "https://xhamster.com/creators/example-creator",
  "entries": [
    {"_type":"url","id":"xh-item-aaa","title":"Creator Video A","url":"https://xhamster.com/videos/creator-video-a-xh-item-aaa","ie_key":"XHamster"},
    {"_type":"url","id":"xh-item-bbb","title":"Creator Video B","url":"https://xhamster.com/videos/creator-video-b-xh-item-bbb","ie_key":"XHamster"}
  ]
}"#;

const CANNED_GENERIC_JSON: &str = r#"{
  "id": "unknown",
  "title": "Generic Fallback",
  "extractor": "generic",
  "extractor_key": "Generic",
  "webpage_url": "https://example.com/video/12345",
  "original_url": "https://example.com/video/12345",
  "formats": [
    {"format_id":"default","format_note":"default","ext":"mp4","height":720,"width":1280,"tbr":1000000.0,"vcodec":"avc1","acodec":"mp4a","resolution":"1280x720"}
  ]
}"#;

const CANNED_LUXURETV_JSON: &str = r#"{
  "id": "16982",
  "title": "Sample LuxureTV Video",
  "extractor": "LuxureTV",
  "extractor_key": "LuxureTV",
  "webpage_url": "https://www.luxuretv.com/videos/sample-video-16982.html",
  "original_url": "https://www.luxuretv.com/videos/sample-video-16982.html",
  "formats": [
    {"format_id":"480p","format_note":"480p","ext":"mp4","height":480,"width":854,"tbr":800000.0,"vcodec":"avc1","acodec":"mp4a","resolution":"854x480"},
    {"format_id":"720p","format_note":"720p","ext":"mp4","height":720,"width":1280,"tbr":1600000.0,"vcodec":"avc1","acodec":"mp4a","resolution":"1280x720"},
    {"format_id":"1080p","format_note":"1080p","ext":"mp4","height":1080,"width":1920,"tbr":3200000.0,"vcodec":"avc1","acodec":"mp4a","resolution":"1920x1080"}
  ]
}"#;

const CANNED_XXXBP_JSON: &str = r#"{
  "id": "12345",
  "title": "Sample XXXBP Video",
  "extractor": "XxxbpTV",
  "extractor_key": "XxxbpTV",
  "webpage_url": "https://www.xxxbp.tv/v/12345",
  "original_url": "https://www.xxxbp.tv/v/12345",
  "formats": [
    {"format_id":"360p","format_note":"360p","ext":"mp4","height":360,"width":640,"tbr":500000.0,"vcodec":"avc1","acodec":"mp4a","resolution":"640x360"},
    {"format_id":"720p","format_note":"720p","ext":"mp4","height":720,"width":1280,"tbr":1500000.0,"vcodec":"avc1","acodec":"mp4a","resolution":"1280x720"}
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
        .probe(
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            &download_core::CookieChannel::default(),
            &download_core::ProxySettings::default(),
        )
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
        .probe(
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            &download_core::CookieChannel::default(),
            &download_core::ProxySettings::default(),
        )
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
fn fake_ytdlp_extractor_probe_youtube_playlist_multi_item() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert(
        "https://www.youtube.com/playlist?list=PLabc123",
        CANNED_YOUTUBE_PLAYLIST_JSON,
    );

    let result = fake
        .probe(
            "https://www.youtube.com/playlist?list=PLabc123",
            &download_core::CookieChannel::default(),
            &download_core::ProxySettings::default(),
        )
        .unwrap();

    assert_eq!(result.items.len(), 2);
    assert_eq!(result.source_name, Some("My Awesome Playlist".to_string()));
    assert!(result
        .items
        .iter()
        .all(|i| i.qualities.iter().any(|q| q.id == "best")));
}

#[test]
fn fake_ytdlp_extractor_probe_xhamster_single_video() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert(
        "https://xhamster.com/videos/example-xh-video-12345",
        CANNED_XHAMSTER_JSON,
    );

    let result = fake
        .probe(
            "https://xhamster.com/videos/example-xh-video-12345",
            &download_core::CookieChannel::default(),
            &download_core::ProxySettings::default(),
        )
        .unwrap();

    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].id, "xh-xh-video-12345");
    assert!(result.items[0].qualities.iter().any(|q| q.id == "720p"));
    assert_eq!(result.source_name, None);
}

#[test]
fn fake_ytdlp_extractor_probe_xhamster_user_multi_item() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert(
        "https://xhamster.com/creators/example-creator",
        CANNED_XHAMSTER_USER_JSON,
    );

    let result = fake
        .probe(
            "https://xhamster.com/creators/example-creator",
            &download_core::CookieChannel::default(),
            &download_core::ProxySettings::default(),
        )
        .unwrap();

    assert_eq!(result.items.len(), 2);
    assert_eq!(result.source_name, Some("example-creator".to_string()));
}

#[test]
fn generic_extractor_hard_fails() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert("https://example.com/video/12345", CANNED_GENERIC_JSON);

    let err = fake
        .probe(
            "https://example.com/video/12345",
            &download_core::CookieChannel::default(),
            &download_core::ProxySettings::default(),
        )
        .unwrap_err();
    assert!(
        err.contains("generic extractor") || err.contains("unsupported"),
        "generic extractor should hard-fail, got: {err}"
    );
}

#[test]
fn luxuretv_plugin_probe_returns_quality_list() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert(
        "https://www.luxuretv.com/videos/sample-video-16982.html",
        CANNED_LUXURETV_JSON,
    );

    let result = fake
        .probe(
            "https://www.luxuretv.com/videos/sample-video-16982.html",
            &download_core::CookieChannel::default(),
            &download_core::ProxySettings::default(),
        )
        .unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].title, "Sample LuxureTV Video");
    let qualities = &result.items[0].qualities;
    assert!(qualities.iter().any(|q| q.id == "720p"), "{qualities:?}");
    assert!(qualities.iter().any(|q| q.id == "1080p"), "{qualities:?}");
}

#[test]
fn xxxbp_plugin_probe_returns_quality_list() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert("https://www.xxxbp.tv/v/12345", CANNED_XXXBP_JSON);

    let result = fake
        .probe(
            "https://www.xxxbp.tv/v/12345",
            &download_core::CookieChannel::default(),
            &download_core::ProxySettings::default(),
        )
        .unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].title, "Sample XXXBP Video");
    let qualities = &result.items[0].qualities;
    assert!(qualities.iter().any(|q| q.id == "360p"), "{qualities:?}");
    assert!(qualities.iter().any(|q| q.id == "720p"), "{qualities:?}");
}

#[test]
fn luxuretv_plugin_miss_generic_hard_fails() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert(
        "https://www.luxuretv.com/videos/sample-video-16982.html",
        CANNED_GENERIC_JSON,
    );
    let err = fake
        .probe(
            "https://www.luxuretv.com/videos/sample-video-16982.html",
            &download_core::CookieChannel::default(),
            &download_core::ProxySettings::default(),
        )
        .unwrap_err();
    assert!(
        err.contains("generic") || err.contains("unsupported"),
        "plugin miss must hard-fail, got: {err}"
    );
}

#[test]
fn fake_extractor_fails_on_unknown_url() {
    let fake = FakeYtDlpExtractor::new();
    let err = fake
        .probe(
            "https://www.youtube.com/watch?v=unknown",
            &download_core::CookieChannel::default(),
            &download_core::ProxySettings::default(),
        )
        .unwrap_err();
    assert!(err.contains("no canned response"), "got: {err}");
}

#[test]
fn fake_extractor_fails_on_configured_failure() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert_failure("https://www.youtube.com/watch?v=fail");
    let err = fake
        .probe(
            "https://www.youtube.com/watch?v=fail",
            &download_core::CookieChannel::default(),
            &download_core::ProxySettings::default(),
        )
        .unwrap_err();
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
        .probe(
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            &download_core::CookieChannel::default(),
            &download_core::ProxySettings::default(),
        )
        .unwrap();
    let qualities = &result.items[0].qualities;

    let mut ids: Vec<&str> = qualities.iter().map(|q| q.id.as_str()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), qualities.len(), "quality IDs must be unique");
}

/// Minimal HTTP backend for Probe-then-Confirm wiring tests (no live network).
struct StubHttp;

impl download_core::HttpBackend for StubHttp {
    fn fetch(
        &self,
        url: &str,
        _range: Option<std::ops::Range<u64>>,
        sink: &mut dyn std::io::Write,
    ) -> Result<download_core::FetchResult, String> {
        let body = format!("body for {url}").into_bytes();
        let len = body.len() as u64;
        let mut written = 0usize;
        while written < body.len() {
            let n = sink
                .write(&body[written..])
                .map_err(|e| format!("sink write: {e}"))?;
            if n == 0 {
                break;
            }
            written += n;
        }
        Ok(download_core::FetchResult {
            total_length: len,
            bytes_written: written as u64,
        })
    }

    fn supports_range(&self, _url: &str) -> Result<bool, String> {
        Ok(false)
    }

    fn file_size(&self, url: &str) -> Result<u64, String> {
        Ok(format!("body for {url}").len() as u64)
    }
}

/// Probe-then-Confirm: jobs are created only after confirm, never on probe alone.
#[test]
fn youtube_playlist_probe_then_confirm_creates_jobs_only_after_confirm() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert(
        "https://www.youtube.com/playlist?list=PLabc123",
        CANNED_YOUTUBE_PLAYLIST_JSON,
    );

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(StubHttp), Box::new(fake));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let probe = core
        .probe("https://www.youtube.com/playlist?list=PLabc123")
        .unwrap();
    assert_eq!(probe.items.len(), 2);

    // Drain probe events — no JobCompleted / transfers yet
    while let Ok(ev) = rx.try_recv() {
        assert!(
            !matches!(ev, download_core::CoreEvent::JobCompleted { .. }),
            "probe must not complete transfers"
        );
    }

    let job_ids = core
        .confirm_probe_selection(
            &ProbeSelection {
                url: "https://www.youtube.com/playlist?list=PLabc123".to_string(),
                items: vec![
                    ItemSelection {
                        item_id: "yt-vidOne111".to_string(),
                        quality_id: "best".to_string(),
                    },
                    ItemSelection {
                        item_id: "yt-vidTwo222".to_string(),
                        quality_id: "best".to_string(),
                    },
                ],
                media_extras: None,
            },
            None,
        )
        .unwrap();
    assert_eq!(
        job_ids.len(),
        2,
        "confirm creates jobs; probe alone does not"
    );
}

#[test]
fn xhamster_single_probe_then_confirm_creates_job_only_after_confirm() {
    let mut fake = FakeYtDlpExtractor::new();
    fake.insert(
        "https://xhamster.com/videos/example-xh-video-12345",
        CANNED_XHAMSTER_JSON,
    );

    let tmp = tempfile::tempdir().unwrap();
    let (mut core, rx) = Core::new(Box::new(StubHttp), Box::new(fake));
    core.set_library_root(tmp.path().join("library")).unwrap();

    let probe = core
        .probe("https://xhamster.com/videos/example-xh-video-12345")
        .unwrap();
    assert_eq!(probe.items.len(), 1);

    while let Ok(ev) = rx.try_recv() {
        assert!(
            !matches!(ev, download_core::CoreEvent::JobCompleted { .. }),
            "probe must not complete transfers"
        );
    }

    let job_ids = core
        .confirm_probe_selection(
            &ProbeSelection {
                url: "https://xhamster.com/videos/example-xh-video-12345".to_string(),
                items: vec![ItemSelection {
                    item_id: "xh-xh-video-12345".to_string(),
                    quality_id: "720p".to_string(),
                }],
                media_extras: None,
            },
            None,
        )
        .unwrap();
    assert_eq!(job_ids.len(), 1);
}
