//! WS2: Filename Template, URL input, Download Archive and probe bookkeeping
//! (M-11, M-12, M-18, M-19, L-1, L-2, L-3, L-4, L-6, L-12).

use download_core::*;
use std::io::Write;
use std::path::Path;

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

const VIDEO: &str = "https://www.youtube.com/watch?v=ws2fn";

fn core_with(
    toolchain: FakeExtractorToolchain,
    root: &Path,
) -> (Core, crossbeam_channel::Receiver<CoreEvent>) {
    let (mut core, rx) = Core::new(Box::new(NoHttp), Box::new(toolchain));
    core.set_library_root(root.to_path_buf()).unwrap();
    (core, rx)
}

fn vars<'a>(title: &'a str) -> TemplateVars<'a> {
    TemplateVars {
        title,
        id: "abc",
        uploader: None,
        upload_date: None,
        ext: "mp4",
    }
}

// --- M-11: Filename Template must stay inside the Job Destination ----------

#[test]
fn m11_template_with_path_escape_is_rejected_and_not_stored() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = core_with(FakeExtractorToolchain::new(), &tmp.path().join("lib"));
    let before = core.filename_template().to_string();
    for bad in [
        "/etc/%(title)s.%(ext)s",
        "../%(title)s.%(ext)s",
        "sub/%(title)s.%(ext)s",
        "sub\\%(title)s.%(ext)s",
        "~/%(title)s.%(ext)s",
        "~%(title)s.%(ext)s",
        "$HOME/%(title)s.%(ext)s",
        "%(title)s$X.%(ext)s",
        "..",
        ".",
        "   ",
    ] {
        assert!(
            core.set_filename_template(bad.to_string()).is_err(),
            "accepted {bad:?}"
        );
        assert_eq!(core.filename_template(), before);
    }
    core.set_filename_template("%(uploader)s - %(title)s.%(ext)s".to_string())
        .unwrap();
    assert_eq!(core.filename_template(), "%(uploader)s - %(title)s.%(ext)s");
}

#[test]
fn m11_stored_escaping_template_is_reset_on_load() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("lib");
    {
        let (mut core, _rx) = core_with(FakeExtractorToolchain::new(), &root);
        core.set_filename_template("%(id)s.%(ext)s".to_string())
            .unwrap();
    }
    let queue_path = root.join(".phlegethon").join("jobs.json");
    let mut queue: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&queue_path).unwrap()).unwrap();
    queue["filename_template"] = serde_json::json!("/tmp/outside/%(id)s.%(ext)s");
    std::fs::write(&queue_path, serde_json::to_vec(&queue).unwrap()).unwrap();

    let (mut core, _rx) = Core::new(Box::new(NoHttp), Box::new(FakeExtractorToolchain::new()));
    let report = core.set_library_root(root).unwrap();
    assert_eq!(core.filename_template(), DEFAULT_FILENAME_TEMPLATE);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("Filename Template")),
        "got: {:?}",
        report.warnings
    );
}

// --- M-18: names are capped below the 255-byte filesystem limit ------------

#[test]
fn m18_long_names_are_capped_on_a_char_boundary_and_keep_the_extension() {
    let long_ascii = format!("{}.mp4", "a".repeat(400));
    let capped = sanitize_filename(&long_ascii);
    assert!(capped.len() <= MAX_FILENAME_BYTES, "len {}", capped.len());
    assert!(capped.ends_with(".mp4"));

    let long_wide = format!("{}.mkv", "é".repeat(300));
    let capped = sanitize_filename(&long_wide);
    assert!(capped.len() <= MAX_FILENAME_BYTES, "len {}", capped.len());
    assert!(capped.ends_with(".mkv"));
    assert!(capped.starts_with('é'));

    let short = "Clip [abc].mp4";
    assert_eq!(sanitize_filename(short), short);
}

#[test]
fn m18_extractor_output_for_a_huge_title_is_capped_and_concrete() {
    let tmp = tempfile::tempdir().unwrap();
    let title = "t".repeat(500);
    let (spec, name) = prepare_extractor_output(tmp.path(), "%(title)s.%(ext)s", &vars(&title));
    assert!(name.len() <= MAX_FILENAME_BYTES, "len {}", name.len());
    assert!(name.ends_with(".mp4"));
    assert_eq!(spec, name, "yt-dlp must be given the capped name");
}

