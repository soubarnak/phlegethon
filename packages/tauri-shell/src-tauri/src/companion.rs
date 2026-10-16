//! Companion hand-off receiver for the Tauri Platform Shell.

use companion_protocol::{
    companion_cookie_dir, run_companion_server, write_netscape_cookie_file, CompanionIntent,
    CompanionRequest, CompanionResponse, CompanionResult,
};
use download_core::{CookieChannel, EnqueueOptions, EnqueueOutcome};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;

#[derive(Serialize, Clone)]
struct CompanionIntentEvent {
    url: String,
    result: CompanionResult,
}

/// Companion cookie files that a pending Probe-then-Confirm still needs, keyed by probed URL.
///
/// Core also keeps those sessions only in memory, so after a restart this is empty exactly when
/// Core has forgotten the pending probes.
#[derive(Default)]
struct CookieFileTracker {
    pending_probe: HashMap<String, PathBuf>,
}

impl CookieFileTracker {
    fn hold_for_probe(&mut self, url: &str, file: PathBuf) {
        self.pending_probe.insert(url.to_string(), file);
    }

    fn release_probe(&mut self, url: &str) {
        self.pending_probe.remove(url);
    }

    fn pending_files(&self) -> HashSet<PathBuf> {
        self.pending_probe.values().cloned().collect()
    }
}

/// Serialises hand-offs with sweeps, so a sweep never deletes a file between its creation and
/// the moment a job or pending probe references it.
static COOKIE_FILES: Mutex<Option<CookieFileTracker>> = Mutex::new(None);

fn lock_tracker() -> std::sync::MutexGuard<'static, Option<CookieFileTracker>> {
    // A panic in a handler thread must not stop cookie cleanup for the rest of the session.
    COOKIE_FILES.lock().unwrap_or_else(|e| e.into_inner())
}

fn cookie_dir() -> PathBuf {
    companion_cookie_dir(&crate::dirs_data_dir())
}

