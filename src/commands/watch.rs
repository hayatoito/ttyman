use crate::ipc::{DEFAULT_SESSION_VAR, IpcRequest, resolve_socket_path};
use clap::Args;
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::net::UnixStream;

#[derive(Args, Debug, Clone)]
pub struct WatchArgs {
    /// Target session name (defaults to $TTYMAN_SESSION)
    #[arg(short = 's', long = "session", value_name = "SESSION")]
    pub session: Option<String>,
}

pub fn run(args: WatchArgs) -> anyhow::Result<()> {
    let sock_path = resolve_socket_path(args.session.as_deref())?;

    if io::stdout().is_terminal() {
        if let Ok(current_env_session) = std::env::var(DEFAULT_SESSION_VAR)
            && let Ok(current_sock_path) = resolve_socket_path(Some(&current_env_session))
        {
            let matches = sock_path == current_sock_path
                || (sock_path.exists()
                    && current_sock_path.exists()
                    && std::fs::canonicalize(&sock_path).ok()
                        == std::fs::canonicalize(&current_sock_path).ok());
            if matches {
                anyhow::bail!(
                    "Cannot watch session directly to its own terminal.\nPipe to another program (e.g. `ttyman watch | grep ...`) or run from a separate terminal."
                );
            }
        }

        if sock_path.exists()
            && let Some(my_tty) = crate::ipc::get_current_tty_name()
            && let Ok(info) = crate::ipc::query_session_info(&sock_path)
            && info.pty_slave_path.as_deref() == Some(&my_tty)
        {
            anyhow::bail!(
                "Cannot watch session '{}' from within its own terminal ({my_tty}) to prevent recursive output loop.\nPipe to another program (e.g. `ttyman watch | grep ...`) or run from a separate terminal.",
                sock_path.display()
            );
        }
    }

    let mut stream = UnixStream::connect(&sock_path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to connect to tp session at {}: {e}",
            sock_path.display()
        )
    })?;

    let req = serde_json::to_string(&IpcRequest::Subscribe)?;
    stream.write_all(req.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut stdout = io::stdout();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                stdout.write_all(&buf[..n])?;
                stdout.flush()?;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    Ok(())
}
