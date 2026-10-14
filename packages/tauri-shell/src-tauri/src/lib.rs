//! Phlegethon Tauri Shell — Download Core wrapper.
//!
//! Tauri commands expose the Core API to the frontend.

mod companion;

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
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_dialog::DialogExt;

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// How long a Toolchain status snapshot is reused before re-probing tools.
const TOOLCHAIN_STATUS_TTL: Duration = Duration::from_secs(30);
/// Upper bound for buffered events awaiting `poll_events` (legacy polling path).
const EVENT_BUFFER_CAP: usize = 2048;

struct AppState {
    core: Arc<Mutex<Core>>,
    job_signals: Arc<JobSignals>,
    /// Core event receiver; taken by the drain thread in `setup`.
    event_rx: Mutex<Option<crossbeam_channel::Receiver<CoreEvent>>>,
    /// Summaries kept for `poll_events` after they were pushed as `core-event`.
    event_buffer: Mutex<Vec<EventSummary>>,
    library_root: Mutex<Option<PathBuf>>,
    process_pending_busy: AtomicBool,
    toolchain_status_cache: Mutex<Option<(Instant, ToolchainStatus)>>,
}

impl AppState {
    fn new() -> Self {
        let http = Box::new(ProdHttpBackend::new());
        let sidecar_root = dirs_data_dir();
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
            event_buffer: Mutex::new(Vec::new()),
            library_root: Mutex::new(None),
            process_pending_busy: AtomicBool::new(false),
            toolchain_status_cache: Mutex::new(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Serializable types for the frontend
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(tag = "type")]
enum EnqueueResult {
    EnqueueDirect {
        job_id: String,
        destination: String,
    },
    ProbeThenConfirm {
        url: String,
    },
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
            EnqueueOutcome::ProbeThenConfirm { url } => {
                EnqueueResult::ProbeThenConfirm { url }
            }
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(tag = "type")]
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
        CoreEvent::ProbeResult { .. } | CoreEvent::ProbeFailed { .. } => None,
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

/// Forward Core events to the webview as `core-event` and keep a copy for `poll_events`.
fn start_event_drain(app: tauri::AppHandle, rx: crossbeam_channel::Receiver<CoreEvent>) {
    std::thread::spawn(move || {
        while let Ok(evt) = rx.recv() {
            let Some(summary) = event_to_summary(&evt) else { continue };
            if let Err(err) = app.emit("core-event", &summary) {
                log::warn!("failed to emit core-event: {err}");
            }
            if let Some(state) = app.try_state::<AppState>() {
                if let Ok(mut buf) = state.event_buffer.lock() {
                    if buf.len() >= EVENT_BUFFER_CAP {
                        let overflow = buf.len() + 1 - EVENT_BUFFER_CAP;
                        buf.drain(..overflow);
                    }
                    buf.push(summary);
                }
            }
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

fn ensure_library_root(state: &AppState) -> Result<PathBuf, String> {
    let mut stored = state.library_root.lock().map_err(|e| e.to_string())?;
    if let Some(ref root) = *stored {
        return Ok(root.clone());
    }
    let default_root = default_library_root();
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_library_root(default_root.clone())?;
    *stored = Some(default_root.clone());
    Ok(default_root)
}

#[tauri::command]
fn enqueue_url(
    state: State<AppState>,
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
fn cancel_probe(state: State<AppState>, url: String) -> Result<bool, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.cancel_probe(&url))
}

#[tauri::command]
fn clear_pending_probe(state: State<AppState>, url: String) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.clear_pending_probe(&url)
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
fn get_probe_tier_settings(state: State<AppState>) -> Result<ProbeTierSettings, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.probe_tier_settings())
}

#[tauri::command]
fn set_probe_tier_settings(
    state: State<AppState>,
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
    let status = tauri::async_runtime::spawn_blocking(move || -> Result<ToolchainStatus, String> {
        // `--version` probes run subprocesses; snapshot the extractor handle, not the lock.
        let guard = core.lock().map_err(|e| e.to_string())?;
        Ok(guard.toolchain_status())
    })
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
fn get_default_quality_policy(
    state: State<AppState>,
) -> Result<download_core::DefaultQualityPolicy, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.default_quality_policy())
}

#[tauri::command]
fn set_default_quality_policy(
    state: State<AppState>,
    policy: download_core::DefaultQualityPolicy,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_default_quality_policy(policy)
}

#[tauri::command]
fn get_global_bandwidth_cap(state: State<AppState>) -> Result<Option<u64>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.global_bandwidth_cap())
}

