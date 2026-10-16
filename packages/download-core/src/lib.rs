//! Download Core — shared Rust library for download management.
//!
//! The Core owns URL classification, job lifecycle, and storage.
//! Platform shells consume this library via its public command/event API.

mod bandwidth;
pub mod classification;
mod cookie_channel;
mod disk_space;
mod doh_dns;
mod doh_proxy;
mod doh_ytdlp;
mod download_archive;
mod event;
mod extractor;
mod filename_template;
mod fsutil;
pub mod github_release_source;
mod http;
mod integrity;
mod job;
mod job_signals;
mod launch_sites;
mod media_extras;
mod metalink;
mod mirror_set;
pub mod page_sniffer;
pub mod plugin_sidecar;
mod proxy_settings;
mod quality_policy;
mod sidecar_updates;
mod storage;
pub mod toolchain_discovery;
mod transfer;
mod url_input;
pub mod ytdlp_extractor;
pub use plugin_sidecar::{
    bundled_manifest, ensure_bundled_plugins, install_plugin_package, load_manifest, needs_update,
    plugin_dirs_path, version_is_newer, write_manifest, BundledInstall, PluginManifest,
    MANIFEST_FILENAME, PLUGIN_DIRS_REL, PLUGIN_PACKAGE_NAME,
};
pub use toolchain_discovery::{
    classify_ytdlp_failure, detect_libc, error_code, error_code_hint, tag_error, EjsStatus,
    JsRuntime, JsRuntimeKind, Libc, PlatformInfo, ToolInfo, ToolchainStatus, YtDlpLauncher,
    YtDlpSource, ERROR_CODE_NEEDS_BROWSER_COOKIES, ERROR_CODE_NEEDS_JS_RUNTIME, ERROR_CODE_TIMEOUT,
};
pub use ytdlp_extractor::{FakeYtDlpExtractor, YtDlpConfig, YtDlpExtractorToolchain};

pub use bandwidth::{Clock, ManualClock, SystemClock, TransferProgress};
pub use cookie_channel::{append_ytdlp_cookie_args, ytdlp_cookie_args, CookieChannel};
pub use disk_space::{
    disk_space_margin_bytes, required_free_bytes, FreeSpaceLookup, SystemFreeSpace,
};
pub use download_archive::{ArchiveEntry, ArchiveOutcome, DownloadArchive};
pub use event::CoreEvent;
pub use extractor::{
    is_generic_extractor_key, ExtractionTier, ExtractorDownloadOutcome, ExtractorToolchain,
    FakeExtractorToolchain, ItemSelection, MediaFetchKind, MediaItem, ProbePolicy, ProbeResult,
    ProbeSelection, ProbeTierSettings, Quality, DEFAULT_PROBE_TIMEOUT_SECS,
};
pub use filename_template::{
    media_item_raw_id, prepare_extractor_output, render_template, resolve_collision,
    sanitize_filename, validate_filename_template, TemplateVars, DEFAULT_FILENAME_TEMPLATE,
    MAX_FILENAME_BYTES,
};
pub use github_release_source::{
    select_asset as select_ytdlp_release_asset, GitHubReleaseUpdateSource,
};
pub use http::{ContentRange, FetchResult, HeadInfo, HttpBackend, ProdHttpBackend};
pub use integrity::{hash_from_metalink, ExpectedHash, HashAlgorithm};
pub use job::{EnqueueOutcome, JobState};
pub use job_signals::JobSignals;
pub use media_extras::{
    apply_remux_postprocess, effective_options, ffmpeg_available, ytdlp_media_extras_args,
    MediaExtrasCapabilities, MediaExtrasOptions, PostprocessOutcome,
};
pub use mirror_set::MirrorSet;
pub use page_sniffer::{
    FakePageSniffer, HtmlPageSniffer, PageSniffer, SniffOutcome, SniffSource, SniffedKind,
    SniffedMedia,
};
pub use proxy_settings::{append_ytdlp_proxy_args, ytdlp_proxy_args, ProxySettings};
pub use quality_policy::{select_default_quality, DefaultQualityPolicy, DEFAULT_MAX_HEIGHT};
pub use sidecar_updates::{
    apply_sidecar_updates, apply_sidecar_updates_on_launch, check_sidecar_updates,
    rollback_sidecar_component, toolchain_binary_path, write_toolchain_manifest,
    CatalogVersionEntry, FakeSidecarUpdateSource, FileSidecarUpdateSource, SidecarApplyOutcome,
    SidecarApplyResult, SidecarComponent, SidecarComponentStatus, SidecarRuntimeState,
    SidecarUpdateCheckResult, SidecarUpdateSettings, SidecarUpdateSource, ToolchainManifest,
    UpdateCatalog, TOOLCHAIN_MANIFEST_FILENAME, UPDATE_CATALOG_FILENAME,
};
pub use url_input::{normalize_url, split_urls, split_urls_report, SplitUrlsReport};

use bandwidth::{shared_token_bucket, SharedTokenBucket};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use transfer::{
    execute_segment_fetch, execute_single_fetch, SegmentError, SegmentFetchParams,
    SingleFetchParams,
};

/// Serializable job row for platform shells.
#[derive(Debug, Clone, serde::Serialize)]
pub struct JobSnapshot {
    pub job_id: String,
    pub url: String,
    pub destination: String,
    /// `pending` | `downloading` | `paused` | `failed` | `completed`.
    pub status: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub title: String,
    /// Final file path once Completed.
    pub path: Option<String>,
    /// True when the Extractor Toolchain (yt-dlp) fetches this job.
    pub is_extractor: bool,
    pub extraction_tier: Option<ExtractionTier>,
    pub last_error: Option<String>,
    /// Position in the Core queue (what `reorder_job` indices refer to).
    pub queue_index: usize,
}

/// Match for a URL already active or queued in Core.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ActiveJobMatch {
    pub job_id: String,
    pub url: String,
    pub status: String,
    pub title: String,
}

/// Resolved Job Destination preview for Probe-then-Confirm before jobs exist.
///
/// Unique id suffixes are not known until confirm; those slots use an ellipsis
/// placeholder (`…`, U+2026) so the shell can show where files will land.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProbeDestinationPreview {
    pub path_display: String,
}

/// Placeholder for the unique job-id suffix in destination previews.
const DEST_ID_PLACEHOLDER: &str = "\u{2026}";

/// Debounce interval for progress-only queue flushes.
const PROGRESS_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// Debounce byte threshold for progress-only queue flushes.
const PROGRESS_FLUSH_BYTES: u64 = 1024 * 1024;
/// How often Core samples Job Destination size while a silent Extractor runs.
const EXTRACTOR_DISK_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The Download Core instance.
///
/// Create one per app session.  Drop to tear down.
/// Default number of concurrent segments for segmented transfer.
pub const DEFAULT_SEGMENT_COUNT: u64 = 4;

/// Default maximum number of jobs allowed to transfer at once.
pub use storage::DEFAULT_ACTIVE_JOB_LIMIT;

pub struct Core {
    http: Arc<dyn HttpBackend>,
    extractor: Arc<dyn ExtractorToolchain>,
    library_root: Option<PathBuf>,
    jobs: Vec<job::Job>,
    event_tx: crossbeam_channel::Sender<CoreEvent>,
    /// Global bandwidth cap in bytes per second.
    /// None = globally uncapped.
    bandwidth_cap: Option<u64>,
    /// The one token bucket every transfer shares for the global cap. Rebuilt
    /// only when the cap changes, so jobs started in different dispatch
    /// rounds draw from the same bucket.
    global_bucket: Option<SharedTokenBucket>,
    /// Maximum concurrent transferring jobs (default [`DEFAULT_ACTIVE_JOB_LIMIT`]).
    active_job_limit: u32,
    default_quality_policy: DefaultQualityPolicy,
    audio_only_mode: bool,
    default_media_extras: MediaExtrasOptions,
    cookie_channel: CookieChannel,
    proxy_settings: ProxySettings,
    sidecar_update_settings: SidecarUpdateSettings,
    sidecar_root: Option<PathBuf>,
    sidecar_runtime_state: SidecarRuntimeState,
    sidecar_update_source: Arc<dyn SidecarUpdateSource>,
    filename_template: String,
    clock: Arc<dyn bandwidth::Clock>,
    /// Pending probe results, keyed by URL.
    /// Stored until confirm or replaced by a new probe for the same URL.
    pending_probes: HashMap<String, ProbeResult>,
    /// Last time a debounced progress flush was written to disk.
    last_progress_flush: Instant,
    /// Bytes written since the last debounced progress flush.
    bytes_since_progress_flush: u64,
    /// Whether a debounced progress flush is pending.
    progress_flush_pending: bool,
    job_signals: Arc<job_signals::JobSignals>,
    free_space: Arc<dyn disk_space::FreeSpaceLookup>,
    download_archive: DownloadArchive,
    /// Core-native page sniffer (tier 3 of the non-Launch-Site fallback chain).
    sniffer: Arc<dyn PageSniffer>,
    /// Settings-backed fallback-chain policy.
    probe_tier_settings: ProbeTierSettings,
    /// Cancel flags for in-flight probes, keyed by probe id, with the normalized
    /// URL each probe runs against (several probes of one URL can be in flight).
    probe_cancels: HashMap<u64, (String, Arc<AtomicBool>)>,
    next_probe_id: u64,
    /// Cookie sessions handed in with an enqueue that still needs a probe,
    /// keyed by normalized URL. Moved onto the jobs at confirm.
    probe_cookie_sessions: HashMap<String, CookieChannel>,
}

/// What [`Core::set_library_root`] found while restoring the queue.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct LibraryRootReport {
    /// Human-readable notes: files moved aside as corrupt, queue entries
    /// skipped, settings reset. Empty when everything loaded cleanly.
    pub warnings: Vec<String>,
}

/// Options for [`Core::enqueue_with_options`].
///
/// Construct with `EnqueueOptions { force_direct: true, ..Default::default() }`
/// so new options do not break callers.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnqueueOptions {
    /// Skip classification and create a plain HTTP job for the URL itself
    /// ("Try plain download" after a failed probe).
    #[serde(default)]
    pub force_direct: bool,
    /// Per-job cookie session (for example a Chrome Companion hand-off).
    ///
    /// Core uses it, instead of the global cookie channel, for every probe,
    /// quality lookup and extractor download that belongs to this enqueue: a
    /// direct URL gets a job that carries the session; a URL that needs
    /// Probe-then-Confirm keeps the session until [`Core::confirm_probe_selection`]
    /// copies it onto each confirmed job (or [`Core::clear_pending_probe`]
    /// drops it). The global channel from [`Core::cookie_channel`] is never
    /// changed. The session is validated (`CookieChannel::validate`) and
    /// persisted with the job in `jobs.json`; an unconfigured session is
    /// ignored. The caller owns the cookie file's lifetime.
    #[serde(default)]
    pub cookie_session: Option<CookieChannel>,
}

/// Everything a probe needs, captured under the Core lock so the network work
/// can run without it. Build with [`Core::plan_probe`] (or
/// [`ProbePlan::standalone`] for tools), run with [`ProbePlan::run`], then hand
/// the outcome to [`Core::finish_probe`].
pub struct ProbePlan {
    url: String,
    launch_site: bool,
    extractor: Arc<dyn ExtractorToolchain>,
    sniffer: Arc<dyn PageSniffer>,
    cookies: CookieChannel,
    proxy: ProxySettings,
    settings: ProbeTierSettings,
    cancel: Arc<AtomicBool>,
    /// Unique per planned probe, so concurrent probes of one URL stay distinct.
    id: u64,
}

impl ProbePlan {
    /// A plan outside of a [`Core`] (CLI / examples / live checks).
    pub fn standalone(
        url: &str,
        extractor: Arc<dyn ExtractorToolchain>,
        sniffer: Arc<dyn PageSniffer>,
        settings: ProbeTierSettings,
        cookies: CookieChannel,
        proxy: ProxySettings,
    ) -> Result<Self, String> {
        let url = normalize_url(url).ok_or_else(|| "invalid URL".to_string())?;
        let parsed = url::Url::parse(&url).map_err(|_| "invalid URL".to_string())?;
        Ok(Self {
            launch_site: classification::is_launch_site(&parsed),
            url,
            extractor,
            sniffer,
            cookies,
            proxy,
            settings,
            cancel: Arc::new(AtomicBool::new(false)),
            id: 0,
        })
    }

    /// Normalized URL this plan probes.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Whether the URL is one of the four Launch Sites (dedicated extractor or fail).
    pub fn is_launch_site(&self) -> bool {
        self.launch_site
    }

    /// Shared cancel flag; set it to abort [`Self::run`] between tiers.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    fn check_cancel(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::Acquire) {
            Err("probe cancelled".to_string())
        } else {
            Ok(())
        }
    }

    fn policy(&self) -> ProbePolicy {
        ProbePolicy {
            allow_generic: !self.launch_site && self.settings.allow_generic,
            timeout: Duration::from_secs(self.settings.probe_timeout_secs),
            cancel: Some(Arc::clone(&self.cancel)),
        }
    }

    /// Run the probe. Blocking network work; call without holding the Core lock.
    ///
    /// Launch Sites: the dedicated yt-dlp extractor or an explicit error
    /// (spec §6.4, never a silent generic fallback). Other URLs fall through:
    ///
    /// 0. HEAD says non-HTML → one `DirectFile` item.
    /// 1. yt-dlp dedicated extractor → `Dedicated`.
    /// 2. yt-dlp generic extractor (when allowed) → `GenericExtractor` + warning.
    /// 3. Core page sniffer finds media in the HTML → `PageSniff` + warning.
    /// 4. Otherwise an error naming what each tier reported.
    pub fn run(&self) -> Result<ProbeResult, String> {
        let policy = self.policy();

        if self.launch_site {
            let result =
                self.extractor
                    .probe_with_policy(&self.url, &self.cookies, &self.proxy, &policy)?;
            if !result.tier.is_dedicated() {
                return Err(format!(
                    "unsupported Launch Site: '{}' resolved via yt-dlp's generic extractor",
                    self.url
                ));
            }
            return Ok(result);
        }

        // Tier 0: a cheap HEAD tells a file from a page before spawning yt-dlp.
        if self.settings.allow_direct_fallback {
            if let Ok(SniffOutcome::Binary {
                content_type,
                content_length,
                final_url,
                filename,
            }) = self.sniffer.sniff_head(&self.url, &self.proxy)
            {
                return Ok(direct_file_result(
                    &self.url,
                    &content_type,
                    content_length,
                    &final_url,
                    filename,
                ));
            }
        }
        self.check_cancel()?;

        // Tiers 1 + 2: one yt-dlp run; the extractor key tells dedicated from generic.
        let ytdlp_error =
            match self
                .extractor
                .probe_with_policy(&self.url, &self.cookies, &self.proxy, &policy)
            {
                Ok(mut result) if !result.items.is_empty() => {
                    if result.tier == ExtractionTier::GenericExtractor {
                        result.warnings.push(
                            "yt-dlp has no dedicated extractor for this site; the results come \
                         from its generic extractor and the quality list may be incomplete."
                                .to_string(),
                        );
                    }
                    return Ok(result);
                }
                Ok(_) => format!("yt-dlp returned no media items for {}", self.url),
                Err(err) => err,
            };
        self.check_cancel()?;

        // Tier 3: Core's own HTML sniffer.
        let mut sniff_note = String::new();
        if self.settings.allow_page_sniff {
            match self.sniffer.sniff(&self.url, &self.proxy) {
                Ok(SniffOutcome::Page { title, media }) if !media.is_empty() => {
                    let items =
                        page_sniffer::media_items_from_sniff(&self.url, title.as_deref(), media);
                    let source_name = if items.len() > 1 { title } else { None };
                    return Ok(ProbeResult {
                        url: self.url.clone(),
                        items,
                        source_name,
                        tier: ExtractionTier::PageSniff,
                        extractor_key: None,
                        warnings: vec![format!(
                            "yt-dlp could not extract this page ({ytdlp_error}); media URLs \
                             found in the page HTML are offered instead."
                        )],
                    });
                }
                Ok(SniffOutcome::Binary {
                    content_type,
                    content_length,
                    final_url,
                    filename,
                }) if self.settings.allow_direct_fallback => {
                    return Ok(direct_file_result(
                        &self.url,
                        &content_type,
                        content_length,
                        &final_url,
                        filename,
                    ));
                }
                Ok(_) => sniff_note = "; page sniff: no media URLs found in the HTML".to_string(),
                Err(err) => sniff_note = format!("; page sniff: {err}"),
            }
        }

        Err(format!(
            "no downloadable media found for {}. yt-dlp: {ytdlp_error}{sniff_note}",
            self.url
        ))
    }
}

