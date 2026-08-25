use crate::commands::attach::spawn_daemon_supervisor;
use crate::ipc::{DEFAULT_SESSION_VAR, resolve_socket_path};
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct StartArgs {
    /// Path to TOML configuration file
    #[arg(short = 'c', long = "config")]
    pub config: Option<String>,

    /// Target session name
    #[arg(short = 's', long = "session", value_name = "SESSION")]
    pub session: Option<String>,

    /// Command to execute in session (defaults to interactive $SHELL)
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

pub fn run(args: StartArgs) -> anyhow::Result<()> {
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

        if is_inside_session && !is_self_attach {
            if is_alive {
                println!(
                    "[ttyman] Session '{session_display}' is already running in background (nesting prevented).\n\
                     [ttyman] Press 'Ctrl-]' to switch to '{session_display}'."
                );
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
            }
            return Ok(());
        }

        if is_alive {
            println!("Session at {:?} is already running.", sock_path);
            return Ok(());
        }

        spawn_daemon_supervisor(
            Some(sock_path.clone()),
            args.config.as_deref(),
            &args.command,
        )?;
        println!(
            "Started ttyman session in background (socket: {})",
            sock_path.display()
        );
        return Ok(());
    }

    let sock_path = spawn_daemon_supervisor(None, args.config.as_deref(), &args.command)?;
    println!(
        "Started ttyman session in background (socket: {})",
        sock_path.display()
    );
    Ok(())
}
