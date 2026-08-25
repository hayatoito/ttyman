use crate::ipc::{IpcRequest, IpcResponse, resolve_socket_path};
use clap::Args;
use std::io::Read;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Args, Debug, Clone)]
pub struct WriteArgs {
    /// Text to write (reads from standard input if omitted or '-')
    pub text: Option<String>,

    /// Append Enter (newline) after text (submits command atomically)
    #[arg(short = 'E', long = "enter")]
    pub enter: bool,

    /// Wrap text in bracketed-paste sequences (\x1b[200~ ... \x1b[201~)
    #[arg(long = "bracketed-paste")]
    pub bracketed_paste: bool,

    /// Target session name (defaults to $TTYMAN_SESSION)
    #[arg(short = 's', long = "session", value_name = "SESSION")]
    pub session: Option<String>,
}

pub async fn run(args: WriteArgs) -> anyhow::Result<()> {
    let sock_path = resolve_socket_path(args.session.as_deref())?;

    if !sock_path.exists() {
        let display = args.session.as_deref().unwrap_or("current");
        anyhow::bail!("Session '{display}' not found. Is the session active?");
    }

    let raw_text = match args.text.as_deref() {
        Some("-") | None => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
        Some(s) => s.to_string(),
    };

    let stream = UnixStream::connect(&sock_path).await?;
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);

    let req = IpcRequest::Write {
        text: raw_text,
        enter: args.enter,
        bracketed_paste: args.bracketed_paste,
    };
    let mut req_str = serde_json::to_string(&req)?;
    req_str.push('\n');
    writer.write_all(req_str.as_bytes()).await?;
    writer.flush().await?;

    let mut resp_str = String::new();
    buf_reader.read_line(&mut resp_str).await?;
    let resp: IpcResponse = serde_json::from_str(&resp_str)?;

    match resp {
        IpcResponse::Ok(_) => Ok(()),
        IpcResponse::Error(e) => anyhow::bail!("IPC Error: {e}"),
        _ => anyhow::bail!("Unexpected IPC response"),
    }
}
