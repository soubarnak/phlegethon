//! Phlegethon Tauri Shell — Download Core wrapper.
//!
//! Tauri commands expose the Core API to the frontend.

// A release build without `custom-protocol` loads http://localhost:1420 into a webview
// with full IPC access (M-58). Refuse to build it. `make build` and the xbps template
// pass `--features phlegethon-tauri/custom-protocol`.
#[cfg(all(not(debug_assertions), not(feature = "custom-protocol")))]
compile_error!(
    "release builds of phlegethon-tauri need `--features custom-protocol`: \
     without it the window loads http://localhost:1420 instead of the embedded frontend"
);

mod companion;
mod intents;
mod progress;

use download_core::{
    ArchiveEntry, ArchiveOutcome, Core, CoreEvent, EnqueueOptions, EnqueueOutcome,
    GitHubReleaseUpdateSource, JobSignals, PlatformInfo, ProbeDestinationPreview, ProbeSelection,
    ProbeTierSettings, ProdHttpBackend, ToolchainStatus,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// How long a Toolchain status snapshot is reused before re-probing tools.
const TOOLCHAIN_STATUS_TTL: Duration = Duration::from_secs(30);

struct AppState {
    core: Arc<Mutex<Core>>,
    job_signals: Arc<JobSignals>,
    /// Core event receiver; taken by the drain thread in `setup`.
    event_rx: Mutex<Option<crossbeam_channel::Receiver<CoreEvent>>>,
    library_root: Mutex<Option<PathBuf>>,
    process_pending_busy: Arc<AtomicBool>,
    /// Companion and second-launch intents waiting for the webview (M-69).
    intents: intents::IntentInbox<companion::CompanionIntentEvent>,
    toolchain_status_cache: Mutex<Option<(Instant, ToolchainStatus)>>,
    /// Notes from loading a Library Root (corrupt queue moved aside, rows skipped)
    /// that the webview has not shown yet (H-8).
    library_root_warnings: Mutex<Vec<String>>,
}

impl AppState {
    fn new() -> Self {
        let http = Box::new(ProdHttpBackend::new());
        let sidecar_root = dirs_data_dir();
        // The luxuretv / xxxbp.tv plugins are embedded in the Core. Install them before
        // yt-dlp is configured, otherwise those Launch Sites would never resolve.
        match download_core::ensure_bundled_plugins(&sidecar_root) {
            Ok(download_core::BundledInstall::Installed { version }) => {
                log::info!("installed bundled extractor plugins {version}")
            }
            Ok(_) => {}
            Err(err) => log::warn!("could not install bundled extractor plugins: {err}"),
        }
        // Sidecar manifest → YTDLP_PATH → PATH → python3 -m yt_dlp, plus JS runtime.
        let config = download_core::YtDlpConfig::discover(Some(&sidecar_root));
        let extractor = Box::new(download_core::YtDlpExtractorToolchain::new(config));
        let job_signals = Arc::new(JobSignals::default());
        let (mut core, rx) = Core::new_with_clock_and_signals(
            http,
            extractor,
            Arc::new(download_core::SystemClock::new()),
            job_signals.clone(),
        );
        // Toolchain releases come from GitHub; plugin versions from the local catalog.
        core.set_sidecar_update_source(Arc::new(GitHubReleaseUpdateSource::new(
            core.http_backend(),
            PlatformInfo::current(),
        )));

        Self {
            core: Arc::new(Mutex::new(core)),
            job_signals,
            event_rx: Mutex::new(Some(rx)),
            library_root: Mutex::new(None),
            process_pending_busy: Arc::new(AtomicBool::new(false)),
            intents: intents::IntentInbox::default(),
            toolchain_status_cache: Mutex::new(None),
            library_root_warnings: Mutex::new(Vec::new()),
        }
    }

    fn record_library_root_warnings(&self, report: download_core::LibraryRootReport) {
        for warning in &report.warnings {
            log::warn!("library root: {warning}");
        }
        if let Ok(mut pending) = self.library_root_warnings.lock() {
            pending.extend(report.warnings);
        }
    }
}

// ---------------------------------------------------------------------------
// Serializable types for the frontend
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
enum EnqueueResult {
    EnqueueDirect { job_id: String, destination: String },
    ProbeThenConfirm { url: String },
}

impl From<EnqueueOutcome> for EnqueueResult {
    fn from(o: EnqueueOutcome) -> Self {
        match o {
            EnqueueOutcome::EnqueueDirect {
                job_id,
                destination,
            } => EnqueueResult::EnqueueDirect {
                job_id,
                destination: destination.display().to_string(),
            },
            EnqueueOutcome::ProbeThenConfirm { url } => EnqueueResult::ProbeThenConfirm { url },
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "type")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
enum EventSummary {
    JobProgress {
        job_id: String,
        bytes_downloaded: u64,
        total_bytes: Option<u64>,
    },
    JobCompleted {
        job_id: String,
        bytes_written: u64,
        path: String,
    },
    JobFailed {
        job_id: String,
        error: String,
    },
    JobPaused {
        job_id: String,
    },
    JobResumed {
        job_id: String,
    },
    JobRetried {
        job_id: String,
    },
    JobDiscarded {
        job_id: String,
    },
    JobMirrorFailover {
        job_id: String,
        previous_url: String,
        next_url: String,
        error: String,
    },
}

impl EventSummary {
    fn job_id(&self) -> Option<&str> {
        match self {
            EventSummary::JobProgress { job_id, .. }
            | EventSummary::JobCompleted { job_id, .. }
            | EventSummary::JobFailed { job_id, .. }
            | EventSummary::JobPaused { job_id }
            | EventSummary::JobResumed { job_id }
            | EventSummary::JobRetried { job_id }
            | EventSummary::JobDiscarded { job_id }
            | EventSummary::JobMirrorFailover { job_id, .. } => Some(job_id),
        }
    }

    /// The job stops reporting progress after this event, until a retry or resume.
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            EventSummary::JobCompleted { .. }
                | EventSummary::JobFailed { .. }
                | EventSummary::JobPaused { .. }
                | EventSummary::JobDiscarded { .. }
        )
    }
}

fn event_to_summary(evt: &CoreEvent) -> Option<EventSummary> {
    match evt {
        CoreEvent::JobProgress {
            job_id,
            bytes_downloaded,
            total_bytes,
        } => Some(EventSummary::JobProgress {
            job_id: job_id.clone(),
            bytes_downloaded: *bytes_downloaded,
            total_bytes: *total_bytes,
        }),
        CoreEvent::JobCompleted {
            job_id,
            bytes_written,
            path,
        } => Some(EventSummary::JobCompleted {
            job_id: job_id.clone(),
            bytes_written: *bytes_written,
            path: path.to_string_lossy().into_owned(),
        }),
        CoreEvent::JobFailed { job_id, error } => Some(EventSummary::JobFailed {
            job_id: job_id.clone(),
            error: error.clone(),
        }),
        CoreEvent::JobPaused { job_id } => Some(EventSummary::JobPaused {
            job_id: job_id.clone(),
        }),
        CoreEvent::JobResumed { job_id } => Some(EventSummary::JobResumed {
            job_id: job_id.clone(),
        }),
        CoreEvent::JobRetried { job_id } => Some(EventSummary::JobRetried {
            job_id: job_id.clone(),
        }),
        CoreEvent::JobDiscarded { job_id } => Some(EventSummary::JobDiscarded {
            job_id: job_id.clone(),
        }),
        CoreEvent::ProbeResult { .. }
        | CoreEvent::ProbeFailed { .. }
        | CoreEvent::JobsReordered { .. } => None,
        CoreEvent::JobMirrorFailover {
            job_id,
            previous_url,
            next_url,
            error,
        } => Some(EventSummary::JobMirrorFailover {
            job_id: job_id.clone(),
            previous_url: previous_url.clone(),
            next_url: next_url.clone(),
            error: error.clone(),
        }),
    }
}

/// Forward Core events to the webview as `core-event`, with progress limited to about
/// 5 Hz per job (H-25). The thread never takes the Core lock, so a Core thread that
/// blocks on a full event channel is always released.
fn start_event_drain(app: tauri::AppHandle, rx: crossbeam_channel::Receiver<CoreEvent>) {
    std::thread::spawn(move || {
        let emit = |summary: &EventSummary| {
            if let Err(err) = app.emit("core-event", summary) {
                log::warn!("failed to emit core-event: {err}");
            }
        };
        let mut coalescer = progress::ProgressCoalescer::new(progress::PROGRESS_INTERVAL);
        loop {
            let wait = coalescer
                .next_flush_in(Instant::now())
                .unwrap_or(Duration::from_secs(3600));
            match rx.recv_timeout(wait) {
                Ok(evt) => {
                    if matches!(
                        evt,
                        CoreEvent::JobCompleted { .. } | CoreEvent::JobDiscarded { .. }
                    ) {
                        // Own thread: the sweep takes the Core lock, which a long process_pending
                        // can hold, and the event drain must keep delivering progress.
                        let app = app.clone();
                        std::thread::spawn(move || companion::sweep_cookie_files(&app));
                    }
                    if let Some(summary) = event_to_summary(&evt) {
                        coalescer
                            .push(summary, Instant::now())
                            .iter()
                            .for_each(emit);
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    coalescer.flush_all().iter().for_each(emit);
                    break;
                }
            }
            coalescer.flush_due(Instant::now()).iter().for_each(emit);
        }
    });
}

fn core_handle(app: &tauri::AppHandle) -> Result<Arc<Mutex<Core>>, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "app state not ready".to_string())?;
    Ok(Arc::clone(&state.core))
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

fn parse_optional_destination(destination: Option<String>) -> Option<PathBuf> {
    destination.and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    })
}

