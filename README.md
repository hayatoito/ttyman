# ttyman

*Terminal session manager to persist and attach sessions, inspect screens and scrollback, stream live output, and inject input via IPC.*

[![Crates.io](https://img.shields.io/crates/v/ttyman.svg)](https://crates.io/crates/ttyman)
[![CI](https://github.com/hayatoito/ttyman/actions/workflows/ci.yml/badge.svg)](https://github.com/hayatoito/ttyman/actions)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](https://github.com/hayatoito/ttyman)

`ttyman` wraps any shell or command in an invisible PTY with a dedicated UNIX domain socket.

- ⚡ **No UI**: No status bars or window splits. Terminal emulator features (search, scrollback buffer, mouse selection) work directly without interference.
- 📦 **No Central Daemon**: No global server daemon or shared state across sessions. Each session is an independent, isolated 1-to-1 process.
- 🍱 **Bring Your Own Menu**: No rigid, built-in TUI. Plug in `fzf`, `rofi`, or any custom script for interactive session switching, live preview, and renaming.
- 🤖 **Script & Agent Friendly**: Inspect screens, stream live output, or inject input directly from outside.

Through its UNIX domain socket IPC, you can:
- 🔌 **Attach**: Connect interactively to a session, with background persistence and detach support (`ttyman attach`)
- 🏷️ **Rename**: Rename an active running session (`ttyman rename`)
- 📋 **List**: Discover active sessions and inspect persistence and connected clients (`ttyman list`)
- 📖 **Read**: Inspect clean screen snapshots or full scrollback history (`ttyman read`)
- 📺 **Watch**: Stream live session output in real-time (`ttyman watch`)
- ⌨️ **Write**: Inject arbitrary byte sequences (keystrokes, commands, and bracketed paste) from outside (`ttyman write`)

It also includes utilities to record and replay timed terminal streams in `.ttyrec` format (`ttyman record` / `ttyman play`).

---

### Installation

#### Prebuilt Binary (Linux & macOS)

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/hayatoito/ttyman/releases/latest/download/ttyman-installer.sh | sh
```

#### Using Cargo

```bash
cargo install ttyman
# => Installs the `ttyman` executable to ~/.cargo/bin/ttyman
```

---

### Quick Tutorial: Create, Control, and Detach a Session

Open two terminal windows side-by-side:

#### Terminal 1: Start, Detach, and Reattach

Create and attach to a persistent session named `main` in one command:

```bash
# 1. Attach to session "main" (automatically spawns in background if missing):
$ ttyman attach -s main

# 2. Run any command or interactive tool:
$ top

# 3. Open the interactive menu (or detach) using the menu key:
# Press Ctrl-] (select "detach" or another session to switch)
[ttyman: detached]

# 4. Your terminal is back to your local shell, while `top` continues running in the background!

# 5. Reattach at any time to resume interacting:
$ ttyman attach -s main
# => You are back inside the session, with `top` still running continuously!
```

#### Terminal 2: Inspect and Control from Outside

From a separate terminal window, background script, or AI agent:

```bash
# 1. Discover active sessions (note CLIENTS: 1 when attached, 0 when detached)
$ ttyman list
NAME               PID      COMMAND                  SIZE       PERSIST   CLIENTS   AGE
main               12345    /usr/bin/zsh -i          120x40     yes       1         10s

# 2. Read the clean screen snapshot of the "main" session (even when detached!)
$ ttyman read -s main
top - 17:50:00 up 2 days,  1:23,  2 users,  load average: 0.15, 0.08, 0.05
Tasks: 312 total,   1 running, 311 sleeping,   0 stopped,   0 zombie
... (displays the rendered `top` screen snapshot)

# 3. Inject "q" keystroke into the session to quit top
$ ttyman write -s main "q"
# => `top` immediately exits back to the shell prompt!
```

---

### Session Management

#### Starting in Background (`ttyman start`)

`ttyman start` spawns a new session in the background (detached daemon) and immediately returns to your prompt:

```bash
# Start a background session named "worker" (runs default interactive $SHELL):
$ ttyman start -s worker

# Start a background session with a specific command:
$ ttyman start -s build -- cargo watch -x test
```

#### Attaching Interactively (`ttyman attach`)

`ttyman attach` connects your current terminal interactively to a session with full bidirectional I/O. If the target session does not exist, it **automatically spawns it**:

```bash
# Attach to a session (auto-spawns interactive $SHELL if not already running):
$ ttyman attach -s main

# Quick start a new session (creates ttyman-<PID> and attaches immediately):
$ ttyman attach
```

*(Note: Command arguments after `--` are executed when **spawning** a new session; connecting to an already running session ignores trailing command arguments).*

#### Renaming Sessions (`ttyman rename`)

`ttyman rename` renames an active session on the fly:

```bash
# Rename current session from inside its terminal:
$ ttyman rename new-name

# Rename a specific session from outside:
$ ttyman rename -s old-name new-name
```

#### Escape Key & "Bring Your Own Menu" (BYOM) (`menu_key`)

`ttyman` embraces a **"Bring Your Own Menu" (BYOM)** philosophy. Rather than baking in an opinionated, rigid TUI switcher, `ttyman` provides a single configurable `menu_key` (**`Ctrl-]`** / `0x1D`) that delegates to your choice of external menu command (e.g. [`examples/menu.sh`](examples/menu.sh) powered by `fzf`, `rofi`, or any shell script), giving you complete control over your session switching, screen previews, and keybindings.

- **Default Behavior**: Press **`Ctrl-]`** (ASCII `0x1D`) anywhere (even inside fullscreen apps like `vim` or `top`!) to cleanly detach from the session (default command: `echo detach`).
- **Bring Your Own Menu Protocol**: When configured with an interactive selector like `fzf` or a custom script:
  - **`detach`**: Cleanly detaches from the session (the session continues running in the background).
  - **`attach:<name>`** (or **`switch:<name>`** / **`<name>`**): Seamlessly switches your active terminal connection to session `<name>`.
  - **`ESC` / Cancel**: Exits the menu and cleanly restores your current terminal screen with zero phantom input.
- **Session Nesting Prevention & Auto-Background Spawn**:
  - Running `ttyman attach` targeting the *current* session from within its own terminal is blocked to prevent recursive feedback loops.
  - Running `ttyman attach -s <other>` from *inside* an active session automatically starts `<other>` in the background and prompts you to switch with `Ctrl-]`, preventing accidental nested terminal multiplexing.
- **Configuration (`~/.config/ttyman/config.toml` or `-c config.toml`)**:
  ```toml
  [menu]
  # Key to trigger the escape menu / detach (default: "0x1D" / Ctrl-])
  # Use "none" to pass 100% of keystrokes without interception
  key = "0x1D"

  # Shell command executed on menu key (default: "echo detach")
  # Use the bundled interactive manager script with live preview and new session creation:
  command = "~/.config/ttyman/menu.sh"
  ```
- **Window Resizing**: Automatically synchronizes terminal window size (`SIGWINCH`) with the remote PTY (pass `--no-resize` to disable).

> [!TIP]
> **Why Raw Byte Sequences Instead of String Syntax?**
>
> Keyboard notation across different tools is fragmented (`"C-]"`, `"Control-]"`, `"^]"`). By using raw byte sequences (`0x1D,0x64` or `0x10,0x11`), `ttyman` avoids naming ambiguities and can represent any arbitrary key sequence or escape chord. Run `man ascii` in your terminal to look up standard byte codes.

#### Listing Active Sessions (`ttyman list`)

Discover all active sessions and their real-time status:

```bash
$ ttyman list
NAME               PID      COMMAND                  SIZE       PERSIST   CLIENTS   AGE
main               12345    /usr/bin/zsh -i          120x40     yes       1         10s
worker             12399    ./long_task.sh           80x24      yes       0         2m
```

- **Output formats & Maintenance**:
  ```bash
  # Output session list as JSON:
  $ ttyman list --json

  # Automatically remove dead / stale socket files:
  $ ttyman list --clean
  ```

#### How It Works Without a Central Daemon

Unlike traditional terminal multiplexers that run a monolithic background server daemon, `ttyman` has **no central daemon** and maintains **no global state**.

- **Filesystem Discovery**: Whenever a session starts, `ttyman` creates an isolated UNIX domain socket at `$XDG_RUNTIME_DIR/ttyman-<NAME>`. `ttyman list` scans `$XDG_RUNTIME_DIR` for `ttyman-*` socket files and queries each socket directly via IPC.
- **Session Name Resolution (`-s <NAME>`)**: Specifying `-s <NAME>` targets `$XDG_RUNTIME_DIR/ttyman-<NAME>`. Session names are strictly validated (`[a-zA-Z0-9_.-]`, up to 64 characters, no path separators).
- **When `-s` is Omitted**:
  - **Inside a session**: Every session exports `$TTYMAN_SESSION`. Subcommands detect `$TTYMAN_SESSION` and operate on the current session without `-s`.
  - **Outside a session**: `ttyman attach` or `ttyman run` creates a new session identified by its own Process ID (`ttyman-<PID>`).

---

### Observing & Controlling Sessions (IPC)

Every session exposes a dedicated IPC socket, allowing external scripts, monitoring tools, or AI agents to inspect and interact with the terminal in real time.

#### 📖 Reading Screen & Scrollback History (`ttyman read`)

Capture clean rendered text (VT100 escape sequences processed) or full scrollback history from any session:

```bash
# Read visible screen snapshot of an external session:
$ ttyman read -s main

# Read the last 20 lines with ANSI color escape sequences preserved:
$ ttyman read -s main -n 20 --ansi | less -R

# Read entire scrollback history from the start of the session:
$ ttyman read -s main -a
```

##### Self-Inspection from Inside a Session
When running inside a `ttyman` session, `ttyman read` automatically detects `$TTYMAN_SESSION` without `-s`:

```bash
# Search terminal history with fzf:
$ ttyman read -a | fzf

# Search terminal output for errors:
$ ttyman read -a | grep -E "ERROR|FATAL|panic"

# Feed the current terminal screen to an LLM / AI tool:
$ ttyman read | llm "Explain the error on screen and suggest a fix"
```

#### 📺 Live Session Streaming (`ttyman watch`)

`ttyman watch` acts like a **read-only `attach`**: it streams live terminal output in real time without sending any input to the session.

- **Safe Observation**: Keystrokes are not forwarded to the target session. Pressing **`Ctrl-C`** simply exits `watch` and returns to your shell prompt without interrupting the running program.
- **AI Agent Live Pairing**: Ideal for letting background AI agents, scripts, or sidecar tools monitor your terminal workflow in real time and provide proactive feedback.

```bash
# Watch an active session in real time:
$ ttyman watch -s main

# Filter live session stream for errors in real time:
$ ttyman watch -s main | grep -E "ERROR|FATAL"
```

> [!TIP]
> **Example Prompt for AI Agents:**
> *"Monitor my terminal workflow via `ttyman watch -s main` and explain any compiler errors or test failures as they appear."*

#### ⌨️ Input & Command Injection (`ttyman write`)

Inject arbitrary byte sequences, keystrokes, or full commands into a running session from outside:

```bash
# Send a single keystroke (e.g. "q" to quit top or vim):
$ ttyman write -s main "q"

# Submit a shell command atomically with Enter (-E, --enter):
$ ttyman write -s main -E "git status"

# Inject text wrapped in bracketed paste sequences:
$ ttyman write -s main --bracketed-paste < script.py
```

---

### Configuration (`~/.config/ttyman/config.toml`)

`ttyman` automatically loads `$XDG_CONFIG_HOME/ttyman/config.toml` (or `~/.config/ttyman/config.toml`) on startup if present. You can also pass a custom configuration file with `-c, --config <PATH>`.

```toml
# ============================================================
# [menu] - Escape key and interactive switcher control
# ============================================================
[menu]
key = "0x1D"        # Key to trigger menu (default: 0x1D / Ctrl-])
command = "echo detach" # Shell command executed on menu key

# Example for interactive selector with fzf:
# command = "(echo 'detach'; ttyman list --json | jq -r '\"attach:\" + .[].name') | fzf --prompt='ttyman > '"

# ============================================================
# [[remap]] - Client-side input byte transformation rules
# ============================================================
# Remap Ctrl-b (0x02) to Left Arrow (\e[D)
[[remap]]
from = [0x02]
to   = [0x1b, 0x5b, 0x44]

# Remap Ctrl-f (0x06) to Right Arrow (\e[C)
[[remap]]
from = [0x06]
to   = [0x1b, 0x5b, 0x43]

# Multi-stroke chord: Ctrl-x Ctrl-f (0x18, 0x06) sends "fzf\n"
[[remap]]
from = [0x18, 0x06]
to   = [0x66, 0x7a, 0x66, 0x0a]

# ============================================================
# [session] - Daemon / session instance defaults
# ============================================================
[session]
scrollback = 10000  # Number of scrollback history lines retained (default: 10,000)
```

```bash
# Automatically loads ~/.config/ttyman/config.toml if present:
$ ttyman attach -s main

# Or specify a custom config file explicitly:
$ ttyman attach -s main --config custom_keys.toml
```

---

### Foreground Process Wrapping & Scripting (`ttyman run`)

While `ttyman attach` is designed for persistent interactive sessions, `ttyman run` is a **synchronous process wrapper** designed for scripts, Makefiles, and CI pipelines:

```bash
# Wrap an interactive shell ($SHELL) in the foreground with an IPC socket:
$ ttyman run

# Run a specific command with a named session socket:
$ ttyman run -s build -- cargo test

# Pass through exact exit code to parent shell:
$ ttyman run -- cargo test || { echo "Tests failed!"; exit 1; }

# Non-TTY pipe mode:
$ cat data.txt | ttyman run -T never -- ./processor
```

> [!NOTE]
> **When to Use `ttyman attach` vs `ttyman run` (Exit Codes & Persistence)**
>
> | Command | Mental Model | Exit Code | Detachable (`Ctrl-]`) | Primary Use Case |
> | :--- | :--- | :--- | :--- | :--- |
> | **`ttyman attach`** | **Persistent Workspace** | Client connection exit code (0) | ⭕️ **Yes** (survives SSH / terminal close) | Interactive development, long-running builds, AI agent pairing |
> | **`ttyman run`** | **Synchronous Process Wrapper** | **Passes through exact command exit code** | ❌ No (direct foreground child) | Shell scripts, CI pipelines, Makefiles, non-TTY pipes (`-T never`) |
>
> - **Full IPC Capabilities**: `ttyman run` exposes the exact same UNIX domain socket IPC. From other terminals or AI agents, you can `ttyman watch`, `ttyman read`, `ttyman write`, or `ttyman attach` on a running `ttyman run` process.
> - **Lifecycle & Detach**: In the foreground terminal running `ttyman run`, you cannot detach with `Ctrl-]` (all keystrokes are passed directly to the child command). If that terminal emulator or SSH connection closes, the session itself terminates. Use `ttyman attach` whenever you want background persistence that survives terminal disconnects.

---

### Recording & Playback (`.ttyrec` / `ttyplay` Format)

`ttyman` supports the timed frame format used by [ttyrec](https://0xcc.net/ttyrec/) and `ttyplay`. Files recorded with `ttyman record` can be played back with `ttyplay` (and vice versa).

#### Recording Command Pipelines
Because `ttyman record` reads from `stdin`, you can record any command output stream:

```bash
# Record command output and timing:
$ cargo test | ttyman record test_run.ttyrec

