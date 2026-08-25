use crate::InputRemapper;
use crate::commands::run::{SessionSharedState, start_ipc_server};
use crate::ipc::{DEFAULT_SESSION_VAR, IpcRequest, resolve_socket_path};
use crate::pty::{RawGuard, StdinRawFd, get_terminal_winsize, open_pty_pair};
use crate::terminal::Terminal;
use clap::Args;
use nix::libc;
use nix::pty::Winsize;
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::waitpid;
use nix::unistd::ForkResult::{Child, Parent};
use nix::unistd::{close, execvp, fork, pipe, read as nix_read, setsid, write as nix_write};
use std::ffi::CString;
use std::io::{IsTerminal, Read};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

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

pub fn parse_menu_key(s: &str) -> anyhow::Result<Vec<u8>> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }
    s.split(',')
        .map(|token| {
            let t = token.trim();
            if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                u8::from_str_radix(hex, 16)
            } else {
                t.parse::<u8>()
            }
            .map_err(|e| anyhow::anyhow!("Invalid byte token '{t}': {e}"))
        })
        .collect()
}

pub struct MenuDetector {
    chord: Vec<u8>,
    matched: usize,
}

impl MenuDetector {
    pub fn new(chord: Vec<u8>) -> Self {
        Self { chord, matched: 0 }
    }

    pub fn from_config(menu_key_str: &str) -> anyhow::Result<Self> {
        let chord = parse_menu_key(menu_key_str)?;
        Ok(Self::new(chord))
    }

    /// Process input slice. Returns `(to_forward, is_triggered)`.
    pub fn process(&mut self, input: &[u8]) -> (Vec<u8>, bool) {
        if self.chord.is_empty() {
            return (input.to_vec(), false);
        }

        let mut output = Vec::with_capacity(input.len());
        for &b in input {
            if b == self.chord[self.matched] {
                self.matched += 1;
                if self.matched == self.chord.len() {
                    self.matched = 0;
                    return (output, true);
                }
            } else {
                if self.matched > 0 {
                    output.extend_from_slice(&self.chord[..self.matched]);
                    self.matched = 0;
                }
                if b == self.chord[0] {
                    self.matched = 1;
                    if self.chord.len() == 1 {
                        self.matched = 0;
                        return (output, true);
                    }
                } else {
                    output.push(b);
                }
            }
        }
        (output, false)
    }

    pub fn flush(&mut self) -> Vec<u8> {
        if self.matched > 0 {
            let pending = self.chord[..self.matched].to_vec();
            self.matched = 0;
            pending
        } else {
            Vec::new()
        }
    }
}

pub fn run(args: AttachArgs) -> anyhow::Result<()> {
    let _ = crate::ipc::get_runtime_dir()?;
    let current_env_session = std::env::var(DEFAULT_SESSION_VAR).ok();
    let current_env_sock = match current_env_session.as_deref() {
        Some(name) => resolve_socket_path(Some(name)).ok(),
        None => None,
    };
    let target_sock = match args.session.as_deref() {
        Some(target) => Some(resolve_socket_path(Some(target))?),
        None => current_env_sock.clone(),
    };

    if let Some(sock_path) = target_sock {
        let is_inside_session = current_env_sock.is_some();
        let is_self_attach = if let Some(ref cur_sock) = current_env_sock {
            sock_path == *cur_sock
                || (sock_path.exists()
                    && cur_sock.exists()
                    && std::fs::canonicalize(&sock_path).ok()
                        == std::fs::canonicalize(cur_sock).ok())
        } else if sock_path.exists()
            && let Some(my_tty) = crate::ipc::get_current_tty_name()
            && let Ok(info) = crate::ipc::query_session_info(&sock_path)
        {
            info.pty_slave_path.as_deref() == Some(&my_tty)
        } else {
            false
        };

        if is_self_attach {
            anyhow::bail!(
                "Cannot attach to session '{}' from within its own terminal.\n\
                 Press 'Ctrl-]' to detach or switch sessions.",
                sock_path.display()
            );
        }

        let is_alive = if sock_path.exists() {
            match std::os::unix::net::UnixStream::connect(&sock_path) {
                Ok(_) => true,
                Err(_) => {
                    let _ = std::fs::remove_file(&sock_path);
                    false
                }
            }
        } else {
            false
        };

        let session_display = args
            .session
            .as_deref()
            .unwrap_or(sock_path.to_str().unwrap_or("session"));

        // When inside an existing session and targeting a different session,
        // automatically start in background to prevent nested terminal multiplexers.
        if is_inside_session && !is_self_attach {
            if is_alive {
                println!(
                    "[ttyman] Session '{session_display}' is already running in background (nesting prevented).\n\
                     [ttyman] Press 'Ctrl-]' to switch to '{session_display}'."
                );
                return Ok(());
            } else {
                spawn_daemon_supervisor(
                    Some(sock_path.clone()),
                    args.config.as_deref(),
                    &args.command,
                )?;
                println!(
                    "[ttyman] Started session '{session_display}' in background (nesting prevented).\n\
                     [ttyman] Press 'Ctrl-]' to switch to '{session_display}'."
                );
                return Ok(());
            }
        }

        if is_alive {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            return rt.block_on(attach_client(sock_path, args));
        }

        // Session does not exist yet; spawn daemon with designated socket path
        spawn_daemon_supervisor(
            Some(sock_path.clone()),
            args.config.as_deref(),
            &args.command,
        )?;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        return rt.block_on(attach_client(sock_path, args));
    }

    // No target socket specified and not inside an existing session:
    // Spawn a new daemon using its own PID as the socket name.
    let sock_path = spawn_daemon_supervisor(None, args.config.as_deref(), &args.command)?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(attach_client(sock_path, args))
}

