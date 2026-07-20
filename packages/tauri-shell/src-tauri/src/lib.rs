//! Phlegethon Tauri Shell — Download Core wrapper.
//!
//! Tauri commands expose the Core API to the frontend.

use download_core::{
    Core, CoreEvent, EnqueueOutcome, HttpBackend, ItemSelection, ProbeSelection,
    ProdHttpBackend,
};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::State;

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

pub struct AppState {
    pub core: Mutex<Core>,
    pub event_rx: Mutex<crossbeam_channel::Receiver<CoreEvent>>,
    pub library_root: Mutex<Option<PathBuf>>,
}

impl AppState {
    pub fn new() -> Self {
        let http = Box::new(ProdHttpBackend::new());
        let extractor = Box::new(download_core::FakeExtractorToolchain::new());
        let (core, rx) = Core::new(http, extractor);

        Self {
            core: Mutex::new(core),
            event_rx: Mutex::new(rx),
            library_root: Mutex::new(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Serializable types for the frontend
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum EnqueueResult {
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
#[serde(rename_all = "snake_case")]
pub struct JobSummary {
    pub job_id: String,
    pub url: String,
    pub status: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub destination: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum EventSummary {
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
        CoreEvent::ProbeResult { .. } | CoreEvent::ProbeFailed { .. } => None,
    }
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// Set the library root directory.
#[tauri::command]
pub fn set_library_root(
    state: State<AppState>,
    root: String,
) -> Result<(), String> {
    let path = PathBuf::from(&root);
    {
        let mut core = state.core.lock().map_err(|e| e.to_string())?;
        core.set_library_root(path.clone())?;
    }
    let mut stored = state.library_root.lock().map_err(|e| e.to_string())?;
    *stored = Some(path);
    Ok(())
}

/// Enqueue a URL. Returns EnqueueDirect or ProbeThenConfirm.
#[tauri::command]
pub fn enqueue_url(state: State<AppState>, url: String) -> Result<EnqueueResult, String> {
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

/// Probe a URL for downloadable items.
#[tauri::command]
pub fn probe_url(
    state: State<AppState>,
    url: String,
) -> Result<download_core::ProbeResult, String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.probe(&url)
}

/// Confirm a probe selection and create jobs.
#[tauri::command]
pub fn confirm_probe_selection(
    state: State<AppState>,
    selection: ProbeSelection,
) -> Result<Vec<String>, String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.confirm_probe_selection(&selection)
}

/// Process pending jobs (run transfers).
#[tauri::command]
pub fn process_pending(state: State<AppState>) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.process_pending()
}

/// List all jobs with their current state.
#[tauri::command]
pub fn list_jobs(state: State<AppState>) -> Result<Vec<JobSummary>, String> {
    // The Core doesn't expose a public jobs() iterator directly.
    // We manage jobs via enqueue/process and track them on the frontend.
    // This is a best-effort reflection if we add a job accessor later.
    // For now, frontend manages its own job list and this is a no-op.
    Ok(Vec::new())
}

/// Pause a job.
#[tauri::command]
pub fn pause_job(state: State<AppState>, job_id: String) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.pause_job(&job_id)
}

/// Resume a paused job.
#[tauri::command]
pub fn resume_job(state: State<AppState>, job_id: String) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.resume_job(&job_id)
}

/// Retry a failed job.
#[tauri::command]
pub fn retry_job(state: State<AppState>, job_id: String) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.retry_job(&job_id)
}

/// Discard a job from the queue.
#[tauri::command]
pub fn discard_job(state: State<AppState>, job_id: String) -> Result<(), String> {
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.discard_job(&job_id)
}

/// Poll for new events. Drains the event channel.
#[tauri::command]
pub fn poll_events(state: State<AppState>) -> Result<Vec<EventSummary>, String> {
    let rx = state.event_rx.lock().map_err(|e| e.to_string())?;
    let mut events = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(evt) => {
                if let Some(summary) = event_to_summary(&evt) {
                    events.push(summary);
                }
            }
            Err(crossbeam_channel::TryRecvError::Empty) => break,
            Err(crossbeam_channel::TryRecvError::Disconnected) => break,
        }
    }
    Ok(events)
}

/// Get/set the theme preference (stored in app data dir).
#[tauri::command]
pub fn get_library_root(state: State<AppState>) -> Result<Option<String>, String> {
    let stored = state.library_root.lock().map_err(|e| e.to_string())?;
    Ok(stored.as_ref().map(|p| p.display().to_string()))
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

            // Auto-set library root to app data dir
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
            set_library_root,
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
            get_library_root,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
