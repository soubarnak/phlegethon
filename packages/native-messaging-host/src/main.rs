//! Chrome native messaging host — validates extension envelopes and forwards to shell.

use companion_protocol::{
    read_native_message, write_native_message, CompanionRequest, CompanionResponse,
    forward_to_shell, PROTOCOL_VERSION,
};
use std::io;

fn main() {
    if let Err(err) = run() {
        eprintln!("phlegethon-nmh error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();

    let request: CompanionRequest = read_native_message(&mut stdin)?;
    let request_id = request.request_id.clone();

    let response = match request.validate() {
        Ok(()) => forward_to_shell(&request),
        Err(err) => CompanionResponse::err(request_id, err),
    };

    write_native_message(&mut stdout, &response)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use companion_protocol::CompanionIntent;
    use std::io::Cursor;

    #[test]
    fn invalid_request_returns_error_response() {
        let request = CompanionRequest {
            v: PROTOCOL_VERSION,
            op: "enqueue".to_string(),
            request_id: "bad-req".to_string(),
            page_url: "not-a-url".to_string(),
            media_url: None,
            intent: CompanionIntent::ProbeThenConfirm,
            referrer: None,
            session: None,
        };
        let err = request.validate().unwrap_err();
        let response = CompanionResponse::err("bad-req", err);
        assert!(!response.ok);
        assert!(response.error.is_some());

        let mut buf = Vec::new();
        write_native_message(&mut buf, &response).unwrap();
        let mut cursor = Cursor::new(buf);
        let parsed: CompanionResponse = read_native_message(&mut cursor).unwrap();
        assert_eq!(parsed.request_id, "bad-req");
        assert!(!parsed.ok);
    }
}