pub fn spawn_daemon_supervisor(
    target_sock: Option<PathBuf>,
    config: Option<&str>,
    command: &[String],
) -> anyhow::Result<PathBuf> {
    let default_shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let (exec_cmd, exec_args) = if !command.is_empty() {
        (command[0].clone(), command.to_vec())
    } else {
        (
            default_shell.clone(),
            vec![default_shell.clone(), "-i".to_string()],
        )
    };

    let (pipe_r_fd, pipe_w_fd) = pipe()?;
    let config_opt = config.map(|s| s.to_string());

    match unsafe { fork() }? {
        Parent { child: _ } => {
            let _ = close(pipe_w_fd.into_raw_fd());
            let mut pipe_file = unsafe { std::fs::File::from_raw_fd(pipe_r_fd.into_raw_fd()) };
            let mut msg = String::new();
            pipe_file.read_to_string(&mut msg)?;

            let trimmed = msg.trim();
            if let Some(path_str) = trimmed.strip_prefix("READY") {
                let path_clean = path_str.trim();
                if !path_clean.is_empty() {
                    Ok(PathBuf::from(path_clean))
                } else {
                    Ok(target_sock.unwrap_or_default())
                }
            } else if !trimmed.is_empty() {
                anyhow::bail!("{msg}")
            } else {
                anyhow::bail!("daemon process failed to start")
            }
        }
        Child => {
            let _ = close(pipe_r_fd.into_raw_fd());
            let _ = setsid();

            let own_pid = std::process::id();
            let sock_path = match target_sock {
                Some(p) => p,
                None => crate::ipc::default_socket_path(own_pid)?,
            };

            if let Ok(devnull) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")
            {
                let null_raw = devnull.as_raw_fd();
                unsafe {
                    libc::dup2(null_raw, 0);
                    libc::dup2(null_raw, 1);
                    libc::dup2(null_raw, 2);
                }
            }

            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = nix_write(
                        unsafe { BorrowedFd::borrow_raw(pipe_w_fd.into_raw_fd()) },
                        format!("Error: {e}\n").as_bytes(),
                    );
                    std::process::exit(1);
                }
            };

            let pipe_w_raw = pipe_w_fd.into_raw_fd();
            let res = rt.block_on(run_daemon_server(
                sock_path, config_opt, exec_cmd, exec_args, pipe_w_raw,
            ));

            if let Err(e) = res {
                let _ = nix_write(
                    unsafe { BorrowedFd::borrow_raw(pipe_w_raw) },
                    format!("Error: {e}\n").as_bytes(),
                );
                std::process::exit(1);
            }
            std::process::exit(0);
        }
    }
}

