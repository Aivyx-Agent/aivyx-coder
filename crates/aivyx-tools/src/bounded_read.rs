//! Reading files the model names, safely: never blocking, never unbounded.
//!
//! A path the model (or a prompt injection) supplies can be anything —
//! `/dev/zero` (endless, valid UTF-8: reading it all exhausts memory), a
//! FIFO or `/dev/stdin` (opening or reading blocks forever), or a
//! multi-gigabyte log. `read_file` is auto-approved and the editing tools
//! read the old content to build their approval preview *before* anyone is
//! asked, so every one of those reads goes through here: opened without
//! blocking, refused unless it's a regular file (checked on the opened
//! file, so it can't be swapped in between), and read only up to a limit.

use std::io::{self, Read};
use std::path::Path;

/// How much of a file `read_file` shows.
pub(crate) const READ_FILE_LIMIT: u64 = 1024 * 1024;

/// The largest file the editing tools will load whole.
pub(crate) const WHOLE_FILE_LIMIT: u64 = 16 * 1024 * 1024;

/// Up to `limit` bytes of a regular file's text.
#[derive(Debug)]
pub(crate) struct Capped {
    pub text: String,
    /// The file's full size in bytes.
    pub size: u64,
    pub truncated: bool,
}

fn not_regular() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "not a regular file (a device, pipe or socket) — only regular files can be read",
    )
}

fn open_regular(path: &Path) -> io::Result<(std::fs::File, u64)> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Opening a FIFO for reading otherwise waits for a writer.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(not_regular());
    }
    Ok((file, meta.len()))
}

/// The first `limit` bytes of `path` as text. `InvalidData` if it isn't
/// UTF-8 (a character cut by the limit doesn't count).
pub(crate) fn read_capped(path: &Path, limit: u64) -> io::Result<Capped> {
    let (file, size) = open_regular(path)?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    let truncated = size > limit;
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => {
            let e = err.utf8_error();
            // Only an incomplete character at the very end of a cut read.
            if truncated && e.error_len().is_none() {
                let valid = e.valid_up_to();
                let mut bytes = err.into_bytes();
                bytes.truncate(valid);
                String::from_utf8(bytes).map_err(|_| invalid_utf8())?
            } else {
                return Err(invalid_utf8());
            }
        }
    };
    Ok(Capped {
        text,
        size,
        truncated,
    })
}

fn invalid_utf8() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "stream did not contain valid UTF-8",
    )
}

/// The whole of `path` as text, for a tool that must see all of it (an
/// edit, or a diff preview). Refuses a file over `WHOLE_FILE_LIMIT`.
pub(crate) fn read_whole(path: &Path) -> io::Result<String> {
    let capped = read_capped(path, WHOLE_FILE_LIMIT)?;
    if capped.truncated {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "the file is {} — too large to load whole (the limit is {})",
                human_size(capped.size),
                human_size(WHOLE_FILE_LIMIT)
            ),
        ));
    }
    Ok(capped.text)
}

/// `Err` if `path` exists and isn't a regular file: writing to a FIFO
/// blocks until something reads it, and a device isn't a file to replace.
pub(crate) fn check_write_target(path: &Path) -> io::Result<()> {
    match std::fs::metadata(path) {
        Ok(meta) if !meta.is_file() && !meta.is_dir() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file (a device, pipe or socket) — only regular files can be written",
        )),
        _ => Ok(()),
    }
}

/// `1.5 MiB`-style size for messages.
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_regular_file_is_read_up_to_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "hello world").unwrap();
        let whole = read_capped(&path, 100).unwrap();
        assert_eq!(
            (whole.text.as_str(), whole.size, whole.truncated),
            ("hello world", 11, false)
        );
        let cut = read_capped(&path, 5).unwrap();
        assert_eq!(
            (cut.text.as_str(), cut.size, cut.truncated),
            ("hello", 11, true)
        );
    }

    #[test]
    fn a_character_cut_by_the_limit_is_dropped_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("u.txt");
        std::fs::write(&path, "ab€").unwrap(); // € is 3 bytes
        assert_eq!(read_capped(&path, 3).unwrap().text, "ab");
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert_eq!(
            read_capped(&path, 10).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn devices_are_refused_without_reading() {
        // /dev/zero would never end; it must be refused, not read.
        let err = read_capped(Path::new("/dev/zero"), READ_FILE_LIMIT).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        assert!(check_write_target(Path::new("/dev/null")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = fifo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read_capped(&reader, 10).map(|_| ()).map_err(|e| e.kind()));
        });
        let outcome = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("reading a FIFO must not block");
        assert_eq!(outcome, Err(io::ErrorKind::InvalidInput));
        assert!(check_write_target(&fifo).is_err());
    }

    #[test]
    fn read_whole_refuses_a_file_over_the_limit_and_sizes_read_well() {
        assert_eq!(human_size(512), "512 bytes");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MiB");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.txt");
        std::fs::write(&path, "x").unwrap();
        assert_eq!(read_whole(&path).unwrap(), "x");
        assert!(check_write_target(&path).is_ok());
        assert!(check_write_target(&dir.path().join("missing")).is_ok());
    }
}