/// `cap` in bytes per second; `None` or 0 removes the cap.
#[tauri::command]
fn set_global_bandwidth_cap(state: State<AppState>, cap: Option<u64>) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    match cap.filter(|c| *c > 0) {
        Some(cap) => core.set_global_bandwidth_cap(cap),
        None => core.remove_global_bandwidth_cap(),
    }
    Ok(())
}

#[tauri::command]
fn set_job_bandwidth_cap(
    state: State<AppState>,
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
fn get_job_bandwidth_cap(state: State<AppState>, job_id: String) -> Result<Option<u64>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    core.job_bandwidth_cap(&job_id)
}

#[tauri::command]
fn add_job_mirror(state: State<AppState>, job_id: String, url: String) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.add_job_mirror(&job_id, url)
}

#[tauri::command]
fn job_mirrors(state: State<AppState>, job_id: String) -> Result<Vec<String>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    core.job_mirrors(&job_id)
}

#[tauri::command]
fn set_job_expected_hash(
    state: State<AppState>,
    job_id: String,
    algorithm: String,
    digest: String,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_job_expected_hash(&job_id, &algorithm, &digest)
}

/// Render the Filename Template with sample values for the settings preview.
#[tauri::command]
fn preview_filename_template(template: String) -> Result<String, String> {
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
fn confirm_probe_selection(
    state: State<AppState>,
    selection: ProbeSelection,
    destination: Option<String>,
) -> Result<Vec<String>, String> {
    ensure_library_root(&state)?;
    let dest = parse_optional_destination(destination);
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.confirm_probe_selection(&selection, dest)
}

#[tauri::command]
fn preview_probe_destination(
    state: State<AppState>,
    selection: ProbeSelection,
    destination: Option<String>,
) -> Result<ProbeDestinationPreview, String> {
    ensure_library_root(&state)?;
    let dest = parse_optional_destination(destination);
    let core = state.core.lock().map_err(|e| e.to_string())?;
    core.preview_probe_destination(&selection, dest)
}

#[tauri::command]
fn get_library_root(state: State<AppState>) -> Result<Option<String>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core
        .library_root()
        .map(|p| p.display().to_string()))
}

#[tauri::command]
fn set_library_root(state: State<AppState>, path: String) -> Result<String, String> {
    let root = PathBuf::from(path.trim());
    if !root.is_absolute() {
        return Err(format!(
            "Library Root must be an absolute path, got: {}",
            root.display()
        ));
    }
    {
        let mut core = state.core.lock().map_err(|e| e.to_string())?;
        core.set_library_root(root.clone())?;
    }
    {
        let mut stored = state.library_root.lock().map_err(|e| e.to_string())?;
        *stored = Some(root.clone());
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

#[tauri::command]
async fn pick_folder(
    app: tauri::AppHandle,
    title: Option<String>,
) -> Result<PickFolderResult, String> {
    use phlegethon_shell_support::{picker, portal};
    let label = title.unwrap_or_else(|| "Choose folder".to_string());
    let dirs = path_dirs();
    let portal_up = tauri::async_runtime::spawn_blocking(portal::portal_available)
        .await
        .map_err(|e| e.to_string())?;

    match detect_picker_method(&dirs, portal_up) {
        PickerMethod::Portal => {
            // Non-blocking picker + channel: avoids blocking_pick_* wedging the
            // WebKitGTK event loop when rfd used gtk3.
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
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
            Ok(PickFolderResult {
                path: folder
                    .and_then(|path| path.into_path().ok())
                    .map(|p| p.display().to_string()),
                method: PickerMethod::Portal,
            })
        }
        method @ (PickerMethod::Zenity | PickerMethod::Kdialog) => {
            let (backend, program) = picker::detect(&dirs)
                .ok_or_else(|| "folder picker disappeared".to_string())?;
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
        PickerMethod::Manual => Ok(PickFolderResult {
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
        let portal = phlegethon_shell_support::portal::portal_available();
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
fn notify_fallback(summary: String, body: String) -> Result<bool, String> {
    match phlegethon_shell_support::notify::detect(&path_dirs()) {
        Some(program) => phlegethon_shell_support::notify::send(&program, &summary, &body).map(|_| true),
        None => Ok(false),
    }
}

#[tauri::command]
fn process_pending(app: tauri::AppHandle, state: State<AppState>) -> Result<(), String> {
    // yt-dlp downloads can take minutes — don't block the UI invoke thread.
    if state
        .process_pending_busy
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return Ok(());
    }

    std::thread::spawn(move || {
        let clear_busy = || {
            if let Some(state) = app.try_state::<AppState>() {
                state
                    .process_pending_busy
                    .store(false, Ordering::Release);
            }
        };

        if let Some(state) = app.try_state::<AppState>() {
            if let Err(e) = Core::dispatch_pending(Arc::clone(&state.core)) {
                log::error!("dispatch_pending failed: {e}");
            }
        }

        clear_busy();
    });
    Ok(())
}

#[tauri::command]
fn list_jobs(state: State<AppState>) -> Result<Vec<download_core::JobSnapshot>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.list_job_snapshots())
}

#[tauri::command]
fn normalize_url_cmd(url: String) -> Option<String> {
    download_core::normalize_url(&url)
}

#[tauri::command]
fn split_urls_cmd(text: String) -> Vec<String> {
    download_core::split_urls(&text)
}

#[tauri::command]
fn find_active_job_for_url(
    state: State<AppState>,
    url: String,
) -> Result<Option<download_core::ActiveJobMatch>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.find_active_job_for_url(&url))
}

#[tauri::command]
fn pause_job(state: State<AppState>, job_id: String) -> Result<(), String> {
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
fn resume_job(state: State<AppState>, job_id: String) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.resume_job(&job_id)
}

#[tauri::command]
fn retry_job(state: State<AppState>, job_id: String) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.retry_job(&job_id)
}

#[tauri::command]
fn discard_job(state: State<AppState>, job_id: String) -> Result<(), String> {
    // Signal stop without blocking on process_pending's Core mutex.
    if state.job_signals.signal_discard_stop(&job_id) {
        return Ok(());
    }

    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.discard_job(&job_id)
}

#[tauri::command]
fn get_active_job_limit(state: State<AppState>) -> Result<u32, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.active_job_limit())
}

