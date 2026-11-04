use crate::ipc::{IpcRequest, IpcResponse, resolve_existing_socket_path, send_ipc_request};
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct ReadArgs {
    /// Number of recent lines to read (reads visible screen if omitted)
    #[arg(short = 'n', long = "lines")]
    pub lines: Option<usize>,

    /// Read entire scrollback history from the start of the session
    #[arg(short = 'a', long = "all")]
    pub all: bool,

    /// Preserve ANSI color and style escape sequences
    #[arg(long = "ansi")]
    pub ansi: bool,

    /// Target session name (defaults to $TTYMAN_SESSION)
    #[arg(short = 's', long = "session", value_name = "SESSION")]
    pub session: Option<String>,
}

pub async fn run(args: ReadArgs) -> anyhow::Result<()> {
    let sock_path = resolve_existing_socket_path(args.session.as_deref())?;

    let req = IpcRequest::Read {
        lines: args.lines,
        all: args.all,
        with_color: args.ansi,
    };

    let resp = send_ipc_request(&sock_path, &req).await?;
    let text = match resp {
        IpcResponse::Ok(t) => t,
        IpcResponse::Error(e) => anyhow::bail!("{e}"),
        _ => anyhow::bail!("Unexpected response from session"),
    };

    print!("{text}");
    if !text.is_empty() && !text.ends_with('\n') {
        println!();
    }

    Ok(())
}
