use crate::ipc::{IpcRequest, is_self_session, resolve_socket_path};
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
    let session_name = crate::ipc::parse_name_from_socket_path(&sock_path);

    if io::stdout().is_terminal() && is_self_session(&sock_path) {
        anyhow::bail!(
            "Cannot watch session '{session_name}' from within its own terminal to prevent recursive output loop.\n\
             Pipe to another program (e.g. `ttyman watch | grep ...`) or run from a separate terminal."
        );
    }

    let mut stream = UnixStream::connect(&sock_path)
        .map_err(|e| anyhow::anyhow!("Failed to connect to session '{session_name}': {e}"))?;

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
