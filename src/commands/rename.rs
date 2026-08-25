use crate::ipc::{IpcRequest, IpcResponse, resolve_socket_path};
use clap::Args;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Args, Debug, Clone)]
pub struct RenameArgs {
    /// Target session name (defaults to $TTYMAN_SESSION)
    #[arg(short = 's', long = "session", value_name = "SESSION")]
    pub session: Option<String>,

    /// New session name
    #[arg(required = true)]
    pub new_name: String,
}

pub async fn run(args: RenameArgs) -> anyhow::Result<()> {
    crate::ipc::validate_session_name(&args.new_name)?;
    let sock_path = resolve_socket_path(args.session.as_deref())?;

    if !sock_path.exists() {
        let display = args.session.as_deref().unwrap_or("current");
        anyhow::bail!("Session '{display}' not found. Is the session active?");
    }

    let stream = UnixStream::connect(&sock_path).await.map_err(|e| {
        anyhow::anyhow!(
            "Failed to connect to session at '{}': {e}",
            sock_path.display()
        )
    })?;

    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);

    let req = IpcRequest::Rename {
        new_name: args.new_name.clone(),
    };
    let mut req_str = serde_json::to_string(&req)?;
    req_str.push('\n');

    writer.write_all(req_str.as_bytes()).await?;
    writer.flush().await?;

    let mut line = String::new();
    if buf_reader.read_line(&mut line).await? == 0 {
        anyhow::bail!("Session closed connection without responding");
    }

    match serde_json::from_str::<IpcResponse>(&line)? {
        IpcResponse::Ok(msg) => {
            println!("{msg}");
            Ok(())
        }
        IpcResponse::Error(err) => {
            anyhow::bail!("{err}");
        }
        other => {
            anyhow::bail!("Unexpected response from session: {other:?}");
        }
    }
}
