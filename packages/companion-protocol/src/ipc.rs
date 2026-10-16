//! Local socket IPC between the native-messaging host (NMH) and the running Platform Shell.
//!
//! The socket is a filesystem socket (ADR 0005). Its directory is mode 0700 and the socket is
//! mode 0600, so only the owning user can connect. The NMH additionally refuses to connect
//! unless the socket file belongs to its own uid, so a process that squats the path while the
//! shell is closed cannot receive a hand-off frame.

use crate::envelope::{CompanionRequest, CompanionResponse};
use interprocess::local_socket::traits::Listener as _;
use interprocess::local_socket::{
    GenericFilePath, Listener, ListenerOptions, Stream as LocalSocketStream, ToFsName,
};
use interprocess::os::unix::local_socket::ListenerOptionsExt;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// File name of the companion socket inside the companion runtime directory.
pub const COMPANION_SOCKET_FILE: &str = "companion.sock";

/// How long a connection may stay silent before the receiver drops it.
const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// Receiver ceiling on simultaneous connections; extra clients get an error reply.
const MAX_CONNECTIONS: usize = 16;
/// Pause after a failed `accept` so a persistent error does not spin the CPU.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Resolve the companion socket path.
///
/// `$XDG_RUNTIME_DIR/phlegethon/companion.sock` when `XDG_RUNTIME_DIR` is an absolute path,
/// otherwise `<data dir>/phlegethon/run/companion.sock` where the data dir is
/// `$XDG_DATA_HOME` or `$HOME/.local/share`.
pub fn companion_socket_path() -> Result<PathBuf, String> {
    socket_path_from(
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn socket_path_from(
    runtime_dir: Option<PathBuf>,
    data_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf, String> {
    if let Some(runtime) = runtime_dir.filter(|p| p.is_absolute()) {
        return Ok(runtime.join("phlegethon").join(COMPANION_SOCKET_FILE));
    }
    let data = data_home
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.filter(|p| p.is_absolute())
                .map(|h| h.join(".local").join("share"))
        })
        .ok_or_else(|| {
            "no XDG_RUNTIME_DIR, XDG_DATA_HOME or HOME: cannot place the companion socket"
                .to_string()
        })?;
    Ok(data
        .join("phlegethon")
        .join("run")
        .join(COMPANION_SOCKET_FILE))
}

/// Uid of the current process, read from `/proc/self` so no libc binding is needed.
fn current_uid() -> Result<u32, String> {
    fs::metadata("/proc/self")
        .map(|m| m.uid())
        .map_err(|e| format!("read own uid from /proc/self: {e}"))
}

/// Refuse a socket path that is not a socket owned by `expected_uid`.
///
/// A symlink is rejected as well: `symlink_metadata` does not follow it.
pub fn check_socket_owner(path: &Path, expected_uid: u32) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            "Phlegethon desktop app is not running".to_string()
        } else {
            format!("stat companion socket: {e}")
        }
    })?;
    if !meta.file_type().is_socket() {
        return Err(format!(
            "companion socket path is not a socket: {}",
            path.display()
        ));
    }
    if meta.uid() != expected_uid {
        return Err(format!(
            "companion socket {} is owned by uid {}, expected {expected_uid}; refusing to connect",
            path.display(),
            meta.uid()
        ));
    }
    Ok(())
}

/// Forward one validated request to the running shell and return its response.
pub fn forward_to_shell(request: &CompanionRequest) -> CompanionResponse {
    let request_id = request.request_id.clone();
    let result = companion_socket_path().and_then(|path| try_forward_to_shell(&path, request));
    match result {
        Ok(response) => response,
        Err(err) => CompanionResponse::err(request_id, err),
    }
}

/// Like [`forward_to_shell`], for an explicit socket path.
pub fn forward_to_shell_at(path: &Path, request: &CompanionRequest) -> CompanionResponse {
    let request_id = request.request_id.clone();
    match try_forward_to_shell(path, request) {
        Ok(response) => response,
        Err(err) => CompanionResponse::err(request_id, err),
    }
}