# Record a build script:
$ ./deploy.sh | ttyman record deploy.ttyrec
```

#### Recording Interactive Terminal Sessions
Running `ttyman run` piped to `ttyman record` is equivalent to `ttyrec session.ttyrec`:

```bash
# Record an interactive terminal session to a file (equivalent to `ttyrec session.ttyrec`):
$ ttyman run | tee >(ttyman record session.ttyrec)
```

#### Recording Live Sessions (via `watch`)
Stream an active session and record it directly:

```bash
$ ttyman watch -s main | ttyman record session.ttyrec
```

#### Live Terminal Broadcasting via FIFO (Without IPC)
You can also stream an interactive session across terminal windows through a standard Unix FIFO (named pipe):

```bash
# Terminal 1 (Broadcaster):
$ mkfifo "$XDG_RUNTIME_DIR/live.pipe"
$ ttyman run | tee >(ttyman record "$XDG_RUNTIME_DIR/live.pipe")

# Terminal 2 (Spectator - plays live stream in real-time as bytes arrive):
$ ttyman play "$XDG_RUNTIME_DIR/live.pipe"
```
*(Because FIFOs block until data arrives, `--follow` is not needed).*

#### Playback & Duration Inspection
Recorded files can be played back with speed controls:

```bash
# Replay recorded session:
$ ttyman play session.ttyrec