/// Give Core a Library Root and record it as set only after Core accepted it (H-8).
/// A root Core refuses (for example a corrupt `jobs.json`) must not look set, or every
/// later command fails with "Library Root not set" and nothing says why.
fn adopt_library_root(
    core: &mut Core,
    stored: &mut Option<PathBuf>,
    root: PathBuf,
) -> Result<download_core::LibraryRootReport, String> {
    let report = core.set_library_root(root.clone())?;
    *stored = Some(root);
    Ok(report)
}

/// Changing the Library Root reloads the queue and replaces the live Job objects,
/// so a running transfer would lose its job or run twice (H-7).
fn check_root_change_allowed(active_transfers: usize) -> Result<(), String> {
    if active_transfers > 0 {
        return Err(
            "The Library Root cannot change while downloads are running. Pause them first."
                .to_string(),
        );
    }
    Ok(())
}

fn ensure_library_root(state: &AppState) -> Result<PathBuf, String> {
    let mut stored = state.library_root.lock().map_err(|e| e.to_string())?;
    if let Some(ref root) = *stored {
        return Ok(root.clone());
    }
    // The saved preference first: falling back to the default root would silently
    // move the user to another queue when the saved one failed to load.
    let root = load_library_root_preference().unwrap_or_else(default_library_root);
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    let report = adopt_library_root(&mut core, &mut stored, root.clone())?;
    state.record_library_root_warnings(report);
    Ok(root)
}

#[tauri::command]
async fn enqueue_url(
    state: State<'_, AppState>,
    url: String,
    destination: Option<String>,
    force_direct: Option<bool>,
) -> Result<EnqueueResult, String> {
    ensure_library_root(&state)?;
    let dest = parse_optional_destination(destination);
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    let outcome = core.enqueue_with_options(
        &url,
        dest,
        EnqueueOptions {
            force_direct: force_direct.unwrap_or(false),
            ..Default::default()
        },
    )?;
    Ok(outcome.into())
}

/// Probe with the Core mutex released while yt-dlp / the page sniffer run.
#[tauri::command]
async fn probe_url(
    app: tauri::AppHandle,
    url: String,
) -> Result<download_core::ProbeResult, String> {
    {
        let state = app.state::<AppState>();
        ensure_library_root(&state)?;
    }
    let core = core_handle(&app)?;
    tauri::async_runtime::spawn_blocking(move || Core::probe_outside_lock(core, &url))
        .await
        .map_err(|e| format!("probe task failed: {e}"))?
}

#[tauri::command]
async fn cancel_probe(state: State<'_, AppState>, url: String) -> Result<bool, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.cancel_probe(&url))
}

#[tauri::command]
async fn clear_pending_probe(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    url: String,
) -> Result<(), String> {
    {
        let mut core = state.core.lock().map_err(|e| e.to_string())?;
        core.clear_pending_probe(&url)?;
    }
    companion::release_pending_probe(&app, &url);
    Ok(())
}

/// Exact formats for one probe item (flat playlist entries start with presets).
#[tauri::command]
async fn enrich_probe_item_qualities(
    app: tauri::AppHandle,
    probe_url: String,
    item_id: String,
) -> Result<Vec<download_core::Quality>, String> {
    let core = core_handle(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        Core::enrich_probe_item_qualities_outside_lock(core, &probe_url, &item_id)
    })
    .await
    .map_err(|e| format!("enrich task failed: {e}"))?
}

#[tauri::command]
async fn get_probe_tier_settings(state: State<'_, AppState>) -> Result<ProbeTierSettings, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.probe_tier_settings())
}

#[tauri::command]
async fn set_probe_tier_settings(
    state: State<'_, AppState>,
    settings: ProbeTierSettings,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_probe_tier_settings(settings)
}

/// Detected yt-dlp / ffmpeg / JS runtime state; cached for a short while.
#[tauri::command]
async fn get_toolchain_status(
    app: tauri::AppHandle,
    force: Option<bool>,
) -> Result<ToolchainStatus, String> {
    if !force.unwrap_or(false) {
        if let Some(status) = cached_toolchain_status(&app) {
            return Ok(status);
        }
    }
    let core = core_handle(&app)?;
    // `--version` probes run subprocesses: Core snapshots the extractor handle and
    // releases the lock before running them (H-9).
    let status =
        tauri::async_runtime::spawn_blocking(move || Core::toolchain_status_outside_lock(&core))
            .await
            .map_err(|e| format!("toolchain status task failed: {e}"))??;
    store_toolchain_status(&app, Some(status.clone()));
    Ok(status)
}

