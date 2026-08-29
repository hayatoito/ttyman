use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::libc;
use nix::pty::{OpenptyResult, Winsize, openpty};
use nix::unistd::isatty;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};

pub struct StdinRawFd(pub RawFd);

impl AsRawFd for StdinRawFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl AsFd for StdinRawFd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        unsafe { BorrowedFd::borrow_raw(self.0) }
    }
}

pub struct RawGuard {
    orig_flags: Option<i32>,
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        if let Some(fl) = self.orig_flags {
            let _ = fcntl(
                unsafe { BorrowedFd::borrow_raw(0) },
                FcntlArg::F_SETFL(OFlag::from_bits_truncate(fl)),
            );
        }
    }
}

impl RawGuard {
    pub fn enter() -> io::Result<Self> {
        let stdin_fd = unsafe { BorrowedFd::borrow_raw(0) };
        let orig_stdin_flags = fcntl(stdin_fd, FcntlArg::F_GETFL).ok();
        if let Some(fl) = orig_stdin_flags {
            let _ = fcntl(
                stdin_fd,
                FcntlArg::F_SETFL(OFlag::from_bits_truncate(fl) | OFlag::O_NONBLOCK),
            );
        }

        if isatty(stdin_fd).unwrap_or(false) {
            let _ = enable_raw_mode();
        }

        Ok(Self {
            orig_flags: orig_stdin_flags,
        })
    }
}

pub fn get_terminal_winsize(fd: RawFd) -> Option<Winsize> {
    let mut ws = Winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) == 0 {
            Some(ws)
        } else {
            None
        }
    }
}

pub fn set_terminal_winsize(fd: RawFd, ws: &Winsize) {
    unsafe {
        let _ = libc::ioctl(fd, libc::TIOCSWINSZ, ws);
    }
}

pub fn open_pty_pair(winsize: Option<&Winsize>) -> nix::Result<OpenptyResult> {
    let pty_res = openpty(winsize, None)?;
    let flags = fcntl(&pty_res.master, FcntlArg::F_GETFL)?;
    let _ = fcntl(
        &pty_res.master,
        FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
    );
    Ok(pty_res)
}

/// Configures controlling terminal, redirects standard I/O (0, 1, 2) to the PTY slave,
/// sets session environment variables, and executes the specified command.
/// This function never returns on success, and exits with code 127 on failure.
pub fn exec_in_child_pty(
    slave_fd: std::os::fd::OwnedFd,
    session_name: &str,
    daemon_pid: u32,
    exec_cmd: &str,
    exec_args: &[String],
) -> ! {
    let _ = nix::unistd::setsid();
    let slave_raw = slave_fd.as_raw_fd();
    unsafe {
        libc::ioctl(slave_raw, libc::TIOCSCTTY as _, 0);
        libc::dup2(slave_raw, 0);
        libc::dup2(slave_raw, 1);
        libc::dup2(slave_raw, 2);
    }
    drop(slave_fd);

    unsafe {
        std::env::set_var(crate::ipc::DEFAULT_SESSION_VAR, session_name);
        std::env::set_var("TTYMAN_PID", daemon_pid.to_string());
    }

    let c_cmd = std::ffi::CString::new(exec_cmd).unwrap_or_default();
    let c_args: Vec<std::ffi::CString> = exec_args
        .iter()
        .map(|a| std::ffi::CString::new(a.as_str()).unwrap_or_default())
        .collect();

    let _ = nix::unistd::execvp(&c_cmd, &c_args);
    std::process::exit(127);
}
