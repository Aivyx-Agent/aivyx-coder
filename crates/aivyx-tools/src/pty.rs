//! Raw pseudo-terminal allocation via `libc`, consistent with this
//! crate's existing raw-syscall precedent (`process.rs`'s
//! `kill_process_group`) — no new dependency for something four libc
//! calls already cover. See `docs/superpowers/specs/
//! 2026-07-31-repl-pty-design.md`'s "PTY allocation" decision.

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// Allocates a fresh pty pair via the standard POSIX `posix_openpt`/
/// `grantpt`/`unlockpt`/`ptsname_r` sequence. Both fds are already open
/// when this returns, so a caller that wires the *slave* as a child's
/// stdio never needs that child to `open()` any `/dev/pts/*` path itself
/// — this is why pty-based `repl_start` needs no new Landlock grant at
/// all (Landlock governs new path-open syscalls, not inherited fds).
pub(crate) fn open_pty() -> io::Result<(OwnedFd, OwnedFd)> {
    // SAFETY: `posix_openpt` is a standard libc call; a negative return
    // is its documented error signal, checked immediately below.
    let master_fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
    if master_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `master_fd` was just checked non-negative — a valid,
    // freshly-opened fd this function uniquely owns from here on.
    let master = unsafe { OwnedFd::from_raw_fd(master_fd) };

    // SAFETY: both calls are standard libc functions taking a valid pty
    // master fd; each returns non-zero on error, checked immediately.
    if unsafe { libc::grantpt(master.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::unlockpt(master.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }

    let mut name_buf = [0i8; 128];
    // SAFETY: `ptsname_r` writes a NUL-terminated path into `name_buf`
    // (sized well beyond any real `/dev/pts/N` path) and returns 0 on
    // success, checked immediately below.
    let rc =
        unsafe { libc::ptsname_r(master.as_raw_fd(), name_buf.as_mut_ptr(), name_buf.len()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `ptsname_r`'s success (checked above) guarantees
    // NUL-termination within `name_buf`.
    let slave_path = unsafe { CStr::from_ptr(name_buf.as_ptr()) };

    // SAFETY: `slave_path` is a valid NUL-terminated C string from
    // `ptsname_r` above; `open` returns -1 on error, checked immediately.
    let slave_fd = unsafe { libc::open(slave_path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
    if slave_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `slave_fd` was just checked non-negative.
    let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };

    Ok((master, slave))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_pty_returns_two_distinct_valid_fds() {
        let (master, slave) = open_pty().unwrap();
        assert_ne!(master.as_raw_fd(), slave.as_raw_fd());
        // `fcntl(F_GETFD)` fails with EBADF on an invalid fd — a cheap
        // liveness check for both fds.
        assert!(unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFD) } >= 0);
        assert!(unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_GETFD) } >= 0);
    }

    #[test]
    fn master_winsize_round_trips_through_an_ioctl_set_and_get() {
        let (master, _slave) = open_pty().unwrap();
        let set = libc::winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &set) },
            0
        );

        let mut got: libc::winsize = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGWINSZ, &mut got) },
            0
        );
        assert_eq!(got.ws_row, 40);
        assert_eq!(got.ws_col, 120);
    }
}