fn cached_toolchain_status(app: &tauri::AppHandle) -> Option<ToolchainStatus> {
    let state = app.try_state::<AppState>()?;
    let cache = state.toolchain_status_cache.lock().ok()?;
    let (at, status) = cache.as_ref()?;
    (at.elapsed() < TOOLCHAIN_STATUS_TTL).then(|| status.clone())
}

fn store_toolchain_status(app: &tauri::AppHandle, status: Option<ToolchainStatus>) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut cache) = state.toolchain_status_cache.lock() {
            *cache = status.map(|s| (Instant::now(), s));
        }
    }
}

/// Download the latest yt-dlp release into the sidecar and activate it.
#[tauri::command]
async fn install_toolchain_latest(
    app: tauri::AppHandle,
) -> Result<download_core::SidecarApplyResult, String> {
    let core = core_handle(&app)?;
    let result = tauri::async_runtime::spawn_blocking(move || Core::install_toolchain_latest(core))
        .await
        .map_err(|e| format!("install task failed: {e}"))??;
    store_toolchain_status(&app, None);
    Ok(result)
}

#[tauri::command]
async fn get_default_quality_policy(
    state: State<'_, AppState>,
) -> Result<download_core::DefaultQualityPolicy, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.default_quality_policy())
}

#[tauri::command]
async fn set_default_quality_policy(
    state: State<'_, AppState>,
    policy: download_core::DefaultQualityPolicy,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_default_quality_policy(policy)
}

#[tauri::command]
async fn get_global_bandwidth_cap(state: State<'_, AppState>) -> Result<Option<u64>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.global_bandwidth_cap())
}

/// `cap` in bytes per second; `None` or 0 removes the cap.
#[tauri::command]
async fn set_global_bandwidth_cap(
    state: State<'_, AppState>,
    cap: Option<u64>,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    match cap.filter(|c| *c > 0) {
        Some(cap) => core.set_global_bandwidth_cap(cap),
        None => core.remove_global_bandwidth_cap(),
    }
}

#[tauri::command]
async fn set_job_bandwidth_cap(
    state: State<'_, AppState>,
    job_id: String,
    cap: Option<u64>,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    match cap.filter(|c| *c > 0) {
        Some(cap) => core.set_job_bandwidth_cap(&job_id, cap),
        None => core.remove_job_bandwidth_cap(&job_id),
    }
}

#[tauri::command]
async fn get_job_bandwidth_cap(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<Option<u64>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    core.job_bandwidth_cap(&job_id)
}

#[tauri::command]
async fn add_job_mirror(
    state: State<'_, AppState>,
    job_id: String,
    url: String,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.add_job_mirror(&job_id, url)
}

#[tauri::command]
async fn job_mirrors(state: State<'_, AppState>, job_id: String) -> Result<Vec<String>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    core.job_mirrors(&job_id)
}

#[tauri::command]
async fn set_job_expected_hash(
    state: State<'_, AppState>,
    job_id: String,
    algorithm: String,
    digest: String,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_job_expected_hash(&job_id, &algorithm, &digest)
}

/// Render the Filename Template with sample values for the settings preview.
#[tauri::command]
async fn preview_filename_template(template: String) -> Result<String, String> {
    let trimmed = template.trim();
    if trimmed.is_empty() {
        return Err("Filename Template cannot be empty".to_string());
    }
    Ok(download_core::render_template(
        trimmed,
        &download_core::TemplateVars {
            title: "Example Video Title",
            id: "dQw4w9WgXcQ",
            uploader: Some("Example Channel"),
            upload_date: Some("20260101"),
            ext: "mp4",
        },
    ))
}

#[tauri::command]
async fn confirm_probe_selection(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    selection: ProbeSelection,
    destination: Option<String>,
) -> Result<Vec<String>, String> {
    ensure_library_root(&state)?;
    let dest = parse_optional_destination(destination);
    let job_ids = {
        let mut core = state.core.lock().map_err(|e| e.to_string())?;
        core.confirm_probe_selection(&selection, dest)?
    };
    // The confirmed jobs carry the cookie session now; the pending-probe claim is done.
    companion::release_pending_probe(&app, &selection.url);
    Ok(job_ids)
}

#[tauri::command]
async fn preview_probe_destination(
    state: State<'_, AppState>,
    selection: ProbeSelection,
    destination: Option<String>,
) -> Result<ProbeDestinationPreview, String> {
    ensure_library_root(&state)?;
    let dest = parse_optional_destination(destination);
    let core = state.core.lock().map_err(|e| e.to_string())?;
    core.preview_probe_destination(&selection, dest)
}

#[tauri::command]
async fn get_library_root(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.library_root().map(|p| p.display().to_string()))
}

#[tauri::command]
async fn set_library_root(state: State<'_, AppState>, path: String) -> Result<String, String> {
    let root = PathBuf::from(path.trim());
    if !root.is_absolute() {
        return Err(format!(
            "Library Root must be an absolute path, got: {}",
            root.display()
        ));
    }
    {
        let mut stored = state.library_root.lock().map_err(|e| e.to_string())?;
        let mut core = state.core.lock().map_err(|e| e.to_string())?;
        if core.library_root() == Some(root.as_path()) {
            return Ok(root.display().to_string());
        }
        check_root_change_allowed(state.job_signals.active_transfer_count())?;
        let report = adopt_library_root(&mut core, &mut stored, root.clone())?;
        state.record_library_root_warnings(report);
    }
    persist_library_root_preference(&root)?;
    Ok(root.display().to_string())
}

/// Which mechanism produced (or would produce) a folder choice.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum PickerMethod {
    Portal,
    Zenity,
    Kdialog,
    /// No native chooser is available: the UI asks for a typed path.
    Manual,
}

#[derive(Serialize)]
struct PickFolderResult {
    /// `None` when the dialog was cancelled, or when `method` is `manual`.
    path: Option<String>,
    method: PickerMethod,
}

fn path_dirs() -> Vec<PathBuf> {
    phlegethon_shell_support::path_dirs_from_env(std::env::var_os("PATH").as_deref())
}

/// Portal first (it is the only picker that honours sandboxing), then zenity,
/// then kdialog, else ask for a typed path.
fn detect_picker_method(dirs: &[PathBuf], portal: bool) -> PickerMethod {
    use phlegethon_shell_support::picker::{self, PickerBackend};
    if portal {
        return PickerMethod::Portal;
    }
    match picker::detect(dirs) {
        Some((PickerBackend::Zenity, _)) => PickerMethod::Zenity,
        Some((PickerBackend::Kdialog, _)) => PickerMethod::Kdialog,
        None => PickerMethod::Manual,
    }
}

