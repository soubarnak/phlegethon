//! Phlegethon Tauri Shell — Download Core wrapper.
//!
//! Tauri commands expose the Core API to the frontend.

use crossbeam_channel::TryRecvError;
use download_core::{
    Core, CoreEvent, EnqueueOutcome, JobSignals, ProbeSelection, ProdHttpBackend,
};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Manager, State};

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

struct AppState {
    core: Mutex<Core>,
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
            core: Mutex::new(core),
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
            path: _,
        } => Some(EventSummary::JobCompleted {
            job_id: job_id.clone(),
            bytes_written: *bytes_written,
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

#[tauri::command]
fn enqueue_url(state: State<AppState>, url: String) -> Result<EnqueueResult, String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;

    // Auto-set library root if not set
    if state.library_root.lock().map_err(|e| e.to_string())?.is_none() {
        let default_root = dirs_data_dir();
        core.set_library_root(default_root.clone())?;
        *state.library_root.lock().map_err(|e| e.to_string())? = Some(default_root);
    }

    let outcome = core.enqueue(&url)?;
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
) -> Result<Vec<String>, String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.confirm_probe_selection(&selection)
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
            if let Ok(mut core) = state.core.lock() {
                if let Err(e) = core.process_pending() {
                    log::error!("process_pending failed: {e}");
                }
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
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.pause_job(&job_id)
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

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::new())
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            // Auto-set library root
            let state: State<AppState> = app.state();
            let root = dirs_data_dir();
            {
                let mut core = state.core.lock().unwrap();
                let _ = core.set_library_root(root.clone());
            }
            {
                let mut stored = state.library_root.lock().unwrap();
                *stored = Some(root);
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            enqueue_url,
            probe_url,
            confirm_probe_selection,
            process_pending,
            list_jobs,
            pause_job,
            resume_job,
            retry_job,
            discard_job,
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

        let probe = EnqueueResult::ProbeThenConfirm {
            url: "https://example.com/v".into(),
        };
        let json = serde_json::to_value(&probe).unwrap();
        assert_eq!(json["type"], "ProbeThenConfirm");
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
