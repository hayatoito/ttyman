use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_SESSION_VAR: &str = "TTYMAN_SESSION";

pub fn validate_session_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("Session name cannot be empty");
    }
    if name.len() > 64 {
        anyhow::bail!(
            "Session name '{name}' is too long (max 64 characters, got {})",
            name.len()
        );
    }
    if name.starts_with('-') {
        anyhow::bail!("Session name '{name}' cannot start with a hyphen '-'");
    }
    if name == "." || name == ".." {
        anyhow::bail!("Session name '{name}' is reserved and cannot be '.' or '..'");
    }
    if name.ends_with(".sock") {
        anyhow::bail!("Session name '{name}' cannot end with '.sock'");
    }
    for c in name.chars() {
        if !c.is_ascii_alphanumeric() && c != '_' && c != '-' && c != '.' {
            anyhow::bail!(
                "Invalid session name '{name}': only ASCII alphanumeric characters, '_', '-', and '.' are allowed"
            );
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionInfo {
    pub pid: u32,
    pub started_at_epoch_sec: u64,
    pub command: Vec<String>,
    pub term_size: (u16, u16),
    #[serde(default)]
    pub persist: bool,
    #[serde(default)]
    pub clients: usize,
    #[serde(default)]
    pub pty_slave_path: Option<String>,
}

pub fn get_current_tty_name() -> Option<String> {
    use std::os::fd::BorrowedFd;
    nix::unistd::ttyname(unsafe { BorrowedFd::borrow_raw(0) })
        .or_else(|_| nix::unistd::ttyname(unsafe { BorrowedFd::borrow_raw(1) }))
        .or_else(|_| nix::unistd::ttyname(unsafe { BorrowedFd::borrow_raw(2) }))
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

pub fn query_session_info<P: AsRef<std::path::Path>>(sock_path: P) -> anyhow::Result<SessionInfo> {
    use std::io::{BufRead, BufReader, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(sock_path)?;
    let req = IpcRequest::Info;
    let mut req_str = serde_json::to_string(&req)?;
    req_str.push('\n');
    stream.write_all(req_str.as_bytes())?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    match serde_json::from_str::<IpcResponse>(&line) {
        Ok(IpcResponse::Info(info)) => Ok(info),
        Ok(IpcResponse::Error(e)) => anyhow::bail!("{e}"),
        _ => anyhow::bail!("Invalid IPC response for Info"),
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum IpcRequest {
    Read {
        lines: Option<usize>,
        all: bool,
        with_color: bool,
    },
    Redraw,
    Write {
        text: String,
        #[serde(default)]
        enter: bool,
        #[serde(default)]
        bracketed_paste: bool,
    },
    Subscribe,
    Attach {
        #[serde(default)]
        cols: Option<u16>,
        #[serde(default)]
        rows: Option<u16>,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Rename {
        new_name: String,
    },
    Info,
    Ping,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "status", content = "data")]
pub enum IpcResponse {
    Ok(String),
    Info(SessionInfo),
    Error(String),
}

pub fn parse_name_from_socket_path(path: &Path) -> String {
    if let Some(filename) = path.file_name().and_then(|n| n.to_str())
        && let Some(s) = filename.strip_prefix("ttyman-")
    {
        return s.to_string();
    }
    path.to_string_lossy().to_string()
}

pub fn get_runtime_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR")
        && !dir.trim().is_empty()
    {
        return Ok(PathBuf::from(dir.trim()));
    }

    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let uid = nix::unistd::geteuid().as_raw();
    let fallback_dir = PathBuf::from(format!("/tmp/ttyman-{uid}"));
    if !fallback_dir.exists() {
        std::fs::create_dir_all(&fallback_dir)?;
        let _ = std::fs::set_permissions(&fallback_dir, std::fs::Permissions::from_mode(0o700));
    } else {
        let meta = std::fs::symlink_metadata(&fallback_dir)?;
        if meta.file_type().is_symlink() {
            anyhow::bail!("Security error: '{fallback_dir:?}' is a symlink");
        }
        if !meta.is_dir() {
            anyhow::bail!("Security error: '{fallback_dir:?}' is not a directory");
        }
        if meta.uid() != uid {
            anyhow::bail!(
                "Security error: '{fallback_dir:?}' is owned by UID {}, expected UID {uid}",
                meta.uid()
            );
        }
        let _ = std::fs::set_permissions(&fallback_dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(fallback_dir)
}

pub fn default_socket_path(pid: u32) -> anyhow::Result<PathBuf> {
    Ok(get_runtime_dir()?.join(format!("ttyman-{pid}")))
}

pub fn named_socket_path(name: &str) -> anyhow::Result<PathBuf> {
    Ok(get_runtime_dir()?.join(format!("ttyman-{name}")))
}

pub fn resolve_socket_path(target: Option<&str>) -> anyhow::Result<PathBuf> {
    let uid = nix::unistd::geteuid().as_raw();
    let fallback_dir = PathBuf::from(format!("/tmp/ttyman-{uid}"));

    if let Some(s) = target {
        validate_session_name(s)?;
        if let Ok(pid) = s.parse::<u32>() {
            let named = named_socket_path(s)?;
            if named.exists() {
                return Ok(named);
            }
            let def = default_socket_path(pid)?;
            if def.exists() {
                return Ok(def);
            }
            let fb_named = fallback_dir.join(format!("ttyman-{s}"));
            if fb_named.exists() {
                return Ok(fb_named);
            }
            let fb_def = fallback_dir.join(format!("ttyman-{pid}"));
            if fb_def.exists() {
                return Ok(fb_def);
            }
            return Ok(named);
        }
        let named = named_socket_path(s)?;
        if named.exists() {
            return Ok(named);
        }
        let fb_named = fallback_dir.join(format!("ttyman-{s}"));
        if fb_named.exists() {
            return Ok(fb_named);
        }
        return Ok(named);
    }
    if let Ok(s) = std::env::var(DEFAULT_SESSION_VAR)
        && let Ok(()) = validate_session_name(&s)
    {
        let p = if let Ok(pid) = s.parse::<u32>() {
            let named = named_socket_path(&s)?;
            if named.exists() {
                named
            } else {
                let def = default_socket_path(pid)?;
                if def.exists() {
                    def
                } else {
                    let fb_named = fallback_dir.join(format!("ttyman-{s}"));
                    if fb_named.exists() {
                        fb_named
                    } else {
                        let fb_def = fallback_dir.join(format!("ttyman-{pid}"));
                        if fb_def.exists() {
                            fb_def
                        } else {
                            named
                        }
                    }
                }
            }
        } else {
            let named = named_socket_path(&s)?;
            if named.exists() {
                named
            } else {
                let fb_named = fallback_dir.join(format!("ttyman-{s}"));
                if fb_named.exists() {
                    fb_named
                } else {
                    named
                }
            }
        };
        if p.exists() {
            return Ok(p);
        }
    }
    // Fallback: If $TTYMAN_SESSION is missing or stale (e.g. after rename),
    // find the active session socket associated with $TTYMAN_PID.
    if let Ok(pid_str) = std::env::var("TTYMAN_PID")
        && let Ok(pid) = pid_str.parse::<u32>()
    {
        for sock in find_socket_files() {
            if let Ok(info) = query_session_info(&sock)
                && info.pid == pid
            {
                return Ok(sock);
            }
        }
    }
    let _ = get_runtime_dir()?;

    anyhow::bail!(
        "Not running inside a 'ttyman' session ($TTYMAN_SESSION environment variable not found). Use -s, --session <SESSION>."
    )
}

pub fn find_socket_files() -> Vec<PathBuf> {
    let mut search_dirs = Vec::new();
    if let Ok(runtime_dir) = get_runtime_dir() {
        search_dirs.push(runtime_dir);
    }
    let uid = nix::unistd::geteuid().as_raw();
    let fallback_dir = PathBuf::from(format!("/tmp/ttyman-{uid}"));
    if fallback_dir.exists() && !search_dirs.contains(&fallback_dir) {
        search_dirs.push(fallback_dir);
    }

    let mut found_sockets = Vec::new();
    let mut seen_canonical = std::collections::HashSet::new();

    for dir in search_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str())
                    && name.starts_with("ttyman-")
                {
                    let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
                    if seen_canonical.insert(canonical) {
                        found_sockets.push(path);
                    }
                }
            }
        }
    }

    found_sockets.sort();
    found_sockets
}

pub fn bind_unix_listener(path: &Path) -> anyhow::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;

    let _ = std::fs::remove_file(path);
    let prev_mask = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let bind_res = tokio::net::UnixListener::bind(path);
    let _ = nix::sys::stat::umask(prev_mask);
    let listener = bind_res?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ipc_serialization() {
        let req = IpcRequest::Read {
            lines: Some(50),
            all: false,
            with_color: true,
        };
        let json = serde_json::to_string(&req).unwrap();
        let de: IpcRequest = serde_json::from_str(&json).unwrap();
        match de {
            IpcRequest::Read {
                lines,
                all,
                with_color,
            } => {
                assert_eq!(lines, Some(50));
                assert!(!all);
                assert!(with_color);
            }
            _ => panic!("unexpected request"),
        }

        let redraw_req = IpcRequest::Redraw;
        let json = serde_json::to_string(&redraw_req).unwrap();
        let de: IpcRequest = serde_json::from_str(&json).unwrap();
        match de {
            IpcRequest::Redraw => {}
            _ => panic!("unexpected request"),
        }
    }

    #[test]
    fn test_write_serialization() {
        let req = IpcRequest::Write {
            text: "echo hello".into(),
            enter: true,
            bracketed_paste: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        let de: IpcRequest = serde_json::from_str(&json).unwrap();
        match de {
            IpcRequest::Write {
                text,
                enter,
                bracketed_paste,
            } => {
                assert_eq!(text, "echo hello");
                assert!(enter);
                assert!(!bracketed_paste);
            }
            _ => panic!("unexpected request"),
        }
    }

    #[test]
    fn test_session_info_serialization() {
        let info = SessionInfo {
            pid: 12345,
            started_at_epoch_sec: 1700000000,
            command: vec!["zsh".into(), "-i".into()],
            term_size: (24, 80),
            persist: true,
            clients: 2,
            pty_slave_path: Some("/dev/pts/1".into()),
        };
        let resp = IpcResponse::Info(info.clone());
        let json = serde_json::to_string(&resp).unwrap();
        let de: IpcResponse = serde_json::from_str(&json).unwrap();
        match de {
            IpcResponse::Info(i) => {
                assert_eq!(i.pid, 12345);
                assert_eq!(i.command, vec!["zsh", "-i"]);
                assert!(i.persist);
                assert_eq!(i.clients, 2);
                assert_eq!(i.pty_slave_path.as_deref(), Some("/dev/pts/1"));
            }
            _ => panic!("unexpected response"),
        }
    }

    #[test]
    fn test_attach_serialization() {
        let req = IpcRequest::Attach {
            cols: Some(120),
            rows: Some(40),
        };
        let json = serde_json::to_string(&req).unwrap();
        let de: IpcRequest = serde_json::from_str(&json).unwrap();
        match de {
            IpcRequest::Attach { cols, rows } => {
                assert_eq!(cols, Some(120));
                assert_eq!(rows, Some(40));
            }
            _ => panic!("unexpected request"),
        }
    }

    #[test]
    fn test_resize_serialization() {
        let req = IpcRequest::Resize { cols: 80, rows: 24 };
        let json = serde_json::to_string(&req).unwrap();
        let de: IpcRequest = serde_json::from_str(&json).unwrap();
        match de {
            IpcRequest::Resize { cols, rows } => {
                assert_eq!(cols, 80);
                assert_eq!(rows, 24);
            }
            _ => panic!("unexpected request"),
        }
    }

    #[test]
    fn test_rename_serialization() {
        let req = IpcRequest::Rename {
            new_name: "my_worker".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let de: IpcRequest = serde_json::from_str(&json).unwrap();
        match de {
            IpcRequest::Rename { new_name } => {
                assert_eq!(new_name, "my_worker");
            }
            _ => panic!("unexpected request"),
        }
    }

    #[test]
    fn test_validate_session_name() {
        // Valid names
        assert!(validate_session_name("main").is_ok());
        assert!(validate_session_name("worker_1").is_ok());
        assert!(validate_session_name("dev-2.0").is_ok());
        assert!(validate_session_name("12345").is_ok());

        // Invalid names
        assert!(validate_session_name("").is_err()); // empty
        assert!(validate_session_name("a".repeat(65).as_str()).is_err()); // too long
        assert!(validate_session_name("-main").is_err()); // leading hyphen
        assert!(validate_session_name(".").is_err()); // dot
        assert!(validate_session_name("..").is_err()); // double dot
        assert!(validate_session_name("foo/bar").is_err()); // slash
        assert!(validate_session_name("foo\\bar").is_err()); // backslash
        assert!(validate_session_name("hello world").is_err()); // space
        assert!(validate_session_name("foo\nbar").is_err()); // newline
        assert!(validate_session_name("foo.sock").is_err()); // ends with .sock
        assert!(validate_session_name("foo;bar").is_err()); // shell metachar
        assert!(validate_session_name("foo$bar").is_err()); // shell metachar
    }

    #[test]
    fn test_parse_name_from_socket_path() {
        assert_eq!(parse_name_from_socket_path(Path::new("/tmp/ttyman-main")), "main");
        assert_eq!(parse_name_from_socket_path(Path::new("/run/user/1000/ttyman-12345")), "12345");
        assert_eq!(parse_name_from_socket_path(Path::new("ttyman-worker")), "worker");
    }
}
