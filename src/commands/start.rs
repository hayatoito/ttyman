use crate::server::{TargetSession, prepare_session_target, spawn_daemon_supervisor};
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
    let target = prepare_session_target(
        args.session.as_deref(),
        args.config.as_deref(),
        &args.command,
        false,
    )?;

    let sock_to_spawn = match target {
        TargetSession::Handled => return Ok(()),
        TargetSession::Ready {
            sock_path,
            is_alive,
        } => {
            if is_alive {
                let name = args
                    .session
                    .as_deref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| crate::ipc::parse_name_from_socket_path(&sock_path));
                println!("Session '{name}' is already running.");
                return Ok(());
            }
            Some(sock_path)
        }
        TargetSession::Unspecified => None,
    };

    let sock_path = spawn_daemon_supervisor(sock_to_spawn, args.config.as_deref(), &args.command)?;
    let session_name = crate::ipc::parse_name_from_socket_path(&sock_path);
    println!("Started session '{session_name}' in background.");
    Ok(())
}
