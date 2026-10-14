//! Chrome-family native messaging host manifest locations and rendering (Linux).

use serde::Serialize;
use std::path::{Path, PathBuf};

pub const HOST_NAME: &str = "com.phlegethon.bridge";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Chrome,
    Chromium,
    Brave,
    Vivaldi,
    Edge,
}

impl Browser {
    pub const ALL: [Browser; 5] = [
        Browser::Chrome,
        Browser::Chromium,
        Browser::Brave,
        Browser::Vivaldi,
        Browser::Edge,
    ];

    /// Profile directory under `~/.config`.
    fn config_dir(self) -> &'static str {
        match self {
            Browser::Chrome => "google-chrome",
            Browser::Chromium => "chromium",
            Browser::Brave => "BraveSoftware/Brave-Browser",
            Browser::Vivaldi => "vivaldi",
            Browser::Edge => "microsoft-edge",
        }
    }

    /// System-wide manifest directory, where the browser documents one.
    fn system_dir(self) -> Option<&'static str> {
        match self {
            Browser::Chrome => Some("/etc/opt/chrome/native-messaging-hosts"),
            Browser::Chromium => Some("/etc/chromium/native-messaging-hosts"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    System,
}

/// Directories where `<HOST_NAME>.json` must be placed.
pub fn manifest_dirs(home: &Path, scope: Scope, browsers: &[Browser]) -> Vec<PathBuf> {
    browsers
        .iter()
        .filter_map(|b| match scope {
            Scope::User => Some(
                home.join(".config")
                    .join(b.config_dir())
                    .join("NativeMessagingHosts"),
            ),
            Scope::System => b.system_dir().map(PathBuf::from),
        })
        .collect()
}

#[derive(Serialize)]
struct Manifest<'a> {
    name: &'a str,
    description: &'a str,
    path: String,
    #[serde(rename = "type")]
    kind: &'a str,
    allowed_origins: Vec<String>,
}

/// Render the manifest JSON. `host_path` must be absolute (Chrome rejects relative paths).
pub fn render_manifest(host_path: &Path, extension_ids: &[String]) -> Result<String, String> {
    if !host_path.is_absolute() {
        return Err(format!(
            "native messaging host path must be absolute: {}",
            host_path.display()
        ));
    }
    if extension_ids.is_empty() {
        return Err("at least one extension id is required".to_string());
    }
    for id in extension_ids {
        // Chrome extension ids are 32 chars of a-p.
        if id.len() != 32 || !id.bytes().all(|b| (b'a'..=b'p').contains(&b)) {
            return Err(format!("not a valid Chrome extension id: {id}"));
        }
    }
    let manifest = Manifest {
        name: HOST_NAME,
        description: "Phlegethon Download Manager bridge",
        path: host_path.display().to_string(),
        kind: "stdio",
        allowed_origins: extension_ids
            .iter()
            .map(|id| format!("chrome-extension://{id}/"))
            .collect(),
    };
    serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "abcdefghijklmnopabcdefghijklmnop";

    #[test]
    fn user_dirs_follow_each_browser_profile() {
        let dirs = manifest_dirs(Path::new("/home/u"), Scope::User, &Browser::ALL);
        assert!(dirs.contains(&PathBuf::from(
            "/home/u/.config/google-chrome/NativeMessagingHosts"
        )));
        assert!(dirs.contains(&PathBuf::from(
            "/home/u/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts"
        )));
        assert_eq!(dirs.len(), Browser::ALL.len());
    }

    #[test]
    fn system_dirs_only_for_documented_browsers() {
        let dirs = manifest_dirs(Path::new("/home/u"), Scope::System, &Browser::ALL);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/etc/opt/chrome/native-messaging-hosts"),
                PathBuf::from("/etc/chromium/native-messaging-hosts"),
            ]
        );
    }

    #[test]
    fn renders_a_valid_manifest() {
        let json = render_manifest(Path::new("/usr/bin/phlegethon-nmh"), &[ID.to_string()]).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["name"], HOST_NAME);
        assert_eq!(value["type"], "stdio");
        assert_eq!(value["path"], "/usr/bin/phlegethon-nmh");
        assert_eq!(value["allowed_origins"][0], format!("chrome-extension://{ID}/"));
    }

    #[test]
    fn rejects_relative_paths_and_bad_ids() {
        assert!(render_manifest(Path::new("phlegethon-nmh"), &[ID.to_string()]).is_err());
        assert!(render_manifest(Path::new("/usr/bin/x"), &[]).is_err());
        assert!(render_manifest(Path::new("/usr/bin/x"), &["short".to_string()]).is_err());
        assert!(render_manifest(Path::new("/usr/bin/x"), &["ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP".to_string()]).is_err());
    }
}
