//! Keeping depthai's host logs off stdout.
//!
//! depthai-core's host logger is an spdlog `stdout_color_sink_mt`
//! (`src/utility/Logging.cpp`), so its warnings land on file descriptor 1 and
//! would corrupt machine-readable output such as `oak-cam --list --json`.
//! [`redirect_host_logs_to_stderr`] points fd 1 at stderr and hands back the
//! original stdout for the program's own output.

use std::io::{self, Write};

use libc::c_int;

/// The process's original stdout, saved before fd 1 was pointed at stderr.
/// Unbuffered; wrap in a `BufWriter` for many small writes. Closes its
/// descriptor on drop.
#[derive(Debug)]
pub struct OriginalStdout {
    fd: c_int,
}

/// Duplicates fd 1, then makes fd 1 a copy of fd 2, so everything written to
/// "stdout" by depthai (or anything else, including Rust's `std::io::stdout`)
/// goes to stderr. Returns the saved original stdout.
///
/// Call it before any other function in this crate: depthai may log as soon
/// as it initialises.
pub fn redirect_host_logs_to_stderr() -> io::Result<OriginalStdout> {
    // Flush anything Rust already buffered for the real stdout.
    io::stdout().flush()?;
    // SAFETY: plain descriptor syscalls on the process's standard fds.
    let saved = unsafe { libc::dup(1) };
    if saved < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above; `saved` stays open if this fails and is closed below.
    if unsafe { libc::dup2(2, 1) } < 0 {
        let err = io::Error::last_os_error();
        // SAFETY: `saved` is a descriptor we own.
        unsafe { libc::close(saved) };
        return Err(err);
    }
    Ok(OriginalStdout { fd: saved })
}

impl Write for OriginalStdout {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(unix)]
        // SAFETY: `buf` is valid for `buf.len()` bytes; `fd` is ours.
        let n = unsafe { libc::write(self.fd, buf.as_ptr().cast(), buf.len()) };
        #[cfg(windows)]
        // SAFETY: as above; the CRT `_write` takes a `c_uint` count.
        let n = unsafe { libc::write(self.fd, buf.as_ptr().cast(), c_uint_len(buf.len())) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(usize::try_from(n).unwrap_or(0))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(windows)]
fn c_uint_len(len: usize) -> libc::c_uint {
    libc::c_uint::try_from(len).unwrap_or(libc::c_uint::MAX)
}

impl Drop for OriginalStdout {
    fn drop(&mut self) {
        // SAFETY: we own `fd` and close it exactly once.
        unsafe { libc::close(self.fd) };
    }
}
