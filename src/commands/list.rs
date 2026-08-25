use crate::ipc::{IpcRequest, IpcResponse, SessionInfo};
use clap::Args;
use nix::sys::signal::kill;
use nix::unistd::Pid;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Args, Debug, Clone)]
pub struct ListArgs {
    /// Output session list formatted as JSON
    #[arg(long = "json")]
    pub json: bool,

    /// Automatically remove dead / stale socket files
    #[arg(long = "clean")]
    pub clean: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct SessionEntry {
    pub name: String,
    pub pid: u32,
    pub socket: String,
    pub command: Vec<String>,
    pub term_size: (u16, u16),
    pub persist: bool,
    pub clients: usize,
    pub age_secs: u64,
    pub is_alive: bool,
}

use crate::ipc::parse_name_from_socket_path;

async fn query_session(socket_path: &Path) -> Option<SessionInfo> {
    let timeout = Duration::from_millis(150);
    tokio::time::timeout(timeout, async {
        let stream = UnixStream::connect(socket_path).await.ok()?;
        let (reader, mut writer) = stream.into_split();
        let mut buf_reader = BufReader::new(reader);

        let req = IpcRequest::Info;
        let mut req_str = serde_json::to_string(&req).ok()?;
        req_str.push('\n');
        writer.write_all(req_str.as_bytes()).await.ok()?;
        writer.flush().await.ok()?;

        let mut resp_str = String::new();
        buf_reader.read_line(&mut resp_str).await.ok()?;
        match serde_json::from_str::<IpcResponse>(&resp_str).ok()? {
            IpcResponse::Info(info) => Some(info),
            _ => None,
        }
    })
    .await
    .ok()
    .flatten()
}

fn parse_pid_from_socket_name(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    let pid_str = name.strip_prefix("ttyman-")?;
    pid_str.parse::<u32>().ok()
}

fn is_process_alive(pid: u32) -> bool {
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

pub async fn run(args: ListArgs) -> anyhow::Result<()> {
    let _ = crate::ipc::get_runtime_dir()?;
    let socket_paths = crate::ipc::find_socket_files();
    let now_epoch_sec = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut entries = Vec::new();
    let mut cleaned_count = 0;

    for sock in socket_paths {
        let pid_opt = parse_pid_from_socket_name(&sock);
        let info = query_session(&sock).await;

        let pid = if let Some(ref i) = info {
            i.pid
        } else if let Some(p) = pid_opt {
            p
        } else {
            continue;
        };

        let alive = is_process_alive(pid);

        if !alive && args.clean {
            let _ = std::fs::remove_file(&sock);
            cleaned_count += 1;
            continue;
        }

        let started_at = info.as_ref().map(|i| i.started_at_epoch_sec).unwrap_or(0);
        let age_secs = if started_at > 0 && now_epoch_sec >= started_at {
            now_epoch_sec - started_at
        } else {
            0
        };

        let command = info
            .as_ref()
            .map(|i| i.command.clone())
            .unwrap_or_else(|| vec!["[unknown]".to_string()]);
        let term_size = info.as_ref().map(|i| i.term_size).unwrap_or((0, 0));

        let name = parse_name_from_socket_path(&sock);
        let persist = info.as_ref().map(|i| i.persist).unwrap_or(false);
        let clients = info.as_ref().map(|i| i.clients).unwrap_or(0);

        entries.push(SessionEntry {
            name,
            pid,
            socket: sock.to_string_lossy().to_string(),
            command,
            term_size,
            persist,
            clients,
            age_secs,
            is_alive: alive,
        });
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if entries.is_empty() {
        if cleaned_count > 0 {
            println!("No active sessions (cleaned {cleaned_count} stale sockets).");
        } else {
            println!("No active ttyman sessions found.");
        }
        return Ok(());
    }

    // Print table header
    println!(
        "{:<18} {:<8} {:<24} {:<10} {:<9} {:<9} {:<10}",
        "NAME", "PID", "COMMAND", "SIZE", "PERSIST", "CLIENTS", "AGE"
    );

    for entry in &entries {
        let name_display = if entry.name.len() > 17 {
            format!("{}...", &entry.name[..14])
        } else {
            entry.name.clone()
        };

        let cmd_str = entry.command.join(" ");
        let cmd_display = if cmd_str.len() > 23 {
            format!("{}...", &cmd_str[..20])
        } else {
            cmd_str
        };

        let size_display = if entry.term_size == (0, 0) {
            "-".to_string()
        } else {
            format!("{}x{}", entry.term_size.1, entry.term_size.0)
        };

        let persist_display = if !entry.is_alive {
            "dead"
        } else if entry.persist {
            "yes"
        } else {
            "no"
        };

        let clients_display = if !entry.is_alive {
            "-".to_string()
        } else {
            entry.clients.to_string()
        };

        let age_display = if entry.age_secs > 0 {
            format_duration(entry.age_secs)
        } else {
            "-".to_string()
        };

        println!(
            "{:<18} {:<8} {:<24} {:<10} {:<9} {:<9} {:<10}",
            name_display,
            entry.pid,
            cmd_display,
            size_display,
            persist_display,
            clients_display,
            age_display
        );
    }

    if cleaned_count > 0 {
        println!("\n(Cleaned {cleaned_count} dead sockets)");
    }

    Ok(())
}