// --- L-2 / L-3 / L-6: name derivation --------------------------------------

#[test]
fn l2_template_literals_keep_non_ascii_characters() {
    let rendered = render_template("Café – %(id)s.%(ext)s", &vars("t"));
    assert_eq!(rendered, "Café – abc.mp4");
}

#[test]
fn l3_dot_names_become_download() {
    assert_eq!(sanitize_filename("."), "download");
    assert_eq!(sanitize_filename(".."), "download");
    assert_eq!(sanitize_filename(" .. "), "download");
    assert_eq!(sanitize_filename("..."), "...");
}

#[test]
fn l6_url_derived_name_is_percent_decoded_and_safe() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = core_with(FakeExtractorToolchain::new(), &tmp.path().join("lib"));
    let mut name_for = |url: &str| {
        let EnqueueOutcome::EnqueueDirect { job_id, .. } = core.enqueue(url, None).unwrap() else {
            panic!("expected a direct job");
        };
        core.list_job_snapshots()
            .into_iter()
            .find(|j| j.job_id == job_id)
            .unwrap()
            .title
    };
    assert_eq!(name_for("https://example.com/My%20File.zip"), "My File.zip");
    assert_eq!(name_for("https://example.com/a%2Fb.zip"), "a_b.zip");
    assert_eq!(name_for("https://example.com/caf%C3%A9.zip"), "café.zip");
}

// --- M-19: URL splitter ----------------------------------------------------

#[test]
fn m19_comma_inside_a_url_does_not_truncate_it() {
    assert_eq!(
        split_urls("https://example.com/dl?ids=1,2,3"),
        vec!["https://example.com/dl?ids=1,2,3".to_string()]
    );
    assert_eq!(
        split_urls("https://a.example/x, https://b.example/y,"),
        vec![
            "https://a.example/x".to_string(),
            "https://b.example/y".to_string()
        ]
    );
    assert_eq!(
        split_urls("https://a.example/x,https://b.example/y"),
        vec![
            "https://a.example/x".to_string(),
            "https://b.example/y".to_string()
        ]
    );
}

#[test]
fn m19_rejected_tokens_are_reported() {
    let report = split_urls_report("https://a.example/x ftp://b.example/y");
    assert_eq!(report.urls, vec!["https://a.example/x".to_string()]);
    assert_eq!(report.rejected, vec!["ftp://b.example/y".to_string()]);
}

// --- M-12 / L-12: Download Archive -----------------------------------------

fn entry(media_id: &str, job_id: &str, outcome: ArchiveOutcome) -> ArchiveEntry {
    ArchiveEntry {
        media_id: media_id.to_string(),
        title: format!("title {media_id}"),
        url: format!("https://example.com/{media_id}"),
        completed_at_unix_secs: 100,
        destination: std::path::PathBuf::from("/tmp/x"),
        job_id: job_id.to_string(),
        outcome,
        size_bytes: None,
    }
}

#[test]
fn m12_failed_record_does_not_replace_a_completed_entry() {
    let mut archive = DownloadArchive::default();
    archive.record(entry("yt-1", "job-a", ArchiveOutcome::Completed));
    archive.record(entry("yt-1", "job-b", ArchiveOutcome::Failed));
    assert!(archive.contains_completed("yt-1"));
    assert_eq!(archive.list()[0].job_id, "job-a");

    // A later Completed record still wins, and a Failed-only id stays Failed.
    archive.record(entry("yt-1", "job-c", ArchiveOutcome::Completed));
    assert_eq!(archive.list()[0].job_id, "job-c");
    archive.record(entry("yt-2", "job-d", ArchiveOutcome::Failed));
    assert!(!archive.contains_completed("yt-2"));
    assert!(archive.contains("yt-2"));
}

