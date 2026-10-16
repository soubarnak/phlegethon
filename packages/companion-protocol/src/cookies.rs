//! Optional session cookie hand-off — writes Netscape format for Core cookie channel.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Session material from the extension (scoped to page origin).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CompanionSession {
    #[serde(default)]
    pub cookies: Vec<CompanionCookie>,
}

impl CompanionSession {
    pub fn validate(&self) -> Result<(), String> {
        if self.cookies.is_empty() {
            return Err("session must include at least one cookie".to_string());
        }
        for cookie in &self.cookies {
            cookie.validate()?;
        }
        Ok(())
    }

    /// Like [`validate`](Self::validate), and every cookie domain must cover the host of
    /// `url`, the URL the hand-off enqueues. The extension collects cookies for that URL only,
    /// so a cookie for an unrelated site means a forged or buggy hand-off (L-43).
    pub fn validate_for_url(&self, url: &str) -> Result<(), String> {
        self.validate()?;
        let host = url::Url::parse(url)
            .map_err(|e| format!("invalid URL for cookie check: {e}"))?
            .host_str()
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| "URL has no host for cookie check".to_string())?;
        for cookie in &self.cookies {
            if !domain_covers_host(&cookie.domain, &host) {
                return Err(format!(
                    "cookie '{}' has domain '{}' which does not cover host '{host}'",
                    cookie.name, cookie.domain
                ));
            }
        }
        Ok(())
    }
}

/// Cookie domain match: `.example.com` and `example.com` cover `example.com` and its subdomains.
fn domain_covers_host(domain: &str, host: &str) -> bool {
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();
    !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
}

/// One browser cookie exported by the extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanionCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    #[serde(default = "default_path")]
    pub path: String,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
    /// Unix timestamp; 0 means session cookie.
    #[serde(default)]
    pub expiration_date: i64,
}

fn default_path() -> String {
    "/".to_string()
}

impl CompanionCookie {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("cookie name must not be empty".to_string());
        }
        if self.domain.trim().is_empty() {
            return Err("cookie domain must not be empty".to_string());
        }
        // A Netscape row is tab separated and one line long, and the format has no escape
        // sequence. A browser never stores control characters in a cookie, so refuse them
        // instead of letting one field forge extra columns or rows (L-43).
        for (field, text) in [
            ("name", &self.name),
            ("value", &self.value),
            ("domain", &self.domain),
            ("path", &self.path),
        ] {
            if text.chars().any(char::is_control) {
                return Err(format!("cookie {field} contains a control character"));
            }
        }
        Ok(())
    }
}

/// Write cookies to a Netscape-format file for yt-dlp `--cookies`.
///
/// `dir` must be absolute. The directory is created with mode 0700 and the file with mode 0600,
/// because the file holds the page's session cookies in plain text (H-21). The caller owns the
/// file and must delete it when no job needs it any more.
pub fn write_netscape_cookie_file(
    session: &CompanionSession,
    dir: &Path,
) -> Result<PathBuf, String> {
    session.validate()?;
    if !dir.is_absolute() {
        return Err(format!(
            "cookie directory must be absolute, got: {}",
            dir.display()
        ));
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("create cookie dir: {e}"))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod cookie dir: {e}"))?;

    let path = dir.join(format!("companion-{}.cookies.txt", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("create cookie file: {e}"))?;
    if let Err(err) = file.write_all(netscape_text(session).as_bytes()) {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(format!("write cookie file: {err}"));
    }
    Ok(path)
}

fn netscape_text(session: &CompanionSession) -> String {
    let mut text =
        String::from("# Netscape HTTP Cookie File\n# Generated by Phlegethon Chrome Companion\n\n");
    for cookie in &session.cookies {
        let domain_flag = if cookie.domain.starts_with('.') {
            "TRUE"
        } else {
            "FALSE"
        };
        let secure = if cookie.secure { "TRUE" } else { "FALSE" };
        // yt-dlp and curl read HttpOnly cookies from a `#HttpOnly_` domain prefix.
        let http_only = if cookie.http_only { "#HttpOnly_" } else { "" };
        text.push_str(&format!(
            "{http_only}{}\t{domain_flag}\t{}\t{secure}\t{}\t{}\t{}\n",
            cookie.domain, cookie.path, cookie.expiration_date, cookie.name, cookie.value,
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netscape_file_contains_cookie_rows() {
        let session = CompanionSession {
            cookies: vec![CompanionCookie {
                name: "sid".to_string(),
                value: "abc123".to_string(),
                domain: ".example.com".to_string(),
                path: "/".to_string(),
                secure: true,
                http_only: true,
                expiration_date: 1_700_000_000,
            }],
        };
        let dir = tempfile::tempdir().unwrap();
        let path = write_netscape_cookie_file(&session, dir.path()).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("# Netscape HTTP Cookie File"));
        assert!(content.contains("#HttpOnly_.example.com\tTRUE\t/\tTRUE\t1700000000\tsid\tabc123"));
    }

    fn cookie(name: &str, value: &str, domain: &str) -> CompanionCookie {
        CompanionCookie {
            name: name.to_string(),
            value: value.to_string(),
            domain: domain.to_string(),
            path: "/".to_string(),
            secure: false,
            http_only: false,
            expiration_date: 0,
        }
    }

    #[test]
    fn cookie_file_and_directory_are_private() {
        let session = CompanionSession {
            cookies: vec![cookie("sid", "abc", "example.com")],
        };
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("companion-cookies");
        let path = write_netscape_cookie_file(&session, &dir).unwrap();
        let file_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn relative_directory_is_refused_before_anything_is_written() {
        let session = CompanionSession {
            cookies: vec![cookie("sid", "abc", "example.com")],
        };
        let err = write_netscape_cookie_file(&session, Path::new("relative/dir")).unwrap_err();
        assert!(err.contains("absolute"), "{err}");
        assert!(!Path::new("relative/dir").exists());
    }

    #[test]
    fn control_characters_cannot_forge_rows_or_columns() {
        for bad in [
            cookie(
                "a",
                "x\n.evil.com\tTRUE\t/\tFALSE\t0\tsid\tstolen",
                "example.com",
            ),
            cookie("a\tb", "v", "example.com"),
            cookie("a", "v", "example.com\r"),
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        let mut path_cookie = cookie("a", "v", "example.com");
        path_cookie.path = "/p\tq".to_string();
        assert!(path_cookie.validate().is_err());
    }

    #[test]
    fn cookie_domain_must_cover_the_enqueued_host() {
        let session = |domain: &str| CompanionSession {
            cookies: vec![cookie("sid", "v", domain)],
        };
        let url = "https://www.example.com/watch?v=1";
        session(".example.com").validate_for_url(url).unwrap();
        session("www.example.com").validate_for_url(url).unwrap();
        session("EXAMPLE.com").validate_for_url(url).unwrap();
        assert!(session(".other.com").validate_for_url(url).is_err());
        // Suffix of the host string but not a parent domain.
        assert!(session("ample.com").validate_for_url(url).is_err());
        assert!(session("sub.www.example.com")
            .validate_for_url(url)
            .is_err());
    }
}
