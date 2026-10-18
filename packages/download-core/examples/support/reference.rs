//! Shared by `examples/probe.rs` and `tests/reference_urls_live.rs`:
//! the reference-URL file format, a standalone probe runner, and the pure
//! `evaluate` that decides pass / blocked / fail.
//!
//! Included with `#[path]` from both, so it must not depend on either.
#![allow(dead_code)]

use download_core::{
    CookieChannel, ExtractionTier, HtmlPageSniffer, ProbePlan, ProbeResult, ProbeTierSettings,
    ProdHttpBackend, ProxySettings, YtDlpConfig, YtDlpExtractorToolchain,
};
use serde::Deserialize;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Deserialize)]
pub struct ReferenceFile {
    pub version: u32,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub id: String,
    pub site: String,
    /// `single`, `listing`, `generic-page` or `direct-file`; documentation only.
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub expect: Expect,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Expect {
    /// Any of these tiers is acceptable (`dedicated`, `generic_extractor`, `page_sniff`, `direct_file`).
    #[serde(default)]
    pub tiers: Vec<String>,
    /// The yt-dlp extractor key must start with this.
    #[serde(default)]
    pub extractor_key_prefix: Option<String>,
    #[serde(default)]
    pub min_items: Option<usize>,
    /// At least one item must offer this many qualities.
    #[serde(default)]
    pub min_qualities: Option<usize>,
    /// A failure carrying this error code counts as `Blocked`, not `Fail`
    /// (for example `needs_browser_cookies` on a Cloudflare-protected site).
    #[serde(default)]
    pub blocked_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    /// Failed in the documented way; supply cookies to get a real answer.
    Blocked(String),
    Fail(String),
}

impl Verdict {
    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Blocked(_) => "BLOCKED",
            Verdict::Fail(_) => "FAIL",
        }
    }
    pub fn is_ok(&self) -> bool {
        !matches!(self, Verdict::Fail(_))
    }
}

#[derive(Debug, Clone)]
pub struct Row {
    pub id: String,
    pub site: String,
    pub verdict: Verdict,
    pub tier: String,
    pub extractor_key: String,
    pub items: usize,
    pub max_qualities: usize,
    pub secs: f64,
}

pub fn load(path: &Path) -> Result<ReferenceFile, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file: ReferenceFile =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if file.version != 1 {
        return Err(format!(
            "unsupported reference file version {}",
            file.version
        ));
    }
    Ok(file)
}