#[test]
fn l12_two_writers_do_not_lose_each_others_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();

    let mut seed = DownloadArchive::default();
    seed.record(entry("old", "job-0", ArchiveOutcome::Completed));
    seed.save(&root).unwrap();

    let mut a = DownloadArchive::load(&root).unwrap();
    let mut b = DownloadArchive::load(&root).unwrap();
    a.record(entry("from-a", "job-a", ArchiveOutcome::Completed));
    b.record(entry("from-b", "job-b", ArchiveOutcome::Completed));
    a.save(&root).unwrap();
    b.save(&root).unwrap();

    let merged = DownloadArchive::load(&root).unwrap();
    for id in ["old", "from-a", "from-b"] {
        assert!(merged.contains(id), "lost {id}");
    }
    assert!(b.contains("from-a"), "writer adopts the merged contents");
}

#[test]
fn l12_removal_survives_a_concurrent_writer() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let mut seed = DownloadArchive::default();
    seed.record(entry("gone", "job-0", ArchiveOutcome::Completed));
    seed.record(entry("kept", "job-1", ArchiveOutcome::Completed));
    seed.save(&root).unwrap();

    let mut a = DownloadArchive::load(&root).unwrap();
    let mut b = DownloadArchive::load(&root).unwrap();
    assert!(a.remove("gone").is_some());
    b.record(entry("new", "job-2", ArchiveOutcome::Completed));
    a.save(&root).unwrap();
    b.save(&root).unwrap();

    let merged = DownloadArchive::load(&root).unwrap();
    assert!(!merged.contains("gone"), "removal was resurrected");
    assert!(merged.contains("kept") && merged.contains("new"));
}

// --- L-1: probe bookkeeping ------------------------------------------------

fn one_video() -> FakeExtractorToolchain {
    let mut toolchain = FakeExtractorToolchain::new();
    toolchain.insert(
        VIDEO,
        "Clip",
        "https://cdn.youtube.com/ws2fn.mp4",
        vec![("720p", "720p")],
    );
    toolchain
}

#[test]
fn l1_failed_reprobe_drops_the_earlier_result() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = core_with(one_video(), &tmp.path().join("lib"));
    let probed = core.probe(VIDEO).unwrap();
    let item_id = probed.items[0].id.clone();

    let plan = core.plan_probe(VIDEO).unwrap();
    core.finish_probe(&plan, Err("network down".to_string()))
        .unwrap_err();

    let selection = ProbeSelection {
        url: VIDEO.to_string(),
        items: vec![ItemSelection {
            item_id,
            quality_id: "720p".to_string(),
        }],
        media_extras: None,
    };
    let err = core.confirm_probe_selection(&selection, None).unwrap_err();
    assert!(err.contains("no pending probe"), "got: {err}");
    assert!(core.list_job_snapshots().is_empty());
}

#[test]
fn l1_finishing_one_probe_keeps_the_cancel_flag_of_another_for_the_same_url() {
    let url = "https://slow.example.org/page";
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _rx) = core_with(FakeExtractorToolchain::new(), &tmp.path().join("lib"));
    core.set_page_sniffer(std::sync::Arc::new(FakePageSniffer::new()));
    let first = core.plan_probe(url).unwrap();
    let second = core.plan_probe(url).unwrap();

    core.finish_probe(&first, Err("done".to_string()))
        .unwrap_err();
    assert!(
        core.cancel_probe(url),
        "the second probe is still in flight"
    );
    let err = second.run().unwrap_err();
    assert!(err.contains("cancelled"), "got: {err}");
    core.finish_probe(&second, Err(err)).unwrap_err();
    assert!(!core.cancel_probe(url));
}

// --- L-4: reorder emits an event -------------------------------------------

#[test]
fn l4_reorder_job_emits_an_event_after_persisting() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("lib");
    let (mut core, rx) = core_with(FakeExtractorToolchain::new(), &root);
    for i in 0..3 {
        core.enqueue(&format!("https://example.com/f{i}.zip"), None)
            .unwrap();
    }
    rx.try_iter().for_each(drop);

    core.reorder_job(0, 2).unwrap();
    let events: Vec<_> = rx.try_iter().collect();
    assert!(events.contains(&CoreEvent::JobsReordered {
        from_index: 0,
        to_index: 2
    }));

    // A no-op reorder announces nothing.
    core.reorder_job(1, 1).unwrap();
    assert!(rx.try_iter().next().is_none());
}
