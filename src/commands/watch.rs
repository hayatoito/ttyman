use crate::ipc::{
    DEFAULT_SESSION_VAR, IpcRequest, SessionEvent, is_self_session, resolve_socket_path,
};
use clap::Args;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};

#[derive(Args, Debug, Clone)]
pub struct WatchArgs {
    /// Target session name (defaults to $TTYMAN_SESSION)
    #[arg(short = 's', long = "session", value_name = "SESSION")]
    pub session: Option<String>,

    /// [Experimental] Run CMD on each event, with the new output on stdin
    #[arg(long = "exec", value_name = "CMD")]
    pub exec: Option<String>,
}

pub fn run(args: WatchArgs) -> anyhow::Result<()> {
    let sock_path = resolve_socket_path(args.session.as_deref())?;
    let session_name = crate::ipc::parse_name_from_socket_path(&sock_path);

    if args.exec.is_none() && io::stdout().is_terminal() && is_self_session(&sock_path) {
        anyhow::bail!(
            "Cannot watch session '{session_name}' from within its own terminal to prevent recursive output loop.\n\
             Pipe to another program (e.g. `ttyman watch | grep ...`) or run from a separate terminal."
        );
    }
    if args.exec.is_some() && is_self_session(&sock_path) {
        anyhow::bail!("--exec cannot target the session it runs in ('{session_name}')");
    }

    let mut stream = UnixStream::connect(&sock_path)
        .map_err(|e| anyhow::anyhow!("Failed to connect to session '{session_name}': {e}"))?;

    let req = match args.exec {
        None => IpcRequest::Subscribe,
        Some(_) => IpcRequest::SubscribeEvents,
    };
    let req = serde_json::to_string(&req)?;
    stream.write_all(req.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    match args.exec {
        None => print_stream(stream),
        Some(cmd) => exec_events(stream, &cmd, &session_name),
    }
}

fn print_stream(mut stream: UnixStream) -> anyhow::Result<()> {
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

/// Run `cmd` for each event the session daemon sends (one JSON object per line) until the
/// connection ends.
fn exec_events(stream: UnixStream, cmd: &str, session: &str) -> anyhow::Result<()> {
    for line in BufReader::new(stream).lines() {
        let ev: SessionEvent = serde_json::from_str(&line?)?;
        run_exec(cmd, session, &ev);
    }
    Ok(())
}

fn run_exec(cmd: &str, session: &str, ev: &SessionEvent) {
    let child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .env(DEFAULT_SESSION_VAR, session)
        .env("TTYMAN_EVENT", &ev.event)
        .stdin(Stdio::piped())
        .spawn();
    match child {
        Ok(mut c) => {
            if let Some(mut stdin) = c.stdin.take() {
                let _ = stdin.write_all(ev.text.as_bytes());
            }
            let _ = c.wait();
        }
        Err(e) => eprintln!("ttyman watch: failed to run --exec command: {e}"),
    }
}
