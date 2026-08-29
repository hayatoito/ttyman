use clap::{Parser, Subcommand};
use ttyman::commands::{attach, kill, list, read, rename, start, watch, write};

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
    /// Start a session in the background (detached daemon)
    Start(start::StartArgs),

    /// Kill an active session
    Kill(kill::KillArgs),

    /// Attach interactively to a session (spawns if not already running)
    Attach(attach::AttachArgs),

    /// Rename an active session
    Rename(rename::RenameArgs),

    /// Watch and stream a live running session in real-time
    Watch(watch::WatchArgs),

    /// Read screen snapshot or scrollback history from a running session
    Read(read::ReadArgs),

    /// Write text or inject commands into a running session
    Write(write::WriteArgs),

    /// List active sessions
    List(list::ListArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let res = match cli.command {
        Commands::Start(args) => start::run(args),
        Commands::Kill(args) => kill::run(args),
        Commands::Attach(args) => attach::run(args),
        Commands::Rename(args) => ttyman::run_async(rename::run(args)),
        Commands::Watch(args) => watch::run(args),
        Commands::Read(args) => ttyman::run_async(read::run(args)),
        Commands::Write(args) => ttyman::run_async(write::run(args)),
        Commands::List(args) => ttyman::run_async(list::run(args)),
    };
    match res {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}
