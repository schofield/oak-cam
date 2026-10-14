//! Ctrl-C / SIGTERM handling shared by every long-running command.
//!
//! The first SIGINT or SIGTERM sets the flag [`install`] returns so the
//! command can stop cleanly; a second one while shutting down (a stalled
//! stop, a hung device) exits the process immediately with status 130.
//!
//! Windows: the CRT resets a `signal()` handler to `SIG_DFL` before each
//! delivery, so a second Ctrl-C ends the process through the default action
//! rather than `_exit(130)`; the effect (a forced exit) is the same, which
//! is acceptable. Ctrl-Break and console close (`CTRL_BREAK_EVENT`,
//! `CTRL_CLOSE_EVENT`) are not handled and terminate without a clean stop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

/// The flag the handler sets; created on the first [`install`].
static STOP: OnceLock<Arc<AtomicBool>> = OnceLock::new();

extern "C" fn on_signal(_sig: libc::c_int) {
    // OnceLock::get is a single atomic load, and the flag exists before the
    // handler is installed, so this never blocks or allocates.
    if let Some(stop) = STOP.get()
        && stop.swap(true, Ordering::SeqCst)
    {
        // SAFETY: _exit is async-signal-safe.
        unsafe { libc::_exit(130) };
    }
}

/// Installs the SIGINT / SIGTERM handler (idempotent) and returns the stop
/// flag it sets. Every call returns the same flag.
pub fn install() -> Arc<AtomicBool> {
    let stop = Arc::clone(STOP.get_or_init(|| Arc::new(AtomicBool::new(false))));
    let handler = on_signal as extern "C" fn(libc::c_int);
    for sig in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: the handler only loads a OnceLock, swaps an atomic and
        // calls _exit, all async-signal-safe.
        unsafe { libc::signal(sig, handler as libc::sighandler_t) };
    }
    stop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_is_idempotent_and_shares_the_flag() {
        let a = install();
        let b = install();
        assert!(Arc::ptr_eq(&a, &b));
    }
}
