use crate::ipc::{IpcRequest, IpcResponse, resolve_socket_path};
use clap::Args;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

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
    let sock_path = resolve_socket_path(args.session.as_deref())?;

    if !sock_path.exists() {
        let display = args.session.as_deref().unwrap_or("current");
        anyhow::bail!("Session '{display}' not found. Is the session active?");
    }

    let stream = UnixStream::connect(&sock_path).await?;
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);

    // Fetch text from server
    let req = IpcRequest::Read {
        lines: args.lines,
        all: args.all,
        with_color: args.ansi,
    };
    let mut req_str = serde_json::to_string(&req)?;
    req_str.push('\n');
    writer.write_all(req_str.as_bytes()).await?;
    writer.flush().await?;

    let mut resp_str = String::new();
    buf_reader.read_line(&mut resp_str).await?;
    let resp: IpcResponse = serde_json::from_str(&resp_str)?;

    let text = match resp {
        IpcResponse::Ok(t) => t,
        IpcResponse::Error(e) => anyhow::bail!("IPC Error: {e}"),
        _ => anyhow::bail!("Unexpected IPC response"),
    };

    print!("{}", text);
    if !text.is_empty() && !text.ends_with('\n') {
        println!();
    }

    Ok(())
}