/// Run the portal folder dialog. `Failed` means the portal gave up without the user
/// choosing anything, so the caller continues with zenity, kdialog or a typed path (H-20).
async fn pick_with_portal(
    app: &tauri::AppHandle,
    label: String,
) -> Result<phlegethon_shell_support::portal::PortalPick, String> {
    use phlegethon_shell_support::portal;
    // Non-blocking picker + channel: avoids blocking_pick_* wedging the
    // WebKitGTK event loop when rfd used gtk3.
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let started = Instant::now();
    app.dialog()
        .file()
        .set_title(label)
        .pick_folder(move |folder| {
            let _ = tx.send(folder);
        });
    let folder = tauri::async_runtime::spawn_blocking(move || rx.recv())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    Ok(portal::classify_portal_pick(
        folder
            .and_then(|path| path.into_path().ok())
            .map(|p| p.to_path_buf()),
        started.elapsed(),
        portal::HUMAN_DISMISS_FLOOR,
    ))
}

#[tauri::command]
async fn pick_folder(
    app: tauri::AppHandle,
    title: Option<String>,
) -> Result<PickFolderResult, String> {
    use phlegethon_shell_support::picker;
    use phlegethon_shell_support::portal::PortalPick;
    let label = title.unwrap_or_else(|| "Choose folder".to_string());
    let dirs = path_dirs();
    let portal_up = tauri::async_runtime::spawn_blocking(
        phlegethon_shell_support::portal::file_chooser_available,
    )
    .await
    .map_err(|e| e.to_string())?;

    if portal_up {
        match pick_with_portal(&app, label.clone()).await? {
            PortalPick::Picked(path) => {
                return Ok(PickFolderResult {
                    path: Some(path.display().to_string()),
                    method: PickerMethod::Portal,
                })
            }
            PortalPick::Cancelled => {
                return Ok(PickFolderResult {
                    path: None,
                    method: PickerMethod::Portal,
                })
            }
            PortalPick::Failed => {
                log::warn!(
                    "the FileChooser portal failed without a user choice; trying zenity or kdialog"
                )
            }
        }
    }

    match detect_picker_method(&dirs, false) {
        method @ (PickerMethod::Zenity | PickerMethod::Kdialog) => {
            let (backend, program) =
                picker::detect(&dirs).ok_or_else(|| "folder picker disappeared".to_string())?;
            let start = dirs_home();
            let outcome = tauri::async_runtime::spawn_blocking(move || {
                picker::run(backend, &program, &label, start.as_deref())
            })
            .await
            .map_err(|e| e.to_string())??;
            Ok(PickFolderResult {
                path: match outcome {
                    picker::PickOutcome::Picked(p) => Some(p.display().to_string()),
                    picker::PickOutcome::Cancelled => None,
                },
                method,
            })
        }
        // No native chooser works: the UI asks for a typed path.
        _ => Ok(PickFolderResult {
            path: None,
            method: PickerMethod::Manual,
        }),
    }
}

#[derive(Serialize)]
struct PlatformCapabilities {
    folder_picker: PickerMethod,
    portal: bool,
    wayland: bool,
    notify_send: bool,
}

#[tauri::command]
async fn get_platform_capabilities() -> Result<PlatformCapabilities, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let dirs = path_dirs();
        let portal = phlegethon_shell_support::portal::file_chooser_available();
        PlatformCapabilities {
            folder_picker: detect_picker_method(&dirs, portal),
            portal,
            wayland: phlegethon_shell_support::platform::is_wayland(
                &phlegethon_shell_support::platform::current_env(),
            ),
            notify_send: phlegethon_shell_support::notify::detect(&dirs).is_some(),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// `notify-send` fallback for desktops where the notification plugin finds no daemon.
/// Returns false when `notify-send` is not installed.
#[tauri::command]
async fn notify_fallback(summary: String, body: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        match phlegethon_shell_support::notify::detect(&path_dirs()) {
            Some(program) => {
                phlegethon_shell_support::notify::send(&program, &summary, &body).map(|_| true)
            }
            None => Ok(false),
        }
    })
    .await
    .map_err(|e| format!("notify task failed: {e}"))?
}

/// Holds the `process_pending` busy flag. Dropping it clears the flag, also when
/// dispatch panics, so a panic cannot stop every later job from starting (M-52).
struct BusyGuard(Arc<AtomicBool>);

impl BusyGuard {
    fn acquire(flag: &Arc<AtomicBool>) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self(Arc::clone(flag)))
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[tauri::command]
async fn process_pending(state: State<'_, AppState>) -> Result<(), String> {
    // yt-dlp downloads can take minutes — don't block the UI invoke thread.
    let Some(guard) = BusyGuard::acquire(&state.process_pending_busy) else {
        return Ok(());
    };
    let core = Arc::clone(&state.core);

    std::thread::spawn(move || {
        let _guard = guard;
        if let Err(e) = Core::dispatch_pending(core) {
            log::error!("dispatch_pending failed: {e}");
        }
    });
    Ok(())
}

#[tauri::command]
async fn list_jobs(state: State<'_, AppState>) -> Result<Vec<download_core::JobSnapshot>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.list_job_snapshots())
}

#[tauri::command]
async fn normalize_url_cmd(url: String) -> Option<String> {
    download_core::normalize_url(&url)
}

#[tauri::command]
async fn split_urls_cmd(text: String) -> Vec<String> {
    download_core::split_urls(&text)
}

#[tauri::command]
async fn find_active_job_for_url(
    state: State<'_, AppState>,
    url: String,
) -> Result<Option<download_core::ActiveJobMatch>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.find_active_job_for_url(&url))
}

#[tauri::command]
async fn pause_job(state: State<'_, AppState>, job_id: String) -> Result<(), String> {
    // Validate + signal stop without blocking on process_pending's Core mutex.
    {
        let core = state.core.lock().map_err(|e| e.to_string())?;
        core.can_pause_job(&job_id)?;
    }

    if let Some(flag) = state.job_signals.request_pause(&job_id) {
        flag.store(true, Ordering::Release);
    }

    if state.job_signals.is_transfer_active(&job_id) {
        return Ok(());
    }

    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.finish_pause_if_idle(&job_id)
}

#[tauri::command]
async fn resume_job(state: State<'_, AppState>, job_id: String) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.resume_job(&job_id)
}

#[tauri::command]
async fn retry_job(state: State<'_, AppState>, job_id: String) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.retry_job(&job_id)
}

#[tauri::command]
async fn discard_job(state: State<'_, AppState>, job_id: String) -> Result<(), String> {
    // Signal stop without blocking on process_pending's Core mutex.
    if state.job_signals.signal_discard_stop(&job_id) {
        return Ok(());
    }

    // Row first, files after the lock is released (M-53).
    let core = Arc::clone(&state.core);
    tauri::async_runtime::spawn_blocking(move || Core::discard_job_outside_lock(core, &job_id))
        .await
        .map_err(|e| format!("discard task failed: {e}"))?
}

