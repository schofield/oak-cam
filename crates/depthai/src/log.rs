//! depthai log level and log forwarding.

use std::ffi::{CStr, c_char, c_void};
use std::sync::RwLock;

use depthai_sys as sys;

use crate::{LogLevel, Result, check};

type Handler = Box<dyn Fn(LogLevel, &str, &str) + Send + Sync>;

static HANDLER: RwLock<Option<Handler>> = RwLock::new(None);

/// Sets depthai's log level.
///
/// depthai-core v3.10 keeps its host logger private, so the host-side level
/// is taken from `DEPTHAI_LEVEL` when the library first initialises: call
/// this before [`crate::Device::list`] / [`crate::Device::open`]. An existing
/// `DEPTHAI_LEVEL` in the environment wins. The level is also applied to the
/// firmware log level of every device opened afterwards.
pub fn set_log_level(level: LogLevel) -> Result<()> {
    // SAFETY: plain value call.
    check(unsafe { sys::oak_set_log_level(level.to_raw()) })
}

/// Forwards device (firmware) log messages (`level`, `node`, `message`) to
/// `handler` for every device opened after this call; those device logs then
/// no longer go to stdout.
///
/// Only device logs are rerouted. depthai's host-side logger (discovery,
/// connection, pipeline errors) always writes to fd 1 through spdlog and
/// cannot be hooked; use [`crate::redirect_host_logs_to_stderr`] to send
/// it to fd 2 instead.
///
/// The handler runs on depthai threads; it must not block for long.
pub fn set_log_handler<F>(handler: F)
where
    F: Fn(LogLevel, &str, &str) + Send + Sync + 'static,
{
    *HANDLER
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(handler));
    // SAFETY: trampoline is a valid extern "C" fn for the program's lifetime;
    // `user` is unused.
    unsafe { sys::oak_set_log_callback(Some(trampoline), std::ptr::null_mut()) };
}

/// Removes the handler installed by [`set_log_handler`].
pub fn clear_log_handler() {
    // SAFETY: NULL clears the callback.
    unsafe { sys::oak_set_log_callback(None, std::ptr::null_mut()) };
    *HANDLER
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

unsafe extern "C" fn trampoline(
    _user: *mut c_void,
    level: i32,
    node: *const c_char,
    message: *const c_char,
) {
    // Never unwind into C++.
    let _ = std::panic::catch_unwind(|| {
        let text = |p: *const c_char| {
            if p.is_null() {
                String::new()
            } else {
                // SAFETY: the shim passes NUL-terminated strings valid for this call.
                unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
            }
        };
        let guard = HANDLER
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(handler) = guard.as_ref() {
            handler(LogLevel::from_raw(level), &text(node), &text(message));
        }
    });
}