/// Start the background companion listener (NMH → local socket → enqueue).
pub fn start_companion_receiver(app: AppHandle) {
    std::thread::spawn(move || {
        // Files left by a crash, or by jobs that ended while the app was closed.
        sweep_cookie_files(&app);
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

    if let Err(err) = check_intent(request.intent) {
        return CompanionResponse::err(request_id, err);
    }

    let url = request.effective_url().to_string();
    if let Err(err) = ensure_library_root(&state) {
        return CompanionResponse::err(request_id, err);
    }

    let mut guard = lock_tracker();
    let tracker = guard.get_or_insert_with(CookieFileTracker::default);

    // The cookies go to this enqueue only: Core keeps the user's own cookie setting untouched.
    let cookie_file = match &request.session {
        Some(session) => match write_netscape_cookie_file(session, &cookie_dir()) {
            Ok(path) => Some(path),
            Err(err) => return CompanionResponse::err(request_id, err),
        },
        None => None,
    };
    let outcome = match state.core.lock() {
        Ok(mut core) => enqueue_for_companion(&mut core, &url, cookie_file.clone()),
        Err(err) => Err(err.to_string()),
    };
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(err) => {
            if let Some(path) = &cookie_file {
                let _ = std::fs::remove_file(path);
            }
            return CompanionResponse::err(request_id, err);
        }
    };
    if let (Some(path), EnqueueOutcome::ProbeThenConfirm { url: probe_url }) =
        (&cookie_file, &outcome)
    {
        tracker.hold_for_probe(probe_url, path.clone());
    }
    drop(guard);

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

/// Probe-then-Confirm is the only companion path. `EnqueueDirect` would skip the probe sheet
/// for any URL, so refuse it instead of ignoring it. `referrer` is not used: Core takes the
/// referer from the probe result.
fn check_intent(intent: CompanionIntent) -> Result<(), String> {
    match intent {
        CompanionIntent::ProbeThenConfirm => Ok(()),
        CompanionIntent::EnqueueDirect => {
            Err("the companion cannot force a direct download; use probe_then_confirm".to_string())
        }
    }
}

/// Enqueue a hand-off. The cookie file becomes the cookie session of this enqueue only; the
/// user's own cookie setting (`Core::cookie_channel`) is never touched (M-51).
fn enqueue_for_companion(
    core: &mut download_core::Core,
    url: &str,
    cookie_file: Option<PathBuf>,
) -> Result<EnqueueOutcome, String> {
    let options = EnqueueOptions {
        cookie_session: cookie_file.map(|path| CookieChannel {
            cookies_from_browser: None,
            cookie_file: Some(path),
        }),
        ..Default::default()
    };
    core.enqueue_with_options(url, None, options)
}

/// The probe sheet for `url` was confirmed or dismissed: its cookie file may now be unused.
pub fn release_pending_probe(app: &AppHandle, url: &str) {
    if let Some(tracker) = lock_tracker().as_mut() {
        tracker.release_probe(url);
    }
    sweep_cookie_files(app);
}

/// Delete companion cookie files that no job and no pending probe needs any more.
///
/// A Completed job no longer needs its file; a discarded job is gone from the queue. A Failed
/// or Paused job keeps it, because Retry and Resume run the download again.
pub fn sweep_cookie_files(app: &AppHandle) {
    let mut guard = lock_tracker();
    let tracker = guard.get_or_insert_with(CookieFileTracker::default);
    let mut live = tracker.pending_files();

    let state = app.state::<AppState>();
    let Ok(core) = state.core.lock() else {
        return;
    };
    for job in core.list_job_snapshots() {
        if job.status == "completed" {
            continue;
        }
        if let Ok(Some(session)) = core.job_cookie_session(&job.job_id) {
            live.extend(session.cookie_file);
        }
    }
    drop(core);

    match sweep_dir(&cookie_dir(), &live) {
        Ok(0) => {}
        Ok(n) => log::info!("removed {n} unused companion cookie file(s)"),
        Err(err) => log::warn!("sweep companion cookie files: {err}"),
    }
}

/// Remove every companion cookie file in `dir` that is not in `live`. Returns the count removed.
fn sweep_dir(dir: &Path, live: &HashSet<PathBuf>) -> std::io::Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut removed = 0;
    for entry in entries {
        let path = entry?.path();
        let is_cookie_file = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("companion-") && n.ends_with(".cookies.txt"));
        if is_cookie_file && !live.contains(&path) {
            std::fs::remove_file(&path)?;
            removed += 1;
        }
    }
    Ok(removed)
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
    fn sweep_removes_only_unreferenced_companion_cookie_files() {
        let dir = tempfile::tempdir().unwrap();
        let keep = dir.path().join("companion-keep.cookies.txt");
        let stale = dir.path().join("companion-stale.cookies.txt");
        let other = dir.path().join("notes.txt");
        for path in [&keep, &stale, &other] {
            std::fs::write(path, "x").unwrap();
        }
        let live = HashSet::from([keep.clone()]);
        assert_eq!(sweep_dir(dir.path(), &live).unwrap(), 1);
        assert!(keep.exists());
        assert!(!stale.exists());
        assert!(
            other.exists(),
            "files that are not companion cookie files stay"
        );
        assert_eq!(sweep_dir(&dir.path().join("missing"), &live).unwrap(), 0);
    }

    #[test]
    fn handoff_cookies_do_not_replace_the_users_cookie_setting() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut core, _rx) = download_core::Core::new(
            Box::new(download_core::ProdHttpBackend::new()),
            Box::new(download_core::YtDlpExtractorToolchain::new(
                download_core::YtDlpConfig::discover(None),
            )),
        );
        core.set_library_root(tmp.path().join("library")).unwrap();
        let users = CookieChannel {
            cookies_from_browser: Some("firefox".to_string()),
            cookie_file: None,
        };
        core.set_cookie_channel(users.clone()).unwrap();

        let file = tmp.path().join("companion-a.cookies.txt");
        std::fs::write(&file, "x").unwrap();
        let outcome = enqueue_for_companion(
            &mut core,
            "https://example.com/file.zip",
            Some(file.clone()),
        )
        .unwrap();

        let EnqueueOutcome::EnqueueDirect { job_id, .. } = outcome else {
            panic!("a .zip URL is enqueued directly");
        };
        assert_eq!(core.cookie_channel(), users);
        let session = core.job_cookie_session(&job_id).unwrap().unwrap();
        assert_eq!(session.cookie_file, Some(file));
    }

    #[test]
    fn companion_cannot_skip_the_probe_sheet() {
        assert!(check_intent(CompanionIntent::ProbeThenConfirm).is_ok());
        assert!(check_intent(CompanionIntent::EnqueueDirect).is_err());
    }

    #[test]
    fn pending_probe_holds_its_file_until_released() {
        let mut tracker = CookieFileTracker::default();
        let file = PathBuf::from("/data/companion-cookies/companion-a.cookies.txt");
        tracker.hold_for_probe("https://example.com/watch", file.clone());
        assert!(tracker.pending_files().contains(&file));
        tracker.release_probe("https://example.com/watch");
        assert!(tracker.pending_files().is_empty());
    }

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
