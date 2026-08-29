use crate::ipc::{parse_name_from_socket_path, query_session_info, resolve_socket_path};
use clap::Args;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

#[derive(Args, Debug, Clone)]
pub struct KillArgs {
    /// Target session name (defaults to $TTYMAN_SESSION)
    #[arg(short = 's', long = "session", value_name = "SESSION")]
    pub session: Option<String>,
}

pub fn run(args: KillArgs) -> anyhow::Result<()> {
    let sock_path = resolve_socket_path(args.session.as_deref())?;
    let session_name = parse_name_from_socket_path(&sock_path);

    if !sock_path.exists() {
        let display = args.session.as_deref().unwrap_or(&session_name);
        anyhow::bail!("Session '{display}' not found. Is the session active?");
    }

    let info = match query_session_info(&sock_path) {
        Ok(info) => info,
        Err(_) => {
            if !crate::ipc::is_socket_alive(&sock_path) {
                let _ = std::fs::remove_file(&sock_path);
                println!("Removed stale session '{session_name}' (socket was inactive)");
                return Ok(());
            }
            anyhow::bail!("Failed to query session '{session_name}'");
        }
    };

    let pid = Pid::from_raw(info.pid as i32);
    if let Err(e) = kill(pid, Signal::SIGTERM) {
        if e == nix::errno::Errno::ESRCH {
            let _ = std::fs::remove_file(&sock_path);
            println!("Removed stale session '{session_name}' (process was not running)");
            return Ok(());
        }
        anyhow::bail!(
            "Failed to kill session '{session_name}' (PID {}): {e}",
            info.pid
        );
    }

    // Wait briefly for daemon to clean up and exit
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1000);
    while std::time::Instant::now() < deadline {
        if !sock_path.exists() && kill(pid, None).is_err() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // If still lingering after timeout, force kill
    if sock_path.exists() || kill(pid, None).is_ok() {
        let _ = kill(pid, Signal::SIGKILL);
        let _ = std::fs::remove_file(&sock_path);
    }

    println!("Killed session '{session_name}'");
    Ok(())
}