fn try_forward_to_shell(
    path: &Path,
    request: &CompanionRequest,
) -> Result<CompanionResponse, String> {
    check_socket_owner(path, current_uid()?)?;
    let mut stream = UnixStream::connect(path)
        .map_err(|_| "Phlegethon desktop app is not running".to_string())?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|_| stream.set_write_timeout(Some(IO_TIMEOUT)))
        .map_err(|e| format!("set socket timeout: {e}"))?;

    let payload = serde_json::to_vec(request).map_err(|e| format!("serialize request: {e}"))?;
    write_frame(&mut stream, &payload)?;
    let response_bytes = read_frame(&mut stream)?;
    serde_json::from_slice(&response_bytes).map_err(|e| format!("invalid shell response: {e}"))
}

/// Create the socket directory (mode 0700) and bind the companion listener (socket mode 0600).
///
/// A stale socket file left by a crashed shell is removed. A live socket, a non-socket file or
/// a directory owned by another user is an error.
pub fn bind_companion_listener(path: &Path) -> Result<Listener, String> {
    let uid = current_uid()?;
    let dir = path
        .parent()
        .ok_or_else(|| format!("companion socket path has no parent: {}", path.display()))?;
    prepare_socket_dir(dir, uid)?;
    remove_stale_socket(path, uid)?;

    let name = path
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| format!("companion socket name: {e}"))?;
    let listener = ListenerOptions::new()
        .name(name)
        .mode(0o600)
        .create_sync()
        .map_err(|e| format!("create companion listener: {e}"))?;
    // The 0700 directory already keeps other users out; set the socket mode explicitly anyway.
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod companion socket: {e}"))?;
    Ok(listener)
}

fn prepare_socket_dir(dir: &Path, uid: u32) -> Result<(), String> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("create companion socket dir {}: {e}", dir.display()))?;
    let meta = fs::metadata(dir).map_err(|e| format!("stat {}: {e}", dir.display()))?;
    if meta.uid() != uid {
        return Err(format!(
            "companion socket dir {} is owned by uid {}, expected {uid}",
            dir.display(),
            meta.uid()
        ));
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("chmod {}: {e}", dir.display()))?;
    }
    Ok(())
}

fn remove_stale_socket(path: &Path, uid: u32) -> Result<(), String> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("stat {}: {e}", path.display())),
    };
    if !meta.file_type().is_socket() || meta.uid() != uid {
        return Err(format!(
            "{} exists and is not a socket owned by this user; not removing it",
            path.display()
        ));
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(format!(
            "another Phlegethon instance is already listening on {}",
            path.display()
        )),
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            fs::remove_file(path).map_err(|e| format!("remove stale socket: {e}"))
        }
        Err(e) => Err(format!("probe existing socket {}: {e}", path.display())),
    }
}

/// Run the companion receiver on the default socket path, invoking `handler` per request.
pub fn run_companion_server<F>(handler: F) -> Result<(), String>
where
    F: Fn(CompanionRequest) -> CompanionResponse + Send + Sync + 'static,
{
    run_companion_server_at(&companion_socket_path()?, handler)
}

/// Like [`run_companion_server`], for an explicit socket path.
pub fn run_companion_server_at<F>(path: &Path, handler: F) -> Result<(), String>
where
    F: Fn(CompanionRequest) -> CompanionResponse + Send + Sync + 'static,
{
    let listener = bind_companion_listener(path)?;
    serve(listener, handler)
}