#[tauri::command]
async fn get_active_job_limit(state: State<'_, AppState>) -> Result<u32, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.active_job_limit())
}

#[tauri::command]
async fn set_active_job_limit(state: State<'_, AppState>, limit: u32) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_active_job_limit(limit)
}

#[tauri::command]
async fn reorder_job(
    state: State<'_, AppState>,
    from_index: usize,
    to_index: usize,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.reorder_job(from_index, to_index)
}

#[tauri::command]
async fn get_audio_only_mode(state: State<'_, AppState>) -> Result<bool, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.audio_only_mode())
}

#[tauri::command]
async fn set_audio_only_mode(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_audio_only_mode(enabled)
}

#[derive(serde::Deserialize)]
struct QualityInput {
    id: String,
    label: String,
    #[serde(default)]
    size_bytes: Option<u64>,
    #[serde(default)]
    container: Option<String>,
}

#[tauri::command]
async fn select_default_quality(
    state: State<'_, AppState>,
    qualities: Vec<QualityInput>,
) -> Result<Option<String>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    let qualities: Vec<download_core::Quality> = qualities
        .into_iter()
        .map(|q| download_core::Quality {
            id: q.id,
            label: q.label,
            size_bytes: q.size_bytes,
            container: q.container,
        })
        .collect();
    Ok(core.select_default_quality(&qualities))
}

#[tauri::command]
async fn get_default_media_extras(
    state: State<'_, AppState>,
) -> Result<download_core::MediaExtrasOptions, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.default_media_extras())
}

#[tauri::command]
async fn set_default_media_extras(
    state: State<'_, AppState>,
    options: download_core::MediaExtrasOptions,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_default_media_extras(options)
}

#[tauri::command]
async fn get_media_extras_capabilities(
    state: State<'_, AppState>,
) -> Result<download_core::MediaExtrasCapabilities, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.media_extras_capabilities())
}

#[tauri::command]
async fn get_cookie_channel(
    state: State<'_, AppState>,
) -> Result<download_core::CookieChannel, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.cookie_channel())
}

#[tauri::command]
async fn set_cookie_channel(
    state: State<'_, AppState>,
    channel: download_core::CookieChannel,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_cookie_channel(channel)
}

#[tauri::command]
async fn get_proxy_settings(
    state: State<'_, AppState>,
) -> Result<download_core::ProxySettings, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.proxy_settings())
}

#[tauri::command]
async fn set_proxy_settings(
    state: State<'_, AppState>,
    settings: download_core::ProxySettings,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_proxy_settings(settings)
}

#[tauri::command]
async fn get_sidecar_update_settings(
    state: State<'_, AppState>,
) -> Result<download_core::SidecarUpdateSettings, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.sidecar_update_settings())
}

#[tauri::command]
async fn set_sidecar_update_settings(
    state: State<'_, AppState>,
    settings: download_core::SidecarUpdateSettings,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_sidecar_update_settings(settings)
}

/// Check installed vs available sidecar versions (network lookup off the lock).
#[tauri::command]
async fn check_sidecar_updates(
    app: tauri::AppHandle,
) -> Result<download_core::SidecarUpdateCheckResult, String> {
    let core = core_handle(&app)?;
    tauri::async_runtime::spawn_blocking(move || Core::check_sidecar_updates_outside_lock(core))
        .await
        .map_err(|e| format!("update check task failed: {e}"))?
}

#[tauri::command]
async fn apply_sidecar_updates(
    app: tauri::AppHandle,
) -> Result<download_core::SidecarApplyResult, String> {
    let core = core_handle(&app)?;
    // Network and file copies: not on an async worker, and not under the Core lock.
    tauri::async_runtime::spawn_blocking(move || Core::apply_sidecar_updates_outside_lock(core))
        .await
        .map_err(|e| format!("apply task failed: {e}"))?
}

#[tauri::command]
async fn pin_sidecar_version(
    state: State<'_, AppState>,
    component: download_core::SidecarComponent,
    version: String,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.pin_sidecar_version(component, version)
}

#[tauri::command]
async fn unpin_sidecar_version(
    state: State<'_, AppState>,
    component: download_core::SidecarComponent,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.unpin_sidecar_version(component)
}

#[tauri::command]
async fn rollback_sidecar(
    state: State<'_, AppState>,
    component: download_core::SidecarComponent,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.rollback_sidecar(component)
}

#[tauri::command]
async fn get_filename_template(state: State<'_, AppState>) -> Result<String, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.filename_template().to_string())
}

#[tauri::command]
async fn set_filename_template(state: State<'_, AppState>, template: String) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_filename_template(template)
}

/// Folders the user may reveal or open from: the Library Root and every Job
/// Destination in the queue or the Download Archive.
fn output_roots(core: &Core) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = core
        .library_root()
        .map(Path::to_path_buf)
        .into_iter()
        .collect();
    roots.extend(
        core.list_job_snapshots()
            .into_iter()
            .map(|job| PathBuf::from(job.destination)),
    );
    roots.extend(
        core.list_history()
            .into_iter()
            .map(|entry| entry.destination),
    );
    roots
}

/// True when `path` resolves (symlinks and `..` included) to a place inside one of `roots`.
fn path_within_roots(path: &Path, roots: &[PathBuf]) -> bool {
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| path.starts_with(root))
}

async fn checked_output_path(app: &tauri::AppHandle, path: &str) -> Result<PathBuf, String> {
    let core = core_handle(app)?;
    let path = PathBuf::from(path);
    tauri::async_runtime::spawn_blocking(move || {
        let roots = {
            let core = core.lock().map_err(|e| e.to_string())?;
            output_roots(&core)
        };
        if !path.exists() {
            return Err(format!("output path not found: {}", path.display()));
        }
        if !path_within_roots(&path, &roots) {
            return Err(format!(
                "not a Job Destination or the Library Root: {}",
                path.display()
            ));
        }
        Ok(path)
    })
    .await
    .map_err(|e| format!("path check failed: {e}"))?
}

/// How long the file manager D-Bus call may take before falling back to `xdg-open`.
const REVEAL_TIMEOUT: Duration = Duration::from_secs(5);

#[tauri::command]
async fn reveal_output(app: tauri::AppHandle, path: String) -> Result<(), String> {
    let path = checked_output_path(&app, &path).await?;
    let folder = if path.is_dir() {
        path.clone()
    } else {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.clone())
    };
    // `reveal_item_in_dir` talks to org.freedesktop.FileManager1 over D-Bus, which can
    // stall or fail where no file manager implements it. Bound it, then open the folder.
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let app = app.clone();
        let path = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(
                app.opener()
                    .reveal_item_in_dir(path)
                    .map_err(|e| e.to_string()),
            );
        });
    }
    let revealed = tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(REVEAL_TIMEOUT))
        .await
        .map_err(|e| e.to_string())?;
    match revealed {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => {
            log::warn!("reveal failed ({err}), opening the folder instead");
            open_folder(&app, folder)
        }
        Err(_) => {
            log::warn!("reveal timed out, opening the folder instead");
            open_folder(&app, folder)
        }
    }
}

