# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1] - 2026-09-03

### Added
- **`ttyman completion` subcommand**: Generate shell completion scripts for `bash`, `zsh`, `fish`, `powershell`, and `elvish`.
- **Dynamic session name completion**: Real-time auto-completion of active session names for `-s, --session` on Zsh, Bash, and Fish.

### Fixed
- **Test environment isolation**: Strip session environment variables (`$TTYMAN_SESSION`, `$TTYMAN_PID`) in tests to ensure test suites run cleanly inside active sessions.

## [0.2.0] - 2026-08-29

### Added
- **`ttyman kill` subcommand**: Terminate active sessions cleanly by name (`ttyman kill -s <NAME>`).
- **Current session indicator**: `ttyman list` marks the current active session with `*` (matching `$TTYMAN_SESSION`).
- **Signal broadcast**: Broadcast termination notifications (`SIGTERM`, `SIGHUP`, `SIGINT`) to attached clients.
- **Scrollback history inspection**: `ttyman read -a` (`--all`) to retrieve the entire scrollback history.

### Changed
- **Unified session CLI**: Standardized on `-s, --session <SESSION>` across all subcommands, automatically defaulting to `$TTYMAN_SESSION` when inside a session.
- **Byte-transparent passthrough**: Direct, zero-latency byte forwarding between client and session PTY.
- **Dedicated server module**: Centralized PTY management, socket IPC server, and VT100 virtual terminal rendering in `server.rs`.

### Removed
- **Key remapping (`remap`)**: Removed internal keystroke remapping in favor of direct passthrough.
- **Legacy subcommands**: Removed `ttyman run`, `record`, and `play`.
- **`PERSIST` column**: Removed from `ttyman list` as all sessions run uniformly in the background.
- **Dependencies**: Removed `thiserror`, `proptest`, and `zstd`.

## [0.1.0] - 2026-08-20

### Added
- Initial release of `ttyman`.
- Core subcommands: `attach`, `start`, `list`, `read`, `watch`, `write`, `rename`.
- VT100 virtual terminal emulation and live Unix domain socket IPC.
- Interactive menu delegation via `fzf`.
