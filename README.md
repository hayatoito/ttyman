# ttyman

_Zero-interference terminal session manager._

[![Crates.io](https://img.shields.io/crates/v/ttyman.svg)](https://crates.io/crates/ttyman)
[![CI](https://github.com/hayatoito/ttyman/actions/workflows/ci.yml/badge.svg)](https://github.com/hayatoito/ttyman/actions)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](https://github.com/hayatoito/ttyman)

`ttyman` persists, detaches, and reattaches terminal sessions without extra UI. Every session runs in an isolated background PTY with a Unix domain socket for IPC.

```text
[ Your Terminal ] <====== (Ctrl-] detach / attach) ======> [ ttyman session "dev" ]
                                                                  │ (PTY)
                                                            [ $SHELL / vim / top ]
                                                                  │
  [ External Scripts / AI Agent ]                                 │ (UNIX Socket)
  ├── ttyman read  (snapshot / scrollback) ─────────────────────> [ IPC Server ]
  ├── ttyman watch (stream live terminal output) ───────────────>
  └── ttyman write (inject keystrokes / commands) ──────────────>
```

---

## Why ttyman?

- **Byte-Transparent**: Passes raw bytes directly between your PTY and terminal emulator without filtering escape sequences or terminal protocols.
- **No UI**: No status bars, tabs, or split panes. Run `ttyman` directly in your terminal and let your window manager manage windows, use built-in terminal tabs and splits, or pair it with a multiplexer like `tmux` or `herdr`.
- **No Central Daemon**: Each session runs as an independent process with its own socket.
- **Unix Socket IPC**: Read screen snapshots (`read`), stream live output (`watch`), and inject input (`write`).
- **Bring Your Own Menu**: Detach with `Ctrl-]` by default, or configure a custom command (using tools like `fzf`) to switch and manage sessions.

---

## Installation

### Prebuilt Binary (Linux & macOS)

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/hayatoito/ttyman/releases/latest/download/ttyman-installer.sh | sh
```

### Using Cargo

```bash
cargo install ttyman
```

---

## Quickstart

```bash
# 1. Attach to a session named "dev" (creates it if not running):
$ ttyman attach -s dev

# 2. Run a command:
$ top

# 3. Close the terminal window or disconnect SSH.

# 4. Open a new terminal and list sessions:
$ ttyman list
  NAME               PID      COMMAND                          SIZE       CLIENTS   AGE       
  dev                1071043  /usr/bin/zsh -i                  113x75     0         42s       

# 5. Reattach to the session:
$ ttyman attach -s dev
# => Screen state is restored and `top` continues running.
```

---

## Commands

All session-related commands use **`-s, --session <SESSION>`** uniformly. If omitted inside a session, it defaults to the current session (`$TTYMAN_SESSION`).

### `ttyman attach`

Attach interactively to a session.

```bash
# Attach to a session named "dev" (creates it if not running):
$ ttyman attach -s dev

# Start an anonymous session (if -s is omitted, the PID is used as the session name):
$ ttyman attach

# Detach at any time: press Ctrl-]
```

`ttyman attach` without specifying a session name is designed to be used directly or configured in place of `$SHELL` as the default startup command in terminal emulators or multiplexers (like tmux).

### `ttyman start`

Start a session in the background (without attaching):

```bash
# Start background shell:
$ ttyman start -s worker

# Start background command:
$ ttyman start -s test-server -- npm run dev
```

### `ttyman list`

List sessions:

```bash
$ ttyman list
  NAME               PID      COMMAND                          SIZE       CLIENTS   AGE       
* dev                1071043  /usr/bin/zsh -i                  113x75     1         42s       
  worker             1072768  npm run dev                      191x86     0         10m 00s   

# Machine-readable JSON output:
$ ttyman list --json
```

### `ttyman kill`

Terminate a session:

```bash
$ ttyman kill -s dev
```

### `ttyman rename`

Rename a session:

```bash
# Inside a session:
$ ttyman rename new-name

# From outside:
$ ttyman rename -s old-name new-name
```

### `ttyman read`

Read screen text or scrollback history:

```bash
# Read visible screen text:
$ ttyman read -s dev

# Read last 20 lines with ANSI colors:
$ ttyman read -s dev -n 20 --ansi

# Read entire scrollback history:
$ ttyman read -s dev -a
```

If `-s <SESSION>` is omitted inside a session, it defaults to `$TTYMAN_SESSION`:

```bash
# Read visible screen text of the current session:
$ ttyman read

# Read the last 20 lines and filter with fzf:
$ ttyman read -n 20 | fzf

# Read the entire scrollback history and grep for errors:
$ ttyman read -a | grep ERROR

# Read the entire scrollback history and pipe to an LLM:
$ ttyman read -a | llm -p 'Please fix the compile error'
```

### `ttyman watch`

Stream live terminal output in real time (read-only):

```bash
# Stream output to stdout (Ctrl-C exits watch):
$ ttyman watch -s dev

# Pipe to another command:
$ ttyman watch -s test-server | grep --line-buffered -E "ERROR|WARN"
```

You can prompt your AI coding agent to observe your interactive session in real time:

> _"Please run `ttyman watch -s main` in the background to monitor my terminal session and advise me whenever an error occurs."_

`ttyman watch` can be considered a read-only attach mode. No keystrokes are forwarded to the session. For example, pressing Ctrl-C exits `ttyman watch` itself, leaving the session running uninterrupted.

### `ttyman write`

Inject text, keystrokes, or commands into a running session:

```bash
# Send a single keystroke:
$ ttyman write -s dev "q"

# Send a command and press Enter:
$ ttyman write -s dev -E "git status"

# Send raw control keys or bytes (e.g. Ctrl-], Ctrl-C, Escape):
$ printf '\x1d' | ttyman write -s dev
$ ttyman write -s dev $'\x1d'

# Inject multiline text:
$ ttyman write -s dev --bracketed-paste < script.py
```

### `ttyman completion`

Generate shell completion scripts for `bash`, `zsh`, `fish`, `powershell`, or `elvish`:

```bash
# Zsh (add to ~/.zshrc):
eval "$(ttyman completion zsh)"

# Bash (add to ~/.bashrc):
source <(ttyman completion bash)

# Fish:
ttyman completion fish | source
```

---

## "Bring Your Own Menu" (BYOM)

By default, pressing `Ctrl-]` simply detaches from the current session. Under the hood, however, `Ctrl-]` actually triggers a configurable menu command.

`ttyman` has no built-in TUI menu. Instead, `menu.command` defaults to `"echo detach"`, which detaches immediately without prompting.

You can customize `menu.command` in `~/.config/ttyman/config.toml` to point to any shell command or script, and `ttyman` will process whatever it writes to standard output.

### Protocol

The menu command communicates with `ttyman` via standard output:

| Output Line                                    | Action                                           |
| ---------------------------------------------- | ------------------------------------------------ |
| `attach <SESSION_NAME>` (e.g. `attach worker`) | Switch to named session (creates if not running) |
| `attach`                                       | Create a new session and switch to it            |
| `detach`                                       | Detach from session                              |
| _(empty / exit code != 0)_                     | Cancel and return to current screen              |

### Environment Variables Provided to Menu Command

- **`TTYMAN_SESSION`**: Current session name.
- **`TTYMAN_RECENT_SESSIONS`**: Space-separated list of session names in MRU order (e.g. `"worker dev main"`).

### Examples

#### One-Line Switcher with `fzf`

The following configuration switches sessions using `fzf`:

```toml
# ~/.config/ttyman/config.toml
[menu]
key = 0x1D
command = "ttyman list --json | jq -r '.[].name' | fzf | sed 's/^/attach /'"
```

#### Advanced Menu Script

[`examples/menu`](examples/menu) implements a feature-rich interactive menu using `fzf` with live ANSI screen previews and support for switching, creating, renaming, killing, and detaching sessions:

```bash
mkdir -p ~/.config/ttyman
curl -fsSL https://raw.githubusercontent.com/hayatoito/ttyman/main/examples/menu -o ~/.config/ttyman/menu
chmod +x ~/.config/ttyman/menu
```

---

## Configuration (`~/.config/ttyman/config.toml`)

```toml
# ~/.config/ttyman/config.toml

[menu]
key = 0x1D                              # Trigger key byte (default: Ctrl-] / 0x1D)
command = "~/.config/ttyman/menu"       # Command executed on menu key press (default: "echo detach")

[session]
scrollback = 20000                      # Scrollback lines retained (default: 10,000)
```

---

## Environment Variables

### Exported in Session

- **`TTYMAN_SESSION`**: Name of the current session. Subcommands target this session when `-s` is omitted.
- **`TTYMAN_PID`**: Process ID of the session supervisor.

### Exported to `menu.command`

- **`TTYMAN_RECENT_SESSIONS`**: Space-separated list of session names in MRU order (e.g. `"worker dev main"`).

### Configuration & Runtime Dirs

- **`XDG_CONFIG_HOME`**: Base configuration directory (defaults to `~/.config`). Configuration file is loaded from `$XDG_CONFIG_HOME/ttyman/config.toml`.
- **`XDG_RUNTIME_DIR`**: Directory for Unix domain sockets (typically `/run/user/<UID>`). If unset, `ttyman` creates and uses `/tmp/ttyman-<UID>` with `0700` permissions.

---

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
