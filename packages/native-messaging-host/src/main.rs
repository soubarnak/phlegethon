//! Chrome native messaging host — validates extension envelopes and forwards to shell.

use companion_protocol::{
    forward_to_shell, read_native_message, write_native_message, CompanionRequest,
    CompanionResponse, PROTOCOL_VERSION,
};
use std::io;
use std::path::PathBuf;

const MANIFEST_FILE: &str = "com.phlegethon.bridge.json";

fn main() {
    if let Err(err) = run() {
        eprintln!("phlegethon-nmh error: {err}");
        std::process::exit(1);
    }
}

/// Directories `scripts/install-nmh-linux.sh` writes the host manifest to.
fn manifest_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        for sub in [
            "google-chrome",
            "chromium",
            "BraveSoftware/Brave-Browser",
            "vivaldi",
            "microsoft-edge",
        ] {
            dirs.push(config.join(sub).join("NativeMessagingHosts"));
        }
    }
    dirs.push(PathBuf::from("/etc/opt/chrome/native-messaging-hosts"));
    dirs.push(PathBuf::from("/etc/chromium/native-messaging-hosts"));
    dirs
}

/// Origins named by any installed host manifest.
fn installed_allowed_origins() -> Vec<String> {
    manifest_dirs()
        .into_iter()
        .filter_map(|dir| std::fs::read_to_string(dir.join(MANIFEST_FILE)).ok())
        .flat_map(|text| allowed_origins_of(&text))
        .collect()
}

fn allowed_origins_of(manifest: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(manifest)
        .ok()
        .and_then(|v| {
            v.get("allowed_origins").and_then(|o| {
                o.as_array().map(|list| {
                    list.iter()
                        .filter_map(|e| e.as_str().map(str::to_string))
                        .collect()
                })
            })
        })
        .unwrap_or_default()
}

/// Reject a caller whose `chrome-extension://<id>/` origin (Chrome passes it as `argv[1]`) is
/// missing or not listed in an installed manifest. Chrome enforces `allowed_origins` itself;
/// this keeps the host from serving any other process that runs it directly (M-50).
fn check_origin(origin: Option<&str>, allowed: &[String]) -> Result<(), String> {
    let origin = origin.ok_or("no caller origin: run this host from the browser only")?;
    if !origin.starts_with("chrome-extension://") {
        return Err(format!("caller origin is not a Chrome extension: {origin}"));
    }
    if allowed.iter().any(|a| a == origin) {
        Ok(())
    } else {
        Err(format!(
            "caller origin {origin} is not listed in an installed {MANIFEST_FILE}"
        ))
    }
}

fn run() -> Result<(), String> {
    check_origin(
        std::env::args().nth(1).as_deref(),
        &installed_allowed_origins(),
    )?;

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

    fn allowed() -> Vec<String> {
        allowed_origins_of(
            r#"{"name":"com.phlegethon.bridge","allowed_origins":["chrome-extension://abcdefghijklmnopabcdefghijklmnop/"]}"#,
        )
    }

    #[test]
    fn listed_extension_origin_is_accepted() {
        check_origin(
            Some("chrome-extension://abcdefghijklmnopabcdefghijklmnop/"),
            &allowed(),
        )
        .unwrap();
    }

    #[test]
    fn unknown_missing_or_foreign_origin_is_rejected() {
        let allowed = allowed();
        assert!(check_origin(None, &allowed).is_err());
        assert!(check_origin(Some("https://evil.example/"), &allowed).is_err());
        assert!(check_origin(
            Some("chrome-extension://ponmlkjihgfedcbaponmlkjihgfedcba/"),
            &allowed
        )
        .is_err());
        // No installed manifest at all: fail closed.
        assert!(check_origin(
            Some("chrome-extension://abcdefghijklmnopabcdefghijklmnop/"),
            &[]
        )
        .is_err());
    }

    #[test]
    fn malformed_manifest_yields_no_origins() {
        assert!(allowed_origins_of("not json").is_empty());
        assert!(allowed_origins_of(r#"{"allowed_origins":"x"}"#).is_empty());
    }

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
