//! # `ttyman`
//!
//! A Rust library providing terminal TTY management, persistent sessions, in-memory VT100 virtual terminal
//! emulation, and live Unix domain socket IPC.
//!
//! ## Core Modules
//!
//! - [`terminal`]: In-memory VT100 virtual terminal emulator ([`Terminal`]) for real-time screen inspection.
//! - [`config`]: Configuration file parsing ([`Config`]) for menu triggers and session settings.
//! - [`pty`]: Unix PTY pair allocation, terminal raw mode guard ([`RawGuard`]), and window sizing.
//! - [`ipc`]: Inter-process communication protocol types ([`IpcRequest`], [`IpcResponse`]) over Unix domain sockets.
//! - [`commands`]: Subcommand implementations for the `ttyman` CLI binary.
//!
//! ## Examples
//!
//! ### Inspecting VT100 Screen State
//!
//! ```
//! use ttyman::Terminal;
//!
//! let terminal = Terminal::new(24, 80, 1000);
//! terminal.process(b"Hello \x1b[32mWorld\x1b[0m\r\n");
//! let screen = terminal.read(None, false, false);
//! assert!(screen.contains("Hello World"));
//! ```

pub mod commands;
pub mod config;
pub mod ipc;
pub mod pty;
pub mod server;
pub mod terminal;

pub use config::Config;
pub use ipc::{
    DEFAULT_SESSION_VAR, IpcRequest, IpcResponse, bind_unix_listener, default_socket_path,
    get_runtime_dir, is_self_session, is_socket_alive, named_socket_path, send_ipc_request,
    validate_session_name,
};
pub use pty::{RawGuard, StdinRawFd, get_terminal_winsize, open_pty_pair, set_terminal_winsize};
pub use terminal::Terminal;

/// Block on an asynchronous future using a standard multi-threaded Tokio runtime.
pub fn run_async<F: std::future::Future<Output = anyhow::Result<()>>>(f: F) -> anyhow::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(f)
}
