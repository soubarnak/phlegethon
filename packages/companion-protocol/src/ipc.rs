//! Local socket IPC between NMH and running Platform Shell.

use crate::envelope::{CompanionRequest, CompanionResponse};
use interprocess::local_socket::traits::{Listener, Stream};
use interprocess::local_socket::{
    GenericNamespaced, ListenerOptions, Name, Stream as LocalSocketStream, ToNsName,
};
use std::io::{Read, Write};
use std::path::PathBuf;

/// Local socket identifier shared by the NMH and the Tauri shell receiver.
pub const COMPANION_SOCKET_NAME: &str = "phlegethon-companion";

fn socket_name() -> Result<Name<'static>, String> {
    COMPANION_SOCKET_NAME
        .to_ns_name::<GenericNamespaced>()
        .map_err(|e| format!("companion socket name: {e}"))
}

/// Forward one validated request to the running shell and return its response.
pub fn forward_to_shell(request: &CompanionRequest) -> CompanionResponse {
    let request_id = request.request_id.clone();
    match try_forward_to_shell(request) {
        Ok(response) => response,
        Err(err) => CompanionResponse::err(request_id, err),
    }
}

fn try_forward_to_shell(request: &CompanionRequest) -> Result<CompanionResponse, String> {
    let name = socket_name()?;
    let mut stream = LocalSocketStream::connect(name)
        .map_err(|_| "Phlegethon desktop app is not running".to_string())?;

    let payload =
        serde_json::to_vec(request).map_err(|e| format!("serialize request: {e}"))?;
    write_frame(&mut stream, &payload)?;
    let response_bytes = read_frame(&mut stream)?;
    serde_json::from_slice(&response_bytes).map_err(|e| format!("invalid shell response: {e}"))
}

/// Run the companion receiver loop, invoking `handler` for each connection.
pub fn run_companion_server<F>(handler: F) -> Result<(), String>
where
    F: Fn(CompanionRequest) -> CompanionResponse + Send + Sync + 'static,
{
    let name = socket_name()?;
    let opts = ListenerOptions::new().name(name);
    let listener = opts
        .create_sync()
        .map_err(|e| format!("create companion listener: {e}"))?;

    loop {
        let mut stream = match listener.accept() {
            Ok(stream) => stream,
            Err(err) => return Err(format!("accept companion connection: {err}")),
        };

        let request_bytes = match read_frame(&mut stream) {
            Ok(bytes) => bytes,
            Err(err) => {
                let response = CompanionResponse::err("", err);
                let _ = write_response_frame(&mut stream, &response);
                continue;
            }
        };

        let request: CompanionRequest = match serde_json::from_slice(&request_bytes) {
            Ok(req) => req,
            Err(err) => {
                let response = CompanionResponse::err("", format!("invalid request: {err}"));
                let _ = write_response_frame(&mut stream, &response);
                continue;
            }
        };

        let request_id = request.request_id.clone();
        let response = match request.validate() {
            Ok(()) => handler(request),
            Err(err) => CompanionResponse::err(request_id, err),
        };
        let _ = write_response_frame(&mut stream, &response);
    }
}

fn write_response_frame<W: Write>(
    writer: &mut W,
    response: &CompanionResponse,
) -> Result<(), String> {
    let response_bytes =
        serde_json::to_vec(response).map_err(|e| format!("serialize response: {e}"))?;
    write_frame(writer, &response_bytes)
}

/// Accept one connection, read a request frame, invoke `handler`, write response.
pub fn serve_one_intent<F>(handler: F) -> Result<(), String>
where
    F: FnOnce(CompanionRequest) -> CompanionResponse,
{
    let name = socket_name()?;
    let opts = ListenerOptions::new().name(name);
    let listener = opts
        .create_sync()
        .map_err(|e| format!("create companion listener: {e}"))?;

    let mut stream = listener
        .accept()
        .map_err(|e| format!("accept companion connection: {e}"))?;

    let request_bytes = read_frame(&mut stream)?;
    let request: CompanionRequest =
        serde_json::from_slice(&request_bytes).map_err(|e| format!("invalid request: {e}"))?;
    let request_id = request.request_id.clone();

    let response = match request.validate() {
        Ok(()) => handler(request),
        Err(err) => CompanionResponse::err(request_id, err),
    };

    write_response_frame(&mut stream, &response)
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
pub fn companion_cookie_dir(data_root: &PathBuf) -> PathBuf {
    data_root.join("companion-cookies")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{CompanionIntent, CompanionRequest, PROTOCOL_VERSION};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn round_trip_request_over_local_socket() {
        thread::spawn(|| {
            let _ = serve_one_intent(|req| {
                CompanionResponse::ok(
                    req.request_id.clone(),
                    crate::envelope::CompanionResult::ProbeThenConfirm {
                        url: req.effective_url().to_string(),
                    },
                )
            });
        });

        thread::sleep(Duration::from_millis(100));

        let request = CompanionRequest {
            v: PROTOCOL_VERSION,
            op: "enqueue".to_string(),
            request_id: "ipc-test".to_string(),
            page_url: "https://example.com/watch".to_string(),
            media_url: None,
            intent: CompanionIntent::ProbeThenConfirm,
            referrer: None,
            session: None,
        };

        let response = forward_to_shell(&request);
        assert!(response.ok);
    }
}
