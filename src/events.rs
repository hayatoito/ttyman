//! Session event detection for `watch --exec`.
//!
//! One detector runs per session daemon and broadcasts [`SessionEvent`]s to every
//! `SubscribeEvents` client, so clients do not need access to the session's processes.

use crate::ipc::SessionEvent;
use crate::server::SessionSharedState;
use crate::terminal::{NewText, Terminal};
use nix::libc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, oneshot};
use tokio::time::{Instant, MissedTickBehavior};

/// How often the detector checks whether to send an event.
const TICK: Duration = Duration::from_millis(200);
/// Quiet time after a completed line before an event is sent.
const SETTLE: Duration = Duration::from_millis(500);
/// The longest a completed line waits for an event, so output that never pauses (`tail -f` on a
/// busy log) is still reported.
const MAX_WAIT: Duration = Duration::from_secs(10);

/// Watches the session and sends an event when a line was completed and output then paused for
/// [`SETTLE`] (or kept going for [`MAX_WAIT`]): `done` if the shell is in the foreground (a
/// command finished, or waits for input), `output` if a command is still running. Sends `exit`
/// when `shutdown` fires (the session ended). `output_rx` carries the PTY output after it was fed
/// to `terminal`.
pub async fn run_event_detector(
    terminal: Terminal,
    state: Arc<SessionSharedState>,
    mut output_rx: broadcast::Receiver<Vec<u8>>,
    events_tx: broadcast::Sender<SessionEvent>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let emit = |event: &str, new_text: &mut NewText| {
        let _ = events_tx.send(SessionEvent {
            event: event.to_string(),
            text: new_text.take(),
        });
    };
    let mut new_text = NewText::new(&terminal);
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // When a line was first completed since the last event (see `NewText::has_new_line`), if any.
    // Editing a command line (even with autosuggestions, which step down a line and back up)
    // completes no line.
    let mut new_line_at: Option<Instant> = None;
    let mut last_output = Instant::now();

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                emit("exit", &mut new_text);
                return;
            }
            chunk = output_rx.recv() => match chunk {
                Ok(c) => {
                    new_text.push(&c);
                    if new_line_at.is_none() && new_text.has_new_line() {
                        new_line_at = Some(Instant::now());
                    }
                    last_output = Instant::now();
                }
                Err(RecvError::Lagged(_)) => {
                    new_text.mark_truncated();
                    new_line_at.get_or_insert_with(Instant::now);
                    last_output = Instant::now();
                }
                Err(RecvError::Closed) => {
                    emit("exit", &mut new_text);
                    return;
                }
            },
            _ = ticker.tick() => {
                let Some(since) = new_line_at else {
                    continue;
                };
                // Full-screen programs (vim, less, top) are skipped: their redraws are not lines.
                if terminal.alternate_screen() {
                    continue;
                }
                if last_output.elapsed() >= SETTLE || since.elapsed() >= MAX_WAIT {
                    let event = if shell_is_busy(&state) == Some(true) { "output" } else { "done" };
                    emit(event, &mut new_text);
                    new_line_at = None;
                }
            }
        }
    }
}

/// True if a process group other than the shell's owns the terminal foreground, i.e. a command
/// started from the prompt is running. The shell leads its own process group (it called
/// `setsid()`), so its pgid equals its pid. None if unknown (not started yet, or the PTY closed).
fn shell_is_busy(state: &SessionSharedState) -> Option<bool> {
    let shell = state.child_pid.load(Ordering::SeqCst);
    if shell <= 0 {
        return None;
    }
    // SAFETY: tcgetpgrp only queries the fd; an invalid fd returns -1.
    let foreground = unsafe { libc::tcgetpgrp(state.master_raw_fd) };
    if foreground <= 0 {
        return None;
    }
    Some(foreground != shell)
}
