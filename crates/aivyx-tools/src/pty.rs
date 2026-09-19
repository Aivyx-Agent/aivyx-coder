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
    let master_fd =
        unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
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

    // `ptsname_r` (POSIX's reentrant form) doesn't exist on Apple targets
    // — `libc` 0.2 only declares it for `linux_like`, freebsd, and netbsd.
    // Apple only has the older, non-reentrant `ptsname(3)`, so the two
    // platforms need separate lookups; both end up as an owned `CString`
    // so the rest of this function doesn't care which path ran.
    #[cfg(target_os = "linux")]
    let slave_path: std::ffi::CString = {
        // `libc::c_char` (NOT a hardcoded `i8`) — it's `i8` on Apple and
        // x86_64 Linux but `u8` on `aarch64-unknown-linux-gnu`, so a
        // hardcoded width silently breaks on that target.
        let mut name_buf = [0 as libc::c_char; 128];
        // SAFETY: `ptsname_r` writes a NUL-terminated path into `name_buf`
        // (sized well beyond any real `/dev/pts/N` path) and returns 0 on
        // success, checked immediately below.
        let rc =
            unsafe { libc::ptsname_r(master.as_raw_fd(), name_buf.as_mut_ptr(), name_buf.len()) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `ptsname_r`'s success (checked above) guarantees
        // NUL-termination within `name_buf`; copied into owned memory
        // before `name_buf` goes out of scope.
        unsafe { CStr::from_ptr(name_buf.as_ptr()) }.to_owned()
    };

    #[cfg(not(target_os = "linux"))]
    let slave_path: std::ffi::CString = {
        // `ptsname(3)` (unlike `ptsname_r`) is NOT reentrant: it returns a
        // pointer into a static buffer owned by libc that the next
        // `ptsname`/`ptsname_r` call on *any* thread may overwrite. We
        // copy it into an owned `CString` immediately below, with no
        // other libc call in between, so the copy is unaffected by later
        // reuse of that buffer. `open_pty()` itself holds no lock, so two
        // threads calling it concurrently could still race inside libc's
        // static storage between the `ptsname` call and this copy; that's
        // out of scope today because nothing in this crate calls
        // `open_pty()` from more than one thread at a time (each REPL
        // session opens its pty serially during setup), but a future
        // concurrent caller would need its own mutex around this section.
        //
        // SAFETY: `master`'s fd is valid and was just unlocked above;
        // `ptsname` returns NULL on error (checked immediately) and a
        // NUL-terminated string on success.
        let ptr = unsafe { libc::ptsname(master.as_raw_fd()) };
        if ptr.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `ptr` is non-null (checked above) and NUL-terminated
        // per `ptsname(3)`; copied into owned memory right away, before
        // any other libc call can reuse the static buffer it points into.
        unsafe { CStr::from_ptr(ptr) }.to_owned()
    };

    // SAFETY: `slave_path` is a valid NUL-terminated C string from
    // `ptsname`/`ptsname_r` above; `open` returns -1 on error, checked
    // immediately.
    let slave_fd = unsafe {
        libc::open(
            slave_path.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
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

    #[test]
    fn open_pty_marks_both_fds_close_on_exec() {
        // Regression test: `open_pty()` must not leak the master (or
        // slave) fd into every child process this program spawns after a
        // REPL session is opened. Both `posix_openpt` and the slave's
        // `open` must pass `O_CLOEXEC` so `FD_CLOEXEC` is set from
        // creation — verified here directly via `fcntl(F_GETFD)` rather
        // than by spawning a child, since that's the exact property the
        // kernel guarantees `O_CLOEXEC` provides across `exec`.
        let (master, slave) = open_pty().unwrap();

        let master_flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFD) };
        assert!(master_flags >= 0);
        assert_ne!(master_flags & libc::FD_CLOEXEC, 0, "master fd not CLOEXEC");

        let slave_flags = unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_GETFD) };
        assert!(slave_flags >= 0);
        assert_ne!(slave_flags & libc::FD_CLOEXEC, 0, "slave fd not CLOEXEC");
    }
}
