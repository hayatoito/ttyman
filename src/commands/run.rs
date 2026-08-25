use crate::ipc::SessionInfo;
use crate::pty::{
    RawGuard, StdinRawFd, get_parent_termios, get_terminal_winsize, open_pty_pair,
    set_terminal_winsize,
};
use crate::terminal::Terminal;
use crate::{DEFAULT_SESSION_VAR, IpcRequest, IpcResponse, default_socket_path};
use clap::Args;
use nix::libc;
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{ForkResult, fork, read as nix_read, setsid, write as nix_write};
use std::io::IsTerminal;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    /// Target session name
    #[arg(short = 's', long = "session", value_name = "SESSION")]
    pub session: Option<String>,

    /// Path to TOML configuration file (e.g. input remapping)
    #[arg(short = 'c', long = "config")]
    pub config: Option<String>,

    /// Terminal mode: 'never', 'auto', or 'always'
    #[arg(short = 'T', long = "term", default_value = "auto")]
    pub term_mode: String,

    /// Command to execute (defaults to interactive $SHELL)
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

fn determine_socket_path(socket_arg: Option<&str>, pid: u32) -> anyhow::Result<std::path::PathBuf> {
    if let Some(target) = socket_arg {
        let path = crate::ipc::resolve_socket_path(Some(target))?;
        if path.exists() {
            if std::os::unix::net::UnixStream::connect(&path).is_ok() {
                anyhow::bail!(
                    "Cannot start ttyman session: socket '{}' is already in use by an active session",
                    path.display()
                );
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
        Ok(path)
    } else {
        default_socket_path(pid)
    }
}

pub(crate) struct SessionSharedState {
    pub pid: u32,
    pub started_at_epoch_sec: u64,
    pub command: Vec<String>,
    pub master_raw_fd: std::os::fd::RawFd,
    pub child_pid: std::sync::atomic::AtomicI32,
    pub persist: bool,
    pub clients: std::sync::atomic::AtomicUsize,
    pub pty_slave_path: Option<String>,
    pub current_sock_path: std::sync::RwLock<std::path::PathBuf>,
}

pub(crate) fn start_ipc_server(
    listener: UnixListener,
    terminal: Terminal,
    inject_tx: mpsc::UnboundedSender<Vec<u8>>,
    state: Arc<SessionSharedState>,
    broadcast_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
) {
    let my_uid = nix::unistd::geteuid().as_raw();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            // Verify peer credentials (SO_PEERCRED on Linux)
            if let Ok(cred) = stream.peer_cred() {
                if cred.uid() != my_uid {
                    // Reject connection from unauthorized user
                    continue;
                }
            } else {
                continue;
            }

            let terminal = terminal.clone();
            let inject_tx = inject_tx.clone();
            let state = state.clone();
            let broadcast_tx = broadcast_tx.clone();
            tokio::spawn(async move {
                let (reader, mut writer) = stream.into_split();
                let mut buf_reader = BufReader::new(reader);
                let mut line = String::new();
                const MAX_IPC_LINE_BYTES: u64 = 16 * 1024 * 1024; // 16 MB
                while let Ok(n) = (&mut buf_reader).take(MAX_IPC_LINE_BYTES).read_line(&mut line).await {
                    if n == 0 {
                        break;
                    }
                    if line.len() >= MAX_IPC_LINE_BYTES as usize && !line.ends_with('\n') {
                        let resp = IpcResponse::Error("Request exceeds maximum size limit (16MB)".into());
                        let mut resp_str = serde_json::to_string(&resp).unwrap_or_default();
                        resp_str.push('\n');
                        let _ = writer.write_all(resp_str.as_bytes()).await;
                        break;
                    }
                    match serde_json::from_str::<IpcRequest>(&line) {
                        Ok(IpcRequest::Subscribe) => {
                            let mut sub_rx = broadcast_tx.subscribe();
                            let text = terminal.read(None, false, true);
                            let initial_payload = format!("\x1b[2J\x1b[H{text}");
                            if writer.write_all(initial_payload.as_bytes()).await.is_err() {
                                return;
                            }
                            let _ = writer.flush().await;
                            while let Ok(chunk) = sub_rx.recv().await {
                                if writer.write_all(&chunk).await.is_err() {
                                    break;
                                }
                                let _ = writer.flush().await;
                            }
                            return;
                        }
                        Ok(IpcRequest::Attach { cols, rows }) => {
                            state
                                .clients
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                            struct ClientGuard(Arc<SessionSharedState>);
                            impl Drop for ClientGuard {
                                fn drop(&mut self) {
                                    self.0
                                        .clients
                                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                                }
                            }
                            let _client_guard = ClientGuard(state.clone());

                            if let (Some(c), Some(r)) = (cols, rows)
                                && c > 0
                                && r > 0
                            {
                                let ws = nix::pty::Winsize {
                                    ws_row: r,
                                    ws_col: c,
                                    ws_xpixel: 0,
                                    ws_ypixel: 0,
                                };
                                set_terminal_winsize(state.master_raw_fd, &ws);
                                terminal.set_size(r, c);
                                let c_pid =
                                    state.child_pid.load(std::sync::atomic::Ordering::SeqCst);
                                if c_pid > 0 {
                                    let _ =
                                        kill(nix::unistd::Pid::from_raw(-c_pid), Signal::SIGWINCH);
                                }
                            }
                            let initial_payload = terminal.redraw_payload();
                            if writer.write_all(&initial_payload).await.is_err() {
                                return;
                            }
                            let _ = writer.flush().await;

                            let mut sub_rx = broadcast_tx.subscribe();
                            tokio::select! {
                                _ = async {
                                    while let Ok(chunk) = sub_rx.recv().await {
                                        if writer.write_all(&chunk).await.is_err() {
                                            break;
                                        }
                                        let _ = writer.flush().await;
                                    }
                                } => {},
                                _ = async {
                                    let mut client_in = [0u8; 1024];
                                    while let Ok(n) = buf_reader.read(&mut client_in).await {
                                        if n == 0 {
                                            break;
                                        }
                                        let bytes = client_in[..n].to_vec();
                                        if inject_tx.send(bytes).is_err() {
                                            break;
                                        }
                                    }
                                } => {},
                            }
                            return;
                        }
                        Ok(IpcRequest::Resize { cols, rows }) => {
                            if cols > 0 && rows > 0 {
                                let ws = nix::pty::Winsize {
                                    ws_row: rows,
                                    ws_col: cols,
                                    ws_xpixel: 0,
                                    ws_ypixel: 0,
                                };
                                set_terminal_winsize(state.master_raw_fd, &ws);
                                terminal.set_size(rows, cols);
                                let c_pid =
                                    state.child_pid.load(std::sync::atomic::Ordering::SeqCst);
                                if c_pid > 0 {
                                    let _ =
                                        kill(nix::unistd::Pid::from_raw(-c_pid), Signal::SIGWINCH);
                                }
                            }
                            let mut resp_str =
                                serde_json::to_string(&IpcResponse::Ok("resized".into()))
                                    .unwrap_or_default();
                            resp_str.push('\n');
                            if writer.write_all(resp_str.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                        Ok(IpcRequest::Read {
                            lines,
                            all,
                            with_color,
                        }) => {
                            let text = terminal.read(lines, all, with_color);
                            let mut resp_str =
                                serde_json::to_string(&IpcResponse::Ok(text)).unwrap_or_default();
                            resp_str.push('\n');
                            if writer.write_all(resp_str.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                        Ok(IpcRequest::Redraw) => {
                            let payload = terminal.redraw_payload();
                            let text = String::from_utf8_lossy(&payload).to_string();
                            let mut resp_str =
                                serde_json::to_string(&IpcResponse::Ok(text)).unwrap_or_default();
                            resp_str.push('\n');
                            if writer.write_all(resp_str.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                        Ok(IpcRequest::Write {
                            text,
                            enter,
                            bracketed_paste,
                        }) => {
                            let mut payload = text;
                            if bracketed_paste {
                                payload = format!("\x1b[200~{payload}\x1b[201~");
                            }
                            if enter {
                                payload.push('\n');
                            }
                            let _ = inject_tx.send(payload.into_bytes());
                            let mut resp_str =
                                serde_json::to_string(&IpcResponse::Ok("sent".into()))
                                    .unwrap_or_default();
                            resp_str.push('\n');
                            if writer.write_all(resp_str.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                        Ok(IpcRequest::Info) => {
                            let term_size = terminal.size();
                            let resp = IpcResponse::Info(SessionInfo {
                                pid: state.pid,
                                started_at_epoch_sec: state.started_at_epoch_sec,
                                command: state.command.clone(),
                                term_size,
                                persist: state.persist,
                                clients: state.clients.load(std::sync::atomic::Ordering::SeqCst),
                                pty_slave_path: state.pty_slave_path.clone(),
                            });
                            let mut resp_str = serde_json::to_string(&resp).unwrap_or_default();
                            resp_str.push('\n');
                            if writer.write_all(resp_str.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                        Ok(IpcRequest::Rename { new_name }) => {
                            let new_name = new_name.trim();
                            let resp = match crate::ipc::validate_session_name(new_name) {
                                Err(e) => IpcResponse::Error(e.to_string()),
                                Ok(()) => match crate::ipc::named_socket_path(new_name) {
                                    Err(e) => IpcResponse::Error(e.to_string()),
                                    Ok(new_path) => {
                                        let mut current = state.current_sock_path.write().unwrap();
                                        if *current == new_path {
                                            IpcResponse::Ok(format!(
                                                "Session is already named '{new_name}'"
                                            ))
                                        } else if new_path.exists()
                                            && std::os::unix::net::UnixStream::connect(&new_path)
                                                .is_ok()
                                        {
                                            IpcResponse::Error(format!(
                                                "Cannot rename session: target session '{new_name}' already exists and is active"
                                            ))
                                        } else {
                                            if new_path.exists() {
                                                let _ = std::fs::remove_file(&new_path);
                                            }
                                            match std::fs::rename(&*current, &new_path) {
                                                Ok(()) => {
                                                    *current = new_path.clone();
                                                    IpcResponse::Ok(format!(
                                                        "Renamed session to '{new_name}'"
                                                    ))
                                                }
                                                Err(e) => IpcResponse::Error(format!(
                                                    "Failed to rename socket: {e}"
                                                )),
                                            }
                                        }
                                    }
                                },
                            };
                            let mut resp_str = serde_json::to_string(&resp).unwrap_or_default();
                            resp_str.push('\n');
                            if writer.write_all(resp_str.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                        Ok(IpcRequest::Ping) => {
                            let mut resp_str =
                                serde_json::to_string(&IpcResponse::Ok("pong".into()))
                                    .unwrap_or_default();
                            resp_str.push('\n');
                            if writer.write_all(resp_str.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            let mut resp_str =
                                serde_json::to_string(&IpcResponse::Error(e.to_string()))
                                    .unwrap_or_default();
                            resp_str.push('\n');
                            if writer.write_all(resp_str.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                    }
                    line.clear();
                }
            });
        }
    });
}

pub async fn run(cli: RunArgs) -> anyhow::Result<()> {
    let is_stdin_tty = std::io::stdin().is_terminal();
    let use_tty = match cli.term_mode.as_str() {
        "never" => false,
        "always" => true,
        _ => is_stdin_tty,
    };

    let default_shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let (exec_cmd, exec_args) = if !cli.command.is_empty() {
        (cli.command[0].clone(), cli.command.clone())
    } else {
        (
            default_shell.clone(),
            vec![default_shell.clone(), "-i".to_string()],
        )
    };

    if use_tty {
        run_pty_session(cli, exec_cmd, exec_args).await?;
    } else {
        run_pipe_session(cli, exec_cmd, exec_args).await?;
    }

    Ok(())
}

async fn run_pty_session(
    cli: RunArgs,
    exec_cmd: String,
    exec_args: Vec<String>,
) -> anyhow::Result<()> {
    let parent_termios = get_parent_termios();
    let winsize = get_terminal_winsize(0);

    let pty_res = open_pty_pair(winsize.as_ref(), parent_termios.as_ref())?;
    let master_fd = pty_res.master;
    let slave_fd = pty_res.slave;

    let _raw_guard = RawGuard::enter()?;
    let async_stdin = AsyncFd::new(StdinRawFd(0))?;

    let own_pid = std::process::id();
    let sock_path = determine_socket_path(cli.session.as_deref(), own_pid)?;
    let listener = crate::ipc::bind_unix_listener(&sock_path)?;

    let rows = winsize.map(|w| w.ws_row).filter(|&r| r > 0).unwrap_or(24);
    let cols = winsize.map(|w| w.ws_col).filter(|&c| c > 0).unwrap_or(80);
    let file_cfg = crate::remap::Config::load_default_or_explicit(cli.config.as_deref());
    let scrollback = file_cfg.as_ref().map(|c| c.scrollback()).unwrap_or(10_000);
    let terminal = Terminal::new(rows, cols, scrollback);

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
        persist: false,
        clients: std::sync::atomic::AtomicUsize::new(1),
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

    match unsafe { fork() }? {
        ForkResult::Child => {
            drop(master_fd);
            let _ = setsid();
            unsafe {
                libc::ioctl(slave_fd.as_raw_fd(), libc::TIOCSCTTY as _, 0);
            }
            let slave_raw = slave_fd.as_raw_fd();
            unsafe {
                libc::dup2(slave_raw, 0);
                libc::dup2(slave_raw, 1);
                libc::dup2(slave_raw, 2);
            }
            if slave_raw > 2 {
                drop(slave_fd);
            }

            let session_name = crate::ipc::parse_name_from_socket_path(&sock_path);
            // Export environment variables for child processes (safe in single-threaded forked child)
            unsafe {
                std::env::set_var(DEFAULT_SESSION_VAR, &session_name);
                std::env::set_var("TTYMAN_PID", own_pid.to_string());
            }

            let c_cmd = std::ffi::CString::new(exec_cmd)?;
            let c_args: Vec<std::ffi::CString> = exec_args
                .iter()
                .map(|a| std::ffi::CString::new(a.as_str()).unwrap())
                .collect();
            let _ = nix::unistd::execvp(&c_cmd, &c_args);
            std::process::exit(127);
        }
        ForkResult::Parent { child } => {
            shared_state
                .child_pid
                .store(child.as_raw(), std::sync::atomic::Ordering::SeqCst);
            drop(slave_fd);
            let master_file = std::fs::File::from(master_fd);
            let async_master = AsyncFd::new(master_file)?;

            let mut async_stdout = tokio::io::stdout();

            let mut sig_chld = signal(SignalKind::child())?;
            let mut sig_winch = signal(SignalKind::window_change())?;
            let mut sig_term = signal(SignalKind::terminate())?;
            let mut sig_hup = signal(SignalKind::hangup())?;

            let mut remapper =
                crate::remap::Config::load_default_or_explicit(cli.config.as_deref())
                    .and_then(|c| c.to_remapper());

            let mut in_buf = [0u8; 4096];
            let mut out_buf = [0u8; 4096];
            let mut stdin_open = true;

            loop {
                tokio::select! {
                    _ = sig_chld.recv() => {
                        if let Ok(WaitStatus::Exited(_, _)) | Ok(WaitStatus::Signaled(_, _, _)) = waitpid(child, Some(WaitPidFlag::WNOHANG)) {
                            break;
                        }
                    }
                    guard = async_master.readable() => {
                        match guard {
                            Ok(mut ready_guard) => {
                                let res = ready_guard.try_io(|inner| {
                                    nix_read(inner, &mut out_buf)
                                        .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
                                });
                                match res {
                                    Ok(Ok(0)) => break,
                                    Ok(Ok(n)) => {
                                        let _ = async_stdout.write_all(&out_buf[..n]).await;
                                        let _ = async_stdout.flush().await;
                                        terminal.process(&out_buf[..n]);
                                        let _ = broadcast_tx.send(out_buf[..n].to_vec());
                                    }
                                    Ok(Err(_e)) => {
                                        // On Linux PTY, EIO (errno 5) is returned when slave closes (child exited)
                                        break;
                                    }
                                    Err(_would_block) => {}
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    Some(inject_bytes) = inject_rx.recv() => {
                        let _ = nix_write(async_master.get_ref(), &inject_bytes);
                    }
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
                                        let to_send: Vec<u8> = if let Some(ref mut km) = remapper {
                                            km.translate(&in_buf[..n])
                                        } else {
                                            in_buf[..n].to_vec()
                                        };
                                        if !to_send.is_empty() {
                                            let _ = nix_write(async_master.get_ref(), &to_send);
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
                    _ = sig_winch.recv() => {
                        if let Some(ws) = get_terminal_winsize(0).filter(|w| w.ws_row > 0 && w.ws_col > 0) {
                            set_terminal_winsize(async_master.get_ref().as_raw_fd(), &ws);
                            terminal.set_size(ws.ws_row, ws.ws_col);
                        }
                    }
                    _ = sig_term.recv() => {
                        let _ = kill(child, Signal::SIGTERM);
                        break;
                    }
                    _ = sig_hup.recv() => {
                        let _ = kill(child, Signal::SIGHUP);
                        break;
                    }
                }
            }

            let final_sock = shared_state.current_sock_path.read().unwrap().clone();
            let _ = std::fs::remove_file(&final_sock);
            let _ = std::fs::remove_file(&sock_path);
            match waitpid(child, None) {
                Ok(WaitStatus::Exited(_, code)) => {
                    std::process::exit(code);
                }
                Ok(WaitStatus::Signaled(_, sig, _)) => {
                    std::process::exit(128 + sig as i32);
                }
                _ => {}
            }
        }
    }

    Ok(())
}

async fn run_pipe_session(
    cli: RunArgs,
    exec_cmd: String,
    exec_args: Vec<String>,
) -> anyhow::Result<()> {
    let own_pid = std::process::id();
    let sock_path = determine_socket_path(cli.session.as_deref(), own_pid)?;
    let listener = crate::ipc::bind_unix_listener(&sock_path)?;

    let file_cfg = crate::remap::Config::load_default_or_explicit(cli.config.as_deref());
    let scrollback = file_cfg.as_ref().map(|c| c.scrollback()).unwrap_or(10_000);
    let terminal = Terminal::new(24, 80, scrollback);
    let (inject_tx, mut inject_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (broadcast_tx, _) = tokio::sync::broadcast::channel::<Vec<u8>>(1024);

    let shared_state = Arc::new(SessionSharedState {
        pid: own_pid,
        started_at_epoch_sec: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        command: exec_args.clone(),
        master_raw_fd: -1,
        child_pid: std::sync::atomic::AtomicI32::new(0),
        persist: false,
        clients: std::sync::atomic::AtomicUsize::new(1),
        pty_slave_path: None,
        current_sock_path: std::sync::RwLock::new(sock_path.clone()),
    });
    start_ipc_server(
        listener,
        terminal.clone(),
        inject_tx,
        shared_state.clone(),
        broadcast_tx.clone(),
    );

    let session_name = crate::ipc::parse_name_from_socket_path(&sock_path);
    let mut child = tokio::process::Command::new(&exec_cmd)
        .args(&exec_args[1..])
        .env(DEFAULT_SESSION_VAR, &session_name)
        .env("TTYMAN_PID", own_pid.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    if let Some(c_id) = child.id() {
        shared_state
            .child_pid
            .store(c_id as i32, std::sync::atomic::Ordering::SeqCst);
    }

    let mut child_stdin = child.stdin.take();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut out_buf = [0u8; 4096];
    let mut err_buf = [0u8; 4096];

    let mut real_stdout = tokio::io::stdout();
    let mut real_stderr = tokio::io::stderr();

    let mut stdout_open = true;
    let mut stderr_open = true;

    let mut sig_term = signal(SignalKind::terminate())?;

    while stdout_open || stderr_open {
        tokio::select! {
            Some(inject_bytes) = inject_rx.recv() => {
                if let Some(ref mut stdin) = child_stdin {
                    let _ = stdin.write_all(&inject_bytes).await;
                    let _ = stdin.flush().await;
                }
            }
            res = stdout.read(&mut out_buf), if stdout_open => {
                match res {
                    Ok(0) => {
                        stdout_open = false;
                    }
                    Ok(n) => {
                        let _ = real_stdout.write_all(&out_buf[..n]).await;
                        let _ = real_stdout.flush().await;
                        terminal.process(&out_buf[..n]);
                        let _ = broadcast_tx.send(out_buf[..n].to_vec());
                    }
                    Err(_) => {
                        stdout_open = false;
                    }
                }
            }
            res = stderr.read(&mut err_buf), if stderr_open => {
                match res {
                    Ok(0) => {
                        stderr_open = false;
                    }
                    Ok(n) => {
                        let _ = real_stderr.write_all(&err_buf[..n]).await;
                        let _ = real_stderr.flush().await;
                        terminal.process(&err_buf[..n]);
                        let _ = broadcast_tx.send(err_buf[..n].to_vec());
                    }
                    Err(_) => {
                        stderr_open = false;
                    }
                }
            }
            _ = sig_term.recv() => {
                let _ = child.kill().await;
                break;
            }
        }
    }

    let status = child.wait().await?;
    let final_sock = shared_state.current_sock_path.read().unwrap().clone();
    let _ = std::fs::remove_file(&final_sock);
    let _ = std::fs::remove_file(&sock_path);

    if let Some(code) = status.code() {
        std::process::exit(code);
    }

    Ok(())
}