# Replay at 2x speed:
$ ttyman play -s 2.0 session.ttyrec

# Print total duration of recorded session without playing:
$ ttyman play -t session.ttyrec
```

#### Following an Active Recording (`play --follow`)
`ttyman play -f` behaves like `tail -f`, keeping playback open and waiting for new frames:

```bash
# Terminal 1 (Spectator - watches live build as frames arrive):
$ ttyman play -f build.ttyrec

# Terminal 2 (Runner - appends successive commands):
$ cargo test --color=always   | ttyman record -a build.ttyrec
$ cargo clippy --color=always | ttyman record -a build.ttyrec
$ ./deploy.sh                 | ttyman record -a build.ttyrec
```

---

### Subcommands

> **Targeting Sessions (`-s, --session <SESSION>`)**:
> Commands that communicate with or create a session (`attach`, `run`, `read`, `watch`, `write`, `rename`) accept `-s, --session <SESSION>`.
> - **By Name**: Specifying a name (e.g. `-s main`) resolves to `$XDG_RUNTIME_DIR/ttyman-main`.
> - **Inside a Session**: If omitted, `-s` automatically defaults to the `$TTYMAN_SESSION` environment variable.

#### `ttyman attach`
```text
Attach interactively to a session (spawns if not already running)

Usage: ttyman attach [OPTIONS] [COMMAND]...

