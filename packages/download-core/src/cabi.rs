//! C ABI exports for the Download Core — consumed by WinUI 3 via csbindgen.
//!
//! # Design
//!
//! - Opaque `CoreHandle` (`*mut Core`) passed through FFI.
//! - Every function returns `i32`: 0 = success, non‑zero = error.
//! - Error details are written into an optional `error_buf` parameter.
//! - Structured I/O (probe results, confirmations, events) uses **JSON strings**
//!   to keep the ABI stable and narrow.
//! - Progress events are batched via `core_drain_events_json`.
//! - Never unwind a Rust panic across the FFI boundary: every public export
//!   wraps its body in `std::panic::catch_unwind`.
//!
//! # csbindgen usage
//!
//! ```ignore
//! // build.rs
//! csbindgen::Builder::default()
//!     .input_extern_file("src/cabi.rs")
//!     .csharp_dll_name("download_core")
//!     .generate_csharp_file("bindings/CoreBindings.g.cs")
//!     .expect("csbindgen generation failed");
//! ```

use crate::job::EnqueueOutcome;
use crate::extractor::ProbeSelection;
pub use crate::{Core, CoreEvent};
use serde::Serialize;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::{self, AssertUnwindSafe};
use std::ptr;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Wrapper to catch panics.
///
/// On success returns the expression result.
/// On panic returns the fallback value (-1 for i32, null for pointers).
macro_rules! catch_panic_i32 {
    ($expr:expr) => {
        match panic::catch_unwind(AssertUnwindSafe(|| $expr)) {
            Ok(v) => v,
            Err(_) => {
                error_last_error_str(
                    "internal panic in Download Core FFI".to_string(),
                );
                -1
            }
        }
    };
}

/// Same as catch_panic_i32 but returns a null pointer on panic.
macro_rules! catch_panic_ptr {
    ($expr:expr) => {
        match panic::catch_unwind(AssertUnwindSafe(|| $expr)) {
            Ok(v) => v,
            Err(_) => {
                error_last_error_str(
                    "internal panic in Download Core FFI".to_string(),
                );
                ptr::null_mut()
            }
        }
    };
}

// Thread-local (or global) error buffer.  We use a global Mutex<String> so
// that error messages survive across FFI calls without requiring the caller
// to pre-allocate.
use std::sync::OnceLock;

fn error_buf() -> &'static Mutex<String> {
    static BUF: OnceLock<Mutex<String>> = OnceLock::new();
    BUF.get_or_init(|| Mutex::new(String::new()))
}

fn error_last_error_str(msg: String) {
    *error_buf().lock().unwrap() = msg;
}

fn take_error() -> String {
    error_buf().lock().unwrap().clone()
}

/// Convert a C string pointer to a Rust &str (returns empty on null/error).
unsafe fn cstr_to_str<'a>(ptr: *const c_char) -> &'a str {
    if ptr.is_null() {
        return "";
    }
    CStr::from_ptr(ptr).to_str().unwrap_or("")
}

/// Copy a Rust string into a caller-supplied buffer.  Includes NUL terminator.
unsafe fn write_str_to_buf(s: &str, buf: *mut c_char, len: usize) {
    if buf.is_null() || len == 0 {
        return;
    }
    let bytes = s.as_bytes();
    let copy_len = bytes.len().min(len - 1);
    ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, copy_len);
    *buf.add(copy_len) = 0; // NUL-terminate
}

// ---------------------------------------------------------------------------
// Core lifecycle
// ---------------------------------------------------------------------------

/// Create a new Download Core instance.
/// Create a new Download Core instance with production backends.
///
/// Uses `ProdHttpBackend` for HTTP and `YtDlpExtractorToolchain` for probing.
///
/// Returns an opaque handle.  The caller must call `core_destroy` when done.
#[no_mangle]
pub unsafe extern "C" fn core_create() -> *mut Core {
    catch_panic_ptr!({
        let http = Box::new(crate::http::ProdHttpBackend::new());
        let config = crate::ytdlp_extractor::YtDlpConfig::default();
        let extractor = Box::new(crate::ytdlp_extractor::YtDlpExtractorToolchain::new(config));
        let job_signals = std::sync::Arc::new(crate::job_signals::JobSignals::default());
        let (core, rx) = Core::new_with_clock_and_signals(
            http,
            extractor,
            std::sync::Arc::new(crate::SystemClock::new()),
            job_signals.clone(),
        );

        // We need to keep the receiver alive to drain events.
        // We store it in a leaked box alongside the core.
        let ctx = Box::new(CoreContext {
            core: Arc::new(Mutex::new(core)),
            event_rx: Mutex::new(Some(rx)),
            job_signals,
        });
        Box::into_raw(ctx) as *mut Core
    })
}