pub fn spawn_daemon_process(sock_path: &Path, config: Option<&str>) -> anyhow::Result<()> {
    let current_exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(current_exe);
    let session_name = crate::ipc::parse_name_from_socket_path(sock_path);
    cmd.arg("start").arg("-s").arg(&session_name);
    if let Some(cfg) = config {
        cmd.arg("-c").arg(cfg);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let mut child = cmd.spawn()?;
    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("Failed to spawn daemon process for session '{session_name}'");
    }

    // Wait briefly for socket to become connectable
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    while std::time::Instant::now() < deadline {
        if sock_path.exists() && std::os::unix::net::UnixStream::connect(sock_path).is_ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    if sock_path.exists() {
        Ok(())
    } else {
        anyhow::bail!(
            "Timed out waiting for session socket at {}",
            sock_path.display()
        )
    }
}

async fn run_daemon_server(
    sock_path: PathBuf,
    _config: Option<String>,
    exec_cmd: String,
    exec_args: Vec<String>,
    pipe_w_raw_fd: std::os::fd::RawFd,
) -> anyhow::Result<()> {
    let own_pid = std::process::id();
    let listener = crate::ipc::bind_unix_listener(&sock_path)?;

    let default_winsize = Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty_res = open_pty_pair(Some(&default_winsize), None)?;
    let master_fd = pty_res.master;
    let slave_fd = pty_res.slave;

    let file_cfg = crate::remap::Config::load_default_or_explicit(_config.as_deref());
    let scrollback = file_cfg.as_ref().map(|c| c.scrollback()).unwrap_or(10_000);
    let terminal = Terminal::new(24, 80, scrollback);
    let (inject_tx, mut inject_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (broadcast_tx, _) = tokio::sync::broadcast::channel::<Vec<u8>>(1024);
    let started_at_epoch_sec = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let pty_slave_path = nix::unistd::ttyname(&slave_fd)
        .ok()
        .map(|p| p.to_string_lossy().to_string());

    let shared_state = Arc::new(SessionSharedState {
        pid: own_pid,
        started_at_epoch_sec,
        command: exec_args.clone(),
        master_raw_fd: master_fd.as_raw_fd(),
        child_pid: std::sync::atomic::AtomicI32::new(0),
        persist: true,
        clients: std::sync::atomic::AtomicUsize::new(0),
        pty_slave_path,
        current_sock_path: std::sync::RwLock::new(sock_path.clone()),
    });

    start_ipc_server(
        listener,
        terminal.clone(),
        inject_tx,
        shared_state.clone(),
        broadcast_tx.clone(),
    );

    // Notify parent process that daemon is ready
    let ready_msg = format!("READY {}\n", sock_path.display());
    let _ = nix_write(
        unsafe { BorrowedFd::borrow_raw(pipe_w_raw_fd) },
        ready_msg.as_bytes(),
    );
    let _ = close(pipe_w_raw_fd);

    let c_cmd = CString::new(exec_cmd.as_str())?;
    let c_args: Vec<CString> = exec_args
        .iter()
        .map(|s| CString::new(s.as_str()).unwrap_or_default())
        .collect();

    match unsafe { fork() }? {
        Child => {
            drop(master_fd);
            let _ = setsid();
            unsafe {
                libc::ioctl(slave_fd.as_raw_fd(), libc::TIOCSCTTY as _, 0);
                let slave_raw = slave_fd.as_raw_fd();
                libc::dup2(slave_raw, 0);
                libc::dup2(slave_raw, 1);
                libc::dup2(slave_raw, 2);
            }
            drop(slave_fd);

            let session_name = crate::ipc::parse_name_from_socket_path(&sock_path);
            unsafe {
                std::env::set_var(DEFAULT_SESSION_VAR, &session_name);
                std::env::set_var("TTYMAN_PID", own_pid.to_string());
            }

            let _ = execvp(&c_cmd, &c_args);
            std::process::exit(127);
        }
        Parent { child } => {
            drop(slave_fd);
            shared_state
                .child_pid
                .store(child.as_raw(), Ordering::SeqCst);

            let master_file = std::fs::File::from(master_fd);
            let async_master = AsyncFd::new(master_file)?;
            let mut out_buf = [0u8; 8192];
            let mut sig_chld = signal(SignalKind::child())?;
            let mut sig_term = signal(SignalKind::terminate())?;
            let mut sig_int = signal(SignalKind::interrupt())?;

            loop {
                tokio::select! {
                    _ = sig_chld.recv() => {
                        use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
                        if let Ok(WaitStatus::Exited(_, _)) | Ok(WaitStatus::Signaled(_, _, _)) = waitpid(child, Some(WaitPidFlag::WNOHANG)) {
                            break;
                        }
                    }
                    guard = async_master.readable() => {
                        match guard {
                            Ok(mut ready_guard) => {
                                let res = ready_guard.try_io(|inner| {
                                    nix_read(inner.get_ref(), &mut out_buf)
                                        .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
                                });
                                match res {
                                    Ok(Ok(0)) => break,
                                    Ok(Ok(n)) => {
                                        terminal.process(&out_buf[..n]);
                                        let _ = broadcast_tx.send(out_buf[..n].to_vec());
                                    }
                                    Ok(Err(_e)) => break,
                                    Err(_) => {}
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    Some(inject_bytes) = inject_rx.recv() => {
                        let _ = nix_write(async_master.get_ref(), &inject_bytes);
                    }
                    _ = sig_term.recv() => {
                        let _ = kill(child, Signal::SIGTERM);
                        break;
                    }
                    _ = sig_int.recv() => {
                        let _ = kill(child, Signal::SIGINT);
                        break;
                    }
                }
            }

            let final_sock = shared_state.current_sock_path.read().unwrap().clone();
            let _ = std::fs::remove_file(&final_sock);
            let _ = kill(child, Signal::SIGTERM);
            let _ = waitpid(child, None);
        }
    }

    Ok(())
}

enum AttachOutcome {
    Detached,
    Terminated,
    SwitchTo(PathBuf),
}

struct MenuConfig<'a> {
    key: &'a str,
    command: &'a str,
}

async fn attach_single_session(
    sock_path: &PathBuf,
    history_stack: &[PathBuf],
    args: &AttachArgs,
    menu: &MenuConfig<'_>,
    mut remapper: Option<InputRemapper>,
    is_stdin_tty: bool,
    raw_guard: &mut Option<RawGuard>,
) -> anyhow::Result<AttachOutcome> {
    let mut detector = MenuDetector::from_config(menu.key)?;

    let stream = UnixStream::connect(sock_path).await.map_err(|e| {
        anyhow::anyhow!(
            "Failed to connect to ttyman session at {}: {e}",
            sock_path.display()
        )
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
                                let (to_forward, triggered) = detector.process(&in_buf[..n]);
                                if !to_forward.is_empty() {
                                    let remapped = if let Some(ref mut rm) = remapper {
                                        rm.translate(&to_forward)
                                    } else {
                                        to_forward
                                    };
                                    if !remapped.is_empty() {
                                        if writer.write_all(&remapped).await.is_err() {
                                            return Ok(AttachOutcome::Terminated);
                                        }
                                        let _ = writer.flush().await;
                                    }
                                }
                                if triggered {
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
                                        } else if let Some(target) = trimmed.strip_prefix("attach:").or_else(|| trimmed.strip_prefix("switch:")) {
                                            let target_name = target.trim();
                                            if !target_name.is_empty() {
                                                let target_sock = resolve_socket_path(Some(target_name))?;
                                                return Ok(AttachOutcome::SwitchTo(target_sock));
                                            }
                                        } else if !trimmed.is_empty() && !trimmed.starts_with('{') {
                                            // fallback: plain session name
                                            let target_sock = resolve_socket_path(Some(&trimmed))?;
                                            return Ok(AttachOutcome::SwitchTo(target_sock));
                                        }
                                    }

                                    // If cancelled or failed, fetch fresh screen snapshot via independent IPC and redraw local terminal
                                    if let Ok(mut read_stream) = std::os::unix::net::UnixStream::connect(sock_path) {
                                        use std::io::{BufRead, BufReader, Write};
                                        let req = IpcRequest::Redraw;
                                        if let Ok(mut r_str) = serde_json::to_string(&req) {
                                            r_str.push('\n');
                                            let _ = read_stream.write_all(r_str.as_bytes());
                                            let _ = read_stream.flush();
                                            let mut reader = BufReader::new(read_stream);
                                            let mut line = String::new();
                                            if reader.read_line(&mut line).is_ok()
                                                && let Ok(crate::ipc::IpcResponse::Ok(redraw_payload)) =
                                                    serde_json::from_str(&line)
                                            {
                                                let _ = async_stdout.write_all(redraw_payload.as_bytes()).await;
                                                let _ = async_stdout.flush().await;
                                            }
                                        }
                                    }
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
                if let Some(ws) = get_terminal_winsize(0).filter(|w| w.ws_row > 0 && w.ws_col > 0)
                    && let Ok(mut resize_stream) = tokio::net::UnixStream::connect(sock_path).await
                {
                    let resize_req = IpcRequest::Resize {
                        cols: ws.ws_col,
                        rows: ws.ws_row,
                    };
                    if let Ok(mut r_str) = serde_json::to_string(&resize_req) {
                        r_str.push('\n');
                        let _ = resize_stream.write_all(r_str.as_bytes()).await;
                        let _ = resize_stream.flush().await;
                    }
                }
            }
        }
    }
}

async fn attach_client(initial_sock_path: PathBuf, args: AttachArgs) -> anyhow::Result<()> {
    let file_cfg = crate::remap::Config::load_default_or_explicit(args.config.as_deref());
    let menu_key = file_cfg
        .as_ref()
        .map(|c| c.menu_key())
        .unwrap_or_else(|| "0x1d".to_string());
    let menu_command = file_cfg
        .as_ref()
        .map(|c| c.menu_command())
        .unwrap_or_else(|| DEFAULT_MENU_COMMAND.to_string());
    let initial_remapper = file_cfg.as_ref().and_then(|c| c.to_remapper());

    let mut raw_guard = Some(RawGuard::enter()?);
    let is_stdin_tty = std::io::stdin().is_terminal();

    let mut history_stack: Vec<PathBuf> = Vec::new();
    let mut current_sock_path = initial_sock_path;
    let menu = MenuConfig {
        key: &menu_key,
        command: &menu_command,
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
            initial_remapper.clone(),
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
                    spawn_daemon_process(&new_sock_path, args.config.as_deref())?;
                }
                current_sock_path = new_sock_path;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_menu_key() {
        assert_eq!(parse_menu_key("0x1d").unwrap(), vec![0x1D]);
        assert_eq!(parse_menu_key("0x1D").unwrap(), vec![0x1D]);
        assert_eq!(parse_menu_key("29").unwrap(), vec![0x1D]);
        assert_eq!(parse_menu_key("0x10, 0x11").unwrap(), vec![0x10, 0x11]);
        assert_eq!(parse_menu_key("16,17").unwrap(), vec![0x10, 0x11]);
        assert_eq!(parse_menu_key("none").unwrap(), Vec::<u8>::new());
        assert_eq!(parse_menu_key("").unwrap(), Vec::<u8>::new());
        assert!(parse_menu_key("invalid").is_err());
        assert!(parse_menu_key("0x999").is_err());
    }

    #[test]
    fn test_menu_detector_single_key() {
        let mut d = MenuDetector::from_config("0x1D").unwrap();
        let (out, trig) = d.process(b"hello");
        assert_eq!(out, b"hello");
        assert!(!trig);

        let (out, trig) = d.process(&[0x1D]);
        assert_eq!(out, Vec::<u8>::new());
        assert!(trig);
    }

    #[test]
    fn test_menu_detector_chord() {
        let mut d = MenuDetector::from_config("0x10,0x11").unwrap();
        let (out, trig) = d.process(b"a");
        assert_eq!(out, b"a");
        assert!(!trig);

        // Send ctrl-p then something else
        let (out, trig) = d.process(&[0x10, b'x']);
        assert_eq!(out, vec![0x10, b'x']);
        assert!(!trig);

        // Send ctrl-p then ctrl-q
        let (out, trig) = d.process(&[0x10, 0x11]);
        assert_eq!(out, Vec::<u8>::new());
        assert!(trig);
    }
}