#[tauri::command]
fn set_active_job_limit(state: State<AppState>, limit: u32) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_active_job_limit(limit)
}

#[tauri::command]
fn reorder_job(
    state: State<AppState>,
    from_index: usize,
    to_index: usize,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.reorder_job(from_index, to_index)
}

#[tauri::command]
fn get_audio_only_mode(state: State<AppState>) -> Result<bool, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.audio_only_mode())
}

#[tauri::command]
fn set_audio_only_mode(state: State<AppState>, enabled: bool) -> Result<(), String> {
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
fn select_default_quality(
    state: State<AppState>,
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
fn get_default_media_extras(
    state: State<AppState>,
) -> Result<download_core::MediaExtrasOptions, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.default_media_extras())
}

#[tauri::command]
fn set_default_media_extras(
    state: State<AppState>,
    options: download_core::MediaExtrasOptions,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_default_media_extras(options)
}

#[tauri::command]
fn get_media_extras_capabilities(
    state: State<AppState>,
) -> Result<download_core::MediaExtrasCapabilities, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.media_extras_capabilities())
}

#[tauri::command]
fn get_cookie_channel(state: State<AppState>) -> Result<download_core::CookieChannel, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.cookie_channel())
}

#[tauri::command]
fn set_cookie_channel(
    state: State<AppState>,
    channel: download_core::CookieChannel,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_cookie_channel(channel)
}

#[tauri::command]
fn get_proxy_settings(state: State<AppState>) -> Result<download_core::ProxySettings, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.proxy_settings())
}

#[tauri::command]
fn set_proxy_settings(
    state: State<AppState>,
    settings: download_core::ProxySettings,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_proxy_settings(settings)
}

#[tauri::command]
fn get_sidecar_update_settings(
    state: State<AppState>,
) -> Result<download_core::SidecarUpdateSettings, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.sidecar_update_settings())
}

#[tauri::command]
fn set_sidecar_update_settings(
    state: State<AppState>,
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
fn apply_sidecar_updates(state: State<AppState>) -> Result<download_core::SidecarApplyResult, String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.apply_sidecar_updates()
}

#[tauri::command]
fn pin_sidecar_version(
    state: State<AppState>,
    component: download_core::SidecarComponent,
    version: String,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.pin_sidecar_version(component, version)
}

#[tauri::command]
fn unpin_sidecar_version(
    state: State<AppState>,
    component: download_core::SidecarComponent,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.unpin_sidecar_version(component)
}

#[tauri::command]
fn rollback_sidecar(
    state: State<AppState>,
    component: download_core::SidecarComponent,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.rollback_sidecar(component)
}

