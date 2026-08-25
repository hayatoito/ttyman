use clap::{Parser, Subcommand};
use ttyman::commands::{attach, list, play, read, record, rename, run, start, watch, write};

#[derive(Parser, Debug)]
#[command(
    name = "ttyman",
    version,
    about = "Terminal session manager to persist and attach sessions, inspect screens and scrollback, stream live output, and inject input via IPC"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run a command or interactive shell in a foreground PTY proxy with an IPC socket
    Run(run::RunArgs),

    /// Start a session in the background (detached daemon)
    Start(start::StartArgs),

    /// Attach interactively to a session (spawns if not already running)
    Attach(attach::AttachArgs),

    /// Rename an active session
    Rename(rename::RenameArgs),

    /// Record stdin stream into a .ttyrec-compatible format
    Record(record::RecordArgs),

    /// Play back a recorded .ttyrec file (or inspect duration with --time)
    Play(play::PlayArgs),

    /// Watch and stream a live running session in real-time
    Watch(watch::WatchArgs),

    /// Read screen snapshot or scrollback history from a running session
    Read(read::ReadArgs),

    /// Write text or inject commands into a running session
    Write(write::WriteArgs),

    /// List active sessions and inspect socket status
    List(list::ListArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let res = match cli.command {
        Commands::Start(args) => start::run(args),
        Commands::Attach(args) => attach::run(args),
        Commands::Rename(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(rename::run(args))
        }
        Commands::Record(args) => record::run(args),
        Commands::Play(args) => play::run(args),
        Commands::Watch(args) => watch::run(args),
        Commands::Run(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(run::run(args))
        }
        Commands::Read(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(read::run(args))
        }
        Commands::Write(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(write::run(args))
        }
        Commands::List(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(list::run(args))
        }
    };
    match res {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}
