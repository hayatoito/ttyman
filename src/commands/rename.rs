use crate::ipc::{IpcRequest, IpcResponse, resolve_existing_socket_path, send_ipc_request};
use clap::Args;

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
    let sock_path = resolve_existing_socket_path(args.session.as_deref())?;

    let req = IpcRequest::Rename {
        new_name: args.new_name.clone(),
    };

    let resp = send_ipc_request(&sock_path, &req).await?;
    match resp {
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
