//! Session-type detection.

use std::collections::HashMap;

/// Display server the session runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayServer {
    Wayland,
    X11,
    Unknown,
}

/// Detect from `XDG_SESSION_TYPE` / `WAYLAND_DISPLAY` / `DISPLAY`.
pub fn display_server(env: &HashMap<String, String>) -> DisplayServer {
    let get = |k: &str| env.get(k).map(String::as_str).unwrap_or("");
    match get("XDG_SESSION_TYPE") {
        "wayland" => return DisplayServer::Wayland,
        "x11" => return DisplayServer::X11,
        _ => {}
    }
    if !get("WAYLAND_DISPLAY").is_empty() {
        DisplayServer::Wayland
    } else if !get("DISPLAY").is_empty() {
        DisplayServer::X11
    } else {
        DisplayServer::Unknown
    }
}

/// Snapshot of the process environment.
pub fn current_env() -> HashMap<String, String> {
    std::env::vars().collect()
}

pub fn is_wayland(env: &HashMap<String, String>) -> bool {
    display_server(env) == DisplayServer::Wayland
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn session_type_wins() {
        assert_eq!(
            display_server(&env(&[("XDG_SESSION_TYPE", "wayland"), ("DISPLAY", ":0")])),
            DisplayServer::Wayland
        );
        assert_eq!(
            display_server(&env(&[
                ("XDG_SESSION_TYPE", "x11"),
                ("WAYLAND_DISPLAY", "w0")
            ])),
            DisplayServer::X11
        );
    }

    #[test]
    fn falls_back_to_display_variables() {
        assert!(is_wayland(&env(&[("WAYLAND_DISPLAY", "wayland-0")])));
        assert_eq!(
            display_server(&env(&[("DISPLAY", ":1")])),
            DisplayServer::X11
        );
        assert_eq!(display_server(&env(&[])), DisplayServer::Unknown);
    }
}