/// Internal context stored alongside the Core handle.
struct CoreContext {
    core: Arc<Mutex<Core>>,
    event_rx: Mutex<Option<crossbeam_channel::Receiver<CoreEvent>>>,
    job_signals: std::sync::Arc<crate::job_signals::JobSignals>,
}

/// Destroy a Core instance created by `core_create`.
///
/// Safe to call with NULL (no-op).
#[no_mangle]
pub unsafe extern "C" fn core_destroy(handle: *mut Core) {
    if handle.is_null() {
        return;
    }
    let _ = Box::from_raw(handle as *mut CoreContext);
}

/// Set the Library Root directory.
///
/// Returns 0 on success, -1 on error (error details written to `error_buf`).
#[no_mangle]
pub unsafe extern "C" fn core_set_library_root(
    handle: *mut Core,
    path: *const c_char,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let path_str = cstr_to_str(path);
        let mut core_guard = ctx.core.lock().unwrap();
        let core = &mut *core_guard;
        match core.set_library_root(std::path::PathBuf::from(path_str)) {
            Ok(()) => 0,
            Err(e) => {
                let err = format!("set_library_root: {e}");
                write_str_to_buf(&err, error_buf, error_buf_len);
                -1
            }
        }
    })
}

/// Read the current Library Root into `out_path_buf`.
///
/// Returns 0 on success, -1 if unset or on error.
#[no_mangle]
pub unsafe extern "C" fn core_get_library_root(
    handle: *mut Core,
    out_path_buf: *mut c_char,
    out_buf_len: usize,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let core_guard = ctx.core.lock().unwrap();
        let core = &*core_guard;
        match core.library_root() {
            Some(root) => {
                write_str_to_buf(&root.to_string_lossy(), out_path_buf, out_buf_len);
                0
            }
            None => {
                write_str_to_buf("Library Root not set", error_buf, error_buf_len);
                -1
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Enqueue
// ---------------------------------------------------------------------------

/// Enqueue a URL for download.
///
/// On success writes a JSON string to `out_json_buf`:
/// ```json
/// {"type":"enqueue_direct","job_id":"...","destination":"..."}
/// // or
/// {"type":"probe_then_confirm","url":"..."}
/// ```
/// Returns 0 on success, -1 on error.
///
/// Destination defaults under the Library Root.  For an absolute Job
/// Destination override, use [`core_enqueue_with_destination`].
#[no_mangle]
pub unsafe extern "C" fn core_enqueue(
    handle: *mut Core,
    url: *const c_char,
    out_json_buf: *mut c_char,
    out_buf_len: usize,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    core_enqueue_with_destination(
        handle,
        url,
        std::ptr::null(),
        out_json_buf,
        out_buf_len,
        error_buf,
        error_buf_len,
    )
}

/// Like [`core_enqueue`], but `destination` may be an absolute parent for a
/// nested job-owned Job Destination (null or empty = default under Library Root).
#[no_mangle]
pub unsafe extern "C" fn core_enqueue_with_destination(
    handle: *mut Core,
    url: *const c_char,
    destination: *const c_char,
    out_json_buf: *mut c_char,
    out_buf_len: usize,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let url_str = cstr_to_str(url);
        let dest = if destination.is_null() {
            None
        } else {
            let s = cstr_to_str(destination);
            if s.is_empty() {
                None
            } else {
                Some(std::path::PathBuf::from(s))
            }
        };
        let mut core_guard = ctx.core.lock().unwrap();
        let core = &mut *core_guard;

        match core.enqueue(url_str, dest) {
            Ok(outcome) => {
                let json = match outcome {
                    EnqueueOutcome::EnqueueDirect {
                        job_id,
                        destination,
                    } => serde_json::json!({
                        "type": "enqueue_direct",
                        "job_id": job_id,
                        "destination": destination.to_string_lossy(),
                    }),
                    EnqueueOutcome::ProbeThenConfirm { url } => {
                        serde_json::json!({
                            "type": "probe_then_confirm",
                            "url": url,
                        })
                    }
                };
                let s = json.to_string();
                write_str_to_buf(&s, out_json_buf, out_buf_len);
                0
            }
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Probe-then-Confirm
// ---------------------------------------------------------------------------

/// Probe a URL using the Extractor Toolchain.
///
/// On success writes the probe result as JSON to `out_json_buf`.
/// Returns 0 on success, -1 on error.
#[no_mangle]
pub unsafe extern "C" fn core_probe(
    handle: *mut Core,
    url: *const c_char,
    out_json_buf: *mut c_char,
    out_buf_len: usize,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let url_str = cstr_to_str(url);
        let mut core_guard = ctx.core.lock().unwrap();
        let core = &mut *core_guard;

        match core.probe(url_str) {
            Ok(result) => {
                let json = serde_json::to_value(&result).unwrap();
                let s = json.to_string();
                write_str_to_buf(&s, out_json_buf, out_buf_len);
                0
            }
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

/// Confirm a probe selection and create download jobs.
///
/// `selection_json` is a JSON string:
/// ```json
/// {"url":"https://...","items":[{"item_id":"...","quality_id":"..."}]}
/// ```
/// Confirm a probe selection and create download jobs.
///
/// `selection_json` is a [`ProbeSelection`] object.  Optional top-level
/// `"destination"` (absolute path string) is the Save-to parent; each job
/// nests a unique Job Destination under it.
///
/// On success writes a JSON array of job IDs to `out_json_buf`.
/// Returns 0 on success, -1 on error.
#[no_mangle]
pub unsafe extern "C" fn core_confirm_probe_selection(
    handle: *mut Core,
    selection_json: *const c_char,
    out_json_buf: *mut c_char,
    out_buf_len: usize,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let json_str = cstr_to_str(selection_json);

        #[derive(serde::Deserialize)]
        struct ConfirmArgs {
            url: String,
            items: Vec<crate::extractor::ItemSelection>,
            #[serde(default)]
            destination: Option<std::path::PathBuf>,
            #[serde(default)]
            media_extras: Option<crate::media_extras::MediaExtrasOptions>,
        }

        let args: ConfirmArgs = match serde_json::from_str(json_str) {
            Ok(s) => s,
            Err(e) => {
                let err = format!("invalid selection JSON: {e}");
                write_str_to_buf(&err, error_buf, error_buf_len);
                return -1;
            }
        };

        let selection = ProbeSelection {
            url: args.url,
            items: args.items,
            media_extras: args.media_extras,
        };

        let mut core_guard = ctx.core.lock().unwrap();
        let core = &mut *core_guard;

        match core.confirm_probe_selection(&selection, args.destination) {
            Ok(job_ids) => {
                let json = serde_json::json!(job_ids);
                let s = json.to_string();
                write_str_to_buf(&s, out_json_buf, out_buf_len);
                0
            }
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

/// Preview the Job Destination path for a probe selection without creating jobs.
///
/// `selection_json` matches [`core_confirm_probe_selection`]: a [`ProbeSelection`]
/// object with optional top-level `"destination"` (absolute Save-to parent).
///
/// On success writes a [`ProbeDestinationPreview`] JSON object (`path_display`)
/// to `out_json_buf`. Returns 0 on success, -1 on error.
#[no_mangle]
pub unsafe extern "C" fn core_preview_probe_destination(
    handle: *mut Core,
    selection_json: *const c_char,
    out_json_buf: *mut c_char,
    out_buf_len: usize,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let json_str = cstr_to_str(selection_json);

        #[derive(serde::Deserialize)]
        struct PreviewArgs {
            url: String,
            items: Vec<crate::extractor::ItemSelection>,
            #[serde(default)]
            destination: Option<std::path::PathBuf>,
            #[serde(default)]
            media_extras: Option<crate::media_extras::MediaExtrasOptions>,
        }

        let args: PreviewArgs = match serde_json::from_str(json_str) {
            Ok(s) => s,
            Err(e) => {
                let err = format!("invalid selection JSON: {e}");
                write_str_to_buf(&err, error_buf, error_buf_len);
                return -1;
            }
        };

        let selection = ProbeSelection {
            url: args.url,
            items: args.items,
            media_extras: args.media_extras,
        };

        let core_guard = ctx.core.lock().unwrap();
        let core = &*core_guard;

        match core.preview_probe_destination(&selection, args.destination) {
            Ok(preview) => {
                let s = serde_json::to_string(&preview).unwrap();
                write_str_to_buf(&s, out_json_buf, out_buf_len);
                0
            }
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Lifecycle commands
// ---------------------------------------------------------------------------

macro_rules! lifecycle_cmd {
    ($name:ident, $method:ident) => {
        #[no_mangle]
        pub unsafe extern "C" fn $name(
            handle: *mut Core,
            job_id: *const c_char,
            error_buf: *mut c_char,
            error_buf_len: usize,
        ) -> i32 {
            catch_panic_i32!({
                let ctx = &*(handle as *mut CoreContext);
                let jid = cstr_to_str(job_id);
                let mut core_guard = ctx.core.lock().unwrap();
                let core = &mut *core_guard;
                match core.$method(jid) {
                    Ok(()) => 0,
                    Err(e) => {
                        write_str_to_buf(&e, error_buf, error_buf_len);
                        -1
                    }
                }
            })
        }
    };
}

#[no_mangle]
pub unsafe extern "C" fn core_pause_job(
    handle: *mut Core,
    job_id: *const c_char,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let jid = cstr_to_str(job_id);

        {
            let core_guard = ctx.core.lock().unwrap();
            let core = &*core_guard;
            if let Err(err) = core.can_pause_job(jid) {
                write_str_to_buf(&err, error_buf, error_buf_len);
                return -1;
            }
        }

        if let Some(flag) = ctx.job_signals.request_pause(jid) {
            flag.store(true, std::sync::atomic::Ordering::Release);
        }

        if ctx.job_signals.is_transfer_active(jid) {
            return 0;
        }

        let mut core_guard = ctx.core.lock().unwrap();
        let core = &mut *core_guard;
        match core.finish_pause_if_idle(jid) {
            Ok(()) => 0,
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

lifecycle_cmd!(core_resume_job, resume_job);
lifecycle_cmd!(core_retry_job, retry_job);

#[no_mangle]
pub unsafe extern "C" fn core_discard_job(
    handle: *mut Core,
    job_id: *const c_char,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let jid = cstr_to_str(job_id);

        // Signal stop without waiting on the Core mutex held by process_pending.
        if ctx.job_signals.signal_discard_stop(jid) {
            return 0;
        }

        let mut core_guard = ctx.core.lock().unwrap();
        let core = &mut *core_guard;
        match core.discard_job(jid) {
            Ok(()) => 0,
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Processing
// ---------------------------------------------------------------------------

/// Process pending jobs — run transfers for any queued jobs.
///
/// Returns 0 on success, -1 on error.
#[no_mangle]
pub unsafe extern "C" fn core_process_pending(
    handle: *mut Core,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        match Core::dispatch_pending(Arc::clone(&ctx.core)) {
            Ok(()) => 0,
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Drain pending events and write them as a JSON array to `out_json_buf`.
///
/// ```json
/// [{"type":"JobProgress","job_id":"...","bytes_downloaded":0,"total_bytes":100}]
/// ```
///
/// Returns the number of events drained.  Returns -1 on error.
#[no_mangle]
pub unsafe extern "C" fn core_drain_events_json(
    handle: *mut Core,
    out_json_buf: *mut c_char,
    out_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let rx_guard = ctx.event_rx.lock().unwrap();
        let rx = rx_guard.as_ref().unwrap();

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        let count = events.len() as i32;

        // Serialise to JSON array
        let json = serde_json::to_value(&events).unwrap();
        let s = json.to_string();
        write_str_to_buf(&s, out_json_buf, out_buf_len);
        count
    })
}

// ---------------------------------------------------------------------------
// Active Job Limit + queue reorder
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn core_get_active_job_limit(handle: *mut Core) -> u32 {
    let ctx = &*(handle as *mut CoreContext);
    let core_guard = ctx.core.lock().unwrap();
    core_guard.active_job_limit()
}

#[no_mangle]
pub unsafe extern "C" fn core_set_active_job_limit(
    handle: *mut Core,
    limit: u32,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let mut core_guard = ctx.core.lock().unwrap();
        let core = &mut *core_guard;
        match core.set_active_job_limit(limit) {
            Ok(()) => 0,
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn core_reorder_job(
    handle: *mut Core,
    from_index: usize,
    to_index: usize,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let mut core_guard = ctx.core.lock().unwrap();
        let core = &mut *core_guard;
        match core.reorder_job(from_index, to_index) {
            Ok(()) => 0,
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Default Quality Policy + audio-only mode
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn core_get_audio_only_mode(handle: *mut Core) -> i32 {
    let ctx = &*(handle as *mut CoreContext);
    let core_guard = ctx.core.lock().unwrap();
    if core_guard.audio_only_mode() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn core_set_audio_only_mode(
    handle: *mut Core,
    enabled: i32,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let mut core_guard = ctx.core.lock().unwrap();
        let core = &mut *core_guard;
        match core.set_audio_only_mode(enabled != 0) {
            Ok(()) => 0,
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

/// Select the default quality id for a JSON array of qualities.
///
/// Input: `[{"id":"1080p","label":"1080p","size_bytes":null,"container":"mp4"}]`
/// Output: `"1080p"` (written to `out_json_buf` as a JSON string)
#[no_mangle]
pub unsafe extern "C" fn core_select_default_quality_json(
    handle: *mut Core,
    qualities_json: *const c_char,
    out_json_buf: *mut c_char,
    out_buf_len: usize,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let core_guard = ctx.core.lock().unwrap();
        let core = &*core_guard;

        let qualities_str = cstr_to_str(qualities_json);

        let qualities: Vec<crate::extractor::Quality> = match serde_json::from_str(qualities_str) {
            Ok(q) => q,
            Err(e) => {
                write_str_to_buf(
                    &format!("parse qualities JSON: {e}"),
                    error_buf,
                    error_buf_len,
                );
                return -1;
            }
        };

        let selected = core.select_default_quality(&qualities);
        let json = match selected {
            Some(id) => serde_json::to_string(&id).unwrap(),
            None => "null".to_string(),
        };
        write_str_to_buf(&json, out_json_buf, out_buf_len);
        0
    })
}

#[no_mangle]
pub unsafe extern "C" fn core_get_default_media_extras_json(
    handle: *mut Core,
    out_json_buf: *mut c_char,
    out_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let core_guard = ctx.core.lock().unwrap();
        let json = serde_json::to_string(&core_guard.default_media_extras()).unwrap();
        write_str_to_buf(&json, out_json_buf, out_buf_len);
        0
    })
}

#[no_mangle]
pub unsafe extern "C" fn core_set_default_media_extras_json(
    handle: *mut Core,
    options_json: *const c_char,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let json_str = cstr_to_str(options_json);
        let options: crate::media_extras::MediaExtrasOptions =
            match serde_json::from_str(json_str) {
                Ok(o) => o,
                Err(e) => {
                    write_str_to_buf(
                        &format!("parse media extras JSON: {e}"),
                        error_buf,
                        error_buf_len,
                    );
                    return -1;
                }
            };
        let mut core_guard = ctx.core.lock().unwrap();
        match core_guard.set_default_media_extras(options) {
            Ok(()) => 0,
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn core_get_media_extras_capabilities_json(
    handle: *mut Core,
    out_json_buf: *mut c_char,
    out_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let core_guard = ctx.core.lock().unwrap();
        let json = serde_json::to_string(&core_guard.media_extras_capabilities()).unwrap();
        write_str_to_buf(&json, out_json_buf, out_buf_len);
        0
    })
}

#[no_mangle]
pub unsafe extern "C" fn core_get_cookie_channel_json(
    handle: *mut Core,
    out_json_buf: *mut c_char,
    out_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let core_guard = ctx.core.lock().unwrap();
        let json = serde_json::to_string(&core_guard.cookie_channel()).unwrap();
        write_str_to_buf(&json, out_json_buf, out_buf_len);
        0
    })
}

#[no_mangle]
pub unsafe extern "C" fn core_set_cookie_channel_json(
    handle: *mut Core,
    channel_json: *const c_char,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let json_str = cstr_to_str(channel_json);
        let channel: crate::cookie_channel::CookieChannel = match serde_json::from_str(json_str) {
            Ok(c) => c,
            Err(e) => {
                write_str_to_buf(
                    &format!("parse cookie channel JSON: {e}"),
                    error_buf,
                    error_buf_len,
                );
                return -1;
            }
        };

        let mut core_guard = ctx.core.lock().unwrap();
        let core = &mut *core_guard;
        match core.set_cookie_channel(channel) {
            Ok(()) => 0,
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Bandwidth caps
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn core_set_global_bandwidth_cap(
    handle: *mut Core,
    cap: u64,
) {
    let ctx = &*(handle as *mut CoreContext);
    let mut core_guard = ctx.core.lock().unwrap();
    let core = &mut *core_guard;
    core.set_global_bandwidth_cap(cap);
}

#[no_mangle]
pub unsafe extern "C" fn core_remove_global_bandwidth_cap(
    handle: *mut Core,
) {
    let ctx = &*(handle as *mut CoreContext);
    let mut core_guard = ctx.core.lock().unwrap();
    let core = &mut *core_guard;
    core.remove_global_bandwidth_cap();
}

#[no_mangle]
pub unsafe extern "C" fn core_set_job_bandwidth_cap(
    handle: *mut Core,
    job_id: *const c_char,
    cap: u64,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let jid = cstr_to_str(job_id);
        let mut core_guard = ctx.core.lock().unwrap();
        let core = &mut *core_guard;
        match core.set_job_bandwidth_cap(jid, cap) {
            Ok(()) => 0,
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn core_remove_job_bandwidth_cap(
    handle: *mut Core,
    job_id: *const c_char,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let jid = cstr_to_str(job_id);
        let mut core_guard = ctx.core.lock().unwrap();
        let core = &mut *core_guard;
        match core.remove_job_bandwidth_cap(jid) {
            Ok(()) => 0,
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Mirror Sets
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn core_add_job_mirror(
    handle: *mut Core,
    job_id: *const c_char,
    url: *const c_char,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let jid = cstr_to_str(job_id);
        let url_str = cstr_to_str(url);
        let mut core_guard = ctx.core.lock().unwrap();
        let core = &mut *core_guard;
        match core.add_job_mirror(jid, url_str.to_string()) {
            Ok(()) => 0,
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn core_job_mirrors(
    handle: *mut Core,
    job_id: *const c_char,
    out_json_buf: *mut c_char,
    out_buf_len: usize,
    error_buf: *mut c_char,
    error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let jid = cstr_to_str(job_id);
        let mut core_guard = ctx.core.lock().unwrap();
        let core = &mut *core_guard;
        match core.job_mirrors(jid) {
            Ok(mirrors) => {
                let json = serde_json::json!(mirrors);
                let s = json.to_string();
                write_str_to_buf(&s, out_json_buf, out_buf_len);
                0
            }
            Err(e) => {
                write_str_to_buf(&e, error_buf, error_buf_len);
                -1
            }
        }
    })
}

// ---------------------------------------------------------------------------
// URL input helpers (#57)
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn url_normalize_json(
    url: *const c_char,
    out_json_buf: *mut c_char,
    out_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let url_str = cstr_to_str(url);
        let json = match crate::normalize_url(url_str) {
            Some(normalized) => serde_json::Value::String(normalized),
            None => serde_json::Value::Null,
        };
        write_str_to_buf(&json.to_string(), out_json_buf, out_buf_len);
        0
    })
}

#[no_mangle]
pub unsafe extern "C" fn url_split_json(
    text: *const c_char,
    out_json_buf: *mut c_char,
    out_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let text_str = cstr_to_str(text);
        let urls = crate::split_urls(text_str);
        let json = serde_json::to_string(&urls).unwrap_or_else(|_| "[]".to_string());
        write_str_to_buf(&json, out_json_buf, out_buf_len);
        0
    })
}

#[no_mangle]
pub unsafe extern "C" fn core_find_active_job_for_url(
    handle: *mut Core,
    url: *const c_char,
    out_json_buf: *mut c_char,
    out_buf_len: usize,
    _error_buf: *mut c_char,
    _error_buf_len: usize,
) -> i32 {
    catch_panic_i32!({
        let ctx = &*(handle as *mut CoreContext);
        let url_str = cstr_to_str(url);
        let core_guard = ctx.core.lock().unwrap();
        let core = &*core_guard;
        let json = match core.find_active_job_for_url(url_str) {
            Some(matched) => serde_json::to_string(&matched).unwrap_or_else(|_| "null".to_string()),
            None => "null".to_string(),
        };
        write_str_to_buf(&json, out_json_buf, out_buf_len);
        0
    })
}

// ---------------------------------------------------------------------------
// Serialisation support for C ABI types
// ---------------------------------------------------------------------------

impl Serialize for crate::extractor::ProbeResult {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("ProbeResult", 3)?;
        st.serialize_field("url", &self.url)?;
        st.serialize_field("items", &self.items)?;
        st.serialize_field("source_name", &self.source_name)?;
        st.end()
    }
}

impl Serialize for crate::extractor::MediaItem {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("MediaItem", 5)?;
        st.serialize_field("id", &self.id)?;
        st.serialize_field("title", &self.title)?;
        st.serialize_field("url", &self.url)?;
        st.serialize_field("qualities", &self.qualities)?;
        st.serialize_field("archived", &self.archived)?;
        st.end()
    }
}

impl Serialize for CoreEvent {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        match self {
            CoreEvent::ProbeResult {
                url,
                items,
                source_name,
            } => {
                let mut st = s.serialize_struct("CoreEvent", 4)?;
                st.serialize_field("type", "ProbeResult")?;
                st.serialize_field("url", url)?;
                st.serialize_field("items", items)?;
                st.serialize_field("source_name", source_name)?;
                st.end()
            }
            CoreEvent::ProbeFailed { url, error } => {
                let mut st = s.serialize_struct("CoreEvent", 3)?;
                st.serialize_field("type", "ProbeFailed")?;
                st.serialize_field("url", url)?;
                st.serialize_field("error", error)?;
                st.end()
            }
            CoreEvent::JobProgress {
                job_id,
                bytes_downloaded,
                total_bytes,
            } => {
                let mut st = s.serialize_struct("CoreEvent", 4)?;
                st.serialize_field("type", "JobProgress")?;
                st.serialize_field("job_id", job_id)?;
                st.serialize_field("bytes_downloaded", bytes_downloaded)?;
                st.serialize_field("total_bytes", total_bytes)?;
                st.end()
            }
            CoreEvent::JobCompleted {
                job_id,
                bytes_written,
                path,
            } => {
                let mut st = s.serialize_struct("CoreEvent", 4)?;
                st.serialize_field("type", "JobCompleted")?;
                st.serialize_field("job_id", job_id)?;
                st.serialize_field("bytes_written", bytes_written)?;
                st.serialize_field("path", &path.to_string_lossy().to_string())?;
                st.end()
            }
            CoreEvent::JobPaused { job_id } => {
                let mut st = s.serialize_struct("CoreEvent", 2)?;
                st.serialize_field("type", "JobPaused")?;
                st.serialize_field("job_id", job_id)?;
                st.end()
            }
            CoreEvent::JobResumed { job_id } => {
                let mut st = s.serialize_struct("CoreEvent", 2)?;
                st.serialize_field("type", "JobResumed")?;
                st.serialize_field("job_id", job_id)?;
                st.end()
            }
            CoreEvent::JobFailed { job_id, error } => {
                let mut st = s.serialize_struct("CoreEvent", 3)?;
                st.serialize_field("type", "JobFailed")?;
                st.serialize_field("job_id", job_id)?;
                st.serialize_field("error", error)?;
                st.end()
            }
            CoreEvent::JobRetried { job_id } => {
                let mut st = s.serialize_struct("CoreEvent", 2)?;
                st.serialize_field("type", "JobRetried")?;
                st.serialize_field("job_id", job_id)?;
                st.end()
            }
            CoreEvent::JobDiscarded { job_id } => {
                let mut st = s.serialize_struct("CoreEvent", 2)?;
                st.serialize_field("type", "JobDiscarded")?;
                st.serialize_field("job_id", job_id)?;
                st.end()
            }
            CoreEvent::JobMirrorFailover {
                job_id,
                previous_url,
                next_url,
                error,
            } => {
                let mut st = s.serialize_struct("CoreEvent", 5)?;
                st.serialize_field("type", "JobMirrorFailover")?;
                st.serialize_field("job_id", job_id)?;
                st.serialize_field("previous_url", previous_url)?;
                st.serialize_field("next_url", next_url)?;
                st.serialize_field("error", error)?;
                st.end()
            }
        }
    }
}