/// One-item `DirectFile` probe result for a URL that serves a non-HTML body.
fn direct_file_result(
    url: &str,
    content_type: &str,
    content_length: Option<u64>,
    final_url: &str,
    filename: Option<String>,
) -> ProbeResult {
    use sha2::{Digest, Sha256};
    let name = filename
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| job::filename_from_url(final_url));
    let container = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .filter(|ext| !ext.is_empty() && ext.len() <= 5);
    let digest = Sha256::digest(final_url.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    ProbeResult {
        url: url.to_string(),
        items: vec![MediaItem {
            id: format!("file-{hex}"),
            title: name,
            url: final_url.to_string(),
            qualities: vec![Quality {
                id: "file".to_string(),
                label: match content_length {
                    Some(n) => format!("Original file ({})", human_bytes(n)),
                    None => "Original file".to_string(),
                },
                size_bytes: content_length,
                container,
            }],
            archived: false,
            uploader: None,
            upload_date: None,
            fetch_kind: MediaFetchKind::DirectHttp,
            qualities_are_presets: false,
            referer: None,
        }],
        source_name: None,
        tier: ExtractionTier::DirectFile,
        extractor_key: None,
        warnings: vec![format!(
            "Server reports {content_type}; downloading as a plain file."
        )],
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Everything an enrich call needs; see [`Core::enrich_probe_item_qualities_outside_lock`].
pub struct EnrichPlan {
    url_key: String,
    item_id: String,
    item_url: String,
    fetch_kind: MediaFetchKind,
    existing: Vec<Quality>,
    extractor: Arc<dyn ExtractorToolchain>,
    cookies: CookieChannel,
    proxy: ProxySettings,
}

impl EnrichPlan {
    /// Blocking network work; call without the Core lock.
    pub fn run(&self) -> Result<Vec<Quality>, String> {
        match self.fetch_kind {
            // Plain files have exactly the qualities the sniffer found.
            MediaFetchKind::DirectHttp => Ok(self.existing.clone()),
            MediaFetchKind::Extractor => {
                self.extractor
                    .probe_item_qualities(&self.item_url, &self.cookies, &self.proxy)
            }
        }
    }
}

enum UnlockedTransferPlan {
    SingleFetch {
        job_id: String,
        bytes_received: u64,
        url: String,
        part_path: PathBuf,
        file_path: PathBuf,
        range: Option<Range<u64>>,
        if_range: Option<String>,
        append: bool,
        truncate: bool,
        http: Arc<dyn HttpBackend>,
        global_bucket: Option<SharedTokenBucket>,
        job_bucket: Option<SharedTokenBucket>,
        pause_requested: Arc<AtomicBool>,
        transfer_active: Arc<AtomicBool>,
        progress: TransferProgress,
    },
    /// Multi-connection Range transfer (fresh job, server supports ranges, no cap).
    Segmented {
        job_id: String,
        url: String,
        part_path: PathBuf,
        file_path: PathBuf,
        total_size: u64,
        /// Segments still to fetch, with the counter each one advances.
        work: Vec<SegmentWork>,
        /// `If-Range` validator for segments that continue earlier progress.
        if_range: Option<String>,
        /// The plan continues a stored table rather than starting a new one.
        resumed: bool,
        http: Arc<dyn HttpBackend>,
        global_bucket: Option<SharedTokenBucket>,
        job_bucket: Option<SharedTokenBucket>,
        pause_requested: Arc<AtomicBool>,
        transfer_active: Arc<AtomicBool>,
        progress: TransferProgress,
    },
    /// Extractor Toolchain (yt-dlp) download; runs on its own thread while Core
    /// polls Job Destination growth for progress.
    ExtractorFetch {
        job_id: String,
        url: String,
        format: String,
        dest: PathBuf,
        output_spec: String,
        media_extras: MediaExtrasOptions,
        cookies: CookieChannel,
        proxy: ProxySettings,
        extractor: Arc<dyn ExtractorToolchain>,
        stop: Arc<AtomicBool>,
        transfer_active: Arc<AtomicBool>,
        live_bytes: Arc<AtomicU64>,
        event_tx: crossbeam_channel::Sender<CoreEvent>,
        total_for_progress: Option<u64>,
        referer: Option<String>,
    },
}

enum UnlockedTransferOutcome {
    Fetched {
        job_id: String,
        bytes_received: u64,
        part_path: PathBuf,
        file_path: PathBuf,
        outcome: transfer::SingleFetchOutcome,
        failed_url: String,
    },
    Segmented {
        job_id: String,
        part_path: PathBuf,
        file_path: PathBuf,
        failed_url: String,
        total_size: u64,
        resumed: bool,
        result: SegmentedDone,
    },
    Extracted {
        job_id: String,
        dest: PathBuf,
        result: Result<ExtractorDownloadOutcome, String>,
        /// Remux result, computed outside the Core lock; `Some` only for a completed download.
        post: Option<PostprocessOutcome>,
        stop: Arc<AtomicBool>,
    },
    /// The transfer code panicked; the job is failed so it cannot stay active.
    Panicked { job_id: String, message: String },
}

/// Answers to the HEAD requests planning needs, fetched without the Core
/// lock. Planning uses them only while the job still points at `url`.
struct PlanningHead {
    url: String,
    supports_range: Result<bool, String>,
    file_size: Result<u64, String>,
}

/// Reserve `total` bytes for the `.part` so a full disk fails the job at the
/// start. A filesystem without `fallocate` falls back to a sparse file.
fn preallocate(file: &std::fs::File, total: u64) -> Result<(), String> {
    use fs2::FileExt;
    match file.allocate(total) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(28) => Err(format!(
            "no space left on the device for {total} bytes: {e}"
        )),
        Err(_) => file
            .set_len(total)
            .map_err(|e| format!("preallocate partial: {e}")),
    }
}

enum SegmentedPlanning {
    /// The job uses a single connection.
    NotApplicable,
    Plan(Box<UnlockedTransferPlan>),
    /// The job was finalized during planning (already complete, or empty).
    Finalized,
}

/// One segment's unfinished range and the counter its fetch advances.
struct SegmentWork {
    range: Range<u64>,
    written: Arc<AtomicU64>,
}

/// What a Segmented Transfer run left behind: the validators any finished
/// segment reported, and the failure that stopped it, if any. Segments that
/// did finish keep their progress in the job's segment table either way.
struct SegmentedDone {
    paused: bool,
    etag: Option<String>,
    last_modified: Option<String>,
    error: Option<SegmentError>,
}

impl UnlockedTransferOutcome {
    fn job_id(&self) -> &str {
        match self {
            Self::Fetched { job_id, .. }
            | Self::Segmented { job_id, .. }
            | Self::Extracted { job_id, .. }
            | Self::Panicked { job_id, .. } => job_id,
        }
    }
}

impl UnlockedTransferPlan {
    fn job_id(&self) -> &str {
        match self {
            Self::SingleFetch { job_id, .. }
            | Self::Segmented { job_id, .. }
            | Self::ExtractorFetch { job_id, .. } => job_id,
        }
    }

    fn transfer_active_flag(&self) -> Arc<AtomicBool> {
        match self {
            Self::SingleFetch {
                transfer_active, ..
            }
            | Self::Segmented {
                transfer_active, ..
            }
            | Self::ExtractorFetch {
                transfer_active, ..
            } => transfer_active.clone(),
        }
    }

    /// Run the transfer. A panic inside it must not leave the job marked
    /// active, so it is caught, the flag is cleared, and the job is reported
    /// as failed through [`UnlockedTransferOutcome::Panicked`].
    fn run(self) -> UnlockedTransferOutcome {
        let job_id = self.job_id().to_string();
        let active = self.transfer_active_flag();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_inner())) {
            Ok(outcome) => outcome,
            Err(payload) => {
                active.store(false, Ordering::Release);
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|m| m.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_string());
                UnlockedTransferOutcome::Panicked { job_id, message }
            }
        }
    }

    fn run_inner(self) -> UnlockedTransferOutcome {
        match self {
            Self::SingleFetch {
                job_id,
                bytes_received,
                url,
                part_path,
                file_path,
                range,
                if_range,
                append,
                truncate,
                http,
                global_bucket,
                job_bucket,
                pause_requested,
                transfer_active,
                progress,
                ..
            } => {
                let failed_url = url.clone();
                let outcome = execute_single_fetch(SingleFetchParams {
                    http,
                    url,
                    part_path: part_path.clone(),
                    range,
                    if_range,
                    append,
                    truncate,
                    global_bucket,
                    job_bucket,
                    pause_requested,
                    transfer_active,
                    progress: Some(progress),
                });
                UnlockedTransferOutcome::Fetched {
                    job_id,
                    bytes_received,
                    part_path,
                    file_path,
                    outcome,
                    failed_url,
                }
            }
            Self::Segmented {
                job_id,
                url,
                part_path,
                file_path,
                total_size,
                work,
                if_range,
                resumed,
                http,
                global_bucket,
                job_bucket,
                pause_requested,
                transfer_active,
                progress,
            } => {
                let failed_url = url.clone();
                let result = run_segmented_fetch(
                    http,
                    &url,
                    &part_path,
                    work,
                    if_range,
                    global_bucket,
                    job_bucket,
                    pause_requested,
                    Some(progress),
                );
                transfer_active.store(false, Ordering::Release);
                UnlockedTransferOutcome::Segmented {
                    job_id,
                    part_path,
                    file_path,
                    failed_url,
                    total_size,
                    resumed,
                    result,
                }
            }
            Self::ExtractorFetch {
                job_id,
                url,
                format,
                dest,
                output_spec,
                media_extras,
                cookies,
                proxy,
                extractor,
                stop,
                transfer_active,
                live_bytes,
                event_tx,
                total_for_progress,
                referer,
            } => {
                let ffmpeg_dir = extractor.ffmpeg_dir().map(Path::to_path_buf);
                let mut result = run_extractor_fetch(ExtractorFetchParams {
                    extractor,
                    url,
                    format,
                    dest: dest.clone(),
                    output_spec,
                    media_extras: media_extras.clone(),
                    cookies,
                    proxy,
                    stop: stop.clone(),
                    live_bytes,
                    event_tx,
                    job_id: job_id.clone(),
                    total_for_progress,
                    referer,
                });
                // Remux runs here, without the Core mutex, and ends with the job's stop flag.
                let post = match &result {
                    Ok(ExtractorDownloadOutcome::Completed(path)) => Some(apply_remux_postprocess(
                        path,
                        &media_extras,
                        ffmpeg_dir.as_deref(),
                        stop.as_ref(),
                    )),
                    _ => None,
                };
                if matches!(post, Some(PostprocessOutcome::Stopped)) {
                    result = Ok(ExtractorDownloadOutcome::Stopped);
                }
                transfer_active.store(false, Ordering::Release);
                UnlockedTransferOutcome::Extracted {
                    job_id,
                    dest,
                    result,
                    post,
                    stop,
                }
            }
        }
    }
}

struct ExtractorFetchParams {
    extractor: Arc<dyn ExtractorToolchain>,
    url: String,
    format: String,
    dest: PathBuf,
    output_spec: String,
    media_extras: MediaExtrasOptions,
    cookies: CookieChannel,
    proxy: ProxySettings,
    stop: Arc<AtomicBool>,
    live_bytes: Arc<AtomicU64>,
    event_tx: crossbeam_channel::Sender<CoreEvent>,
    job_id: String,
    total_for_progress: Option<u64>,
    referer: Option<String>,
}

/// Run an Extractor Toolchain download on a worker thread and emit
/// [`CoreEvent::JobProgress`] from on-disk growth under the Job Destination
/// until it finishes or stops. Runs without the Core mutex.
fn run_extractor_fetch(params: ExtractorFetchParams) -> Result<ExtractorDownloadOutcome, String> {
    let ExtractorFetchParams {
        extractor,
        url,
        format,
        dest,
        output_spec,
        media_extras,
        cookies,
        proxy,
        stop,
        live_bytes,
        event_tx,
        job_id,
        total_for_progress,
        referer,
    } = params;
    let (result_tx, result_rx) = mpsc::channel();
    let dest_worker = dest.clone();
    std::thread::spawn(move || {
        let outcome = extractor.download_with_referer(
            &url,
            &format,
            &dest_worker,
            stop.as_ref(),
            &media_extras,
            &cookies,
            &proxy,
            &output_spec,
            referer.as_deref(),
        );
        let _ = result_tx.send(outcome);
    });

    let mut last_emitted: Option<u64> = None;
    loop {
        match result_rx.try_recv() {
            Ok(outcome) => break outcome,
            Err(mpsc::TryRecvError::Empty) => {
                let on_disk = storage::destination_byte_total(&dest);
                if last_emitted != Some(on_disk) {
                    live_bytes.store(on_disk, Ordering::Release);
                    let _ = event_tx.send(CoreEvent::JobProgress {
                        job_id: job_id.clone(),
                        bytes_downloaded: on_disk,
                        total_bytes: total_for_progress,
                    });
                    last_emitted = Some(on_disk);
                }
                std::thread::sleep(EXTRACTOR_DISK_POLL_INTERVAL);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                break Err("extractor download thread disconnected".into());
            }
        }
    }
}

/// Multi-connection Range transfer into `part_path`. Runs without the Core mutex.
///
/// Each `work` entry fetches the unfinished tail of one segment and advances
/// its counter as bytes reach the disk, so an interrupted run leaves an exact
/// record of what is done.
#[allow(clippy::too_many_arguments)]
fn run_segmented_fetch(
    http: Arc<dyn HttpBackend>,
    url: &str,
    part_path: &Path,
    work: Vec<SegmentWork>,
    if_range: Option<String>,
    global_bucket: Option<SharedTokenBucket>,
    job_bucket: Option<SharedTokenBucket>,
    pause_requested: Arc<AtomicBool>,
    progress: Option<TransferProgress>,
) -> SegmentedDone {
    let segment_results: Vec<Result<transfer::SegmentFetched, SegmentError>> =
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for item in work {
                let http_ref = http.clone();
                let url_ref = url.to_string();
                let part_ref = part_path.to_path_buf();
                let global = global_bucket.clone();
                let job = job_bucket.clone();
                let pause = pause_requested.clone();
                let prog = progress.clone();
                let validator = if_range.clone();
                handles.push(scope.spawn(move || {
                    execute_segment_fetch(SegmentFetchParams {
                        http: http_ref,
                        url: url_ref,
                        part_path: part_ref,
                        range: item.range,
                        if_range: validator,
                        written: item.written,
                        global_bucket: global,
                        job_bucket: job,
                        pause_requested: pause,
                        progress: prog,
                    })
                }));
            }
            handles
                .into_iter()
                .map(|h| {
                    h.join().unwrap_or_else(|_| {
                        Err(SegmentError::Failed("segment worker panicked".to_string()))
                    })
                })
                .collect()
        });

    let mut done = SegmentedDone {
        paused: pause_requested.load(Ordering::Acquire),
        etag: None,
        last_modified: None,
        error: None,
    };
    for result in segment_results {
        match result {
            Ok(fetched) => {
                if done.etag.is_none() && done.last_modified.is_none() {
                    done.etag = fetched.etag;
                    done.last_modified = fetched.last_modified;
                }
            }
            // A server that ignores Range outranks any other failure.
            Err(SegmentError::RangeIgnored) => done.error = Some(SegmentError::RangeIgnored),
            Err(err) => {
                if done.error.is_none() {
                    done.error = Some(err);
                }
            }
        }
    }
    done
}

