//! Versioned JSON envelope for Companion ↔ desktop hand-off.

use serde::{Deserialize, Serialize};

use crate::cookies::CompanionSession;

pub const PROTOCOL_VERSION: u32 = 1;
pub const NATIVE_HOST_NAME: &str = "com.phlegethon.bridge";

/// Longest error text a reply carries. Errors may echo a URL that is nearly the 1 MiB frame
/// cap; an uncapped echo would push the reply past the native-messaging limit (L-46).
const MAX_ERROR_CHARS: usize = 1000;

fn truncate_error(mut text: String) -> String {
    if let Some((cut, _)) = text.char_indices().nth(MAX_ERROR_CHARS) {
        text.truncate(cut);
        text.push('…');
    }
    text
}

/// Intent hint from the extension; Download Core still classifies the final path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompanionIntent {
    #[default]
    ProbeThenConfirm,
    EnqueueDirect,
}

/// Extension → NMH → shell enqueue request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanionRequest {
    pub v: u32,
    pub op: String,
    pub request_id: String,
    pub page_url: String,
    #[serde(default)]
    pub media_url: Option<String>,
    #[serde(default)]
    pub intent: CompanionIntent,
    #[serde(default)]
    pub referrer: Option<String>,
    #[serde(default)]
    pub session: Option<CompanionSession>,
}

impl CompanionRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.v != PROTOCOL_VERSION {
            return Err(format!(
                "unsupported protocol version {}, expected {PROTOCOL_VERSION}",
                self.v
            ));
        }
        if self.op != "enqueue" {
            return Err(format!("unsupported op '{}', expected 'enqueue'", self.op));
        }
        if self.request_id.trim().is_empty() {
            return Err("request_id must not be empty".to_string());
        }
        let url = self.effective_url();
        if url.trim().is_empty() {
            return Err("page_url or media_url must be set".to_string());
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(format!("URL must be http(s), got: {url}"));
        }
        if let Some(session) = &self.session {
            session.validate_for_url(url)?;
        }
        Ok(())
    }

    /// URL to enqueue: explicit media URL wins over page URL.
    pub fn effective_url(&self) -> &str {
        self.media_url
            .as_deref()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or(self.page_url.as_str())
    }
}

/// Shell → NMH → extension acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanionResponse {
    pub v: u32,
    pub request_id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<CompanionResult>,
}

impl CompanionResponse {
    pub fn ok(request_id: impl Into<String>, result: CompanionResult) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            request_id: request_id.into(),
            ok: true,
            error: None,
            result: Some(result),
        }
    }

    pub fn err(request_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            request_id: request_id.into(),
            ok: false,
            error: Some(truncate_error(error.into())),
            result: None,
        }
    }
}

/// Enqueue outcome returned to the extension (mirrors shell enqueue result).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum CompanionResult {
    EnqueueDirect { job_id: String, destination: String },
    ProbeThenConfirm { url: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_json() {
        let req = CompanionRequest {
            v: PROTOCOL_VERSION,
            op: "enqueue".to_string(),
            request_id: "req-1".to_string(),
            page_url: "https://example.com/watch".to_string(),
            media_url: None,
            intent: CompanionIntent::ProbeThenConfirm,
            referrer: Some("https://example.com/".to_string()),
            session: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: CompanionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, req);
        parsed.validate().unwrap();
    }

    #[test]
    fn effective_url_prefers_media_url() {
        let req = CompanionRequest {
            v: PROTOCOL_VERSION,
            op: "enqueue".to_string(),
            request_id: "r".to_string(),
            page_url: "https://example.com/page".to_string(),
            media_url: Some("https://cdn.example.com/video.mp4".to_string()),
            intent: CompanionIntent::default(),
            referrer: None,
            session: None,
        };
        assert_eq!(req.effective_url(), "https://cdn.example.com/video.mp4");
    }

    #[test]
    fn validate_rejects_cookie_for_unrelated_host() {
        let mut req = CompanionRequest {
            v: PROTOCOL_VERSION,
            op: "enqueue".to_string(),
            request_id: "r".to_string(),
            page_url: "https://example.com/watch".to_string(),
            media_url: None,
            intent: CompanionIntent::default(),
            referrer: None,
            session: Some(CompanionSession {
                cookies: vec![crate::cookies::CompanionCookie {
                    name: "sid".to_string(),
                    value: "v".to_string(),
                    domain: ".evil.example".to_string(),
                    path: "/".to_string(),
                    secure: false,
                    http_only: false,
                    expiration_date: 0,
                }],
            }),
        };
        assert!(req.validate().unwrap_err().contains("does not cover host"));
        req.session.as_mut().unwrap().cookies[0].domain = ".example.com".to_string();
        req.validate().unwrap();
    }

    #[test]
    fn error_reply_echoing_huge_url_fits_native_message_cap() {
        let req = CompanionRequest {
            v: PROTOCOL_VERSION,
            op: "enqueue".to_string(),
            request_id: "r".to_string(),
            page_url: format!("ftp://{}", "a".repeat(1_048_500)),
            media_url: None,
            intent: CompanionIntent::default(),
            referrer: None,
            session: None,
        };
        let response = CompanionResponse::err("r", req.validate().unwrap_err());
        let mut buf = Vec::new();
        crate::wire::write_native_message(&mut buf, &response)
            .expect("error reply must fit the native-messaging cap");
        assert!(buf.len() < 4096);
    }

    #[test]
    fn validate_rejects_non_http_url() {
        let req = CompanionRequest {
            v: PROTOCOL_VERSION,
            op: "enqueue".to_string(),
            request_id: "r".to_string(),
            page_url: "ftp://example.com".to_string(),
            media_url: None,
            intent: CompanionIntent::default(),
            referrer: None,
            session: None,
        };
        let err = req.validate().unwrap_err();
        assert!(err.contains("http"));
    }
}
