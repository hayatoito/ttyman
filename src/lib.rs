//! # `ttyman`
//!
//! A Rust library providing terminal TTY management, persistent sessions, in-memory VT100 virtual terminal
//! emulation, input byte remapping, and `ttyrec` / `ttyplay` timed frame encoding & decoding.
//!
//! ## Core Modules
//!
//! - [`mod@format`]: Binary frame parser, encoder, and stream reader for `.ttyrec` format.
//! - [`terminal`]: In-memory VT100 virtual terminal emulator ([`Terminal`]) for real-time screen inspection.
//! - [`remap`]: Keystroke and byte sequence transformation engine ([`InputRemapper`]).
//! - [`pty`]: Unix PTY pair allocation, terminal raw mode guard ([`RawGuard`]), and window sizing.
//! - [`ipc`]: Inter-process communication protocol types ([`IpcRequest`], [`IpcResponse`]) over Unix domain sockets.
//! - [`commands`]: Subcommand implementations for the `ttyman` CLI binary.
//!
//! ## Examples
//!
//! ### Parsing `.ttyrec` Frames
//!
//! ```no_run
//! use ttyman::ttyrec::read_frame;
//! use std::fs::File;
//!
//! let mut file = File::open("session.ttyrec").unwrap();
//! while let Ok(Some(frame)) = read_frame(&mut file) {
//!     println!(
//!         "Frame timestamp: {}s {}us, len: {}",
//!         frame.header.sec, frame.header.usec, frame.header.len
//!     );
//! }
//! ```
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
pub mod ipc;
pub mod pty;
pub mod remap;
pub mod terminal;
pub mod ttyrec;

pub use ttyrec::{
    extract_frame_from_buffer, read_frame, read_header, write_frame, write_header, write_raw_frame,
    Frame, FrameError, FrameReader, Header, HeaderError, HEADER_SIZE, MAX_RECORD_LEN,
};
pub use ipc::{
    DEFAULT_SESSION_VAR, IpcRequest, IpcResponse, bind_unix_listener, default_socket_path,
    get_runtime_dir, named_socket_path, validate_session_name,
};
pub use pty::{
    RawGuard, StdinRawFd, get_parent_termios, get_terminal_winsize, open_pty_pair,
    set_terminal_winsize,
};
pub use remap::InputRemapper;
pub use terminal::Terminal;
