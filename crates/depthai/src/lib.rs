//! Safe wrapper over the oak-cam C shim around depthai-core v3 (`depthai-sys`).
//!
//! - Discovery and device queries: [`Device::list`], [`Device::open`],
//!   camera features, IR, chip temperature.
//! - Pipelines as a typestate: [`PipelineBuilder`] → [`CameraHandle`] /
//!   [`OutputHandle`] → [`PipelineBuilder::start`] → [`RunningPipeline`].
//! - Frames: [`OutputQueue`] → [`Frame`] → [`FrameView`].
//! - Controls: [`ControlParams`] → [`ControlQueue`] (live) or
//!   [`CameraHandle::set_initial_control`].
//!
//! Drop order is enforced with `Arc`: queues keep the pipeline alive, and
//! the C++ pipeline keeps the device connection alive.

mod control;
mod device;
mod enums;
mod env;
mod frame;
mod log;
mod pipeline;
mod stdio;

use std::ffi::{CStr, c_char};

use depthai_sys as sys;

pub use control::{AfMode, Antibanding, AwbMode, ControlParams, ControlQueue};
pub use device::{CameraFeatures, Device, DeviceInfo, SensorConfig};
pub use enums::{DeviceState, ImgType, LogLevel, Platform, Protocol, SensorType, Socket, UsbSpeed};
pub use env::env_default;
pub use frame::{Frame, FrameView};
pub use log::{clear_log_handler, set_log_handler, set_log_level};
pub use pipeline::{
    CameraHandle, OutputHandle, OutputQueue, OutputSpec, PipelineBuilder, ResizeMode,
    RunningPipeline, SensorSpec,
};
pub use stdio::{OriginalStdout, redirect_host_logs_to_stderr};

/// Errors from the depthai shim.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// A shim call failed; the message is `oak_last_error()` for that call.
    #[error("depthai: {0}")]
    Depthai(String),
    /// An argument could not be passed to C (e.g. an interior NUL byte).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Turns a shim return code into a `Result`: negative codes become
/// [`Error::Depthai`] carrying this thread's `oak_last_error()`; `0` and
/// positive (poll "got one") codes pass through as `Ok`.
pub fn check(rc: i32) -> Result<()> {
    if rc < 0 {
        Err(Error::Depthai(last_error()))
    } else {
        Ok(())
    }
}

/// This thread's last shim error message.
fn last_error() -> String {
    // SAFETY: oak_last_error returns a valid NUL-terminated per-thread string.
    let msg = unsafe { CStr::from_ptr(sys::oak_last_error()) };
    let msg = msg.to_string_lossy();
    if msg.is_empty() {
        "unknown error (oak_last_error is empty)".to_owned()
    } else {
        msg.into_owned()
    }
}

/// The depthai-core version the shim was compiled against, e.g. `"3.10.0"`.
pub fn version() -> &'static str {
    // SAFETY: static NUL-terminated storage inside the shim.
    let v = unsafe { CStr::from_ptr(sys::oak_version()) };
    v.to_str().unwrap_or("unknown")
}

/// `std::chrono::steady_clock::now()` in ns: the clock of
/// [`FrameView::ts_host_ns`]. `steady_now_ns() - ts_host_ns` is the
/// capture-to-host latency.
pub fn steady_now_ns() -> i64 {
    let mut ns = 0i64;
    // SAFETY: ns is a valid out-param; the call cannot fail.
    let rc = unsafe { sys::oak_steady_now_ns(&raw mut ns) };
    debug_assert_eq!(rc, 0);
    ns
}

/// Reads a NUL-terminated string out of a fixed-size C buffer.
pub(crate) fn fixed_str(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c.to_ne_bytes()[0])
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Bytes as C chars (test helper; `c_char` is `i8` or `u8` by platform).
#[cfg(test)]
pub(crate) fn c_chars(bytes: &[u8]) -> Vec<c_char> {
    bytes.iter().map(|&b| c_char::from_ne_bytes([b])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_str_stops_at_nul() {
        let mut buf = [0 as c_char; 8];
        buf[..3].copy_from_slice(&c_chars(b"OAK"));
        buf[4] = c_chars(b"x")[0];
        assert_eq!(fixed_str(&buf), "OAK");
        let full = c_chars(b"abc");
        assert_eq!(fixed_str(&full), "abc");
    }

    #[test]
    fn version_is_pinned() {
        assert_eq!(version(), "3.10.0");
    }

    #[test]
    fn steady_clock_is_monotonic() {
        let a = steady_now_ns();
        let b = steady_now_ns();
        assert!(a > 0 && b >= a);
    }

    #[test]
    fn check_maps_codes() {
        assert!(check(0).is_ok());
        assert!(check(1).is_ok());
        assert!(matches!(check(-1), Err(Error::Depthai(_))));
    }
}
