use crate::ipc::{
    DEFAULT_SESSION_VAR, IpcRequest, IpcResponse, SessionInfo, bind_unix_listener,
    default_socket_path, named_socket_path, parse_name_from_socket_path, resolve_socket_path,
    validate_session_name,
};
use crate::pty::{exec_in_child_pty, open_pty_pair, set_terminal_winsize};
use crate::terminal::Terminal;
use nix::libc;
use nix::pty::Winsize;
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::waitpid;
use nix::unistd::ForkResult::{Child, Parent};
use nix::unistd::{close, fork, pipe, read as nix_read, setsid, write as nix_write};
use std::io::Read;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

pub struct SessionSharedState {
    pub pid: u32,
    pub started_at_epoch_sec: u64,
    pub command: Vec<String>,
    pub master_raw_fd: RawFd,
    pub child_pid: std::sync::atomic::AtomicI32,
    pub clients: std::sync::atomic::AtomicUsize,
    pub pty_slave_path: Option<String>,
    pub current_sock_path: std::sync::RwLock<PathBuf>,
}

pub fn start_ipc_server(
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
                while let Ok(n) = (&mut buf_reader)
                    .take(MAX_IPC_LINE_BYTES)
                    .read_line(&mut line)
                    .await
                {
                    if n == 0 {
                        break;
                    }
                    if line.len() >= MAX_IPC_LINE_BYTES as usize && !line.ends_with('\n') {
                        let resp =
                            IpcResponse::Error("Request exceeds maximum size limit (16MB)".into());
                        let mut resp_str = serde_json::to_string(&resp).unwrap_or_default();
                        resp_str.push('\n');
                        let _ = writer.write_all(resp_str.as_bytes()).await;
                        break;
                    }
                    let resp = match serde_json::from_str::<IpcRequest>(&line) {
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
                        Ok(req) => handle_ipc_request(req, &state, &terminal, &inject_tx),
                        Err(e) => IpcResponse::Error(e.to_string()),
                    };
                    let mut resp_str = serde_json::to_string(&resp).unwrap_or_default();
                    resp_str.push('\n');
                    if writer.write_all(resp_str.as_bytes()).await.is_err() {
                        break;
                    }
                    line.clear();
                }
            });
        }
    });
}

fn handle_ipc_request(
    req: IpcRequest,
    state: &SessionSharedState,
    terminal: &Terminal,
    inject_tx: &mpsc::UnboundedSender<Vec<u8>>,
) -> IpcResponse {
    match req {
        IpcRequest::Resize { cols, rows } => {
            if cols > 0 && rows > 0 {
                let ws = nix::pty::Winsize {
                    ws_row: rows,
                    ws_col: cols,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };
                set_terminal_winsize(state.master_raw_fd, &ws);
                terminal.set_size(rows, cols);
                let c_pid = state.child_pid.load(std::sync::atomic::Ordering::SeqCst);
                if c_pid > 0 {
                    let _ = kill(nix::unistd::Pid::from_raw(-c_pid), Signal::SIGWINCH);
                }
            }
            IpcResponse::Ok("resized".into())
        }
        IpcRequest::Read {
            lines,
            all,
            with_color,
        } => IpcResponse::Ok(terminal.read(lines, all, with_color)),
        IpcRequest::Redraw => {
            IpcResponse::Ok(String::from_utf8_lossy(&terminal.redraw_payload()).to_string())
        }
        IpcRequest::Write {
            mut text,
            enter,
            bracketed_paste,
        } => {
            if bracketed_paste {
                text = format!("\x1b[200~{text}\x1b[201~");
            }
            if enter {
                text.push('\n');
            }
            let _ = inject_tx.send(text.into_bytes());
            IpcResponse::Ok("sent".into())
        }
        IpcRequest::Info => IpcResponse::Info(SessionInfo {
            pid: state.pid,
            started_at_epoch_sec: state.started_at_epoch_sec,
            command: state.command.clone(),
            term_size: terminal.size(),
            clients: state.clients.load(std::sync::atomic::Ordering::SeqCst),
            pty_slave_path: state.pty_slave_path.clone(),
        }),
        IpcRequest::Rename { new_name } => {
            let new_name = new_name.trim();
            match validate_session_name(new_name).and_then(|_| named_socket_path(new_name)) {
                Err(e) => IpcResponse::Error(e.to_string()),
                Ok(new_path) => {
                    let mut current = state.current_sock_path.write().unwrap();
                    if *current == new_path {
                        IpcResponse::Ok(format!("Session is already named '{new_name}'"))
                    } else if new_path.exists()
                        && std::os::unix::net::UnixStream::connect(&new_path).is_ok()
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
                                IpcResponse::Ok(format!("Renamed session to '{new_name}'"))
                            }
                            Err(e) => IpcResponse::Error(format!("Failed to rename socket: {e}")),
                        }
                    }
                }
            }
        }
        IpcRequest::Ping => IpcResponse::Ok("pong".into()),
        _ => IpcResponse::Error("Unsupported request".into()),
    }
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
    let config_opt = config.map(ToString::to_string);

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
                None => default_socket_path(own_pid)?,
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

