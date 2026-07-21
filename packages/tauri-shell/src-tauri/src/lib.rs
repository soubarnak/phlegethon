//! Phlegethon Tauri Shell — Download Core wrapper.
//!
//! Tauri commands expose the Core API to the frontend.

use crossbeam_channel::TryRecvError;
use download_core::{
    Core, CoreEvent, EnqueueOutcome, JobSignals, ProbeDestinationPreview, ProbeSelection,
    ProdHttpBackend,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Manager, State};
use tauri_plugin_dialog::DialogExt;

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

struct AppState {
    core: Arc<Mutex<Core>>,
    job_signals: Arc<JobSignals>,
    event_rx: Mutex<crossbeam_channel::Receiver<CoreEvent>>,
    library_root: Mutex<Option<PathBuf>>,
    process_pending_busy: AtomicBool,
}

impl AppState {
    fn new() -> Self {
        let http = Box::new(ProdHttpBackend::new());
        let ytdlp_bin = std::env::var_os("YTDLP_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("yt-dlp"));
        let config = download_core::YtDlpConfig {
            binary_path: ytdlp_bin,
            ..download_core::YtDlpConfig::default()
        }
        .with_sidecar_plugins(dirs_data_dir());
        let extractor = Box::new(download_core::YtDlpExtractorToolchain::new(config));
        let job_signals = Arc::new(JobSignals::default());
        let (core, rx) = Core::new_with_clock_and_signals(
            http,
            extractor,
            Arc::new(download_core::SystemClock::new()),
            job_signals.clone(),
        );

        Self {
            core: Arc::new(Mutex::new(core)),
            job_signals,
            event_rx: Mutex::new(rx),
            library_root: Mutex::new(None),
            process_pending_busy: AtomicBool::new(false),
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
        CoreEvent::JobMirrorFailover { .. } => None,
    }
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
) -> Result<EnqueueResult, String> {
    ensure_library_root(&state)?;
    let dest = parse_optional_destination(destination);
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    let outcome = core.enqueue(&url, dest)?;
    Ok(outcome.into())
}

#[tauri::command]
fn probe_url(
    state: State<AppState>,
    url: String,
) -> Result<download_core::ProbeResult, String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.probe(&url)
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

#[tauri::command]
async fn pick_folder(
    app: tauri::AppHandle,
    title: Option<String>,
) -> Result<Option<String>, String> {
    let label = title.unwrap_or_else(|| "Choose folder".to_string());
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
    Ok(folder
        .and_then(|path| path.into_path().ok())
        .map(|p| p.display().to_string()))
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
fn poll_events(state: State<AppState>) -> Result<Vec<EventSummary>, String> {
    let rx = state.event_rx.lock().map_err(|e| e.to_string())?;
    let mut events = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(evt) => {
                if let Some(summary) = event_to_summary(&evt) {
                    events.push(summary);
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }
    Ok(events)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
            {
                let mut core = state.core.lock().unwrap();
                let _ = core.set_library_root(root.clone());
            }
            {
                let mut stored = state.library_root.lock().unwrap();
                *stored = Some(root.clone());
            }
            let _ = persist_library_root_preference(&root);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            enqueue_url,
            probe_url,
            confirm_probe_selection,
            preview_probe_destination,
            get_library_root,
            set_library_root,
            pick_folder,
            process_pending,
            list_jobs,
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
            select_default_quality,
            reorder_job,
            poll_events,
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
