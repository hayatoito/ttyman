use crate::ipc::{DEFAULT_SESSION_VAR, IpcRequest, resolve_socket_path};
use crate::pty::{RawGuard, StdinRawFd, get_terminal_winsize};
use crate::server::{spawn_daemon_process, spawn_daemon_supervisor};
use clap::Args;
use nix::unistd::read as nix_read;
use std::io::IsTerminal;
use std::path::PathBuf;
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};

pub const DEFAULT_MENU_COMMAND: &str = "echo detach";

#[derive(Args, Debug, Clone)]
pub struct AttachArgs {
    /// Do not synchronize terminal window size on attach or resize
    #[arg(long = "no-resize")]
    pub no_resize: bool,

    /// Path to TOML configuration file if spawning a new session
    #[arg(short = 'c', long = "config")]
    pub config: Option<String>,

    /// Target session name (defaults to $TTYMAN_SESSION)
    #[arg(short = 's', long = "session", value_name = "SESSION")]
    pub session: Option<String>,

    /// Command to execute if creating a new session (defaults to interactive $SHELL)
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

pub fn run(args: AttachArgs) -> anyhow::Result<()> {
    let target = crate::server::prepare_session_target(
        args.session.as_deref(),
        args.config.as_deref(),
        &args.command,
        true,
    )?;

    let sock_path = match target {
        crate::server::TargetSession::Handled => return Ok(()),
        crate::server::TargetSession::Ready {
            sock_path,
            is_alive,
        } => {
            if !is_alive {
                spawn_daemon_supervisor(
                    Some(sock_path.clone()),
                    args.config.as_deref(),
                    &args.command,
                )?;
            }
            sock_path
        }
        crate::server::TargetSession::Unspecified => {
            spawn_daemon_supervisor(None, args.config.as_deref(), &args.command)?
        }
    };

    crate::run_async(attach_client(sock_path, args))
}

enum AttachOutcome {
    Detached,
    Terminated,
    SwitchTo(PathBuf),
}

struct MenuContext<'a> {
    key: u8,
    command: &'a str,
}

async fn attach_single_session(
    sock_path: &PathBuf,
    history_stack: &[PathBuf],
    args: &AttachArgs,
    menu: &MenuContext<'_>,
    is_stdin_tty: bool,
    raw_guard: &mut Option<RawGuard>,
) -> anyhow::Result<AttachOutcome> {
    let stream = UnixStream::connect(sock_path).await.map_err(|e| {
        let name = crate::ipc::parse_name_from_socket_path(sock_path);
        anyhow::anyhow!("Failed to connect to session '{name}': {e}")
    })?;

    let (mut reader, mut writer) = stream.into_split();

    let winsize = if args.no_resize || !is_stdin_tty {
        None
    } else {
        get_terminal_winsize(0).filter(|w| w.ws_row > 0 && w.ws_col > 0)
    };

    let req = IpcRequest::Attach {
        cols: winsize.as_ref().map(|w| w.ws_col),
        rows: winsize.as_ref().map(|w| w.ws_row),
    };
    let mut req_str = serde_json::to_string(&req)?;
    req_str.push('\n');
    writer.write_all(req_str.as_bytes()).await?;
    writer.flush().await?;

    let async_stdin = AsyncFd::new(StdinRawFd(0))?;
    let mut async_stdout = tokio::io::stdout();
    let mut sig_winch = if !args.no_resize && is_stdin_tty {
        Some(signal(SignalKind::window_change())?)
    } else {
        None
    };

    let mut out_buf = [0u8; 8192];
    let mut in_buf = [0u8; 1024];
    let mut stdin_open = true;

    loop {
        tokio::select! {
            // Inbound: session output -> client stdout
            res = reader.read(&mut out_buf) => {
                match res {
                    Ok(0) => return Ok(AttachOutcome::Terminated),
                    Ok(n) => {
                        let _ = async_stdout.write_all(&out_buf[..n]).await;
                        let _ = async_stdout.flush().await;
                    }
                    Err(_) => return Ok(AttachOutcome::Terminated),
                }
            }
            // Outbound: client stdin -> session input
            guard = async_stdin.readable(), if stdin_open => {
                match guard {
                    Ok(mut ready_guard) => {
                        let res = ready_guard.try_io(|inner| {
                            nix_read(inner.get_ref(), &mut in_buf)
                                .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
                        });
                        match res {
                            Ok(Ok(0)) => {
                                stdin_open = false;
                            }
                            Ok(Ok(n)) => {
                                if let Some(pos) = in_buf[..n].iter().position(|&b| b == menu.key) {
                                    if pos > 0 {
                                        if writer.write_all(&in_buf[..pos]).await.is_err() {
                                            return Ok(AttachOutcome::Terminated);
                                        }
                                        let _ = writer.flush().await;
                                    }
                                    // Temporarily leave raw mode so interactive menu gets a normal TTY
                                    drop(raw_guard.take());

                                    let recent_sessions: Vec<String> = history_stack
                                        .iter()
                                        .rev()
                                        .map(|p| crate::ipc::parse_name_from_socket_path(p))
                                        .collect();
                                    let recent_sessions_str = recent_sessions.join(" ");

                                    let current_session_name = crate::ipc::parse_name_from_socket_path(sock_path);
                                    let status_res = std::process::Command::new("sh")
                                        .arg("-c")
                                        .arg(menu.command)
                                        .env(DEFAULT_SESSION_VAR, &current_session_name)
                                        .env("TTYMAN_RECENT_SESSIONS", &recent_sessions_str)
                                        .stdin(std::process::Stdio::inherit())
                                        .stdout(std::process::Stdio::piped())
                                        .stderr(std::process::Stdio::inherit())
                                        .output();

                                    // Re-enter raw mode
                                    *raw_guard = Some(RawGuard::enter()?);

                                    if let Ok(output) = status_res && output.status.success() {
                                        let trimmed = String::from_utf8_lossy(&output.stdout).trim().to_string();
                                        if trimmed.eq_ignore_ascii_case("detach") {
                                            return Ok(AttachOutcome::Detached);
                                        } else if trimmed.eq_ignore_ascii_case("attach") {
                                            let new_sock = spawn_daemon_process(None, args.config.as_deref())?;
                                            return Ok(AttachOutcome::SwitchTo(new_sock));
                                        } else if let Some(target) = trimmed.strip_prefix("attach ") {
                                            let target_name = target.trim();
                                            if !target_name.is_empty() {
                                                let target_sock = resolve_socket_path(Some(target_name))?;
                                                return Ok(AttachOutcome::SwitchTo(target_sock));
                                            } else {
                                                let new_sock = spawn_daemon_process(None, args.config.as_deref())?;
                                                return Ok(AttachOutcome::SwitchTo(new_sock));
                                            }
                                        }
                                    }

                                    // If cancelled or failed, fetch fresh screen snapshot via independent IPC and redraw local terminal
                                    if let Ok(crate::ipc::IpcResponse::Ok(redraw_payload)) =
                                         crate::ipc::send_ipc_request(sock_path, &IpcRequest::Redraw).await
                                    {
                                         let _ = async_stdout.write_all(redraw_payload.as_bytes()).await;
                                         let _ = async_stdout.flush().await;
                                    }
                                } else {
                                    if writer.write_all(&in_buf[..n]).await.is_err() {
                                        return Ok(AttachOutcome::Terminated);
                                    }
                                    let _ = writer.flush().await;
                                }
                            }
                            Ok(Err(_)) => {
                                stdin_open = false;
                            }
                            Err(_would_block) => {}
                        }
                    }
                    Err(_) => {
                        stdin_open = false;
                    }
                }
            }
            // Resize: handle terminal window change (SIGWINCH)
            _ = async {
                if let Some(ref mut sw) = sig_winch {
                    sw.recv().await
                } else {
                    None
                }
            }, if sig_winch.is_some() => {
                if let Some(ws) = get_terminal_winsize(0).filter(|w| w.ws_row > 0 && w.ws_col > 0) {
                    let resize_req = IpcRequest::Resize {
                        cols: ws.ws_col,
                        rows: ws.ws_row,
                    };
                    let _ = crate::ipc::send_ipc_request(sock_path, &resize_req).await;
                }
            }
        }
    }
}