fn open_folder(app: &tauri::AppHandle, folder: PathBuf) -> Result<(), String> {
    app.opener()
        .open_path(folder.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn open_output(app: tauri::AppHandle, path: String) -> Result<(), String> {
    let path = checked_output_path(&app, &path).await?;
    if !path.is_file() {
        return Err(format!("output file not found: {}", path.display()));
    }
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn is_main_window_focused(app: tauri::AppHandle) -> Result<bool, String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    window.is_focused().map_err(|e| e.to_string())
}

/// Show the main window and focus it. `show()` runs first: a hidden or minimized
/// window cannot take focus, and a failed `set_focus()` must not leave it hidden (L-52).
fn raise_main_window(app: &tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    let shown = window.show().map_err(|e| e.to_string());
    let _ = window.unminimize();
    let focused = window.set_focus().map_err(|e| e.to_string());
    shown.and(focused)
}

#[tauri::command]
async fn focus_main_window(app: tauri::AppHandle) -> Result<(), String> {
    raise_main_window(&app)
}

/// Send an intent to the webview, or keep it until the webview asks (M-69).
pub(crate) fn deliver_companion_intent(
    app: &tauri::AppHandle,
    intent: companion::CompanionIntentEvent,
) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if let Some(intent) = state.intents.deliver(intent) {
        if let Err(err) = app.emit("companion-intent", &intent) {
            log::warn!("failed to emit companion-intent: {err}");
        }
    }
}

/// Intents that arrived before the webview mounted. Calling this also tells the shell
/// the webview now listens for `companion-intent`.
#[tauri::command]
async fn take_pending_intents(
    state: State<'_, AppState>,
) -> Result<Vec<companion::CompanionIntentEvent>, String> {
    Ok(state.intents.take_pending())
}

/// Notes from loading the Library Root (a corrupt queue file moved aside, rows
/// skipped) that were not shown yet. The webview calls this on mount and after it
/// changes the root (H-8).
#[tauri::command]
async fn take_library_root_warnings(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let mut pending = state
        .library_root_warnings
        .lock()
        .map_err(|e| e.to_string())?;
    Ok(std::mem::take(&mut *pending))
}

/// HTTP(S) URLs in the argv of a second launch. `argv[0]` is the program path.
fn urls_from_argv(argv: &[String]) -> Vec<String> {
    argv.iter()
        .skip(1)
        .filter_map(|arg| download_core::normalize_url(arg))
        .collect()
}

/// A second launch hands its URLs to this instance, which enqueues them like a
/// Browser Companion hand-off, and brings its window to the front (H-19).
fn handle_second_launch(app: &tauri::AppHandle, argv: Vec<String>) {
    let urls = urls_from_argv(&argv);
    let app = app.clone();
    // Enqueue takes the Core lock: keep it off the plugin callback thread.
    std::thread::spawn(move || {
        for url in urls {
            let Some(state) = app.try_state::<AppState>() else {
                return;
            };
            let outcome = ensure_library_root(&state).and_then(|_| {
                state
                    .core
                    .lock()
                    .map_err(|e| e.to_string())
                    .and_then(|mut core| core.enqueue(&url, None))
            });
            match outcome {
                Ok(outcome) => deliver_companion_intent(
                    &app,
                    companion::CompanionIntentEvent {
                        url: url.clone(),
                        result: companion::companion_result_from_outcome(&url, outcome),
                    },
                ),
                Err(err) => log::warn!("could not enqueue {url} from second launch: {err}"),
            }
        }
        let main_app = app.clone();
        let _ = app.run_on_main_thread(move || {
            if let Err(err) = raise_main_window(&main_app) {
                log::warn!("could not focus main window: {err}");
            }
        });
    });
}

// ---------------------------------------------------------------------------
// History (Download Archive query)
// ---------------------------------------------------------------------------

#[tauri::command]
async fn list_history(state: State<'_, AppState>) -> Result<Vec<ArchiveEntry>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.list_history())
}

#[tauri::command]
async fn list_history_by_outcome(
    state: State<'_, AppState>,
    outcome: String,
) -> Result<Vec<ArchiveEntry>, String> {
    match outcome.as_str() {
        "Completed" => {
            let core = state.core.lock().map_err(|e| e.to_string())?;
            Ok(core.list_history_by_outcome(ArchiveOutcome::Completed))
        }
        "Failed" => {
            let core = state.core.lock().map_err(|e| e.to_string())?;
            Ok(core.list_history_by_outcome(ArchiveOutcome::Failed))
        }
        _ => Err(format!(
            "unknown outcome '{outcome}', expected 'Completed' or 'Failed'"
        )),
    }
}

#[tauri::command]
async fn remove_history_entry(state: State<'_, AppState>, media_id: String) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    match core.remove_history_entry(&media_id) {
        Ok(_) => Ok(()),
        Err(err) => Err(err),
    }
}

#[tauri::command]
async fn history_len(state: State<'_, AppState>) -> Result<usize, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.history_len())
}