pub fn spawn_daemon_process(
    sock_path: Option<&Path>,
    config: Option<&str>,
) -> anyhow::Result<PathBuf> {
    let current_exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(current_exe);
    cmd.arg("start");
    if let Some(p) = sock_path {
        let session_name = parse_name_from_socket_path(p);
        cmd.arg("-s").arg(&session_name);
    }
    if let Some(cfg) = config {
        cmd.arg("-c").arg(cfg);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());

    let output = cmd.output()?;
    if !output.status.success() {
        let display = sock_path
            .map(parse_name_from_socket_path)
            .unwrap_or_else(|| "anonymous".to_string());
        anyhow::bail!("Failed to start session '{display}'");
    }

    if let Some(p) = sock_path {
        Ok(p.to_path_buf())
    } else {
        let stdout_str = String::from_utf8_lossy(&output.stdout);
        if let Some(pos) = stdout_str.find("Started session '") {
            let rest = &stdout_str[pos + 17..];
            if let Some(end) = rest.find('\'') {
                let name = rest[..end].trim();
                return resolve_socket_path(Some(name));
            }
        }
        anyhow::bail!("Failed to parse session name from start output: {stdout_str}");
    }
}

pub enum TargetSession {
    Handled,
    Ready { sock_path: PathBuf, is_alive: bool },
    Unspecified,
}

/// Resolve target session and prevent nested attachment loops.
pub fn prepare_session_target(
    session_arg: Option<&str>,
    config: Option<&str>,
    command: &[String],
    disallow_self_attach: bool,
) -> anyhow::Result<TargetSession> {
    let _ = crate::ipc::get_runtime_dir()?;
    let current_env_session = std::env::var(DEFAULT_SESSION_VAR).ok();
    let current_env_sock = current_env_session
        .as_deref()
        .and_then(|name| resolve_socket_path(Some(name)).ok());
    let target_sock: Option<PathBuf> = match session_arg {
        Some(target) => Some(resolve_socket_path(Some(target))?),
        None => current_env_sock.clone(),
    };

    let Some(sock_path) = target_sock else {
        return Ok(TargetSession::Unspecified);
    };

    let session_display_string;
    let session_display = match session_arg {
        Some(s) => s,
        None => {
            session_display_string = crate::ipc::parse_name_from_socket_path(&sock_path);
            &session_display_string
        }
    };

    let is_inside_session = current_env_sock.is_some();
    let is_self_attach = crate::ipc::is_self_session(&sock_path);
    if disallow_self_attach && is_self_attach {
        anyhow::bail!("Already in session '{session_display}'.");
    }

    let is_alive = crate::ipc::is_socket_alive(&sock_path);

    if handle_nested_session(
        &sock_path,
        session_display,
        is_inside_session,
        is_self_attach,
        is_alive,
        config,
        command,
    )? {
        return Ok(TargetSession::Handled);
    }

    Ok(TargetSession::Ready {
        sock_path,
        is_alive,
    })
}

fn handle_nested_session(
    sock_path: &Path,
    session_display: &str,
    is_inside_session: bool,
    is_self_attach: bool,
    is_alive: bool,
    config: Option<&str>,
    command: &[String],
) -> anyhow::Result<bool> {
    if is_inside_session && !is_self_attach {
        if !is_alive {
            spawn_daemon_supervisor(Some(sock_path.to_path_buf()), config, command)?;
        }
        println!(
            "Cannot attach to session '{session_display}' from inside a ttyman session.\n\
             Press the menu key to detach first, or switch sessions if your menu supports it."
        );
        Ok(true)
    } else {
        Ok(false)
    }
}