Arguments:
  [COMMAND]...  Command to execute if creating a new session (defaults to interactive $SHELL)

Options:
      --no-resize          Do not synchronize terminal window size on attach or resize
  -c, --config <CONFIG>    Path to TOML configuration file if spawning a new session
  -s, --session <SESSION>  Target session name (defaults to $TTYMAN_SESSION)
  -h, --help               Print help
```

#### `ttyman start`
```text
Start a session in the background (detached daemon)

Usage: ttyman start [OPTIONS] [COMMAND]...

Arguments:
  [COMMAND]...  Command to execute in session (defaults to interactive $SHELL)

Options:
  -c, --config <CONFIG>    Path to TOML configuration file
  -s, --session <SESSION>  Target session name
  -h, --help               Print help
```

#### `ttyman rename`
```text
Rename an active session

Usage: ttyman rename [OPTIONS] <NEW_NAME>

Arguments:
  <NEW_NAME>  New session name

Options:
  -s, --session <SESSION>  Target session name (defaults to $TTYMAN_SESSION)
  -h, --help               Print help
```

#### `ttyman list`
```text
List active sessions and inspect socket status

Usage: ttyman list [OPTIONS]

Options:
      --json   Output session list formatted as JSON
      --clean  Automatically remove dead / stale socket files
  -h, --help   Print help