/// Accept connections forever. Each connection gets its own thread, so one idle or slow client
/// cannot block the others. An accept error is logged and the loop continues.
pub fn serve<F>(listener: Listener, handler: F) -> Result<(), String>
where
    F: Fn(CompanionRequest) -> CompanionResponse + Send + Sync + 'static,
{
    let handler = Arc::new(handler);
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let stream = match listener.accept() {
            Ok(stream) => stream,
            Err(err) => {
                log::warn!("accept companion connection: {err}");
                std::thread::sleep(ACCEPT_RETRY_DELAY);
                continue;
            }
        };
        // std's UnixStream carries the read and write timeouts the interprocess stream lacks.
        let LocalSocketStream::UdSocket(uds) = stream;
        let mut stream = match uds.inner().try_clone() {
            Ok(std_stream) => std_stream,
            Err(err) => {
                log::warn!("duplicate companion connection fd: {err}");
                continue;
            }
        };
        if stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .and_then(|_| stream.set_write_timeout(Some(IO_TIMEOUT)))
            .is_err()
        {
            continue;
        }
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            let _ = write_response_frame(
                &mut stream,
                &CompanionResponse::err("", "too many companion connections"),
            );
            continue;
        }
        let handler = Arc::clone(&handler);
        let conn_active = Arc::clone(&active);
        let spawned = std::thread::Builder::new()
            .name("companion-conn".to_string())
            .spawn(move || {
                handle_connection(&mut stream, handler.as_ref());
                conn_active.fetch_sub(1, Ordering::SeqCst);
            });
        if let Err(err) = spawned {
            log::warn!("spawn companion connection thread: {err}");
            active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn handle_connection<F>(stream: &mut UnixStream, handler: &F)
where
    F: Fn(CompanionRequest) -> CompanionResponse,
{
    let request_bytes = match read_frame(stream) {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = write_response_frame(stream, &CompanionResponse::err("", err));
            return;
        }
    };
    let request: CompanionRequest = match serde_json::from_slice(&request_bytes) {
        Ok(req) => req,
        Err(err) => {
            let response = CompanionResponse::err("", format!("invalid request: {err}"));
            let _ = write_response_frame(stream, &response);
            return;
        }
    };
    let request_id = request.request_id.clone();
    let response = match request.validate() {
        Ok(()) => handler(request),
        Err(err) => CompanionResponse::err(request_id, err),
    };
    let _ = write_response_frame(stream, &response);
}

fn write_response_frame<W: Write>(
    writer: &mut W,
    response: &CompanionResponse,
) -> Result<(), String> {
    let response_bytes =
        serde_json::to_vec(response).map_err(|e| format!("serialize response: {e}"))?;
    write_frame(writer, &response_bytes)
}

fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> Result<(), String> {
    let len = u32::try_from(payload.len())
        .map_err(|_| format!("frame too large: {} bytes", payload.len()))?;
    writer
        .write_all(&len.to_ne_bytes())
        .and_then(|_| writer.write_all(payload))
        .and_then(|_| writer.flush())
        .map_err(|e| format!("write frame: {e}"))
}

fn read_frame<R: Read>(reader: &mut R) -> Result<Vec<u8>, String> {
    let mut len_buf = [0u8; 4];
    reader
        .read_exact(&mut len_buf)
        .map_err(|e| format!("read frame length: {e}"))?;
    let len = u32::from_ne_bytes(len_buf) as usize;
    if len > 1024 * 1024 {
        return Err(format!("frame too large: {len} bytes"));
    }
    let mut buf = vec![0u8; len];
    reader
        .read_exact(&mut buf)
        .map_err(|e| format!("read frame body: {e}"))?;
    Ok(buf)
}