#[tauri::command]
fn get_filename_template(state: State<AppState>) -> Result<String, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.filename_template().to_string())
}

#[tauri::command]
fn set_filename_template(state: State<AppState>, template: String) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_filename_template(template)
}

#[tauri::command]
fn reveal_output(app: tauri::AppHandle, path: String) -> Result<(), String> {
    if !std::path::Path::new(&path).exists() {
        return Err(format!("output path not found: {path}"));
    }
    app.opener()
        .reveal_item_in_dir(path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_output(app: tauri::AppHandle, path: String) -> Result<(), String> {
    let path_ref = std::path::Path::new(&path);
    if !path_ref.is_file() {
        return Err(format!("output file not found: {path}"));
    }
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn is_main_window_focused(app: tauri::AppHandle) -> Result<bool, String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    window.is_focused().map_err(|e| e.to_string())
}

#[tauri::command]
fn focus_main_window(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    window
        .set_focus()
        .map_err(|e| e.to_string())
        .and_then(|_| window.show().map_err(|e| e.to_string()))
}

/// Legacy polling path: events are pushed as `core-event`; this drains the
/// buffered copies for frontends that still poll.
#[tauri::command]
fn poll_events(state: State<AppState>) -> Result<Vec<EventSummary>, String> {
    let mut buf = state.event_buffer.lock().map_err(|e| e.to_string())?;
    Ok(std::mem::take(&mut *buf))
}

// ---------------------------------------------------------------------------
// History (Download Archive query)
// ---------------------------------------------------------------------------

#[tauri::command]
fn list_history(state: State<AppState>) -> Result<Vec<ArchiveEntry>, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.list_history())
}

#[tauri::command]
fn list_history_by_outcome(
    state: State<AppState>,
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
fn remove_history_entry(
    state: State<AppState>,
    media_id: String,
) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    match core.remove_history_entry(&media_id) {
        Ok(_) => Ok(()),
        Err(err) => Err(err),
    }
}

#[tauri::command]
fn history_len(state: State<AppState>) -> Result<usize, String> {
    let core = state.core.lock().map_err(|e| e.to_string())?;
    Ok(core.history_len())
}

/// Remove all history entries, or only `"Completed"` / `"Failed"` ones.
#[tauri::command]
fn clear_history(state: State<AppState>, outcome: Option<String>) -> Result<usize, String> {
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
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("phlegethon")
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
    std::fs::write(library_root_preference_path(), root.to_string_lossy().as_bytes())
        .map_err(|e| format!("persist library root: {e}"))
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(AppState::new())
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            let state: State<AppState> = app.state();
            let root = load_library_root_preference().unwrap_or_else(default_library_root);
            let sidecar_root = dirs_data_dir();
            {
                let mut core = state.core.lock().unwrap();
                if let Err(err) = core.set_sidecar_root(sidecar_root) {
                    log::warn!("sidecar root: {err}");
                }
                if let Err(err) = core.set_library_root(root.clone()) {
                    log::warn!("library root: {err}");
                }
            }
            {
                let mut stored = state.library_root.lock().unwrap();
                *stored = Some(root.clone());
            }
            let _ = persist_library_root_preference(&root);

            // Push Core events to the webview instead of making it poll.
            if let Some(rx) = state.event_rx.lock().unwrap().take() {
                start_event_drain(app.handle().clone(), rx);
            }

            // Deferred / on-launch sidecar updates may hit the network: never on the UI thread.
            {
                let core = Arc::clone(&state.core);
                std::thread::spawn(move || {
                    if let Ok(mut guard) = core.lock() {
                        if let Err(err) = guard.apply_sidecar_updates_on_launch() {
                            log::warn!("sidecar update on launch: {err}");
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
            poll_events,
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

    #[test]
    fn enqueue_result_type_tags_match_frontend() {
        let direct = EnqueueResult::EnqueueDirect {
            job_id: "job-1".into(),
            destination: "/tmp/x".into(),
        };
        let json = serde_json::to_value(&direct).unwrap();
        assert_eq!(json["type"], "EnqueueDirect");
        assert_eq!(json["job_id"], "job-1");
        assert_eq!(json["destination"], "/tmp/x");

        let probe = EnqueueResult::ProbeThenConfirm {
            url: "https://example.com".into(),
        };
        let json = serde_json::to_value(&probe).unwrap();
        assert_eq!(json["type"], "ProbeThenConfirm");
        assert_eq!(json["url"], "https://example.com");
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