/// `[code] message` -> `code`.
pub fn error_code(err: &str) -> Option<&str> {
    let rest = err.strip_prefix('[')?;
    let (code, _) = rest.split_once(']')?;
    (!code.is_empty() && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')).then_some(code)
}

pub fn tier_name(tier: ExtractionTier) -> String {
    serde_json::to_value(tier)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

pub fn evaluate(expect: &Expect, outcome: &Result<ProbeResult, String>) -> Verdict {
    let result = match outcome {
        Ok(result) => result,
        Err(err) => {
            return match (&expect.blocked_error, error_code(err)) {
                (Some(want), Some(got)) if want == got => Verdict::Blocked(err.clone()),
                _ => Verdict::Fail(format!("probe failed: {err}")),
            };
        }
    };
    let tier = tier_name(result.tier);
    if !expect.tiers.is_empty() && !expect.tiers.contains(&tier) {
        return Verdict::Fail(format!("tier {tier}, expected one of {:?}", expect.tiers));
    }
    if let Some(prefix) = &expect.extractor_key_prefix {
        let key = result.extractor_key.as_deref().unwrap_or("");
        if !key.starts_with(prefix.as_str()) {
            return Verdict::Fail(format!("extractor {key:?}, expected prefix {prefix:?}"));
        }
    }
    if let Some(min) = expect.min_items {
        if result.items.len() < min {
            return Verdict::Fail(format!(
                "{} items, expected at least {min}",
                result.items.len()
            ));
        }
    }
    if let Some(min) = expect.min_qualities {
        let best = result
            .items
            .iter()
            .map(|i| i.qualities.len())
            .max()
            .unwrap_or(0);
        if best < min {
            return Verdict::Fail(format!(
                "best item has {best} qualities, expected at least {min}"
            ));
        }
    }
    Verdict::Pass
}

#[derive(Clone, Default)]
pub struct RunOptions {
    pub settings: ProbeTierSettings,
    pub cookies: CookieChannel,
    pub proxy: ProxySettings,
}

/// Probe with the real toolchain found on this machine, the same way the app does.
pub fn probe(url: &str, opts: &RunOptions) -> (Result<ProbeResult, String>, f64) {
    let started = Instant::now();
    let outcome = (|| {
        // Same embedded plugins the app installs, in a scratch sidecar directory
        // (override with PHLEGETHON_SIDECAR_ROOT, e.g. to reuse an installed yt-dlp).
        let root = std::env::var_os("PHLEGETHON_SIDECAR_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("phlegethon-probe-sidecar"));
        download_core::ensure_bundled_plugins(&root)?;
        let config = YtDlpConfig::discover(Some(&root));
        let extractor = Arc::new(YtDlpExtractorToolchain::new(config));
        let sniffer = Arc::new(HtmlPageSniffer::new(Arc::new(ProdHttpBackend::new())));
        ProbePlan::standalone(
            url,
            extractor,
            sniffer,
            opts.settings,
            opts.cookies.clone(),
            opts.proxy.clone(),
        )?
        .run()
    })();
    (outcome, started.elapsed().as_secs_f64())
}

pub fn run_entry(entry: &Entry, opts: &RunOptions) -> (Row, Result<ProbeResult, String>) {
    let (outcome, secs) = probe(&entry.url, opts);
    let verdict = evaluate(&entry.expect, &outcome);
    let (tier, key, items, max_q) = match &outcome {
        Ok(r) => (
            tier_name(r.tier),
            r.extractor_key.clone().unwrap_or_default(),
            r.items.len(),
            r.items.iter().map(|i| i.qualities.len()).max().unwrap_or(0),
        ),
        Err(_) => (String::new(), String::new(), 0, 0),
    };
    (
        Row {
            id: entry.id.clone(),
            site: entry.site.clone(),
            verdict,
            tier,
            extractor_key: key,
            items,
            max_qualities: max_q,
            secs,
        },
        outcome,
    )
}

pub fn markdown_table(rows: &[Row]) -> String {
    let mut out = String::from(
        "| id | site | result | tier | extractor | items | best qualities | secs | detail |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
    );
    for r in rows {
        let detail = match &r.verdict {
            Verdict::Pass => String::new(),
            Verdict::Blocked(d) | Verdict::Fail(d) => d.replace('|', "\\|").replace('\n', " "),
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {:.1} | {} |\n",
            r.id,
            r.site,
            r.verdict.label(),
            r.tier,
            r.extractor_key,
            r.items,
            r.max_qualities,
            r.secs,
            detail
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use download_core::{MediaFetchKind, MediaItem, Quality};

    fn quality(label: &str) -> Quality {
        Quality {
            id: label.to_string(),
            label: label.to_string(),
            size_bytes: None,
            container: None,
        }
    }

    fn result(
        tier: ExtractionTier,
        key: Option<&str>,
        items: usize,
        qualities: usize,
    ) -> ProbeResult {
        ProbeResult {
            url: "https://example.com".to_string(),
            items: (0..items)
                .map(|n| MediaItem {
                    id: n.to_string(),
                    title: format!("item {n}"),
                    url: format!("https://example.com/{n}"),
                    qualities: (0..qualities).map(|q| quality(&format!("{q}p"))).collect(),
                    archived: false,
                    uploader: None,
                    upload_date: None,
                    fetch_kind: MediaFetchKind::Extractor,
                    qualities_are_presets: false,
                    referer: None,
                })
                .collect(),
            source_name: None,
            tier,
            extractor_key: key.map(str::to_string),
            warnings: vec![],
        }
    }

    #[test]
    fn error_codes_parse() {
        assert_eq!(
            error_code("[needs_browser_cookies] blocked"),
            Some("needs_browser_cookies")
        );
        assert_eq!(error_code("plain message"), None);
        assert_eq!(error_code("[Mixed Case] x"), None);
        assert_eq!(error_code("[] x"), None);
    }

    #[test]
    fn passes_when_everything_matches() {
        let expect = Expect {
            tiers: vec!["dedicated".into()],
            extractor_key_prefix: Some("XHamster".into()),
            min_items: Some(1),
            min_qualities: Some(3),
            blocked_error: None,
        };
        let ok = Ok(result(ExtractionTier::Dedicated, Some("XHamster"), 1, 5));
        assert_eq!(evaluate(&expect, &ok), Verdict::Pass);
    }

    #[test]
    fn each_mismatch_fails_with_a_reason() {
        let expect = Expect {
            tiers: vec!["dedicated".into()],
            extractor_key_prefix: Some("Youtube".into()),
            min_items: Some(2),
            min_qualities: Some(2),
            blocked_error: None,
        };
        let wrong_tier = Ok(result(
            ExtractionTier::GenericExtractor,
            Some("Youtube"),
            2,
            2,
        ));
        assert!(matches!(evaluate(&expect, &wrong_tier), Verdict::Fail(m) if m.contains("tier")));
        let wrong_key = Ok(result(ExtractionTier::Dedicated, Some("Generic"), 2, 2));
        assert!(
            matches!(evaluate(&expect, &wrong_key), Verdict::Fail(m) if m.contains("extractor"))
        );
        let few_items = Ok(result(ExtractionTier::Dedicated, Some("Youtube"), 1, 2));
        assert!(matches!(evaluate(&expect, &few_items), Verdict::Fail(m) if m.contains("items")));
        let few_q = Ok(result(ExtractionTier::Dedicated, Some("Youtube"), 2, 1));
        assert!(matches!(evaluate(&expect, &few_q), Verdict::Fail(m) if m.contains("qualities")));
    }

    #[test]
    fn documented_blocking_error_is_not_a_failure() {
        let expect = Expect {
            blocked_error: Some("needs_browser_cookies".into()),
            ..Expect::default()
        };
        let blocked = Err("[needs_browser_cookies] Cloudflare".to_string());
        assert!(matches!(evaluate(&expect, &blocked), Verdict::Blocked(_)));
        assert!(evaluate(&expect, &blocked).is_ok());
        let other = Err("[timeout] too slow".to_string());
        assert!(matches!(evaluate(&expect, &other), Verdict::Fail(_)));
        assert!(
            !evaluate(&Expect::default(), &blocked).is_ok(),
            "unexpected block is a failure"
        );
    }

    #[test]
    fn table_has_one_row_per_entry_and_escapes_pipes() {
        let rows = vec![Row {
            id: "a".into(),
            site: "s".into(),
            verdict: Verdict::Fail("x | y".into()),
            tier: "dedicated".into(),
            extractor_key: "K".into(),
            items: 1,
            max_qualities: 2,
            secs: 1.25,
        }];
        let table = markdown_table(&rows);
        assert_eq!(table.lines().count(), 3);
        assert!(table.contains("x \\| y"));
        assert!(table.contains("| FAIL |"));
    }
}