/// Remove all history entries, or only `"Completed"` / `"Failed"` ones.
#[tauri::command]
async fn clear_history(
    state: State<'_, AppState>,
    outcome: Option<String>,
) -> Result<usize, String> {
    let filter = match outcome.as_deref() {
        None | Some("") | Some("All") => None,
        Some("Completed") => Some(ArchiveOutcome::Completed),
        Some("Failed") => Some(ArchiveOutcome::Failed),
        Some(other) => {
            return Err(format!(
                "unknown outcome '{other}', expected 'Completed', 'Failed' or none"
            ))
        }
    };
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.clear_history(filter)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn dirs_home() -> Option<PathBuf> {
    dirs::home_dir()
}

fn dirs_data_dir() -> PathBuf {
    match dirs::data_dir() {
        Some(dir) => dir.join("phlegethon"),
        // No home directory (HOME unset): never a path relative to the working directory,
        // which would put the queue, cookie files and sidecars wherever the app started.
        None => {
            let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
            log::error!("no user data directory; falling back to the temporary directory");
            std::env::temp_dir().join(format!("phlegethon-{user}"))
        }
    }
}

fn default_library_root() -> PathBuf {
    dirs_data_dir().join("library")
}

fn library_root_preference_path() -> PathBuf {
    dirs_data_dir().join("library_root.txt")
}

fn load_library_root_preference() -> Option<PathBuf> {
    let path = library_root_preference_path();
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let root = PathBuf::from(trimmed);
    if root.is_absolute() {
        Some(root)
    } else {
        None
    }
}

fn persist_library_root_preference(root: &Path) -> Result<(), String> {
    let data = dirs_data_dir();
    std::fs::create_dir_all(&data).map_err(|e| format!("create data dir: {e}"))?;
    std::fs::write(
        library_root_preference_path(),
        root.to_string_lossy().as_bytes(),
    )
    .map_err(|e| format!("persist library root: {e}"))
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

/// Directory for the log file, next to the other Phlegethon state.
fn log_dir() -> PathBuf {
    dirs_data_dir().join("logs")
}

/// Log to a file in release builds too (M-47): the companion receiver dying or a
/// failed plugin install left no trace before. Debug builds also print to stdout.
fn log_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri_plugin_log::{RotationStrategy, Target, TargetKind};
    let mut builder = tauri_plugin_log::Builder::default()
        .level(log::LevelFilter::Info)
        .clear_targets()
        .target(Target::new(TargetKind::Folder {
            path: log_dir(),
            file_name: Some("phlegethon".to_string()),
        }))
        .max_file_size(2_000_000)
        .rotation_strategy(RotationStrategy::KeepSome(3));
    if cfg!(debug_assertions) {
        builder = builder.target(Target::new(TargetKind::Stdout));
    }
    builder.build()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must come first: a second launch has to exit before anything else starts.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            handle_second_launch(app, argv);
        }))
        .plugin(log_plugin())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .on_page_load(|webview, payload| {
            // A reload drops the webview's listeners: buffer intents until it mounts again.
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Started) {
                if let Some(state) = webview.app_handle().try_state::<AppState>() {
                    state.intents.stop_listening();
                }
            }
        })
        .setup(|app| {
            // Managed here, after the log plugin is installed: `AppState::new` logs, and
            // those lines were dropped when it ran before the logger existed.
            app.manage(AppState::new());
            let state: State<'_, AppState> = app.state();
            let root = load_library_root_preference().unwrap_or_else(default_library_root);
            let sidecar_root = dirs_data_dir();
            {
                let mut stored = state.library_root.lock().map_err(|e| e.to_string())?;
                let mut core = state.core.lock().map_err(|e| e.to_string())?;
                if let Err(err) = core.set_sidecar_root(sidecar_root) {
                    log::warn!("sidecar root: {err}");
                }
                match adopt_library_root(&mut core, &mut stored, root.clone()) {
                    Ok(report) => {
                        state.record_library_root_warnings(report);
                        let _ = persist_library_root_preference(&root);
                    }
                    // Not recorded as set: commands retry and report this error (H-8).
                    Err(err) => log::error!("library root {}: {err}", root.display()),
                }
            }

            // Push Core events to the webview instead of making it poll.
            if let Some(rx) = state.event_rx.lock().map_err(|e| e.to_string())?.take() {
                start_event_drain(app.handle().clone(), rx);
            }

            // On-launch and deferred sidecar updates hit the network, so they run on their
            // own thread and release the Core lock during the download. When a transfer is
            // running the update waits; the loop below applies it once the app is idle.
            {
                let core = Arc::clone(&state.core);
                std::thread::spawn(move || {
                    match Core::apply_sidecar_updates_on_launch_outside_lock(Arc::clone(&core)) {
                        Ok(result) => log::info!("sidecar update on launch: {:?}", result.outcome),
                        Err(err) => log::warn!("sidecar update on launch: {err}"),
                    }
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(30));
                        match Core::apply_deferred_sidecar_updates(Arc::clone(&core)) {
                            Ok(result) if !result.applied.is_empty() => {
                                log::info!("applied deferred sidecar update: {:?}", result.applied)
                            }
                            Ok(_) => {}
                            Err(err) => log::warn!("deferred sidecar update: {err}"),
                        }
                    }
                });
            }

            companion::start_companion_receiver(app.handle().clone());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            enqueue_url,
            probe_url,
            cancel_probe,
            clear_pending_probe,
            enrich_probe_item_qualities,
            get_probe_tier_settings,
            set_probe_tier_settings,
            get_toolchain_status,
            install_toolchain_latest,
            get_default_quality_policy,
            set_default_quality_policy,
            get_global_bandwidth_cap,
            set_global_bandwidth_cap,
            set_job_bandwidth_cap,
            get_job_bandwidth_cap,
            add_job_mirror,
            job_mirrors,
            set_job_expected_hash,
            preview_filename_template,
            clear_history,
            confirm_probe_selection,
            preview_probe_destination,
            get_library_root,
            set_library_root,
            pick_folder,
            get_platform_capabilities,
            notify_fallback,
            process_pending,
            list_jobs,
            normalize_url_cmd,
            split_urls_cmd,
            find_active_job_for_url,
            pause_job,
            resume_job,
            retry_job,
            discard_job,
            get_active_job_limit,
            set_active_job_limit,
            get_audio_only_mode,
            set_audio_only_mode,
            get_default_media_extras,
            set_default_media_extras,
            get_media_extras_capabilities,
            get_cookie_channel,
            set_cookie_channel,
            get_proxy_settings,
            set_proxy_settings,
            get_sidecar_update_settings,
            set_sidecar_update_settings,
            check_sidecar_updates,
            apply_sidecar_updates,
            pin_sidecar_version,
            unpin_sidecar_version,
            rollback_sidecar,
            get_filename_template,
            set_filename_template,
            select_default_quality,
            reorder_job,
            reveal_output,
            open_output,
            is_main_window_focused,
            focus_main_window,
            take_pending_intents,
            take_library_root_warnings,
            list_history,
            list_history_by_outcome,
            remove_history_entry,
            history_len,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract_samples() -> serde_json::Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../frontend/src/tauri/contract/samples.json"
        );
        serde_json::from_str(&std::fs::read_to_string(path).expect("read samples.json"))
            .expect("parse samples.json")
    }

    // M-71: the frontend reads the same samples (src/tauri/contract.test.ts) and checks them
    // against the TypeScript types generated from these serde types.
    #[test]
    fn enqueue_result_matches_the_contract_samples() {
        let results = vec![
            EnqueueResult::EnqueueDirect {
                job_id: "j".into(),
                destination: "/lib/jobs/j".into(),
            },
            EnqueueResult::ProbeThenConfirm {
                url: "https://example.com/v".into(),
            },
        ];
        assert_eq!(
            serde_json::to_value(&results).unwrap(),
            contract_samples()["EnqueueResult"]
        );
    }

    #[test]
    fn enqueue_result_and_companion_result_use_the_same_tags() {
        let enqueue = EnqueueResult::from(EnqueueOutcome::ProbeThenConfirm { url: "u".into() });
        let companion = companion::companion_result_from_outcome(
            "u",
            EnqueueOutcome::ProbeThenConfirm { url: "u".into() },
        );
        assert_eq!(
            serde_json::to_value(enqueue).unwrap(),
            serde_json::to_value(companion).unwrap()
        );
    }

    #[test]
    fn event_summary_matches_the_contract_samples() {
        let events = vec![
            EventSummary::JobProgress {
                job_id: "j".into(),
                bytes_downloaded: 5,
                total_bytes: None,
            },
            EventSummary::JobCompleted {
                job_id: "j".into(),
                bytes_written: 9,
                path: "/lib/j/f.bin".into(),
            },
            EventSummary::JobFailed {
                job_id: "j".into(),
                error: "boom".into(),
            },
            EventSummary::JobPaused { job_id: "j".into() },
            EventSummary::JobResumed { job_id: "j".into() },
            EventSummary::JobRetried { job_id: "j".into() },
            EventSummary::JobDiscarded { job_id: "j".into() },
            EventSummary::JobMirrorFailover {
                job_id: "j".into(),
                previous_url: "https://a".into(),
                next_url: "https://b".into(),
                error: "e".into(),
            },
        ];
        assert_eq!(
            serde_json::to_value(&events).unwrap(),
            contract_samples()["EventSummary"]
        );
    }

    #[test]
    fn companion_intent_event_matches_the_contract_samples() {
        let events = vec![
            companion::CompanionIntentEvent {
                url: "https://example.com/a.iso".into(),
                result: companion_protocol::CompanionResult::EnqueueDirect {
                    job_id: "j".into(),
                    destination: "/lib/jobs/j".into(),
                },
            },
            companion::CompanionIntentEvent {
                url: "https://example.com/v".into(),
                result: companion_protocol::CompanionResult::ProbeThenConfirm {
                    url: "https://example.com/v".into(),
                },
            },
        ];
        assert_eq!(
            serde_json::to_value(&events).unwrap(),
            contract_samples()["CompanionIntentEvent"]
        );
    }

    #[test]
    fn busy_guard_blocks_a_second_dispatch_until_dropped() {
        let flag = Arc::new(AtomicBool::new(false));
        let guard = BusyGuard::acquire(&flag).expect("first acquire");
        assert!(BusyGuard::acquire(&flag).is_none());
        drop(guard);
        assert!(BusyGuard::acquire(&flag).is_some());
    }

    #[test]
    fn busy_guard_clears_the_flag_when_dispatch_panics() {
        let flag = Arc::new(AtomicBool::new(false));
        let guard = BusyGuard::acquire(&flag).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = guard;
            panic!("dispatch blew up");
        }));
        assert!(result.is_err());
        assert!(
            BusyGuard::acquire(&flag).is_some(),
            "busy flag stayed set after a panic"
        );
    }

    #[test]
    fn second_launch_argv_yields_only_http_urls() {
        let argv = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        assert_eq!(
            urls_from_argv(&argv(&[
                "/usr/bin/phlegethon",
                "--flag",
                "https://example.com/a#frag",
                "file:///etc/passwd",
                "http://example.org/b/",
            ])),
            vec!["https://example.com/a", "http://example.org/b"]
        );
        // argv[0] is the program path, never a URL.
        assert!(urls_from_argv(&argv(&["https://example.com/"])).is_empty());
        assert!(urls_from_argv(&[]).is_empty());
    }

    #[test]
    fn root_change_is_refused_while_transfers_run() {
        assert!(check_root_change_allowed(0).is_ok());
        let err = check_root_change_allowed(2).unwrap_err();
        assert!(err.contains("Library Root"), "got: {err}");
    }

    struct NoHttp;
    impl download_core::HttpBackend for NoHttp {
        fn fetch(
            &self,
            _url: &str,
            _range: Option<std::ops::Range<u64>>,
            _sink: &mut dyn std::io::Write,
        ) -> Result<download_core::FetchResult, String> {
            Err("offline".into())
        }
        fn supports_range(&self, _url: &str) -> Result<bool, String> {
            Err("offline".into())
        }
        fn file_size(&self, _url: &str) -> Result<u64, String> {
            Err("offline".into())
        }
    }

    fn test_core() -> Core {
        Core::new(
            Box::new(NoHttp),
            Box::new(download_core::FakeExtractorToolchain::new()),
        )
        .0
    }

    #[test]
    fn library_root_core_refuses_is_not_recorded_as_set() {
        let tmp = tempfile::tempdir().unwrap();
        // A regular file cannot become the Library Root directory.
        let not_a_dir = tmp.path().join("library");
        std::fs::write(&not_a_dir, b"x").unwrap();

        let mut core = test_core();
        let mut stored = None;
        assert!(adopt_library_root(&mut core, &mut stored, not_a_dir).is_err());
        assert_eq!(stored, None, "shell recorded a root Core refused");
    }

    #[test]
    fn library_root_core_accepts_is_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let mut core = test_core();
        let mut stored = None;
        adopt_library_root(&mut core, &mut stored, tmp.path().to_path_buf()).unwrap();
        assert_eq!(stored.as_deref(), Some(tmp.path()));
    }

    #[test]
    fn library_root_report_carries_the_corrupt_queue_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let queue_dir = tmp.path().join(".phlegethon");
        std::fs::create_dir_all(&queue_dir).unwrap();
        std::fs::write(queue_dir.join("jobs.json"), b"{ not json").unwrap();

        let mut core = test_core();
        let mut stored = None;
        let report = adopt_library_root(&mut core, &mut stored, tmp.path().to_path_buf()).unwrap();

        assert!(
            !report.warnings.is_empty(),
            "the user must be told the queue was reset"
        );
        assert_eq!(stored.as_deref(), Some(tmp.path()));
    }

    #[test]
    fn output_paths_must_stay_inside_a_known_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        let inside = root.join("job").join("file.mp4");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::fs::write(&inside, b"x").unwrap();
        let outside = tmp.path().join("secret.txt");
        std::fs::write(&outside, b"x").unwrap();
        let roots = vec![root.clone()];

        assert!(path_within_roots(&inside, &roots));
        assert!(path_within_roots(&root, &roots));
        assert!(!path_within_roots(&outside, &roots));
        // `..` and a missing file never pass.
        assert!(!path_within_roots(&root.join("../secret.txt"), &roots));
        assert!(!path_within_roots(&root.join("missing"), &roots));
    }

    #[test]
    fn log_file_lives_under_the_data_dir() {
        assert!(log_dir().starts_with(dirs_data_dir()));
    }

    #[test]
    fn event_summary_type_tags_match_frontend() {
        let evt = EventSummary::JobProgress {
            job_id: "job-1".into(),
            bytes_downloaded: 10,
            total_bytes: Some(100),
        };
        let json = serde_json::to_value(&evt).unwrap();
        assert_eq!(json["type"], "JobProgress");
    }
}

#[cfg(all(test, unix))]
mod picker_method_tests {
    use super::{detect_picker_method, PickerMethod};
    use std::os::unix::fs::PermissionsExt;

    fn tool(dir: &std::path::Path, name: &str) {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn portal_wins_then_zenity_then_kdialog_then_manual() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = vec![dir.path().to_path_buf()];
        assert_eq!(detect_picker_method(&dirs, false), PickerMethod::Manual);
        tool(dir.path(), "kdialog");
        assert_eq!(detect_picker_method(&dirs, false), PickerMethod::Kdialog);
        tool(dir.path(), "zenity");
        assert_eq!(detect_picker_method(&dirs, false), PickerMethod::Zenity);
        assert_eq!(detect_picker_method(&dirs, true), PickerMethod::Portal);
    }
}
