use clap::{CommandFactory, Parser, Subcommand};
use ttyman::commands::{attach, completion, kill, list, read, rename, start, watch, write};

#[derive(Parser, Debug)]
#[command(name = "ttyman", version, about = "Terminal session manager")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start a session in the background
    Start(start::StartArgs),

    /// Kill an active session
    Kill(kill::KillArgs),

    /// Attach to a session
    Attach(attach::AttachArgs),

    /// Rename an active session
    Rename(rename::RenameArgs),

    /// Stream live output from a session
    Watch(watch::WatchArgs),

    /// Read screen or scrollback from a session
    Read(read::ReadArgs),

    /// Write input to a session
    Write(write::WriteArgs),

    /// List active sessions
    List(list::ListArgs),

    /// Generate shell completion script
    Completion(completion::CompletionArgs),
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
        Commands::Completion(args) => {
            completion::run(args, &mut Cli::command());
            Ok(())
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