async fn attach_client(initial_sock_path: PathBuf, args: AttachArgs) -> anyhow::Result<()> {
    let file_cfg = crate::config::Config::load_default_or_explicit(args.config.as_deref());
    let menu_key = file_cfg
        .as_ref()
        .map(crate::config::Config::menu_key)
        .unwrap_or(0x1D);
    let default_cmd = "echo detach".to_string();
    let menu_command = file_cfg
        .as_ref()
        .map(crate::config::Config::menu_command)
        .unwrap_or(&default_cmd);

    let mut raw_guard = Some(RawGuard::enter()?);
    let is_stdin_tty = std::io::stdin().is_terminal();

    let mut history_stack: Vec<PathBuf> = Vec::new();
    let mut current_sock_path = initial_sock_path;
    let menu = MenuContext {
        key: menu_key,
        command: menu_command,
    };

    loop {
        // Track session in LRU history stack (most recently visited at the end)
        history_stack.retain(|p| p != &current_sock_path);
        history_stack.push(current_sock_path.clone());

        match attach_single_session(
            &current_sock_path,
            &history_stack,
            &args,
            &menu,
            is_stdin_tty,
            &mut raw_guard,
        )
        .await?
        {
            AttachOutcome::Detached => {
                drop(raw_guard.take());
                eprintln!("\r\n[ttyman: detached]");
                return Ok(());
            }
            AttachOutcome::Terminated => {
                // The current session terminated; remove it from history
                history_stack.retain(|p| p != &current_sock_path);

                // 1. Find the next session to switch to from the history stack in reverse order (LRU)
                let mut next_target = None;
                while let Some(prev) = history_stack.pop() {
                    if prev.exists() && std::os::unix::net::UnixStream::connect(&prev).is_ok() {
                        next_target = Some(prev);
                        break;
                    }
                }

                // 2. Fallback: if history stack is empty, discover any other active session on the system
                if next_target.is_none() {
                    let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
                    for sock in crate::ipc::find_socket_files() {
                        if sock != current_sock_path
                            && sock.exists()
                            && std::os::unix::net::UnixStream::connect(&sock).is_ok()
                        {
                            let mtime = std::fs::metadata(&sock)
                                .and_then(|m| m.modified())
                                .unwrap_or(std::time::UNIX_EPOCH);
                            candidates.push((sock, mtime));
                        }
                    }
                    candidates.sort_by(|a, b| b.1.cmp(&a.1));
                    if let Some((sock, _)) = candidates.into_iter().next() {
                        next_target = Some(sock);
                    }
                }

                if let Some(next_sock) = next_target {
                    let display_name = crate::ipc::parse_name_from_socket_path(&next_sock);
                    eprintln!("\r\n[ttyman: switched to session '{display_name}']");
                    current_sock_path = next_sock;
                    continue;
                }

                // No other sessions available; exit cleanly back to host shell
                return Ok(());
            }
            AttachOutcome::SwitchTo(new_sock_path) => {
                if !new_sock_path.exists()
                    || std::os::unix::net::UnixStream::connect(&new_sock_path).is_err()
                {
                    spawn_daemon_process(Some(&new_sock_path), args.config.as_deref())?;
                }
                current_sock_path = new_sock_path;
            }
        }
    }
}
