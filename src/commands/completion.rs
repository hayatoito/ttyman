use clap::Args;
use clap_complete::Shell;

#[derive(Args, Debug, Clone)]
pub struct CompletionArgs {
    /// Target shell to generate completions for
    #[arg(value_enum)]
    pub shell: Shell,
}

pub fn run(args: CompletionArgs, cmd: &mut clap::Command) {
    let mut buf = Vec::new();
    clap_complete::generate(args.shell, cmd, "ttyman", &mut buf);
    let mut script = String::from_utf8_lossy(&buf).to_string();

    match args.shell {
        Shell::Zsh => {
            script = script.replace(":SESSION:_default", ":SESSION:_ttyman_sessions");
            script.push_str(
                "\n\
_ttyman_sessions() {\n\
    local -a sessions\n\
    sessions=(${(f)\"$(ttyman list --json 2>/dev/null | sed -n 's/.*\"name\": \"\\([^\"]*\\)\".*/\\1/p' 2>/dev/null)\"})\n\
    _describe 'active session' sessions\n\
}\n",
            );
        }
        Shell::Bash => {
            script = script.replace(
                "                --session)\n                    COMPREPLY=($(compgen -f \"${cur}\"))\n                    return 0\n                    ;;",
                "                --session)\n                    _ttyman_sessions\n                    return 0\n                    ;;",
            );
            script = script.replace(
                "                -s)\n                    COMPREPLY=($(compgen -f \"${cur}\"))\n                    return 0\n                    ;;",
                "                -s)\n                    _ttyman_sessions\n                    return 0\n                    ;;",
            );
            script.push_str(
                "\n\
_ttyman_sessions() {\n\
    local sessions\n\
    sessions=$(ttyman list --json 2>/dev/null | sed -n 's/.*\"name\": \"\\([^\"]*\\)\".*/\\1/p' 2>/dev/null)\n\
    COMPREPLY=( $(compgen -W \"${sessions}\" -- \"${cur}\") )\n\
}\n",
            );
        }
        Shell::Fish => {
            let mut modified_lines = Vec::new();
            for line in script.lines() {
                if line.contains("-l session") && line.ends_with("-r") {
                    modified_lines.push(format!("{line} -f -a '(__fish_ttyman_sessions)'"));
                } else {
                    modified_lines.push(line.to_string());
                }
            }
            script = modified_lines.join("\n");
            script.push_str(
                "\n\
function __fish_ttyman_sessions\n\
    ttyman list --json 2>/dev/null | sed -n 's/.*\"name\": \"\\([^\"]*\\)\".*/\\1/p' 2>/dev/null\n\
end\n",
            );
        }
        _ => {}
    }

    print!("{script}");
}
