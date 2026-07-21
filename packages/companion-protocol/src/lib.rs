//! Chrome Companion hand-off protocol — shared between NMH and Platform Shells.
//!
//! Transport: Chrome native messaging (stdio) → NMH → local socket → shell.

pub mod cookies;
pub mod envelope;
pub mod ipc;
pub mod wire;

pub use cookies::{write_netscape_cookie_file, CompanionCookie, CompanionSession};
pub use envelope::{
    CompanionIntent, CompanionRequest, CompanionResponse, CompanionResult, NATIVE_HOST_NAME,
    PROTOCOL_VERSION,
};
pub use ipc::{
    companion_cookie_dir, forward_to_shell, run_companion_server, serve_one_intent,
    COMPANION_SOCKET_NAME,
};
pub use wire::{read_native_message, write_native_message};
