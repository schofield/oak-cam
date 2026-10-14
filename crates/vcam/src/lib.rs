//! OS virtual cameras for oak-cam: each OAK sensor ([`CameraId`]) is published
//! as its own webcam other applications can open.
//!
//! Ported from stream-concern's `sc-interchange::vcam` (MIT, same author) and
//! trimmed: the compositor/pack types are replaced by the small borrowed
//! [`FrameSrc`], and every backend implements [`VirtualCamera`].
//!
//! ## Backends
//!
//! - **macOS**: a CoreMediaIO camera extension (Swift system extension in
//!   `extensions/oak-camera-extension`, embedded in `oak-cam.app`) exposes one
//!   device per camera; [`macos::MacCamera`] enqueues IOSurface-backed
//!   `CMSampleBuffer`s into that device's sink stream. [`macos::install`]
//!   drives the `OSSystemExtensionRequest` activation.
//! - **Linux**: v4l2loopback via raw ioctls and `write(2)` (`linux` module).
//! - **Windows 11**: `MFCreateVirtualCamera` with the custom media source in
//!   `extensions/oak-mf-vcam`, fed through a per-user shared-memory ring
//!   (`windows` module).
//! - [`NullCamera`]: counts frames, never fails; for tests and `--no-vcam`.
//!
//! [`CameraFormat`] holds the shared byte geometry (strides, fourccs).
//! [`test_pattern`] renders moving patterns so every backend can be verified
//! without an OAK attached.

// The ported docs name Apple frameworks and types (CoreMediaIO, IOSurface,
// CMSampleBuffer…) throughout; backticking every one hurts readability.
#![allow(clippy::doc_markdown)]

mod camera_id;
mod format;
mod frame;
mod null;
pub mod test_pattern;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "windows")]
pub mod windows;

pub use camera_id::{CAMERAS, CameraId, CameraKind, CameraUuids, Resolution};
pub use format::CameraFormat;
pub use frame::{FrameError, FrameSrc, Nv12Dest, write_nv12};
pub use null::NullCamera;

/// Why a virtual camera could not start.
#[derive(Debug, thiserror::Error)]
pub enum VcamError {
    /// The OS side is missing: the macOS extension is not active, no
    /// v4l2loopback device exists, the Windows media source is not
    /// registered. The string tells the user what to do.
    #[error("{0}")]
    NotAvailable(String),
    /// The requested size cannot be published.
    #[error("unsupported size {width}x{height}: {reason}")]
    InvalidSize {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
        /// Why it was refused.
        reason: &'static str,
    },
    /// An OS call failed.
    #[error("{0}")]
    Backend(String),
}

/// Why one frame was not published. Drops are normal under backpressure
/// (latest-wins): the caller counts them and moves on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    /// [`VirtualCamera::start`] has not succeeded (or [`VirtualCamera::stop`] ran).
    NotStarted,
    /// The frame's size differs from the started size, or its planes are too
    /// short for its geometry.
    FormatMismatch,
    /// The consumer side is behind: the sink queue is full (macOS), the device
    /// would block (Linux `EAGAIN`).
    QueueFull,
    /// Every pooled destination buffer is still in flight (macOS).
    PoolExhausted,
    /// Nobody has opened the camera yet, so there is nowhere to write
    /// (Windows ring not created yet).
    NoConsumer,
    /// Another producer owns this camera (Windows).
    Busy,
    /// An OS call failed; details are logged.
    Backend,
}

/// Outcome of [`VirtualCamera::publish`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Publish {
    /// The frame was handed to the OS.
    Published,
    /// The frame was dropped.
    Dropped(DropReason),
}

/// Running counters of one virtual camera since its last start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VcamStats {
    /// Frames handed to the OS.
    pub published: u64,
    /// Frames dropped (any [`DropReason`]).
    pub dropped: u64,
    /// Monotonic host time of the last published frame, nanoseconds (0 = none).
    pub last_publish_ns: u64,
}

impl VcamStats {
    /// Count `outcome`.
    pub fn record(&mut self, outcome: Publish, now_ns: u64) {
        match outcome {
            Publish::Published => {
                self.published += 1;
                self.last_publish_ns = now_ns;
            }
            Publish::Dropped(_) => self.dropped += 1,
        }
    }
}

/// One OS virtual camera. Created per [`CameraId`], owned by one capture
/// thread, and never blocks in [`publish`](Self::publish).
pub trait VirtualCamera: Send {
    /// Attach to the OS camera and prepare to publish `width` x `height`
    /// frames at about `fps`. Calling it again restarts at the new size.
    fn start(&mut self, width: u32, height: u32, fps: f32) -> Result<(), VcamError>;
    /// Publish one frame (NV12 as-is, Gray8 as NV12 with neutral chroma).
    fn publish(&mut self, src: &FrameSrc<'_>) -> Publish;
    /// Counters since the last start.
    fn stats(&self) -> VcamStats;
    /// Detach from the OS camera. Idempotent.
    fn stop(&mut self);
}

/// The platform backend for `id`: [`macos::MacCamera`], the v4l2loopback
/// camera, the Media Foundation camera, or a [`NullCamera`] where no backend
/// exists. Nothing is opened until [`VirtualCamera::start`].
#[must_use]
pub fn platform_camera(id: CameraId) -> Box<dyn VirtualCamera> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacCamera::new(id))
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::V4l2LoopbackCamera::new(id))
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(windows::WinCamera::new(id))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Box::new(NullCamera::new(id))
    }
}

/// Monotonic nanoseconds since the first call in this process (stats only).
#[must_use]
pub fn monotonic_ns() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = *EPOCH.get_or_init(Instant::now);
    epoch.elapsed().as_nanos() as u64 + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_record_counts_both_outcomes() {
        let mut s = VcamStats::default();
        s.record(Publish::Published, 5);
        s.record(Publish::Dropped(DropReason::QueueFull), 6);
        s.record(Publish::Published, 7);
        assert_eq!(
            s,
            VcamStats {
                published: 2,
                dropped: 1,
                last_publish_ns: 7
            }
        );
    }

    #[test]
    fn monotonic_ns_advances() {
        let a = monotonic_ns();
        let b = monotonic_ns();
        assert!(a > 0 && b >= a);
    }
}
