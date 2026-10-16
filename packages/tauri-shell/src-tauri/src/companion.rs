//! Companion hand-off receiver for the Tauri Platform Shell.

use companion_protocol::{
    companion_cookie_dir, run_companion_server, write_netscape_cookie_file, CompanionRequest,
    CompanionResponse, CompanionResult,
};
use download_core::{CookieChannel, EnqueueOutcome};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;

#[derive(Serialize, Clone)]
struct CompanionIntentEvent {
    url: String,
    result: CompanionResult,
}

/// Start the background companion listener (NMH → local socket → enqueue).
pub fn start_companion_receiver(app: AppHandle) {
    std::thread::spawn(move || {
        if let Err(err) =
            run_companion_server(move |request| handle_companion_intent(&app, request))
        {
            log::error!("companion receiver stopped: {err}");
        }
    });
}

fn handle_companion_intent(app: &AppHandle, request: CompanionRequest) -> CompanionResponse {
    let state = app.state::<AppState>();
    let request_id = request.request_id.clone();

    if let Some(session) = &request.session {
        let cookie_dir = companion_cookie_dir(&crate::dirs_data_dir());
        match write_netscape_cookie_file(session, &cookie_dir) {
            Ok(path) => {
                let channel = CookieChannel {
                    cookies_from_browser: None,
                    cookie_file: Some(path),
                };
                if let Ok(mut core) = state.core.lock() {
                    if let Err(err) = core.set_cookie_channel(channel) {
                        return CompanionResponse::err(request_id, err);
                    }
                }
            }
            Err(err) => return CompanionResponse::err(request_id, err),
        }
    }

    let url = request.effective_url().to_string();
    if let Err(err) = ensure_library_root(&state) {
        return CompanionResponse::err(request_id, err);
    }

    let outcome = match state.core.lock() {
        Ok(mut core) => core.enqueue(&url, None),
        Err(err) => return CompanionResponse::err(request_id, err.to_string()),
    };

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(err) => return CompanionResponse::err(request_id, err),
    };

    let result = companion_result_from_outcome(&url, outcome);
    let event = CompanionIntentEvent {
        url: url.clone(),
        result: result.clone(),
    };

    if let Err(err) = app.emit("companion-intent", &event) {
        log::warn!("failed to emit companion-intent: {err}");
    }

    let _ = app.run_on_main_thread({
        let app = app.clone();
        move || {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
                let _ = window.show();
            }
        }
    });

    CompanionResponse::ok(request_id, result)
}

fn companion_result_from_outcome(_url: &str, outcome: EnqueueOutcome) -> CompanionResult {
    match outcome {
        EnqueueOutcome::EnqueueDirect {
            job_id,
            destination,
        } => CompanionResult::EnqueueDirect {
            job_id,
            destination: destination.display().to_string(),
        },
        EnqueueOutcome::ProbeThenConfirm { url: probe_url } => {
            CompanionResult::ProbeThenConfirm { url: probe_url }
        }
    }
}

fn ensure_library_root(state: &AppState) -> Result<(), String> {
    let mut stored = state.library_root.lock().map_err(|e| e.to_string())?;
    if stored.is_some() {
        return Ok(());
    }
    let default_root = crate::default_library_root();
    let mut core = state.core.lock().map_err(|e| e.to_string())?;
    core.set_library_root(default_root.clone())?;
    *stored = Some(default_root);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use download_core::EnqueueOutcome;
    use std::path::PathBuf;

    #[test]
    fn maps_probe_outcome_without_auto_confirm() {
        let result = companion_result_from_outcome(
            "https://example.com",
            EnqueueOutcome::ProbeThenConfirm {
                url: "https://example.com".to_string(),
            },
        );
        assert!(matches!(result, CompanionResult::ProbeThenConfirm { .. }));
    }

    #[test]
    fn maps_direct_enqueue_outcome() {
        let result = companion_result_from_outcome(
            "https://example.com/file.mp4",
            EnqueueOutcome::EnqueueDirect {
                job_id: "job-1".to_string(),
                destination: PathBuf::from("/tmp/out"),
            },
        );
        assert!(matches!(result, CompanionResult::EnqueueDirect { .. }));
    }
}
