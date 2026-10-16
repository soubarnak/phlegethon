//! Chrome Companion hand-off protocol — shared between NMH and Platform Shells.
//!
//! Transport: Chrome native messaging (stdio) → NMH → filesystem socket (ADR 0005) → shell.

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
    bind_companion_listener, check_socket_owner, companion_cookie_dir, companion_socket_path,
    forward_to_shell, forward_to_shell_at, run_companion_server, run_companion_server_at, serve,
    COMPANION_SOCKET_FILE,
};
pub use wire::{read_native_message, write_native_message};