/// Directory for ephemeral companion cookie files under the app data root.
pub fn companion_cookie_dir(data_root: &Path) -> PathBuf {
    data_root.join("companion-cookies")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{CompanionIntent, CompanionResult, PROTOCOL_VERSION};
    use std::os::unix::net::UnixListener;
    use std::thread;

    fn request(id: &str) -> CompanionRequest {
        CompanionRequest {
            v: PROTOCOL_VERSION,
            op: "enqueue".to_string(),
            request_id: id.to_string(),
            page_url: "https://example.com/watch".to_string(),
            media_url: None,
            intent: CompanionIntent::ProbeThenConfirm,
            referrer: None,
            session: None,
        }
    }

    fn echo_handler(req: CompanionRequest) -> CompanionResponse {
        CompanionResponse::ok(
            req.request_id.clone(),
            CompanionResult::ProbeThenConfirm {
                url: req.effective_url().to_string(),
            },
        )
    }

    /// Bind a server on a fresh temp path and return the path plus the temp dir guard.
    fn start_server() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run").join(COMPANION_SOCKET_FILE);
        let listener = bind_companion_listener(&path).unwrap();
        thread::spawn(move || {
            let _ = serve(listener, echo_handler);
        });
        (dir, path)
    }

    #[test]
    fn round_trip_request_over_local_socket() {
        let (_dir, path) = start_server();
        let response = forward_to_shell_at(&path, &request("ipc-test"));
        assert!(response.ok, "{:?}", response.error);
        assert_eq!(response.request_id, "ipc-test");
    }

    #[test]
    fn socket_and_directory_are_private() {
        let (_dir, path) = start_server();
        let sock_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(sock_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn existing_loose_directory_is_tightened() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        fs::create_dir(&run).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o755)).unwrap();
        let _listener = bind_companion_listener(&run.join(COMPANION_SOCKET_FILE)).unwrap();
        assert_eq!(
            fs::metadata(&run).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn stale_socket_is_removed_on_bind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run").join(COMPANION_SOCKET_FILE);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Bind then drop without unlinking: leaves a socket file nobody listens on.
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        bind_companion_listener(&path).expect("stale socket must be replaced");
    }

    #[test]
    fn live_socket_is_not_stolen() {
        let (_dir, path) = start_server();
        let err = bind_companion_listener(&path).unwrap_err();
        assert!(err.contains("already listening"), "{err}");
    }

    #[test]
    fn regular_file_at_socket_path_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(COMPANION_SOCKET_FILE);
        fs::write(&path, b"precious").unwrap();
        assert!(bind_companion_listener(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"precious");
    }

    #[test]
    fn client_refuses_socket_owned_by_another_uid() {
        let (_dir, path) = start_server();
        let me = current_uid().unwrap();
        check_socket_owner(&path, me).unwrap();
        let err = check_socket_owner(&path, me + 1).unwrap_err();
        assert!(err.contains("refusing to connect"), "{err}");
    }

    #[test]
    fn client_refuses_non_socket_and_reports_missing_app() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plain");
        fs::write(&file, b"x").unwrap();
        assert!(check_socket_owner(&file, current_uid().unwrap())
            .unwrap_err()
            .contains("not a socket"));
        let response = forward_to_shell_at(&dir.path().join("absent.sock"), &request("r"));
        assert_eq!(
            response.error.as_deref(),
            Some("Phlegethon desktop app is not running")
        );
    }

    #[test]
    fn idle_client_does_not_block_other_clients() {
        let (_dir, path) = start_server();
        // Connect and send nothing: the receiver thread for this client waits on read.
        let _idle = UnixStream::connect(&path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let path2 = path.clone();
        thread::spawn(move || {
            let _ = tx.send(forward_to_shell_at(&path2, &request("second")));
        });
        let response = rx
            .recv_timeout(Duration::from_secs(3))
            .expect("second client must be served while the first is idle");
        assert!(response.ok);
    }

    #[test]
    fn socket_path_prefers_runtime_dir_then_data_dir() {
        let runtime = Some(PathBuf::from("/run/user/1000"));
        assert_eq!(
            socket_path_from(runtime, None, None).unwrap(),
            PathBuf::from("/run/user/1000/phlegethon/companion.sock")
        );
        assert_eq!(
            socket_path_from(None, None, Some(PathBuf::from("/home/u"))).unwrap(),
            PathBuf::from("/home/u/.local/share/phlegethon/run/companion.sock")
        );
        assert_eq!(
            socket_path_from(
                Some(PathBuf::from("relative")),
                Some(PathBuf::from("/data")),
                None
            )
            .unwrap(),
            PathBuf::from("/data/phlegethon/run/companion.sock")
        );
        assert!(socket_path_from(None, None, None).is_err());
    }
}
