//! Live checks of the reference URLs in `docs/reference-urls.json`.
//!
//! Ignored by default: they need network access and a working yt-dlp.
//!
//! ```text
//! cargo test -p download-core --test reference_urls_live -- --ignored --nocapture
//! PHLEGETHON_REF_FILTER=xhamster cargo test -p download-core --test reference_urls_live -- --ignored --nocapture
//! ```
//!
//! `PHLEGETHON_REF_FILTER` matches the `site` field. Sites that sit behind a browser
//! check report BLOCKED (a documented failure) unless cookies are supplied, so they do not
//! fail the run; set `PHLEGETHON_REF_COOKIES_FROM_BROWSER=firefox` to test them for real.

#[path = "../examples/support/reference.rs"]
mod reference;

use std::path::PathBuf;

fn reference_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/reference-urls.json")
}

/// Not ignored: the reference file must parse and stay well formed.
#[test]
fn reference_file_is_well_formed() {
    let file = reference::load(&reference_file()).expect("reference file loads");
    assert!(!file.entries.is_empty());
    let mut ids = std::collections::HashSet::new();
    for entry in &file.entries {
        assert!(ids.insert(entry.id.clone()), "duplicate id {}", entry.id);
        let url = url::Url::parse(&entry.url).unwrap_or_else(|e| panic!("{}: {e}", entry.id));
        assert!(matches!(url.scheme(), "http" | "https"), "{}", entry.id);
        for tier in &entry.expect.tiers {
            assert!(
                [
                    "dedicated",
                    "generic_extractor",
                    "page_sniff",
                    "direct_file"
                ]
                .contains(&tier.as_str()),
                "{}: unknown tier {tier}",
                entry.id
            );
        }
    }
    // The URLs the product owner supplied must stay in the file.
    for needle in [
        "xhamster.com/videos/your-eshita",
        "xhamster.com/creators/your-eshita",
        "en.luxuretv.com/videos/music-sucks-but-love-the-one-on-the-left-511995.html",
        "xxxbp.tv/video/793979/first-time-sex-with-old-couple",
    ] {
        assert!(
            file.entries.iter().any(|e| e.url.contains(needle)),
            "reference URL missing: {needle}"
        );
    }
}

#[test]
#[ignore = "needs network and yt-dlp"]
fn reference_urls_resolve_with_the_real_toolchain() {
    let file = reference::load(&reference_file()).expect("reference file loads");
    let filter = std::env::var("PHLEGETHON_REF_FILTER").ok();
    let mut opts = reference::RunOptions::default();
    opts.cookies.cookies_from_browser = std::env::var("PHLEGETHON_REF_COOKIES_FROM_BROWSER").ok();

    let mut rows = Vec::new();
    for entry in file.entries.iter().filter(|e| {
        filter
            .as_deref()
            .map_or(true, |f| e.site.eq_ignore_ascii_case(f))
    }) {
        rows.push(reference::run_entry(entry, &opts).0);
    }
    assert!(!rows.is_empty(), "no entries matched PHLEGETHON_REF_FILTER");
    println!("{}", reference::markdown_table(&rows));
    let failures: Vec<_> = rows
        .iter()
        .filter(|r| !r.verdict.is_ok())
        .map(|r| r.id.as_str())
        .collect();
    assert!(failures.is_empty(), "failed: {failures:?}");
}