async fn run_daemon_server(
    sock_path: PathBuf,
    _config: Option<String>,
    exec_cmd: String,
    exec_args: Vec<String>,
    pipe_w_raw_fd: RawFd,
) -> anyhow::Result<()> {
    let own_pid = std::process::id();
    let listener = bind_unix_listener(&sock_path)?;

    let default_winsize = Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty_res = open_pty_pair(Some(&default_winsize))?;
    let master_fd = pty_res.master;
    let slave_fd = pty_res.slave;

    let file_cfg = crate::config::Config::load_default_or_explicit(_config.as_deref());
    let scrollback = file_cfg
        .as_ref()
        .map(crate::config::Config::scrollback)
        .unwrap_or(10_000);
    let terminal = Terminal::new(24, 80, scrollback);
    let (inject_tx, inject_rx) = mpsc::unbounded_channel::<Vec<u8>>();
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

    match unsafe { fork() }? {
        Child => {
            drop(master_fd);
            let session_name = parse_name_from_socket_path(&sock_path);
            exec_in_child_pty(slave_fd, &session_name, own_pid, &exec_cmd, &exec_args);
        }
        Parent { child } => {
            drop(slave_fd);
            shared_state
                .child_pid
                .store(child.as_raw(), Ordering::SeqCst);

            let master_file = std::fs::File::from(master_fd);
            let async_master = AsyncFd::new(master_file)?;

            let _ = run_pty_event_loop(
                child,
                async_master,
                terminal,
                broadcast_tx,
                inject_rx,
                shared_state,
            )
            .await;

            Ok(())
        }
    }
}

pub async fn run_pty_event_loop(
    child: nix::unistd::Pid,
    async_master: AsyncFd<std::fs::File>,
    terminal: Terminal,
    broadcast_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
    mut inject_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    shared_state: Arc<SessionSharedState>,
) -> anyhow::Result<i32> {
    let mut out_buf = [0u8; 8192];
    let mut sig_chld = signal(SignalKind::child())?;
    let mut sig_term = signal(SignalKind::terminate())?;
    let mut sig_int = signal(SignalKind::interrupt())?;
    let mut sig_hup = signal(SignalKind::hangup())?;

    loop {
        tokio::select! {
            _ = sig_chld.recv() => {
                use nix::sys::wait::{WaitPidFlag, WaitStatus};
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
                let master_file = async_master.get_ref();
                let _ = nix_write(master_file, &inject_bytes);
            }
            _ = sig_term.recv() => {
                let _ = broadcast_tx.send(b"\r\n[ttyman: Session terminated by SIGTERM]\r\n".to_vec());
                let _ = kill(nix::unistd::Pid::from_raw(-child.as_raw()), Signal::SIGTERM);
                let _ = kill(child, Signal::SIGTERM);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                break;
            }
            _ = sig_int.recv() => {
                let _ = broadcast_tx.send(b"\r\n[ttyman: Session interrupted by SIGINT]\r\n".to_vec());
                let _ = kill(nix::unistd::Pid::from_raw(-child.as_raw()), Signal::SIGINT);
                let _ = kill(child, Signal::SIGINT);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                break;
            }
            _ = sig_hup.recv() => {
                let _ = broadcast_tx.send(b"\r\n[ttyman: Session terminated by SIGHUP]\r\n".to_vec());
                let _ = kill(nix::unistd::Pid::from_raw(-child.as_raw()), Signal::SIGHUP);
                let _ = kill(child, Signal::SIGHUP);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                break;
            }
        }
    }

    let current_path = shared_state.current_sock_path.read().unwrap().clone();
    if current_path.exists() {
        let _ = std::fs::remove_file(current_path);
    }

    use nix::sys::wait::{WaitPidFlag, WaitStatus};
    let _ = kill(nix::unistd::Pid::from_raw(-child.as_raw()), Signal::SIGHUP);
    let _ = kill(child, Signal::SIGHUP);
    let _ = kill(nix::unistd::Pid::from_raw(-child.as_raw()), Signal::SIGTERM);
    let _ = kill(child, Signal::SIGTERM);
    drop(async_master);

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    let mut exit_code = 0;
    while std::time::Instant::now() < deadline {
        match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, code)) => {
                exit_code = code;
                break;
            }
            Ok(WaitStatus::Signaled(_, sig, _)) => {
                exit_code = 128 + sig as i32;
                break;
            }
            _ => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if kill(child, None).is_ok() {
        let _ = kill(nix::unistd::Pid::from_raw(-child.as_raw()), Signal::SIGKILL);
        let _ = kill(child, Signal::SIGKILL);
        match waitpid(child, None) {
            Ok(WaitStatus::Exited(_, code)) => {
                exit_code = code;
            }
            Ok(WaitStatus::Signaled(_, sig, _)) => {
                exit_code = 128 + sig as i32;
            }
            _ => {}
        }
    }
    Ok(exit_code)
}
