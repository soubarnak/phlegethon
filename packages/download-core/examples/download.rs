//! End-to-end download through the Core with the real toolchain, the way the app does it:
//! probe, choose a quality, confirm, run the transfer, check the file.
//!
//! ```text
//! cargo run -p download-core --example download -- <url> --out DIR [--max-height 240] [--items 1]
//! ```
//!
//! `DIR` should be new or empty. Exits non-zero unless every job completed with a file
//! on disk. Uses the plugins embedded in the Core (installed into `PHLEGETHON_SIDECAR_ROOT`
//! or a scratch directory) and honours `HTTPS_PROXY`.

use download_core::{
    select_default_quality, Core, DefaultQualityPolicy, ItemSelection, ProbeSelection,
    ProdHttpBackend, YtDlpConfig, YtDlpExtractorToolchain,
};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut url = None;
    let mut out = None;
    let mut max_height = 240u32;
    let mut items = 1usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--max-height" => {
                max_height = value("--max-height")?.parse().map_err(|_| "bad --max-height")?
            }
            "--items" => items = value("--items")?.parse().map_err(|_| "bad --items")?,
            other if !other.starts_with("--") && url.is_none() => url = Some(other.to_string()),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    let url = url.ok_or("usage: download <url> --out DIR [--max-height N] [--items N]")?;
    let out = out.ok_or("--out DIR is required")?;
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let out = out.canonicalize().map_err(|e| e.to_string())?;

    let root = std::env::var_os("PHLEGETHON_SIDECAR_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("phlegethon-probe-sidecar"));
    download_core::ensure_bundled_plugins(&root)?;
    let toolchain = YtDlpExtractorToolchain::new(YtDlpConfig::discover(Some(&root)));
    let (mut core, _events) = Core::new(Box::new(ProdHttpBackend::new()), Box::new(toolchain));
    core.set_library_root(out.clone())?;

    let probe = core.probe(&url)?;
    println!(
        "probed: {} item(s), tier {}, extractor {}",
        probe.items.len(),
        serde_json::to_string(&probe.tier).unwrap_or_default(),
        probe.extractor_key.as_deref().unwrap_or("-")
    );
    let policy = DefaultQualityPolicy { max_height, prefer_mp4: true };
    let selected: Vec<ItemSelection> = probe
        .items
        .iter()
        .filter(|i| !i.archived)
        .take(items)
        .filter_map(|item| {
            select_default_quality(&item.qualities, &policy, false).map(|quality_id| {
                println!("  {} -> {quality_id}", item.title);
                ItemSelection { item_id: item.id.clone(), quality_id }
            })
        })
        .collect();
    if selected.is_empty() {
        return Err("nothing to download".to_string());
    }
    let selection = ProbeSelection { url: url.clone(), items: selected, media_extras: None };
    let job_ids = core.confirm_probe_selection(&selection, None)?;
    println!("queued {} job(s); downloading...", job_ids.len());
    core.process_pending()?;

    let mut failed = false;
    for snap in core.list_job_snapshots() {
        let size = snap
            .path
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        println!(
            "{} {} ({} bytes on disk) {}",
            snap.status,
            snap.path.as_deref().unwrap_or("-"),
            size,
            snap.last_error.as_deref().unwrap_or("")
        );
        if snap.status != "completed" || size == 0 {
            failed = true;
        }
    }
    if failed {
        Err("at least one job did not complete".to_string())
    } else {
        Ok(())
    }
}
