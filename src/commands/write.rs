use crate::ipc::{IpcRequest, IpcResponse, resolve_socket_path, send_ipc_request};
use clap::Args;
use std::io::Read;

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
        let session_name = crate::ipc::parse_name_from_socket_path(&sock_path);
        let display = args.session.as_deref().unwrap_or(&session_name);
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

    let req = IpcRequest::Write {
        text: raw_text,
        enter: args.enter,
        bracketed_paste: args.bracketed_paste,
    };

    let resp = send_ipc_request(&sock_path, &req).await?;
    match resp {
        IpcResponse::Ok(_) => Ok(()),
        IpcResponse::Error(e) => anyhow::bail!("{e}"),
        _ => anyhow::bail!("Unexpected response from session"),
    }
}
