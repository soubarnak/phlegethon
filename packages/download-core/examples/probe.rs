//! Probe URLs with the real toolchain, the way the app does.
//!
//! ```text
//! cargo run -p download-core --example probe -- <url> [options]
//! cargo run -p download-core --example probe -- --check docs/reference-urls.json [--site NAME]
//!
//! options:
//!   --no-generic          do not accept yt-dlp's generic extractor
//!   --no-sniff            do not scan the page for media
//!   --no-direct           do not treat non-HTML responses as plain files
//!   --timeout SECS        probe timeout (default 45)
//!   --cookies-from-browser NAME    pass browser cookies to yt-dlp
//!   --cookie-file PATH    Netscape cookie file for yt-dlp
//!   --proxy URL           http(s)/socks proxy
//!   --json                print the probe result as JSON
//! ```
//!
//! `--check` runs every entry of a reference file, prints a markdown table and exits
//! non-zero when any entry FAILs. An entry that fails in its documented way (for example
//! `needs_browser_cookies` on a Cloudflare-protected site) is reported as BLOCKED and
//! does not fail the run.

#[path = "support/reference.rs"]
mod reference;

use download_core::ProbeResult;
use reference::{Entry, RunOptions, Verdict};
use std::path::PathBuf;
use std::process::ExitCode;

struct Args {
    url: Option<String>,
    check: Option<PathBuf>,
    site: Option<String>,
    json: bool,
    opts: RunOptions,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        url: None,
        check: None,
        site: None,
        json: false,
        opts: RunOptions::default(),
    };
    let mut it = std::env::args().skip(1);
    let value = |it: &mut std::iter::Skip<std::env::Args>, flag: &str| {
        it.next().ok_or_else(|| format!("{flag} needs a value"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--check" => args.check = Some(PathBuf::from(value(&mut it, "--check")?)),
            "--site" => args.site = Some(value(&mut it, "--site")?),
            "--json" => args.json = true,
            "--no-generic" => args.opts.settings.allow_generic = false,
            "--no-sniff" => args.opts.settings.allow_page_sniff = false,
            "--no-direct" => args.opts.settings.allow_direct_fallback = false,
            "--timeout" => {
                args.opts.settings.probe_timeout_secs = value(&mut it, "--timeout")?
                    .parse()
                    .map_err(|_| "--timeout must be a number of seconds".to_string())?
            }
            "--cookies-from-browser" => {
                args.opts.cookies.cookies_from_browser =
                    Some(value(&mut it, "--cookies-from-browser")?)
            }
            "--cookie-file" => {
                args.opts.cookies.cookie_file =
                    Some(PathBuf::from(value(&mut it, "--cookie-file")?))
            }
            "--proxy" => args.opts.proxy.url = Some(value(&mut it, "--proxy")?),
            "-h" | "--help" => return Err(String::new()),
            other if other.starts_with("--") => return Err(format!("unknown option {other}")),
            other => {
                if args.url.replace(other.to_string()).is_some() {
                    return Err("only one URL is accepted (use --check for many)".to_string());
                }
            }
        }
    }
    if args.url.is_none() && args.check.is_none() {
        return Err("give a URL or --check FILE".to_string());
    }
    Ok(args)
}

fn print_result(result: &ProbeResult, secs: f64, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(result).unwrap_or_default()
        );
        return;
    }
    println!("URL        {}", result.url);
    println!("Tier       {}", reference::tier_name(result.tier));
    println!(
        "Extractor  {}",
        result.extractor_key.as_deref().unwrap_or("-")
    );
    println!("Time       {secs:.1}s");
    println!("Items      {}", result.items.len());
    for w in &result.warnings {
        println!("Warning    {w}");
    }
    for (n, item) in result.items.iter().take(20).enumerate() {
        let qualities: Vec<&str> = item.qualities.iter().map(|q| q.label.as_str()).collect();
        println!(
            "{:>3}. {}{}\n     {}\n     qualities: {}",
            n + 1,
            item.title,
            if item.archived {
                "  [already downloaded]"
            } else {
                ""
            },
            item.url,
            if qualities.is_empty() {
                "-".to_string()
            } else {
                qualities.join(", ")
            }
        );
    }
    if result.items.len() > 20 {
        println!("... and {} more", result.items.len() - 20);
    }
}

fn run_check(path: &PathBuf, site: Option<&str>, opts: &RunOptions) -> ExitCode {
    let file = match reference::load(path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let entries: Vec<&Entry> = file
        .entries
        .iter()
        .filter(|e| site.map_or(true, |s| e.site.eq_ignore_ascii_case(s)))
        .collect();
    if entries.is_empty() {
        eprintln!("no entries match --site {}", site.unwrap_or(""));
        return ExitCode::from(2);
    }
    let mut rows = Vec::new();
    for entry in entries {
        eprintln!("probing {} ({})", entry.id, entry.url);
        rows.push(reference::run_entry(entry, opts).0);
    }
    print!("{}", reference::markdown_table(&rows));
    if rows.iter().all(|r| r.verdict.is_ok()) {
        if rows
            .iter()
            .any(|r| matches!(r.verdict, Verdict::Blocked(_)))
        {
            eprintln!("note: BLOCKED rows need browser cookies (--cookies-from-browser NAME) for a real check");
        }
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("error: {msg}\n");
            }
            eprintln!(
                "{}",
                include_str!("probe.rs")
                    .lines()
                    .skip(2)
                    .take(21)
                    .map(|l| l.trim_start_matches("//! ").trim_start_matches("//!"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            return ExitCode::from(2);
        }
    };
    if let Some(path) = &args.check {
        return run_check(path, args.site.as_deref(), &args.opts);
    }
    let url = args.url.as_deref().unwrap_or_default();
    let (outcome, secs) = reference::probe(url, &args.opts);
    match outcome {
        Ok(result) => {
            print_result(&result, secs, args.json);
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("probe failed after {secs:.1}s: {err}");
            ExitCode::FAILURE
        }
    }
}