impl Core {
    /// Create a new Core with the given HTTP backend and extractor toolchain.
    ///
    /// Returns a `(Core, Receiver<CoreEvent>)` pair.  The caller reads events
    /// from the receiver (typically in a shell event loop).
    pub fn new(
        http: Box<dyn HttpBackend>,
        extractor: Box<dyn ExtractorToolchain>,
    ) -> (Self, crossbeam_channel::Receiver<CoreEvent>) {
        Self::new_with_clock(http, extractor, Arc::new(SystemClock::new()))
    }

    /// Create a Core with an injectable clock (used in acceptance tests).
    pub fn new_with_clock(
        http: Box<dyn HttpBackend>,
        extractor: Box<dyn ExtractorToolchain>,
        clock: Arc<dyn bandwidth::Clock>,
    ) -> (Self, crossbeam_channel::Receiver<CoreEvent>) {
        Self::new_with_clock_and_signals(
            http,
            extractor,
            clock,
            Arc::new(job_signals::JobSignals::default()),
        )
    }

    /// Create a Core with shared job signals (used by the C ABI).
    pub fn new_with_clock_and_signals(
        http: Box<dyn HttpBackend>,
        extractor: Box<dyn ExtractorToolchain>,
        clock: Arc<dyn bandwidth::Clock>,
        job_signals: Arc<job_signals::JobSignals>,
    ) -> (Self, crossbeam_channel::Receiver<CoreEvent>) {
        Self::new_with_clock_signals_and_free_space(
            http,
            extractor,
            clock,
            job_signals,
            Arc::new(disk_space::SystemFreeSpace),
        )
    }

    /// Create a Core with shared job signals and injectable free-space lookup (tests).
    pub fn new_with_clock_signals_and_free_space(
        http: Box<dyn HttpBackend>,
        extractor: Box<dyn ExtractorToolchain>,
        clock: Arc<dyn bandwidth::Clock>,
        job_signals: Arc<job_signals::JobSignals>,
        free_space: Arc<dyn disk_space::FreeSpaceLookup>,
    ) -> (Self, crossbeam_channel::Receiver<CoreEvent>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let http: Arc<dyn HttpBackend> = Arc::from(http);
        let sniffer: Arc<dyn PageSniffer> =
            Arc::new(page_sniffer::HtmlPageSniffer::new(Arc::clone(&http)));
        let core = Self {
            http,
            extractor: Arc::from(extractor),
            library_root: None,
            jobs: Vec::new(),
            event_tx: tx,
            bandwidth_cap: None,
            global_bucket: None,
            active_job_limit: DEFAULT_ACTIVE_JOB_LIMIT,
            default_quality_policy: DefaultQualityPolicy::default(),
            audio_only_mode: false,
            default_media_extras: MediaExtrasOptions::default(),
            cookie_channel: CookieChannel::default(),
            proxy_settings: ProxySettings::default(),
            sidecar_update_settings: SidecarUpdateSettings::default(),
            sidecar_root: None,
            sidecar_runtime_state: SidecarRuntimeState::default(),
            sidecar_update_source: Arc::new(FileSidecarUpdateSource),
            filename_template: DEFAULT_FILENAME_TEMPLATE.to_string(),
            clock,
            pending_probes: HashMap::new(),
            last_progress_flush: Instant::now(),
            bytes_since_progress_flush: 0,
            progress_flush_pending: false,
            job_signals,
            free_space,
            download_archive: DownloadArchive::default(),
            sniffer,
            probe_tier_settings: ProbeTierSettings::default(),
            probe_cancels: HashMap::new(),
            next_probe_id: 0,
            probe_cookie_sessions: HashMap::new(),
        };
        (core, rx)
    }

    /// Create a Core with injectable free-space lookup (acceptance tests).
    pub fn new_with_free_space(
        http: Box<dyn HttpBackend>,
        extractor: Box<dyn ExtractorToolchain>,
        free_space: Arc<dyn disk_space::FreeSpaceLookup>,
    ) -> (Self, crossbeam_channel::Receiver<CoreEvent>) {
        Self::new_with_clock_signals_and_free_space(
            http,
            extractor,
            Arc::new(SystemClock::new()),
            Arc::new(job_signals::JobSignals::default()),
            free_space,
        )
    }

    pub fn job_signals(&self) -> Arc<job_signals::JobSignals> {
        self.job_signals.clone()
    }

    fn register_job_signals(&self, job: &job::Job) {
        self.job_signals.register(
            job.id(),
            job.pause_requested().clone(),
            job.discard_requested().clone(),
            job.transfer_active().clone(),
        );
    }

    /// Finalize a pause when no transfer is active (C ABI / post-transfer).
    pub fn can_pause_job(&self, job_id: &str) -> Result<(), String> {
        let job = self
            .jobs
            .iter()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        if !job.is_pending() {
            return Err(format!(
                "cannot pause job {job_id}: current state is {:?}",
                job.state()
            ));
        }
        Ok(())
    }

    pub fn finish_pause_if_idle(&mut self, job_id: &str) -> Result<(), String> {
        let transfer_active = self
            .jobs
            .iter()
            .find(|j| j.id() == job_id)
            .map(|j| j.transfer_active().load(Ordering::Acquire))
            .unwrap_or(false);
        if transfer_active {
            return Ok(());
        }
        self.finalize_pause(job_id)
    }