```

#### `ttyman read`
```text
Read screen snapshot or scrollback history from a running session

Usage: ttyman read [OPTIONS]

Options:
  -n, --lines <LINES>              Number of recent lines to read (reads visible screen if omitted)
  -a, --all                        Read entire scrollback history from the start of the session
      --ansi                       Preserve ANSI color and style escape sequences
  -s, --session <SESSION>          Target session name (defaults to $TTYMAN_SESSION)
  -h, --help                       Print help
```

#### `ttyman watch`
```text
Watch and stream a live running session in real-time

Usage: ttyman watch [OPTIONS]

Options:
  -s, --session <SESSION>          Target session name (defaults to $TTYMAN_SESSION)
  -h, --help                       Print help
```

#### `ttyman write`
```text
Write text or inject commands into a running session

Usage: ttyman write [OPTIONS] [TEXT]

Arguments:
  [TEXT]  Text to write (reads from standard input if omitted or '-')

Options:
  -E, --enter                      Append Enter (newline) after text (submits command atomically)
      --bracketed-paste            Wrap text in bracketed-paste sequences (\x1b[200~ ... \x1b[201~)
  -s, --session <SESSION>          Target session name (defaults to $TTYMAN_SESSION)
  -h, --help                       Print help
```

#### `ttyman run`
```text
Run a command or interactive shell in a foreground PTY proxy with an IPC socket

Usage: ttyman run [OPTIONS] [COMMAND]...

Arguments:
  [COMMAND]...  Command to execute (defaults to interactive $SHELL)

Options:
  -s, --session <SESSION>  Target session name
  -c, --config <CONFIG>    Path to TOML configuration file (e.g. input remapping)
  -T, --term <TERM_MODE>   Terminal mode: 'never', 'auto', or 'always' [default: auto]
  -h, --help               Print help
```

#### `ttyman record`
```text
Record stdin stream into a .ttyrec-compatible format

Usage: ttyman record [OPTIONS] [FILE]

Arguments:
  [FILE]  Output file to write to (writes to stdout if omitted or '-')

Options:
  -a, --append  Open output file in append mode instead of overwrite mode
  -h, --help    Print help
```

#### `ttyman play`
```text
Play back a recorded .ttyrec file (or inspect duration with --time)

Usage: ttyman play [OPTIONS] [FILES]...

Arguments:
  [FILES]...  Files to play or inspect (reads from stdin if omitted or '-')

Options:
  -s, --speed <SPEED>  Playback speed multiplier (e.g. 1.0, 2.0, 0.5) [default: 1]
  -n, --no-wait        No-wait mode (play immediately without delay)
  -f, --follow         Follow mode (tail growing file)
  -t, --time           Inspect and print total duration of recorded session(s) without playing
  -h, --help           Print help
```

---

### Environment Variables

#### Exported Inside a Session
When a command or shell runs inside a `ttyman` session, the following environment variables are automatically exported to all child processes:

- **`TTYMAN_SESSION`**: The name of the current session (e.g. `main`). Subcommands (`read`, `write`, `watch`, `attach`, `rename`) automatically detect this variable when `-s` is omitted inside a session.
- **`TTYMAN_PID`**: The process ID (PID) of the session supervisor process managing the current PTY.

#### Exported to `menu.command` ("Bring Your Own Menu")
When interactive `menu.command` (e.g., `menu.sh`) is triggered by the menu escape key (default `Ctrl-]`):

- **`TTYMAN_SESSION`**: The name of the currently active session.
- **`TTYMAN_RECENT_SESSIONS`**: Space-separated list of session names in Most-Recently-Used (MRU / LRU) order visited by this client (e.g. `"worker dev main"`). Useful for sorting switcher menus or implementing quick toggle to the previous session.

#### Respected by `ttyman`
- **`XDG_CONFIG_HOME`**: Base directory for configuration files (`ttyman` automatically loads `$XDG_CONFIG_HOME/ttyman/config.toml` or `~/.config/ttyman/config.toml` on startup if present).
- **`XDG_RUNTIME_DIR`**: The base runtime directory where session UNIX domain sockets (`ttyman-*`) are created and discovered by `ttyman list` (defaults to `$XDG_RUNTIME_DIR` if set, e.g. `/run/user/<UID>`, or safely falls back to `/tmp/ttyman-<UID>` with mode `0700` if unset). Sockets are stored with mode `0600` inside this user-isolated directory.
- **`SHELL`**: The default interactive shell launched when creating a new session without specifying command arguments (falls back to `/bin/sh` if unset).

---

### License

Licensed under either of:

- Apache License, Version 2.0 (<http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license (<http://opensource.org/licenses/MIT>)

at your option.

License: MIT OR Apache-2.0