    fn finalize_pause(&mut self, job_id: &str) -> Result<(), String> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        if !job.is_pending() {
            return Ok(());
        }
        job.pause()?;
        job.clear_pause_requested();
        self.event_tx
            .send(CoreEvent::JobPaused {
                job_id: job_id.to_string(),
            })
            .map_err(|_| "event channel closed".to_string())?;
        self.persist_queue_now()
    }

    /// Set the Library Root — the base directory for default Job Destinations
    /// and Core metadata (`.phlegethon/`).
    ///
    /// Creates the directory if it does not exist and restores any persisted
    /// queue state from `{library_root}/.phlegethon/jobs.json`.
    ///
    /// Refuses (`Err`) while any transfer is active, because the swap replaces
    /// every live job and its signals. The same applies when the root is the
    /// current one: pause the jobs or wait first.
    ///
    /// Nothing changes unless the whole load succeeds. An unreadable
    /// `jobs.json` or `archive.json` does not make the root unusable: the file
    /// is moved to `<name>.corrupt-<timestamp>`, Core starts with an empty
    /// queue or archive, and the returned [`LibraryRootReport`] says so. Single
    /// bad queue rows are skipped the same way. Show the warnings to the user.
    pub fn set_library_root(&mut self, root: PathBuf) -> Result<LibraryRootReport, String> {
        if self.transfers_active() {
            return Err(
                "cannot change the Library Root while transfers are active; pause them or wait \
                 for them to finish"
                    .to_string(),
            );
        }
        std::fs::create_dir_all(&root).map_err(|e| format!("create library root: {e}"))?;
        let mut loaded = storage::load_queue(&root)?;
        let (archive, archive_warning) = download_archive::DownloadArchive::load_or_reset(&root)?;
        let mut warnings = std::mem::take(&mut loaded.warnings);
        warnings.extend(archive_warning);

        // Everything is loaded; commit it together.
        for job in &self.jobs {
            self.job_signals.unregister(job.id());
        }
        for job in &loaded.jobs {
            self.register_job_signals(job);
        }
        self.jobs = loaded.jobs;
        self.bandwidth_cap = loaded.global_bandwidth_cap;
        self.rebuild_global_bucket();
        self.active_job_limit = loaded.active_job_limit;
        self.default_quality_policy = loaded.quality.policy;
        self.audio_only_mode = loaded.quality.audio_only_mode;
        self.default_media_extras = loaded.media_extras.defaults;
        self.cookie_channel = loaded.cookie_channel;
        self.proxy_settings = loaded.proxy_settings;
        // A persisted setting the HTTP client rejects (for example an `https://` proxy saved
        // by an older release) must not stop the library from opening; the backend refuses
        // requests until the user fixes the setting.
        if let Err(e) = self.http.apply_proxy_settings(&self.proxy_settings) {
            eprintln!("saved proxy setting rejected: {e}");
        }
        self.sidecar_update_settings = loaded.sidecar_update_settings;
        self.filename_template = loaded.filename_template;
        self.probe_tier_settings = loaded.probe_tier_settings;
        self.download_archive = archive;
        self.library_root = Some(root);
        self.last_progress_flush = Instant::now();
        self.bytes_since_progress_flush = 0;
        self.progress_flush_pending = false;
        Ok(LibraryRootReport { warnings })
    }

    /// Current Library Root, if set.
    pub fn library_root(&self) -> Option<&Path> {
        self.library_root.as_deref()
    }

    fn require_absolute_destination(path: &Path) -> Result<(), String> {
        if path.is_absolute() {
            Ok(())
        } else {
            Err(format!(
                "Job Destination override must be an absolute path, got: {}",
                path.display()
            ))
        }
    }

    fn persist_queue_now(&mut self) -> Result<(), String> {
        if let Some(root) = &self.library_root {
            let queue = storage::build_queue(
                &self.jobs,
                self.bandwidth_cap,
                self.active_job_limit,
                &self.default_quality_policy,
                self.audio_only_mode,
                &self.default_media_extras,
                &self.cookie_channel,
                &self.proxy_settings,
                &self.sidecar_update_settings,
                &self.filename_template,
                &self.probe_tier_settings,
            );
            storage::save_queue(root, &queue)?;
        }
        self.last_progress_flush = Instant::now();
        self.bytes_since_progress_flush = 0;
        self.progress_flush_pending = false;
        Ok(())
    }

    fn note_progress_bytes(&mut self, bytes: u64) -> Result<(), String> {
        if bytes == 0 {
            return Ok(());
        }
        self.bytes_since_progress_flush += bytes;
        self.progress_flush_pending = true;
        let due = self.bytes_since_progress_flush >= PROGRESS_FLUSH_BYTES
            || self.last_progress_flush.elapsed() >= PROGRESS_FLUSH_INTERVAL;
        if due {
            self.persist_queue_now()?;
        }
        Ok(())
    }

    /// Save the segment tables of running Segmented Transfers. Their workers
    /// advance counters without the Core lock, so the dispatch tick is what
    /// carries that progress into `jobs.json` before a crash can lose it.
    fn persist_segment_progress(&mut self) -> Result<(), String> {
        let live = self.jobs.iter().any(|j| {
            j.has_live_segments() || (j.is_pending() && j.transfer_active().load(Ordering::Acquire))
        });
        if live && self.last_progress_flush.elapsed() >= PROGRESS_FLUSH_INTERVAL {
            self.persist_queue_now()?;
        }
        Ok(())
    }

    fn flush_pending_progress(&mut self) -> Result<(), String> {
        if self.progress_flush_pending {
            self.persist_queue_now()?;
        }
        Ok(())
    }

    /// Current queue position of a job. Positions shift on Discard and
    /// reorder, so never hold one across a released lock.
    fn job_index(&self, job_id: &str) -> Option<usize> {
        self.jobs.iter().position(|j| j.id() == job_id)
    }

    fn resolve_known_total(
        &self,
        job_index: usize,
        url: &str,
        head: Option<&PlanningHead>,
    ) -> Option<u64> {
        self.jobs[job_index]
            .total_bytes()
            .or_else(|| self.head_file_size(url, head).ok().filter(|&n| n > 0))
    }

    /// `file_size` from the pre-fetched HEAD when it is for `url`, else live.
    fn head_file_size(&self, url: &str, head: Option<&PlanningHead>) -> Result<u64, String> {
        match head {
            Some(h) if h.url == url => h.file_size.clone(),
            _ => self.http.file_size(url),
        }
    }

    fn head_supports_range(&self, url: &str, head: Option<&PlanningHead>) -> Result<bool, String> {
        match head {
            Some(h) if h.url == url => h.supports_range.clone(),
            _ => self.http.supports_range(url),
        }
    }

    fn check_disk_space_before_start(
        &self,
        destination: &Path,
        total_bytes: u64,
        bytes_received: u64,
    ) -> Result<(), String> {
        let remaining = total_bytes.saturating_sub(bytes_received);
        if remaining == 0 {
            return Ok(());
        }
        let free = self.free_space.free_bytes(destination)?;
        if disk_space::has_sufficient_free_space(free, remaining) {
            Ok(())
        } else {
            Err(disk_space::insufficient_disk_space_error(
                destination,
                free,
                remaining,
            ))
        }
    }

    fn fail_job_for_disk_space(
        &mut self,
        job_index: usize,
        job_id: &str,
        error: String,
    ) -> Result<(), String> {
        self.finalize_failed_job(job_id, job_index, error)
    }

    /// Mark a job Failed, emit `JobFailed`, and record media identity in the archive.
    fn finalize_failed_job(
        &mut self,
        job_id: &str,
        job_index: usize,
        error: String,
    ) -> Result<(), String> {
        if !self.jobs[job_index].mark_failed() {
            // Already Completed: a late failure must not undo it.
            return Ok(());
        }
        self.jobs[job_index].set_last_error(Some(error.clone()));
        self.record_media_identity(job_id, job_index);
        let persisted = self.persist_queue_now();
        self.event_tx
            .send(CoreEvent::JobFailed {
                job_id: job_id.to_string(),
                error,
            })
            .map_err(|_| "event channel closed".to_string())?;
        persisted
    }

    fn guard_disk_space_for_transfer(
        &mut self,
        job_index: usize,
        job_id: &str,
        destination: &Path,
        url: &str,
        bytes_received: u64,
        head: Option<&PlanningHead>,
    ) -> Result<bool, String> {
        let Some(total) = self.resolve_known_total(job_index, url, head) else {
            // The size is unknown, so only the safety margin can be required.
            // A failed lookup does not block the job.
            if let Ok(free) = self.free_space.free_bytes(destination) {
                if !disk_space::has_sufficient_free_space(free, 0) {
                    let err = disk_space::insufficient_disk_space_error(destination, free, 0);
                    self.fail_job_for_disk_space(job_index, job_id, err)?;
                    return Ok(false);
                }
            }
            return Ok(true);
        };
        if let Err(err) = self.check_disk_space_before_start(destination, total, bytes_received) {
            self.fail_job_for_disk_space(job_index, job_id, err)?;
            return Ok(false);
        }
        self.jobs[job_index].set_total_bytes(Some(total));
        Ok(true)
    }

    // -----------------------------------------------------------------------
    // Enqueue
    // -----------------------------------------------------------------------

    /// Classify and enqueue a URL.
    ///
    /// Returns the classification outcome.  For `EnqueueDirect`, a job is
    /// created and ready to process.  For `ProbeThenConfirm`, the shell
    /// should present a probe sheet and call back with selections.
    ///
    /// `destination` — optional absolute parent for a job-owned Job
    /// Destination.  When `None`, the destination is derived under the
    /// Library Root.  When `Some`, Core creates a unique nested folder under
    /// that absolute parent (including outside the Library Root) — never the
    /// parent itself.  Relative paths are rejected.
    pub fn enqueue(
        &mut self,
        url_str: &str,
        destination: Option<PathBuf>,
    ) -> Result<EnqueueOutcome, String> {
        self.enqueue_with_options(url_str, destination, EnqueueOptions::default())
    }

    /// [`Self::enqueue`] with [`EnqueueOptions`] (e.g. force a plain download).
    pub fn enqueue_with_options(
        &mut self,
        url_str: &str,
        destination: Option<PathBuf>,
        options: EnqueueOptions,
    ) -> Result<EnqueueOutcome, String> {
        let url_str = normalize_url(url_str).ok_or_else(|| "invalid URL".to_string())?;
        let parsed = url::Url::parse(&url_str).map_err(|_| "invalid URL".to_string())?;
        let root = self
            .library_root
            .as_ref()
            .ok_or_else(|| "Library Root not set".to_string())?;

        if let Some(ref dest) = destination {
            Self::require_absolute_destination(dest)?;
        }
        let cookie_session = match options.cookie_session {
            Some(session) => {
                let session = session.normalized();
                session.validate()?;
                session.is_configured().then_some(session)
            }
            None => None,
        };

        let kind = if options.force_direct {
            classification::Kind::EnqueueDirect
        } else {
            classification::classify(&parsed)
        };
        match kind {
            classification::Kind::EnqueueDirect => {
                let mut job = match destination {
                    Some(dest) => job::Job::new_with_source_at(url_str.clone(), dest, None),
                    None => job::Job::new(url_str.clone(), root.clone()),
                };
                if options.force_direct {
                    job.set_extraction_tier(ExtractionTier::DirectFile);
                }
                job.set_cookie_session(cookie_session);
                let job_id = job.id().to_string();
                let dest = job.destination().to_path_buf();
                self.push_jobs_persisted(vec![job])?;
                Ok(EnqueueOutcome::EnqueueDirect {
                    job_id,
                    destination: dest,
                })
            }
            classification::Kind::ProbeThenConfirm => {
                match cookie_session {
                    Some(session) => {
                        self.probe_cookie_sessions.insert(url_str.clone(), session);
                    }
                    None => {
                        self.probe_cookie_sessions.remove(&url_str);
                    }
                }
                Ok(EnqueueOutcome::ProbeThenConfirm { url: url_str })
            }
        }
    }

    /// Append new jobs to the queue and persist, all or nothing: when the save
    /// fails the jobs are taken out again, so memory never holds jobs that
    /// `jobs.json` lacks (a retry would otherwise queue duplicates).
    fn push_jobs_persisted(&mut self, jobs: Vec<job::Job>) -> Result<(), String> {
        let first_new = self.jobs.len();
        for job in jobs {
            self.register_job_signals(&job);
            self.jobs.push(job);
        }
        if let Err(err) = self.persist_queue_now() {
            for job in self.jobs.drain(first_new..) {
                self.job_signals.unregister(job.id());
            }
            return Err(err);
        }
        Ok(())
    }

    /// Find an active or queued job matching a normalized URL.
    ///
    /// Completed jobs are ignored. Pending probes for the same URL also match.
    pub fn find_active_job_for_url(&self, url_str: &str) -> Option<ActiveJobMatch> {
        let normalized = normalize_url(url_str)?;

        for job in &self.jobs {
            if job.state() == JobState::Completed {
                continue;
            }
            if normalize_url(job.url()).as_deref() == Some(normalized.as_str()) {
                let title = job
                    .source()
                    .map(str::to_string)
                    .unwrap_or_else(|| job.filename().to_string());
                return Some(ActiveJobMatch {
                    job_id: job.id().to_string(),
                    url: job.url().to_string(),
                    status: match job.state() {
                        JobState::Pending => "pending",
                        JobState::Paused => "paused",
                        JobState::Failed => "failed",
                        JobState::Completed => "completed",
                    }
                    .to_string(),
                    title,
                });
            }
        }

        if self.pending_probes.contains_key(&normalized) {
            return Some(ActiveJobMatch {
                job_id: String::new(),
                url: normalized.clone(),
                status: "probing".to_string(),
                title: normalized,
            });
        }

        None
    }

    // -----------------------------------------------------------------------
    // Lifecycle commands
    // -----------------------------------------------------------------------

    /// Pause a pending job.  Emits `CoreEvent::JobPaused`.
    ///
    /// A paused job is skipped by `process_pending` until resumed.
    /// When a transfer is in flight, pause takes effect without waiting
    /// for the full body read to finish.
    pub fn pause_job(&mut self, job_id: &str) -> Result<(), String> {
        self.can_pause_job(job_id)?;

        if let Some(flag) = self.job_signals.request_pause(job_id) {
            flag.store(true, Ordering::Release);
        }

        if self.job_signals.is_transfer_active(job_id) {
            return Ok(());
        }

        self.finalize_pause(job_id)
    }

    /// Resume a paused job.  Emits `CoreEvent::JobResumed`.
    ///
    /// The job is marked as pending and will be processed on the next
    /// `process_pending` call.
    pub fn resume_job(&mut self, job_id: &str) -> Result<(), String> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;

        job.resume()?;

        self.event_tx
            .send(CoreEvent::JobResumed {
                job_id: job_id.to_string(),
            })
            .map_err(|_| "event channel closed".to_string())?;

        self.persist_queue_now()?;
        Ok(())
    }

    /// Retry a failed job.  Emits `CoreEvent::JobRetried`.
    ///
    /// The job is marked as pending and eligible for processing again.
    pub fn retry_job(&mut self, job_id: &str) -> Result<(), String> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;

        job.retry()?;
        job.set_last_error(None);

        self.event_tx
            .send(CoreEvent::JobRetried {
                job_id: job_id.to_string(),
            })
            .map_err(|_| "event channel closed".to_string())?;

        self.persist_queue_now()?;
        Ok(())
    }

    /// Discard a job from the queue.  Emits `CoreEvent::JobDiscarded`.
    ///
    /// Unfinished jobs (Pending / Paused / Failed, including in-flight) are
    /// stopped, removed from the queue, and have their Job Destination files
    /// deleted.  Completed jobs lose only the queue row — files stay on disk.
    ///
    /// Once discarded the job cannot be paused/resumed/retried.
    /// Discarding an already-discarded (or non-existent) job returns an error.
    pub fn discard_job(&mut self, job_id: &str) -> Result<(), String> {
        let pos = self
            .jobs
            .iter()
            .position(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;

        let completed = self.jobs[pos].is_completed();
        let destination = self.jobs[pos].destination().to_path_buf();

        if self.job_signals.signal_discard_stop(job_id) {
            // Transfer thread will finalize discard when the body read stops.
            return Ok(());
        }

        self.finalize_discard(job_id, completed, &destination)
    }

    fn finalize_discard(
        &mut self,
        job_id: &str,
        completed: bool,
        destination: &Path,
    ) -> Result<(), String> {
        let pos = self
            .jobs
            .iter()
            .position(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;

        // Delete first. When the delete fails the job stays in the queue (and
        // on disk), so the user can retry the Discard; removing the row first
        // would bring the job back from `jobs.json` after a restart.
        if !completed && destination.exists() {
            if let Err(err) = self.delete_job_destination(destination) {
                self.jobs[pos].clear_discard_requested();
                self.jobs[pos].clear_pause_requested();
                return Err(err);
            }
        }

        self.jobs.remove(pos);
        self.job_signals.unregister(job_id);
        let persisted = self.persist_queue_now();
        self.event_tx
            .send(CoreEvent::JobDiscarded {
                job_id: job_id.to_string(),
            })
            .map_err(|_| "event channel closed".to_string())?;
        persisted
    }

    /// Delete a job's whole Job Destination (Discard of an unfinished job).
    ///
    /// Refuses a folder that is, or contains, the Library Root: a Job
    /// Destination is job-owned and never a shared folder.
    fn delete_job_destination(&self, destination: &Path) -> Result<(), String> {
        if !destination.is_absolute()
            || self
                .library_root
                .as_deref()
                .is_some_and(|root| root.starts_with(destination))
        {
            return Err(format!(
                "refusing to delete {destination:?}: not a job-owned Job Destination"
            ));
        }
        std::fs::remove_dir_all(destination)
            .map_err(|e| format!("delete Job Destination {destination:?}: {e}"))
    }

    /// After an in-flight body read stops, either Discard or Pause.
    fn finalize_stop_after_transfer(&mut self, job_id: &str) -> Result<(), String> {
        let (discard, completed, destination) = {
            let job = self
                .jobs
                .iter()
                .find(|j| j.id() == job_id)
                .ok_or_else(|| format!("job {job_id} not found"))?;
            (
                job.is_discard_requested(),
                job.is_completed(),
                job.destination().to_path_buf(),
            )
        };
        if discard {
            self.finalize_discard(job_id, completed, &destination)
        } else {
            self.finalize_pause(job_id)
        }
    }

    /// List current jobs for shell queue UIs.
    pub fn list_job_snapshots(&self) -> Vec<JobSnapshot> {
        self.jobs
            .iter()
            .enumerate()
            .map(|(queue_index, job)| {
                let title = job
                    .media_title()
                    .or(job.source())
                    .map(str::to_string)
                    .unwrap_or_else(|| job.filename().to_string());
                let status = match job.state() {
                    JobState::Pending if job.transfer_active().load(Ordering::Acquire) => {
                        "downloading"
                    }
                    JobState::Pending => "pending",
                    JobState::Paused => "paused",
                    JobState::Failed => "failed",
                    JobState::Completed => "completed",
                };
                JobSnapshot {
                    job_id: job.id().to_string(),
                    url: job.url().to_string(),
                    destination: job.destination().display().to_string(),
                    status: status.to_string(),
                    bytes_downloaded: job.bytes_downloaded_snapshot(),
                    total_bytes: job.total_bytes(),
                    title,
                    path: if job.is_completed() {
                        Some(job.final_path().display().to_string())
                    } else {
                        None
                    },
                    is_extractor: job.extractor_format().is_some(),
                    extraction_tier: job.extraction_tier(),
                    last_error: job.last_error().map(str::to_string),
                    queue_index,
                }
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Bandwidth cap
    // -----------------------------------------------------------------------

    /// Set the global bandwidth cap in bytes per second.
    ///
    /// When set, aggregate throughput across all jobs is limited to this rate.
    /// None/unset = globally uncapped.
    ///
    /// A cap of 0 is rejected: a bucket that never refills would stall every
    /// transfer. Use [`Self::remove_global_bandwidth_cap`] for uncapped.
    pub fn set_global_bandwidth_cap(&mut self, cap: u64) -> Result<(), String> {
        if cap == 0 {
            return Err("Bandwidth Cap must be at least 1 byte per second".to_string());
        }
        self.bandwidth_cap = Some(cap);
        self.rebuild_global_bucket();
        self.persist_queue_now()
    }

    /// Remove the global bandwidth cap (restore uncapped default).
    pub fn remove_global_bandwidth_cap(&mut self) -> Result<(), String> {
        self.bandwidth_cap = None;
        self.rebuild_global_bucket();
        self.persist_queue_now()
    }

    fn rebuild_global_bucket(&mut self) {
        self.global_bucket = self
            .bandwidth_cap
            .map(|rate| shared_token_bucket(rate, self.clock.clone()));
    }

    /// Return the current global bandwidth cap, if set.
    pub fn global_bandwidth_cap(&self) -> Option<u64> {
        self.bandwidth_cap
    }

    /// Set a per-job bandwidth cap in bytes per second.
    ///
    /// Effective rate is `min(global, per-job)` when both are set.
    pub fn set_job_bandwidth_cap(&mut self, job_id: &str, cap: u64) -> Result<(), String> {
        if cap == 0 {
            return Err("Bandwidth Cap must be at least 1 byte per second".to_string());
        }
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        job.set_bandwidth_cap(cap);
        self.persist_queue_now()?;
        Ok(())
    }

    /// Remove the per-job bandwidth cap for the given job.
    pub fn remove_job_bandwidth_cap(&mut self, job_id: &str) -> Result<(), String> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        job.remove_bandwidth_cap();
        self.persist_queue_now()?;
        Ok(())
    }

    /// Return the per-job bandwidth cap for the given job, if set.
    pub fn job_bandwidth_cap(&self, job_id: &str) -> Result<Option<u64>, String> {
        let job = self
            .jobs
            .iter()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        Ok(job.bandwidth_cap())
    }

    // -----------------------------------------------------------------------
    // Active Job Limit + queue reorder
    // -----------------------------------------------------------------------

    /// Return the Active Job Limit (maximum concurrent transferring jobs).
    pub fn active_job_limit(&self) -> u32 {
        self.active_job_limit
    }

    /// Set the Active Job Limit.  Must be at least 1.
    ///
    /// Lowering the limit does not preempt in-flight transfers; it only
    /// affects how many waiting jobs may start on the next dispatch.
    pub fn set_active_job_limit(&mut self, limit: u32) -> Result<(), String> {
        if limit == 0 {
            return Err("Active Job Limit must be at least 1".to_string());
        }
        self.active_job_limit = limit;
        self.persist_queue_now()?;
        Ok(())
    }

    /// Reorder the queue by moving the job at `from_index` to `to_index`.
    ///
    /// Queue order controls which waiting job starts next when a slot frees.
    /// Already-active transfers are not preempted.
    pub fn reorder_job(&mut self, from_index: usize, to_index: usize) -> Result<(), String> {
        if from_index >= self.jobs.len() || to_index >= self.jobs.len() {
            return Err(format!(
                "reorder index out of bounds (len {}, from {}, to {})",
                self.jobs.len(),
                from_index,
                to_index
            ));
        }
        if from_index == to_index {
            return Ok(());
        }
        let job = self.jobs.remove(from_index);
        self.jobs.insert(to_index, job);
        // Persist before announcing, so a restart never contradicts the event.
        self.persist_queue_now()?;
        let _ = self.event_tx.send(CoreEvent::JobsReordered {
            from_index,
            to_index,
        });
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Default Quality Policy + audio-only mode
    // -----------------------------------------------------------------------

    /// Return the Default Quality Policy (max height, prefer mp4).
    pub fn default_quality_policy(&self) -> DefaultQualityPolicy {
        self.default_quality_policy
    }

    /// Update the Default Quality Policy and persist it.
    pub fn set_default_quality_policy(
        &mut self,
        policy: DefaultQualityPolicy,
    ) -> Result<(), String> {
        if policy.max_height == 0 {
            return Err("Default Quality Policy max height must be at least 1".to_string());
        }
        self.default_quality_policy = policy;
        self.persist_queue_now()?;
        Ok(())
    }

    /// Whether audio-only mode overrides the video Default Quality Policy.
    pub fn audio_only_mode(&self) -> bool {
        self.audio_only_mode
    }

    /// Enable or disable audio-only mode and persist the setting.
    pub fn set_audio_only_mode(&mut self, enabled: bool) -> Result<(), String> {
        self.audio_only_mode = enabled;
        self.persist_queue_now()?;
        Ok(())
    }

    /// Apply the active Default Quality Policy to a quality list.
    pub fn select_default_quality(&self, qualities: &[Quality]) -> Option<String> {
        select_default_quality(
            qualities,
            &self.default_quality_policy,
            self.audio_only_mode,
        )
    }

    // -----------------------------------------------------------------------
    // Media extras defaults + capabilities
    // -----------------------------------------------------------------------

    /// Settings-backed defaults for write/embed media options.
    pub fn default_media_extras(&self) -> MediaExtrasOptions {
        self.default_media_extras
    }

    /// Update media extras defaults and persist them.
    pub fn set_default_media_extras(&mut self, options: MediaExtrasOptions) -> Result<(), String> {
        self.default_media_extras = options;
        self.persist_queue_now()?;
        Ok(())
    }

    /// Capabilities for shells to disable embed toggles when ffmpeg is missing.
    pub fn media_extras_capabilities(&self) -> MediaExtrasCapabilities {
        let ffmpeg_dir = self.extractor.ffmpeg_dir();
        if ffmpeg_available(ffmpeg_dir) {
            MediaExtrasCapabilities {
                ffmpeg_available: true,
                embed_disabled_reason: None,
            }
        } else {
            MediaExtrasCapabilities::unavailable()
        }
    }

    // -----------------------------------------------------------------------
    // Cookie auth channel
    // -----------------------------------------------------------------------

    /// Current cookie auth channel (settings + future Companion hand-off).
    pub fn cookie_channel(&self) -> CookieChannel {
        self.cookie_channel.clone()
    }

    /// Update the cookie auth channel and persist it.
    ///
    /// Companion and desktop settings both use this single channel.
    pub fn set_cookie_channel(&mut self, channel: CookieChannel) -> Result<(), String> {
        let channel = channel.normalized();
        channel.validate()?;
        self.cookie_channel = channel;
        self.persist_queue_now()
    }

    /// Cookie session attached to one job, if any (see
    /// [`EnqueueOptions::cookie_session`]). `None` means the job uses the
    /// global cookie channel.
    pub fn job_cookie_session(&self, job_id: &str) -> Result<Option<CookieChannel>, String> {
        let index = self
            .job_index(job_id)
            .ok_or_else(|| format!("job not found: {job_id}"))?;
        Ok(self.jobs[index].cookie_session().cloned())
    }

    /// Current global proxy settings.
    pub fn proxy_settings(&self) -> ProxySettings {
        self.proxy_settings.clone()
    }

    /// Update global proxy settings and persist them.
    ///
    /// HTTP transfers and future Extractor Toolchain invocations honor the proxy.
    /// Clearing the URL restores direct connectivity.
    pub fn set_proxy_settings(&mut self, settings: ProxySettings) -> Result<(), String> {
        let settings = settings.normalized();
        settings.validate()?;
        self.http.apply_proxy_settings(&settings)?;
        self.proxy_settings = settings;
        self.persist_queue_now()
    }

    /// Current sidecar update manager settings.
    pub fn sidecar_update_settings(&self) -> SidecarUpdateSettings {
        self.sidecar_update_settings.clone()
    }

    /// Update sidecar update manager settings and persist them.
    pub fn set_sidecar_update_settings(
        &mut self,
        settings: SidecarUpdateSettings,
    ) -> Result<(), String> {
        self.sidecar_update_settings = settings;
        self.persist_queue_now()
    }

    /// App-managed sidecar root for Extractor Toolchain and Extractor Plugins.
    pub fn sidecar_root(&self) -> Option<&Path> {
        self.sidecar_root.as_deref()
    }

    /// Set the sidecar root and load persisted runtime state from disk.
    pub fn set_sidecar_root(&mut self, root: PathBuf) -> Result<(), String> {
        std::fs::create_dir_all(&root).map_err(|e| format!("create sidecar root: {e}"))?;
        self.sidecar_runtime_state = sidecar_updates::load_sidecar_runtime_state(&root)?;
        self.extractor.apply_sidecar_root(&root);
        self.sidecar_root = Some(root);
        Ok(())
    }

    /// Replace the sidecar update source (acceptance tests).
    pub fn set_sidecar_update_source(&mut self, source: Arc<dyn SidecarUpdateSource>) {
        self.sidecar_update_source = source;
    }

    /// Shared HTTP backend (shells reuse it for the release update source).
    pub fn http_backend(&self) -> Arc<dyn HttpBackend> {
        Arc::clone(&self.http)
    }

    /// Active update source (shells clone it to do network work off the lock).
    pub fn sidecar_update_source(&self) -> Arc<dyn SidecarUpdateSource> {
        Arc::clone(&self.sidecar_update_source)
    }

    /// [`Self::check_sidecar_updates`] with the Core mutex released during the lookup.
    pub fn check_sidecar_updates_outside_lock(
        core: Arc<Mutex<Self>>,
    ) -> Result<SidecarUpdateCheckResult, String> {
        let (root, settings, source) = {
            let guard = core.lock().map_err(|e| e.to_string())?;
            (
                guard.require_sidecar_root()?.to_path_buf(),
                guard.sidecar_update_settings.clone(),
                Arc::clone(&guard.sidecar_update_source),
            )
        };
        // A manual Check must see upstream now, not a lookup cached up to an hour ago.
        source.invalidate_cache();
        check_sidecar_updates(&root, &settings, source.as_ref())
    }

    /// Download the latest toolchain release off the lock, then swap it in.
    ///
    /// Returns `BlockedMidTransfer` without downloading when transfers are
    /// active; `NothingToApply` when the installed version is current.
    pub fn install_toolchain_latest(core: Arc<Mutex<Self>>) -> Result<SidecarApplyResult, String> {
        let (root, settings, source) = {
            let guard = core.lock().map_err(|e| e.to_string())?;
            if guard.transfers_active() {
                return Ok(SidecarApplyResult {
                    outcome: SidecarApplyOutcome::BlockedMidTransfer,
                    applied: Vec::new(),
                });
            }
            (
                guard.require_sidecar_root()?.to_path_buf(),
                guard.sidecar_update_settings.clone(),
                Arc::clone(&guard.sidecar_update_source),
            )
        };
        // Network work (release lookup + asset download + checksum) outside the lock.
        let prefetched =
            sidecar_updates::prefetch_toolchain_update(&root, &settings, source.as_ref())?;
        if prefetched.is_none() {
            return Ok(SidecarApplyResult {
                outcome: SidecarApplyOutcome::NothingToApply,
                applied: Vec::new(),
            });
        }
        let mut guard = core.lock().map_err(|e| e.to_string())?;
        guard.apply_sidecar_updates()
    }

    fn transfers_active(&self) -> bool {
        self.job_signals.active_transfer_count() > 0
    }

    fn require_sidecar_root(&self) -> Result<&Path, String> {
        self.sidecar_root
            .as_deref()
            .ok_or_else(|| "sidecar root not configured".to_string())
    }

    /// Manual check for Extractor Toolchain and Extractor Plugin updates.
    pub fn check_sidecar_updates(&self) -> Result<SidecarUpdateCheckResult, String> {
        let root = self.require_sidecar_root()?;
        check_sidecar_updates(
            root,
            &self.sidecar_update_settings,
            self.sidecar_update_source.as_ref(),
        )
    }

    /// Apply available sidecar updates when idle; defer while transfers are active.
    pub fn apply_sidecar_updates(&mut self) -> Result<SidecarApplyResult, String> {
        let root = self.require_sidecar_root()?.to_path_buf();
        let transfers_active = self.transfers_active();
        let result = apply_sidecar_updates(
            &root,
            &self.sidecar_update_settings,
            self.sidecar_update_source.as_ref(),
            transfers_active,
            &mut self.sidecar_runtime_state,
        )?;
        self.extractor.apply_sidecar_root(&root);
        Ok(result)
    }

    /// On launch: apply deferred updates and optional on-launch check when idle.
    pub fn apply_sidecar_updates_on_launch(&mut self) -> Result<SidecarApplyResult, String> {
        let root = self.require_sidecar_root()?.to_path_buf();
        let transfers_active = self.transfers_active();
        let result = apply_sidecar_updates_on_launch(
            &root,
            &self.sidecar_update_settings,
            self.sidecar_update_source.as_ref(),
            transfers_active,
            &mut self.sidecar_runtime_state,
        )?;
        self.extractor.apply_sidecar_root(&root);
        Ok(result)
    }

    /// Pin a sidecar component to a specific version.
    pub fn pin_sidecar_version(
        &mut self,
        component: SidecarComponent,
        version: String,
    ) -> Result<(), String> {
        let trimmed = version.trim();
        if trimmed.is_empty() {
            return Err("pin version cannot be empty".to_string());
        }
        match component {
            SidecarComponent::Toolchain => {
                self.sidecar_update_settings.toolchain_pin = Some(trimmed.to_string());
            }
            SidecarComponent::Plugin => {
                self.sidecar_update_settings.plugin_pin = Some(trimmed.to_string());
            }
        }
        self.persist_queue_now()
    }

    /// Clear a sidecar version pin.
    pub fn unpin_sidecar_version(&mut self, component: SidecarComponent) -> Result<(), String> {
        match component {
            SidecarComponent::Toolchain => self.sidecar_update_settings.toolchain_pin = None,
            SidecarComponent::Plugin => self.sidecar_update_settings.plugin_pin = None,
        }
        self.persist_queue_now()
    }

    /// Roll back one sidecar component to the previous installed version.
    pub fn rollback_sidecar(&mut self, component: SidecarComponent) -> Result<(), String> {
        let root = self.require_sidecar_root()?.to_path_buf();
        rollback_sidecar_component(
            &root,
            component,
            self.transfers_active(),
            &mut self.sidecar_runtime_state,
        )?;
        self.extractor.apply_sidecar_root(&root);
        Ok(())
    }

    /// Current Filename Template for extractor job outputs.
    pub fn filename_template(&self) -> &str {
        &self.filename_template
    }

    /// Update the Filename Template and persist it.
    pub fn set_filename_template(&mut self, template: String) -> Result<(), String> {
        validate_filename_template(&template)?;
        self.filename_template = template.trim().to_string();
        self.persist_queue_now()
    }

    fn apply_extractor_filename(&self, job: &mut job::Job, item: &MediaItem, quality: &Quality) {
        let ext = quality.container.as_deref().unwrap_or("mp4");
        let raw_id = media_item_raw_id(&item.id);
        let vars = TemplateVars {
            title: &item.title,
            id: raw_id,
            uploader: item.uploader.as_deref(),
            upload_date: item.upload_date.as_deref(),
            ext,
        };
        let (ytdlp_spec, basename) =
            prepare_extractor_output(job.destination(), &self.filename_template, &vars);
        job.set_ytdlp_output_spec(ytdlp_spec);
        job.set_filename(basename);
        job.set_media_probe_metadata(item.uploader.clone(), item.upload_date.clone());
    }

    fn extractor_output_for_job(&self, job: &job::Job) -> (String, String) {
        if let (Some(media_id), Some(title)) = (job.media_id(), job.media_title()) {
            let ext = job
                .filename()
                .rsplit_once('.')
                .map(|(_, e)| e)
                .unwrap_or("mp4");
            let vars = TemplateVars {
                title,
                id: media_item_raw_id(media_id),
                uploader: job.media_uploader(),
                upload_date: job.media_upload_date(),
                ext,
            };
            return prepare_extractor_output(job.destination(), &self.filename_template, &vars);
        }
        (
            self.filename_template.clone(),
            render_template(
                &self.filename_template,
                &TemplateVars {
                    title: "download",
                    id: "id",
                    uploader: None,
                    upload_date: None,
                    ext: "mp4",
                },
            ),
        )
    }

    /// Effective media extras for a probe confirm (defaults + optional override).
    pub fn resolve_media_extras(
        &self,
        override_options: Option<MediaExtrasOptions>,
    ) -> MediaExtrasOptions {
        let base = override_options.unwrap_or(self.default_media_extras);
        effective_options(&base, &self.media_extras_capabilities())
    }

    /// Media extras stored on a job (after confirm).
    pub fn job_media_extras(&self, job_id: &str) -> Result<MediaExtrasOptions, String> {
        self.jobs
            .iter()
            .find(|j| j.id() == job_id)
            .map(|j| j.media_extras())
            .ok_or_else(|| format!("job {job_id} not found"))
    }

    // -----------------------------------------------------------------------
    // Mirror Set management
    // -----------------------------------------------------------------------

    /// Add a mirror URL to an existing job's Mirror Set.
    ///
    /// The mirror is appended to the ordered list and will be tried
    /// if the current mirror fails.
    pub fn add_job_mirror(&mut self, job_id: &str, url: String) -> Result<(), String> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        job.add_mirror(url);
        self.persist_queue_now()?;
        Ok(())
    }

    /// Parse a Metalink / `.meta4` XML document and add discovered URLs
    /// as mirrors to the specified job.
    pub fn add_metalink_to_job(&mut self, job_id: &str, xml: &str) -> Result<usize, String> {
        let urls = MirrorSet::from_metalink(xml);
        let count = urls.len();
        let expected_hash = hash_from_metalink(xml);
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        let mut changed = false;
        if let Some(hash) = expected_hash {
            job.set_expected_hash(hash);
            changed = true;
        }
        for url in urls {
            job.add_mirror(url);
            changed = true;
        }
        if changed {
            self.persist_queue_now()?;
        }
        Ok(count)
    }

    /// Set a user-supplied expected hash for post-completion Integrity Check.
    pub fn set_job_expected_hash(
        &mut self,
        job_id: &str,
        algorithm: &str,
        digest: &str,
    ) -> Result<(), String> {
        let hash = ExpectedHash::parse(algorithm, digest)?;
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        job.set_expected_hash(hash);
        self.persist_queue_now()?;
        Ok(())
    }

    /// Load an expected hash from a GNU checksum sidecar file on disk.
    pub fn set_job_expected_hash_from_sidecar(
        &mut self,
        job_id: &str,
        sidecar_path: &Path,
    ) -> Result<(), String> {
        let text = std::fs::read_to_string(sidecar_path)
            .map_err(|e| format!("read sidecar {}: {e}", sidecar_path.display()))?;
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        let hash = ExpectedHash::from_sidecar_for_file(&text, job.filename())?;
        job.set_expected_hash(hash);
        self.persist_queue_now()?;
        Ok(())
    }

    /// Return all mirror URLs for a job.
    pub fn job_mirrors(&self, job_id: &str) -> Result<Vec<String>, String> {
        let job = self
            .jobs
            .iter()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        Ok(job.mirror_urls().to_vec())
    }

    // -----------------------------------------------------------------------
    // Probe-then-Confirm
    // -----------------------------------------------------------------------

    /// Probe a URL (synchronous; holds `&mut self` for the whole network call).
    ///
    /// Shells should prefer [`Self::probe_outside_lock`]. This method keeps the
    /// simple API for tests and tools: plan → run → finish.
    ///
    /// Emits `CoreEvent::ProbeResult` on success (result stored for
    /// [`Self::confirm_probe_selection`]) or `CoreEvent::ProbeFailed`.
    pub fn probe(&mut self, url: &str) -> Result<ProbeResult, String> {
        let plan = self.plan_probe(url)?;
        let outcome = plan.run();
        self.finish_probe(&plan, outcome)
    }

    /// Probe with the Core mutex released during the network work.
    pub fn probe_outside_lock(core: Arc<Mutex<Self>>, url: &str) -> Result<ProbeResult, String> {
        let plan = {
            let mut guard = core.lock().map_err(|e| e.to_string())?;
            guard.plan_probe(url)?
        };
        let outcome = plan.run();
        let mut guard = core.lock().map_err(|e| e.to_string())?;
        guard.finish_probe(&plan, outcome)
    }

    /// Capture everything a probe needs (settings, toolchain, cancel flag).
    pub fn plan_probe(&mut self, url: &str) -> Result<ProbePlan, String> {
        let cookies = normalize_url(url)
            .and_then(|key| self.probe_cookie_sessions.get(&key).cloned())
            .unwrap_or_else(|| self.cookie_channel.clone());
        let mut plan = ProbePlan::standalone(
            url,
            Arc::clone(&self.extractor),
            Arc::clone(&self.sniffer),
            self.probe_tier_settings,
            cookies,
            self.proxy_settings.clone(),
        )?;
        self.next_probe_id += 1;
        plan.id = self.next_probe_id;
        self.probe_cancels
            .insert(plan.id, (plan.url().to_string(), plan.cancel_flag()));
        Ok(plan)
    }

    /// Record a probe outcome: archive marks, pending store, events.
    ///
    /// Only this plan's own cancel flag is released, so another probe of the
    /// same URL that is still running keeps its flag. A failed probe also drops
    /// any result stored for the URL by an earlier probe, so the sheet the user
    /// is looking at cannot be confirmed from a stale result.
    pub fn finish_probe(
        &mut self,
        plan: &ProbePlan,
        outcome: Result<ProbeResult, String>,
    ) -> Result<ProbeResult, String> {
        self.probe_cancels.remove(&plan.id);
        let url = plan.url();
        match outcome {
            Ok(mut result) => {
                self.mark_archived_probe_items(&mut result.items);
                self.pending_probes.insert(url.to_string(), result.clone());
                self.event_tx
                    .send(CoreEvent::ProbeResult {
                        url: url.to_string(),
                        items: result.items.clone(),
                        source_name: result.source_name.clone(),
                    })
                    .map_err(|_| "event channel closed".to_string())?;
                Ok(result)
            }
            Err(err) => {
                self.pending_probes.remove(url);
                self.event_tx
                    .send(CoreEvent::ProbeFailed {
                        url: url.to_string(),
                        error: err.clone(),
                    })
                    .map_err(|_| "event channel closed".to_string())?;
                Err(err)
            }
        }
    }

    /// Ask an in-flight probe to stop between tiers. Returns whether one was found.
    pub fn cancel_probe(&self, url: &str) -> bool {
        let Some(key) = normalize_url(url) else {
            return false;
        };
        let mut found = false;
        for (_, (probe_url, flag)) in self.probe_cancels.iter() {
            if *probe_url == key {
                flag.store(true, Ordering::Release);
                found = true;
            }
        }
        found
    }

    /// Current fallback-chain settings.
    pub fn probe_tier_settings(&self) -> ProbeTierSettings {
        self.probe_tier_settings
    }

    /// Update and persist fallback-chain settings.
    pub fn set_probe_tier_settings(&mut self, settings: ProbeTierSettings) -> Result<(), String> {
        settings.validate()?;
        self.probe_tier_settings = settings;
        self.persist_queue_now()
    }

    /// Replace the page sniffer (tests inject [`FakePageSniffer`]).
    pub fn set_page_sniffer(&mut self, sniffer: Arc<dyn PageSniffer>) {
        self.sniffer = sniffer;
    }

    /// Detected Extractor Toolchain state (yt-dlp, ffmpeg, JS runtime, EJS).
    ///
    /// Runs `--version` probes; shells should call it off the main thread and cache.
    pub fn toolchain_status(&self) -> ToolchainStatus {
        let mut status = self.extractor.toolchain_status();
        status.sidecar_root = self.sidecar_root.as_ref().map(|p| p.display().to_string());
        status
    }

    /// Fetch exact formats for one item from a pending probe and update stored qualities.
    pub fn enrich_probe_item_qualities(
        &mut self,
        probe_url: &str,
        item_id: &str,
    ) -> Result<Vec<Quality>, String> {
        let plan = self.plan_enrich(probe_url, item_id)?;
        let qualities = plan.run()?;
        Ok(self.finish_enrich(&plan, qualities))
    }

    /// [`Self::enrich_probe_item_qualities`] with the mutex released during yt-dlp.
    pub fn enrich_probe_item_qualities_outside_lock(
        core: Arc<Mutex<Self>>,
        probe_url: &str,
        item_id: &str,
    ) -> Result<Vec<Quality>, String> {
        let plan = {
            let guard = core.lock().map_err(|e| e.to_string())?;
            guard.plan_enrich(probe_url, item_id)?
        };
        let qualities = plan.run()?;
        let mut guard = core.lock().map_err(|e| e.to_string())?;
        Ok(guard.finish_enrich(&plan, qualities))
    }

    fn plan_enrich(&self, probe_url: &str, item_id: &str) -> Result<EnrichPlan, String> {
        let url_key = normalize_url(probe_url).ok_or_else(|| "invalid URL".to_string())?;
        let probe = self
            .pending_probes
            .get(&url_key)
            .ok_or_else(|| format!("no pending probe for {}. Call probe() first.", url_key))?;
        let item = probe
            .items
            .iter()
            .find(|i| i.id == item_id)
            .ok_or_else(|| format!("item '{item_id}' not found in probe result for {url_key}"))?;
        let cookies = self
            .probe_cookie_sessions
            .get(&url_key)
            .cloned()
            .unwrap_or_else(|| self.cookie_channel.clone());
        Ok(EnrichPlan {
            url_key,
            item_id: item_id.to_string(),
            item_url: item.url.clone(),
            fetch_kind: item.fetch_kind,
            existing: item.qualities.clone(),
            extractor: Arc::clone(&self.extractor),
            cookies,
            proxy: self.proxy_settings.clone(),
        })
    }

    fn finish_enrich(&mut self, plan: &EnrichPlan, qualities: Vec<Quality>) -> Vec<Quality> {
        if let Some(probe) = self.pending_probes.get_mut(&plan.url_key) {
            if let Some(item) = probe.items.iter_mut().find(|i| i.id == plan.item_id) {
                item.qualities = qualities.clone();
                item.qualities_are_presets = false;
            }
        }
        qualities
    }

    /// Preview where [`Self::confirm_probe_selection`] will place Job
    /// Destinations, without creating jobs.
    ///
    /// Always a unique nested Job Destination: under Library Root `jobs/` by
    /// default, or under an absolute Save-to parent when overridden.  Preview
    /// includes a sanitized source/title subfolder when available, with an
    /// ellipsis standing in for the unique id suffix.
    pub fn preview_probe_destination(
        &self,
        selection: &ProbeSelection,
        destination: Option<PathBuf>,
    ) -> Result<ProbeDestinationPreview, String> {
        let root = self
            .library_root
            .as_ref()
            .ok_or_else(|| "Library Root not set".to_string())?;

        if let Some(ref dest) = destination {
            Self::require_absolute_destination(dest)?;
        }

        if selection.items.is_empty() {
            return Err("selection has no items".to_string());
        }

        let url_key = normalize_url(&selection.url)
            .ok_or_else(|| format!("invalid URL: {}", selection.url))?;
        let probe = self
            .pending_probes
            .get(&url_key)
            .ok_or_else(|| format!("no pending probe for {}. Call probe() first.", url_key))?;

        let items_map: HashMap<&str, &MediaItem> =
            probe.items.iter().map(|i| (i.id.as_str(), i)).collect();

        let first = &selection.items[0];
        let item = items_map.get(first.item_id.as_str()).ok_or_else(|| {
            format!(
                "item '{}' not found in probe result for {}",
                first.item_id, selection.url
            )
        })?;

        let job_source = Self::probe_job_source(probe, item);
        let path =
            Self::preview_destination_path(root, destination.as_deref(), job_source.as_deref());

        Ok(ProbeDestinationPreview {
            path_display: path.display().to_string(),
        })
    }

    fn probe_job_source(probe: &ProbeResult, item: &MediaItem) -> Option<String> {
        probe.source_name.clone().or_else(|| {
            if probe.items.len() == 1 {
                Some(item.title.clone())
            } else {
                None
            }
        })
    }

    fn preview_destination_path(
        library_root: &Path,
        destination: Option<&Path>,
        job_source: Option<&str>,
    ) -> PathBuf {
        let parent = match destination {
            Some(dest) => dest.to_path_buf(),
            None => library_root.join("jobs"),
        };

        match job_source {
            Some(name) => {
                let safe = job::sanitize_folder_name(name);
                parent.join(format!("{safe}-{DEST_ID_PLACEHOLDER}"))
            }
            None => parent.join(DEST_ID_PLACEHOLDER),
        }
    }

    /// Confirm a probe selection and create jobs for the selected items.
    ///
    /// Takes a [`ProbeSelection`] that references a previously probed URL
    /// and lists which items at which qualities to download.  Returns the
    /// job IDs of the created jobs.
    ///
    /// Each selected item creates one job with a unique job-owned Job
    /// Destination.  Without `destination`, destinations live under the
    /// Library Root `jobs/` segment (source/title naming when available).
    /// With an absolute `destination` override, each item nests under that
    /// parent (same naming rules, no extra `jobs/` segment) — never the
    /// shared parent itself.
    ///
    /// Errors if no pending probe exists for the URL, or if the selection
    /// references unknown items or qualities.
    pub fn confirm_probe_selection(
        &mut self,
        selection: &ProbeSelection,
        destination: Option<PathBuf>,
    ) -> Result<Vec<String>, String> {
        let root = self
            .library_root
            .as_ref()
            .ok_or_else(|| "Library Root not set".to_string())?;

        if let Some(ref dest) = destination {
            Self::require_absolute_destination(dest)?;
        }

        let url_key = normalize_url(&selection.url)
            .ok_or_else(|| format!("invalid URL: {}", selection.url))?;
        let probe = self
            .pending_probes
            .get(&url_key)
            .ok_or_else(|| format!("no pending probe for {}. Call probe() first.", url_key))?;

        // Build lookup maps
        let items_map: HashMap<&str, &MediaItem> =
            probe.items.iter().map(|i| (i.id.as_str(), i)).collect();

        let mut new_jobs: Vec<job::Job> = Vec::new();
        let media_extras = self.resolve_media_extras(selection.media_extras);

        for sel in &selection.items {
            let item = items_map.get(sel.item_id.as_str()).ok_or_else(|| {
                format!(
                    "item '{}' not found in probe result for {}",
                    sel.item_id, selection.url
                )
            })?;

            // Validate quality exists; carry toolchain size onto the job when known.
            let quality = item
                .qualities
                .iter()
                .find(|q| q.id == sel.quality_id)
                .ok_or_else(|| {
                    format!(
                        "quality '{}' not found for item '{}' (available: {:?})",
                        sel.quality_id,
                        sel.item_id,
                        item.qualities.iter().map(|q| &q.id).collect::<Vec<_>>()
                    )
                })?;

            let job_source = Self::probe_job_source(probe, item);
            // Sniffed variants carry their own URL in the quality id.
            let (quality_id, quality_url) = page_sniffer::split_quality_url(&sel.quality_id);
            let media_url = quality_url
                .map(str::to_string)
                .unwrap_or_else(|| item.url.clone());

            let mut job = if let Some(ref dest_parent) = destination {
                job::Job::new_with_source_at(media_url.clone(), dest_parent.clone(), job_source)
            } else {
                job::Job::new_with_source(media_url.clone(), root.clone(), job_source)
            };
            job.set_media_extras(media_extras);
            job.set_media_identity(item.id.clone(), item.title.clone());
            job.set_extraction_tier(probe.tier);
            job.set_referer(item.referer.clone());
            job.set_cookie_session(self.probe_cookie_sessions.get(&url_key).cloned());
            match item.fetch_kind {
                MediaFetchKind::Extractor => {
                    // Media behind an extractor is fetched by yt-dlp, not plain HTTP of the page URL.
                    job.set_extractor_format(quality_id);
                    self.apply_extractor_filename(&mut job, item, quality);
                }
                MediaFetchKind::DirectHttp => {
                    // A plain file: Core's HTTP backend fetches `media_url` itself.
                    if probe.tier == ExtractionTier::DirectFile {
                        let name = sanitize_filename(&item.title);
                        if !name.is_empty() {
                            job.set_filename(name);
                        }
                    }
                }
            }
            if let Some(size) = quality.size_bytes {
                job.set_total_bytes(Some(size));
            }

            new_jobs.push(job);
        }

        // The whole selection is valid; queue it in one step. A failed save
        // keeps the pending probe so the user can confirm again without
        // duplicating anything.
        let job_ids: Vec<String> = new_jobs.iter().map(|j| j.id().to_string()).collect();
        self.push_jobs_persisted(new_jobs)?;
        self.pending_probes.remove(&url_key);
        self.probe_cookie_sessions.remove(&url_key);
        Ok(job_ids)
    }

    /// Drop a stored probe result for a URL (e.g. user cancelled the probe sheet).
    pub fn clear_pending_probe(&mut self, url_str: &str) -> Result<(), String> {
        let url_key = normalize_url(url_str).ok_or_else(|| format!("invalid URL: {url_str}"))?;
        self.pending_probes.remove(&url_key);
        self.probe_cookie_sessions.remove(&url_key);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Processing
    // -----------------------------------------------------------------------

    /// Process pending jobs synchronously — each transfer runs to completion
    /// before the next starts.  Prefer [`Self::dispatch_pending`] in shells.
    pub fn process_pending(&mut self) -> Result<(), String> {
        let global_bucket = self.global_bucket.clone();

        let indices: Vec<usize> = self
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| j.is_pending())
            .map(|(i, _)| i)
            .collect();

        for i in indices {
            self.transfer_job_at_index(i, &global_bucket)?;
        }

        self.flush_pending_progress()?;
        Ok(())
    }

    /// Start pending transfers up to the Active Job Limit without blocking.
    pub fn dispatch_pending(core: Arc<Mutex<Self>>) -> Result<(), String> {
        let (global_bucket, job_ids) = {
            let mut core = core.lock().map_err(|e| e.to_string())?;
            core.persist_segment_progress()?;
            let slots = core
                .active_job_limit
                .saturating_sub(core.job_signals.active_transfer_count() as u32)
                as usize;
            if slots == 0 {
                return Ok(());
            }
            let global_bucket = core.global_bucket.clone();
            let mut job_ids = Vec::new();
            for job in &core.jobs {
                if !job.is_pending() {
                    continue;
                }
                if job.transfer_active().load(Ordering::Acquire) {
                    continue;
                }
                // Reserve the slot under the lock, so the next round counts
                // this job before its thread has had time to plan.
                job.transfer_active().store(true, Ordering::Release);
                job_ids.push(job.id().to_string());
                if job_ids.len() >= slots {
                    break;
                }
            }
            (global_bucket, job_ids)
        };

        for job_id in job_ids {
            let core = Arc::clone(&core);
            let bucket = global_bucket.clone();
            std::thread::spawn(move || {
                if let Err(e) =
                    Self::run_transfer_outside_lock(Arc::clone(&core), job_id.clone(), bucket)
                {
                    if let Err(report) = Self::report_transfer_thread_failure(core, &job_id, e) {
                        eprintln!("transfer job failure reporting failed for {job_id}: {report}");
                    }
                }
            });
        }
        Ok(())
    }

    /// Mark a background transfer thread failure on the job when still pending.
    fn report_transfer_thread_failure(
        core: Arc<Mutex<Self>>,
        job_id: &str,
        error: String,
    ) -> Result<(), String> {
        let mut core = core.lock().map_err(|e| e.to_string())?;
        let Some(i) = core.jobs.iter().position(|j| j.id() == job_id) else {
            return Ok(());
        };
        if !core.jobs[i].is_pending() {
            return Ok(());
        }
        core.finalize_failed_job(job_id, i, error)
    }

    /// Run a job transfer, releasing the Core mutex during blocking body I/O.
    ///
    /// The caller has claimed the job's Active Job Limit slot (its
    /// `transfer_active` flag) under the lock; every exit releases it.
    fn run_transfer_outside_lock(
        core: Arc<Mutex<Self>>,
        job_id: String,
        global_bucket: Option<SharedTokenBucket>,
    ) -> Result<(), String> {
        loop {
            let head = Self::fetch_head_unlocked(&core, &job_id)?;
            let plan = {
                let mut core = core.lock().map_err(|e| e.to_string())?;
                core.plan_unlocked_transfer(&job_id, &global_bucket, true, head.as_ref())?
            };
            let Some(plan) = plan else {
                return Ok(());
            };

            let outcome = plan.run();

            let retry = {
                let mut core = core.lock().map_err(|e| e.to_string())?;
                let retry = core.finish_unlocked_transfer(outcome)?;
                if retry {
                    // Keep the slot across the failover replan.
                    if let Some(i) = core.job_index(&job_id) {
                        core.jobs[i]
                            .transfer_active()
                            .store(true, Ordering::Release);
                    }
                }
                retry
            };
            if !retry {
                return Ok(());
            }
        }
    }

    /// Ask the origin the HEAD questions planning needs, without the Core
    /// lock: a slow server must not freeze Pause, Discard and queue reads.
    /// Planning re-validates the answers against the job under the lock.
    fn fetch_head_unlocked(
        core: &Arc<Mutex<Self>>,
        job_id: &str,
    ) -> Result<Option<PlanningHead>, String> {
        let (http, url, fresh) = {
            let core = core.lock().map_err(|e| e.to_string())?;
            let Some(i) = core.job_index(job_id) else {
                return Ok(None);
            };
            let job = &core.jobs[i];
            let needs_size = job.is_pending()
                && job.extractor_format().is_none()
                && job.segments().is_empty()
                && (job.total_bytes().is_none() || job.bytes_received() == 0);
            if !needs_size {
                return Ok(None);
            }
            (
                core.http.clone(),
                job.url().to_string(),
                job.bytes_received() == 0,
            )
        };
        let supports_range = if fresh {
            http.supports_range(&url)
        } else {
            Err("not asked for a resume".to_string())
        };
        let file_size = http.file_size(&url);
        Ok(Some(PlanningHead {
            url,
            supports_range,
            file_size,
        }))
    }

    /// Prepare a transfer that can run without holding the Core mutex.
    ///
    /// Every transfer kind (single HTTP fetch, segmented HTTP fetch, Extractor
    /// Toolchain download) is planned under the lock and executed outside it.
    /// Returns `None` when the job is not pending, is already transferring, or
    /// was finalized Failed during planning (disk space, no extractor resume).
    ///
    /// `claimed` means the caller already set the job's `transfer_active` flag
    /// under the lock to reserve an Active Job Limit slot; any outcome other
    /// than a plan releases that reservation.
    fn plan_unlocked_transfer(
        &mut self,
        job_id: &str,
        global_bucket: &Option<SharedTokenBucket>,
        claimed: bool,
        head: Option<&PlanningHead>,
    ) -> Result<Option<UnlockedTransferPlan>, String> {
        let planned = self.plan_unlocked_transfer_inner(job_id, global_bucket, claimed, head);
        if claimed && !matches!(planned, Ok(Some(_))) {
            if let Some(i) = self.job_index(job_id) {
                self.jobs[i]
                    .transfer_active()
                    .store(false, Ordering::Release);
            }
        }
        planned
    }

    fn plan_unlocked_transfer_inner(
        &mut self,
        job_id: &str,
        global_bucket: &Option<SharedTokenBucket>,
        claimed: bool,
        head: Option<&PlanningHead>,
    ) -> Result<Option<UnlockedTransferPlan>, String> {
        let i = self
            .jobs
            .iter()
            .position(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        if !self.jobs[i].is_pending() {
            return Ok(None);
        }
        if !claimed && self.jobs[i].transfer_active().load(Ordering::Acquire) {
            return Ok(None);
        }
        // Pause or Discard arrived after the job was picked: honour it now
        // instead of starting a transfer that would stop at its first write.
        if self.jobs[i].pause_requested().load(Ordering::Acquire)
            || self.jobs[i].is_discard_requested()
        {
            self.finalize_stop_after_transfer(job_id)?;
            return Ok(None);
        }

        let url = self.jobs[i].url().to_string();
        let dest = self.jobs[i].destination().to_path_buf();
        let bytes_received = self.jobs[i].bytes_received();
        let extractor_format = self.jobs[i].extractor_format().map(|s| s.to_string());

        let ytdlp_output_spec = extractor_format.as_ref().map(|_| {
            let (spec, basename) = self.extractor_output_for_job(&self.jobs[i]);
            self.jobs[i].set_filename(basename);
            spec
        });

        if !self.guard_disk_space_for_transfer(i, job_id, &dest, &url, bytes_received, head)? {
            return Ok(None);
        }

        let pause_requested = self.jobs[i].pause_requested().clone();
        let transfer_active = self.jobs[i].transfer_active().clone();

        if let Some(format) = extractor_format {
            let output_spec = ytdlp_output_spec.unwrap_or_else(|| self.filename_template.clone());
            let on_disk = storage::destination_byte_total(&dest);
            if on_disk > 0 && !self.extractor.supports_extractor_resume() {
                self.finalize_failed_job(
                    job_id,
                    i,
                    "extractor cannot resume partial download; use Retry".into(),
                )?;
                return Ok(None);
            }

            let live_bytes = self.jobs[i].live_downloaded();
            live_bytes.store(0, Ordering::Release);
            let total_for_progress = self.jobs[i].total_bytes();
            self.event_tx
                .send(CoreEvent::JobProgress {
                    job_id: job_id.to_string(),
                    bytes_downloaded: 0,
                    total_bytes: total_for_progress,
                })
                .map_err(|_| "event channel closed".to_string())?;

            // Claim the job under the lock so a concurrent dispatch cannot start it twice.
            transfer_active.store(true, Ordering::Release);
            // `pause_requested` is also set by Discard (`signal_discard_stop`);
            // both Pause and Discard use it as the extractor stop signal.
            return Ok(Some(UnlockedTransferPlan::ExtractorFetch {
                job_id: job_id.to_string(),
                url,
                format,
                dest,
                output_spec,
                media_extras: self.jobs[i].media_extras(),
                cookies: self.jobs[i]
                    .cookie_session()
                    .cloned()
                    .unwrap_or_else(|| self.cookie_channel.clone()),
                proxy: self.proxy_settings.clone(),
                extractor: Arc::clone(&self.extractor),
                stop: pause_requested,
                transfer_active,
                live_bytes,
                event_tx: self.event_tx.clone(),
                total_for_progress,
                referer: self.jobs[i].referer().map(str::to_string),
            }));
        }

        let filename = self.jobs[i].filename().to_string();
        let job_bucket = self.jobs[i].bandwidth_bucket(&self.clock);

        match self.plan_segmented_transfer(
            i,
            job_id,
            &url,
            &dest,
            &filename,
            &job_bucket,
            global_bucket,
            head,
        )? {
            SegmentedPlanning::NotApplicable => {}
            SegmentedPlanning::Plan(plan) => return Ok(Some(*plan)),
            SegmentedPlanning::Finalized => return Ok(None),
        }

        let known_total = self.head_file_size(&url, head).ok().filter(|&n| n > 0);
        if let Some(total) = known_total {
            self.jobs[i].set_total_bytes(Some(total));
        }
        let total_for_progress = self.jobs[i].total_bytes();

        self.event_tx
            .send(CoreEvent::JobProgress {
                job_id: job_id.to_string(),
                bytes_downloaded: bytes_received,
                total_bytes: total_for_progress,
            })
            .map_err(|_| "event channel closed".to_string())?;

        std::fs::create_dir_all(&dest).map_err(|e| format!("create job dir {dest:?}: {e}"))?;

        let file_path = dest.join(&filename);
        let part_path = Self::part_path_for(&file_path);

        if bytes_received > 0 && !part_path.exists() && file_path.exists() {
            std::fs::rename(&file_path, &part_path)
                .map_err(|e| format!("migrate partial {file_path:?} -> {part_path:?}: {e}"))?;
        }

        // The `.part` length is the truth for where to resume; the counter can
        // lag a failed attempt or lead a file that was removed.
        let mut bytes_received = bytes_received;
        if bytes_received > 0 {
            let part_len = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
            let total = self.jobs[i].total_bytes();
            if part_len > 0 && Some(part_len) == total {
                // A crash between the last byte and the rename leaves a
                // complete `.part`; asking for `bytes=total-` would get a 416.
                self.complete_job_file(job_id, i, &part_path, &file_path, part_len)?;
                return Ok(None);
            }
            if part_len == 0 || total.is_some_and(|t| part_len > t) {
                std::fs::File::create(&part_path)
                    .map_err(|e| format!("truncate partial {part_path:?}: {e}"))?;
                bytes_received = 0;
            } else {
                bytes_received = part_len;
            }
            self.jobs[i].set_bytes_received(bytes_received);
        }

        let live_bytes = self.jobs[i].live_downloaded();
        live_bytes.store(bytes_received, Ordering::Release);
        let progress = TransferProgress {
            job_id: job_id.to_string(),
            event_tx: self.event_tx.clone(),
            total_bytes: total_for_progress,
            live_bytes,
        };

        let range = if bytes_received > 0 {
            Some(bytes_received..u64::MAX)
        } else {
            None
        };
        let if_range = if range.is_some() {
            self.jobs[i].validator().map(str::to_string)
        } else {
            None
        };

        transfer_active.store(true, Ordering::Release);
        Ok(Some(UnlockedTransferPlan::SingleFetch {
            job_id: job_id.to_string(),
            bytes_received,
            url,
            part_path,
            file_path,
            range,
            if_range,
            append: bytes_received > 0,
            truncate: bytes_received == 0,
            http: self.http.clone(),
            global_bucket: global_bucket.clone(),
            job_bucket,
            pause_requested,
            transfer_active,
            progress,
        }))
    }

    /// Plan a Segmented Transfer: continue the job's segment table, or start a
    /// new one for a fresh job whose server supports ranges and that has no
    /// Bandwidth Cap. The table is persisted before any byte is written, so a
    /// crash never leaves a preallocated `.part` without it.
    #[allow(clippy::too_many_arguments)]
    fn plan_segmented_transfer(
        &mut self,
        i: usize,
        job_id: &str,
        url: &str,
        dest: &Path,
        filename: &str,
        job_bucket: &Option<SharedTokenBucket>,
        global_bucket: &Option<SharedTokenBucket>,
        head: Option<&PlanningHead>,
    ) -> Result<SegmentedPlanning, String> {
        let file_path = dest.join(filename);
        let part_path = Self::part_path_for(&file_path);

        let mut resumed = !self.jobs[i].segments().is_empty();
        if resumed {
            let table = self.jobs[i].segments().to_vec();
            let total = table.last().map_or(0, |s| s.end);
            let part_len = std::fs::metadata(&part_path).map(|m| m.len()).ok();
            if storage::Segment::table_is_valid(&table, total) && part_len == Some(total) {
                self.jobs[i].set_total_bytes(Some(total));
            } else {
                // The file no longer matches the table; start over.
                self.jobs[i].clear_segments();
                self.jobs[i].set_bytes_received(0);
                resumed = false;
            }
        }

        if !resumed {
            let rate_limited = global_bucket.is_some() || job_bucket.is_some();
            if self.jobs[i].bytes_received() != 0
                || self.jobs[i].range_ignored()
                || rate_limited
                || !matches!(self.head_supports_range(url, head), Ok(true))
            {
                return Ok(SegmentedPlanning::NotApplicable);
            }
            let total = match self.head_file_size(url, head) {
                Ok(0) => {
                    self.finalize_failed_job(
                        job_id,
                        i,
                        "empty file, nothing to download".to_string(),
                    )?;
                    return Ok(SegmentedPlanning::Finalized);
                }
                Ok(total) => total,
                Err(_) => return Ok(SegmentedPlanning::NotApplicable),
            };
            std::fs::create_dir_all(dest).map_err(|e| format!("create job dir {dest:?}: {e}"))?;
            self.jobs[i].set_total_bytes(Some(total));
            self.jobs[i].begin_segments(storage::Segment::split(total, DEFAULT_SEGMENT_COUNT));
            self.persist_queue_now()?;
            let part = std::fs::File::create(&part_path)
                .map_err(|e| format!("open partial {part_path:?}: {e}"))?;
            if let Err(err) = preallocate(&part, total) {
                // Out of space now, not midway through the transfer.
                let _ = std::fs::remove_file(&part_path);
                self.jobs[i].clear_segments();
                self.finalize_failed_job(job_id, i, err)?;
                return Ok(SegmentedPlanning::Finalized);
            }
            self.jobs[i].freeze_segments();
        }

        let table = self.jobs[i].segments().to_vec();
        let total_size = table.last().map_or(0, |s| s.end);
        let done: u64 = table.iter().map(|s| s.written).sum();
        if done >= total_size {
            self.jobs[i].clear_segments();
            self.jobs[i].set_bytes_received(total_size);
            self.complete_job_file(job_id, i, &part_path, &file_path, total_size)?;
            return Ok(SegmentedPlanning::Finalized);
        }

        let counters = self.jobs[i].begin_segments(table.clone());
        let work = table
            .iter()
            .zip(counters)
            .filter_map(|(seg, written)| {
                seg.remaining().map(|range| SegmentWork { range, written })
            })
            .collect();
        let if_range = if resumed {
            self.jobs[i].validator().map(str::to_string)
        } else {
            None
        };

        let live_bytes = self.jobs[i].live_downloaded();
        live_bytes.store(done, Ordering::Release);
        let progress = TransferProgress {
            job_id: job_id.to_string(),
            event_tx: self.event_tx.clone(),
            total_bytes: Some(total_size),
            live_bytes,
        };
        self.event_tx
            .send(CoreEvent::JobProgress {
                job_id: job_id.to_string(),
                bytes_downloaded: done,
                total_bytes: Some(total_size),
            })
            .map_err(|_| "event channel closed".to_string())?;

        self.jobs[i]
            .transfer_active()
            .store(true, Ordering::Release);
        Ok(SegmentedPlanning::Plan(Box::new(
            UnlockedTransferPlan::Segmented {
                job_id: job_id.to_string(),
                url: url.to_string(),
                part_path,
                file_path,
                total_size,
                work,
                if_range,
                resumed,
                http: self.http.clone(),
                global_bucket: global_bucket.clone(),
                job_bucket: job_bucket.clone(),
                pause_requested: self.jobs[i].pause_requested().clone(),
                transfer_active: self.jobs[i].transfer_active().clone(),
                progress,
            },
        )))
    }

    /// Apply the result of an unlocked transfer.  Returns `true` when the
    /// mirror failover loop should retry.
    ///
    /// A Discard that arrived while the transfer was finishing is honoured
    /// here: a transfer that ends Completed (or Failed) before it sees the
    /// stop signal would otherwise leave the row behind after `discard_job`
    /// returned `Ok`.
    fn finish_unlocked_transfer(
        &mut self,
        outcome: UnlockedTransferOutcome,
    ) -> Result<bool, String> {
        let job_id = outcome.job_id().to_string();
        let retry = self.apply_unlocked_transfer(outcome)?;
        let Some(index) = self.job_index(&job_id) else {
            return Ok(retry);
        };
        if !self.jobs[index].is_discard_requested() {
            return Ok(retry);
        }
        let completed = self.jobs[index].is_completed();
        let destination = self.jobs[index].destination().to_path_buf();
        self.finalize_discard(&job_id, completed, &destination)?;
        Ok(false)
    }

    fn apply_unlocked_transfer(
        &mut self,
        outcome: UnlockedTransferOutcome,
    ) -> Result<bool, String> {
        // The queue can change while a transfer runs (Discard, reorder), so the
        // job is always resolved by id here, never by a position recorded at
        // plan time.
        let job_id = outcome.job_id().to_string();
        let job_id = job_id.as_str();
        let Some(job_index) = self.job_index(job_id) else {
            return Ok(false);
        };
        match outcome {
            UnlockedTransferOutcome::Fetched {
                bytes_received,
                part_path,
                file_path,
                outcome,
                failed_url,
                ..
            } => {
                if outcome.paused {
                    self.sync_job_progress(job_id, bytes_received)?;
                    self.finalize_stop_after_transfer(job_id)?;
                    return Ok(false);
                }

                match outcome.fetch {
                    Ok(result) => {
                        if bytes_received > 0
                            && (result.range_ignored(bytes_received)
                                || self.validators_changed(job_index, &result))
                        {
                            // The server did not continue the object we hold:
                            // it ignored the Range (status 200), or the object
                            // changed. Appending would corrupt the file.
                            let ignored = result.range_ignored(bytes_received);
                            self.restart_from_zero(job_index, &part_path, ignored)?;
                            return Ok(true);
                        }
                        if self.jobs[job_index].etag().is_none()
                            && self.jobs[job_index].last_modified().is_none()
                        {
                            self.jobs[job_index]
                                .set_validators(result.etag.clone(), result.last_modified.clone());
                        }
                        let reported = result.total_length;
                        let on_disk = self.sync_job_progress(job_id, bytes_received)?;
                        // `total_length` is the object size (Content-Range
                        // total on a 206), so it beats a HEAD from earlier.
                        let total_opt = if reported > 0 {
                            Some(reported)
                        } else {
                            self.jobs[job_index].total_bytes()
                        };
                        self.jobs[job_index].set_total_bytes(total_opt);

                        self.event_tx
                            .send(CoreEvent::JobProgress {
                                job_id: job_id.to_string(),
                                bytes_downloaded: on_disk,
                                total_bytes: total_opt,
                            })
                            .map_err(|_| "event channel closed".to_string())?;

                        match total_opt {
                            Some(total) if on_disk == total => {
                                self.complete_job_file(
                                    job_id, job_index, &part_path, &file_path, total,
                                )?;
                            }
                            Some(total) if on_disk > total => {
                                self.finalize_failed_job(
                                    job_id,
                                    job_index,
                                    format!(
                                        "received {on_disk} bytes but the server reported {total}; use Retry"
                                    ),
                                )?;
                            }
                            Some(_) => {
                                self.flush_pending_progress()?;
                            }
                            None => {
                                // Unknown size: successful fetch to EOF is complete.
                                self.complete_job_file(
                                    job_id, job_index, &part_path, &file_path, on_disk,
                                )?;
                            }
                        }
                        Ok(false)
                    }
                    Err(err) => {
                        // Keep what the failed attempt wrote: the next mirror
                        // resumes from the `.part` length, not a stale offset.
                        let _ = self.sync_job_progress(job_id, bytes_received);
                        self.fail_or_failover(job_id, failed_url, err)
                    }
                }
            }
            UnlockedTransferOutcome::Segmented {
                part_path,
                file_path,
                failed_url,
                total_size,
                resumed,
                result,
                ..
            } => {
                // Whatever the segments wrote is on disk and counted; keep it
                // in the table so a pause, failover or relaunch continues there.
                self.jobs[job_index].freeze_segments();
                let on_disk = self.jobs[job_index].segments_written();
                self.jobs[job_index].set_bytes_received(on_disk);
                let mut result = result;
                match result.error.take() {
                    Some(SegmentError::RangeIgnored) => {
                        // A resume that gets the whole body means the object
                        // changed; a fresh fetch that does means the server
                        // advertised ranges it does not serve.
                        self.restart_segmented(job_index, &part_path, !resumed)?;
                        Ok(true)
                    }
                    other => {
                        if resumed && self.segment_validators_changed(job_index, &result) {
                            self.restart_segmented(job_index, &part_path, false)?;
                            return Ok(true);
                        }
                        if self.jobs[job_index].etag().is_none()
                            && self.jobs[job_index].last_modified().is_none()
                        {
                            self.jobs[job_index]
                                .set_validators(result.etag.clone(), result.last_modified.clone());
                        }
                        if let Some(SegmentError::Failed(err)) = other {
                            return self.fail_or_failover(job_id, failed_url, err);
                        }
                        if result.paused {
                            self.finalize_stop_after_transfer(job_id)?;
                            return Ok(false);
                        }
                        self.event_tx
                            .send(CoreEvent::JobProgress {
                                job_id: job_id.to_string(),
                                bytes_downloaded: on_disk,
                                total_bytes: Some(total_size),
                            })
                            .map_err(|_| "event channel closed".to_string())?;
                        if on_disk == total_size {
                            self.jobs[job_index].clear_segments();
                            self.complete_job_file(
                                job_id, job_index, &part_path, &file_path, total_size,
                            )?;
                        } else {
                            self.finalize_failed_job(
                                job_id,
                                job_index,
                                format!("received {on_disk} of {total_size} bytes; use Retry"),
                            )?;
                        }
                        Ok(false)
                    }
                }
            }
            UnlockedTransferOutcome::Panicked { message, .. } => {
                self.jobs[job_index].freeze_segments();
                let on_disk = self.jobs[job_index].segments_written();
                if on_disk > 0 {
                    self.jobs[job_index].set_bytes_received(on_disk);
                }
                self.finalize_failed_job(
                    job_id,
                    job_index,
                    format!("transfer stopped unexpectedly: {message}"),
                )?;
                Ok(false)
            }
            UnlockedTransferOutcome::Extracted {
                dest,
                stop,
                result,
                post,
                ..
            } => {
                let live_bytes = self.jobs[job_index].live_downloaded();
                let total_for_progress = self.jobs[job_index].total_bytes();
                match result {
                    Ok(ExtractorDownloadOutcome::Completed(path)) => {
                        Self::cleanup_extractor_temp_artifacts(&dest, &path);
                        let post = post.unwrap_or(PostprocessOutcome::Unchanged(path));
                        let (final_path, bytes) = match post {
                            PostprocessOutcome::Unchanged(p) | PostprocessOutcome::Remuxed(p) => {
                                let file_bytes =
                                    std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                                let bytes = Self::destination_byte_total(&dest).max(file_bytes);
                                (p, bytes)
                            }
                            PostprocessOutcome::Stopped => {
                                self.finalize_stop_after_transfer(job_id)?;
                                return Ok(false);
                            }
                            PostprocessOutcome::Failed {
                                error,
                                pre_remux_path,
                            } => {
                                let _ = pre_remux_path;
                                self.finalize_failed_job(job_id, job_index, error)?;
                                return Ok(false);
                            }
                        };
                        live_bytes.store(bytes, Ordering::Release);
                        let _ = self.event_tx.send(CoreEvent::JobProgress {
                            job_id: job_id.to_string(),
                            bytes_downloaded: bytes,
                            total_bytes: total_for_progress,
                        });
                        self.finalize_completed_job(job_id, job_index, &final_path, bytes)?;
                    }
                    Ok(ExtractorDownloadOutcome::Stopped) => {
                        self.finalize_stop_after_transfer(job_id)?;
                    }
                    Err(err) => {
                        // Kill/stop races can surface as toolchain errors; prefer
                        // Pause/Discard finalize when the user already requested stop.
                        if stop.load(Ordering::Acquire) {
                            self.finalize_stop_after_transfer(job_id)?;
                        } else {
                            self.finalize_failed_job(job_id, job_index, err)?;
                        }
                    }
                }
                Ok(false)
            }
        }
    }

    /// True when the object's validators differ from those stored on the job,
    /// so the `.part` holds a different version than the server now serves.
    fn validators_changed(&self, job_index: usize, result: &FetchResult) -> bool {
        let job = &self.jobs[job_index];
        let differs = |stored: Option<&str>, seen: Option<&str>| match (stored, seen) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        };
        differs(job.etag(), result.etag.as_deref())
            || differs(job.last_modified(), result.last_modified.as_deref())
    }

    /// True when a resumed segment's reply carried validators that differ
    /// from the ones stored for the bytes already on disk.
    fn segment_validators_changed(&self, job_index: usize, done: &SegmentedDone) -> bool {
        let job = &self.jobs[job_index];
        let differs = |stored: Option<&str>, seen: Option<&str>| match (stored, seen) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        };
        differs(job.etag(), done.etag.as_deref())
            || differs(job.last_modified(), done.last_modified.as_deref())
    }

    /// Drop a segment table and its `.part` so the next plan starts again.
    /// `range_ignored` marks the job for a single plain connection.
    fn restart_segmented(
        &mut self,
        job_index: usize,
        part_path: &Path,
        range_ignored: bool,
    ) -> Result<(), String> {
        let _ = std::fs::remove_file(part_path);
        let job = &mut self.jobs[job_index];
        job.clear_segments();
        job.set_bytes_received(0);
        job.set_validators(None, None);
        if range_ignored {
            job.set_range_ignored(true);
        }
        self.persist_queue_now()
    }

    /// Truncate the `.part` and forget its progress and validators so the next
    /// plan starts a plain fetch from byte zero.
    fn restart_from_zero(
        &mut self,
        job_index: usize,
        part_path: &Path,
        range_ignored: bool,
    ) -> Result<(), String> {
        std::fs::File::create(part_path)
            .map_err(|e| format!("truncate partial {part_path:?}: {e}"))?;
        let job = &mut self.jobs[job_index];
        job.set_bytes_received(0);
        job.set_validators(None, None);
        if range_ignored {
            job.set_range_ignored(true);
        }
        Ok(())
    }

    /// On transfer error: advance to the next mirror (returns `true` to retry)
    /// or finalize the job Failed.
    fn fail_or_failover(
        &mut self,
        job_id: &str,
        failed_url: String,
        err: String,
    ) -> Result<bool, String> {
        let job_index = self
            .job_index(job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        if self.jobs[job_index].advance_mirror() {
            let next_url = self.jobs[job_index].url().to_string();
            let persisted = self.persist_queue_now();
            self.event_tx
                .send(CoreEvent::JobMirrorFailover {
                    job_id: job_id.to_string(),
                    previous_url: failed_url,
                    next_url,
                    error: err,
                })
                .map_err(|_| "event channel closed".to_string())?;
            persisted?;
            Ok(true)
        } else {
            self.finalize_failed_job(job_id, job_index, err)?;
            Ok(false)
        }
    }

    /// Synchronous transfer used by [`Core::process_pending`]: plans, runs and
    /// finishes the job while the caller holds `&mut self` (tests and the
    /// single-threaded shell loop). The threaded path is
    /// [`Core::dispatch_pending`].
    fn transfer_job_at_index(
        &mut self,
        i: usize,
        global_bucket: &Option<SharedTokenBucket>,
    ) -> Result<(), String> {
        let job_id = self.jobs[i].id().to_string();
        loop {
            let Some(plan) = self.plan_unlocked_transfer(&job_id, global_bucket, false, None)?
            else {
                return Ok(());
            };
            let outcome = plan.run();
            if !self.finish_unlocked_transfer(outcome)? {
                return Ok(());
            }
        }
    }

    fn sync_job_progress(&mut self, job_id: &str, bytes_before: u64) -> Result<u64, String> {
        let job = self
            .jobs
            .iter()
            .find(|j| j.id() == job_id)
            .ok_or_else(|| format!("job {job_id} not found"))?;
        let part_path = Self::part_path_for(&job.final_path());
        let on_disk = std::fs::metadata(&part_path)
            .map(|m| m.len())
            .map_err(|e| format!("stat partial {part_path:?}: {e}"))?;

        if on_disk > bytes_before {
            if let Some(job) = self.jobs.iter_mut().find(|j| j.id() == job_id) {
                let delta = on_disk - bytes_before;
                job.add_bytes_received(delta);
                self.note_progress_bytes(delta)?;
            }
        }
        Ok(on_disk)
    }

    /// Sum sizes of regular files under a Job Destination (recursive).
    ///
    /// Used when the Extractor Toolchain is silent mid-transfer so Core can
    /// still emit [`CoreEvent::JobProgress`] from on-disk growth.
    fn destination_byte_total(dest: &Path) -> u64 {
        storage::destination_byte_total(dest)
    }

    /// Remove Extractor Toolchain temp/partial siblings under a Job Destination.
    ///
    /// Keeps the finalized media path reported by the toolchain. Targets known
    /// yt-dlp leftover suffixes (`.part`, `.ytdl`) so Completed jobs do not
    /// leave orphan partials beside the durable output (issue #42).
    fn cleanup_extractor_temp_artifacts(dest: &Path, keep: &Path) {
        let Ok(entries) = std::fs::read_dir(dest) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path == keep {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let is_temp = name.ends_with(".part")
                || name.ends_with(".ytdl")
                || name.ends_with(".temp")
                || name.ends_with(".tmp");
            if is_temp {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    fn complete_job_file(
        &mut self,
        job_id: &str,
        job_index: usize,
        part_path: &Path,
        file_path: &Path,
        total: u64,
    ) -> Result<(), String> {
        fsutil::rename_durable(part_path, file_path)
            .map_err(|e| format!("rename {part_path:?} -> {file_path:?}: {e}"))?;

        self.finalize_completed_job(job_id, job_index, file_path, total)
    }

    /// Mark a job Completed after optional Integrity Check, or Failed on mismatch.
    fn finalize_completed_job(
        &mut self,
        job_id: &str,
        job_index: usize,
        file_path: &Path,
        bytes_written: u64,
    ) -> Result<(), String> {
        if !self.jobs[job_index].is_pending() {
            return Ok(());
        }
        if let Some(expected) = self.jobs[job_index].expected_hash().cloned() {
            if let Err(err) = expected.verify_file(file_path) {
                return self.finalize_failed_job(job_id, job_index, err);
            }
        }

        self.jobs[job_index].mark_completed();
        if let Some(name) = file_path.file_name().and_then(|n| n.to_str()) {
            self.jobs[job_index].set_filename(name);
        }
        self.record_media_identity(job_id, job_index);
        // Persist before announcing, so a restart never contradicts the event.
        let persisted = self.persist_queue_now();
        self.event_tx
            .send(CoreEvent::JobCompleted {
                job_id: job_id.to_string(),
                bytes_written,
                path: file_path.to_path_buf(),
            })
            .map_err(|_| "event channel closed".to_string())?;
        persisted
    }

    fn mark_archived_probe_items(&self, items: &mut [MediaItem]) {
        for item in items {
            item.archived = self.download_archive.contains_completed(&item.id);
        }
    }

    /// Record a finished media job in the Download Archive.
    ///
    /// Best effort: the archive is History, not queue state, so a failed save
    /// is logged and never blocks the job's own terminal state.
    fn record_media_identity(&mut self, job_id: &str, job_index: usize) {
        let Some(media_id) = self.jobs[job_index].media_id().map(str::to_string) else {
            return;
        };
        let is_completed = self.jobs[job_index].is_completed();
        let title = self.jobs[job_index]
            .media_title()
            .map(str::to_string)
            .unwrap_or_else(|| self.jobs[job_index].filename().to_string());
        let reveal_path = if is_completed {
            self.jobs[job_index].final_path()
        } else {
            self.jobs[job_index].destination().clone()
        };
        let entry = download_archive::ArchiveEntry {
            media_id,
            title,
            url: self.jobs[job_index].url().to_string(),
            completed_at_unix_secs: download_archive::unix_now_secs(),
            destination: reveal_path,
            job_id: job_id.to_string(),
            outcome: if is_completed {
                download_archive::ArchiveOutcome::Completed
            } else {
                download_archive::ArchiveOutcome::Failed
            },
        };
        self.download_archive.record(entry);
        if let Some(root) = &self.library_root {
            if let Err(err) = self.download_archive.save(root) {
                eprintln!("phlegethon: could not save the Download Archive: {err}");
            }
        }
    }

    // -----------------------------------------------------------------------
    // History (Download Archive query)
    // -----------------------------------------------------------------------

    /// Return all archive entries (Completed + Failed), most recent first.
    ///
    /// This is the single source of truth for History views — there is no
    /// second History DB.  The archive only records media identities; plain
    /// HTTP jobs without media_id are not archived.
    pub fn list_history(&self) -> Vec<ArchiveEntry> {
        self.download_archive.list().into_iter().cloned().collect()
    }

    /// Return archive entries matching the given outcome, most recent first.
    pub fn list_history_by_outcome(&self, outcome: ArchiveOutcome) -> Vec<ArchiveEntry> {
        self.download_archive
            .list_by_outcome(outcome)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Remove an entry from the Download Archive by media_id.
    ///
    /// Files on disk are NOT touched — only the archive entry is deleted.
    pub fn remove_history_entry(&mut self, media_id: &str) -> Result<ArchiveEntry, String> {
        let removed = self
            .download_archive
            .remove(media_id)
            .ok_or_else(|| format!("no history entry for media_id '{media_id}'"))?;
        if let Some(root) = &self.library_root {
            self.download_archive.save(root)?;
        }
        Ok(removed)
    }

    /// Remove every archive entry (or only those with `outcome`). Files stay on disk.
    pub fn clear_history(&mut self, outcome: Option<ArchiveOutcome>) -> Result<usize, String> {
        let removed = self
            .download_archive
            .remove_where(|entry| outcome.as_ref().is_none_or(|o| &entry.outcome == o));
        if removed > 0 {
            if let Some(root) = &self.library_root {
                self.download_archive.save(root)?;
            }
        }
        Ok(removed)
    }

    /// Number of entries in the Download Archive.
    pub fn history_len(&self) -> usize {
        self.download_archive.len()
    }

    /// Whether the archive is empty.
    pub fn is_history_empty(&self) -> bool {
        self.download_archive.is_empty()
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Path for an in-progress download (`{final}.part`).
    fn part_path_for(final_path: &Path) -> PathBuf {
        storage::part_path_for(final_path)
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.flush_pending_progress();
    }
}
