//! Media Foundation virtual camera (Windows 11).
//!
//! The camera is a custom media source in `oak_mf_vcam.dll`
//! (`extensions/oak-mf-vcam`), registered for the machine under
//! [`CLSID_STRING`]. [`WinCamera::start`] calls `MFCreateVirtualCamera`
//! (session lifetime, current user) and starts the camera; the Frame Server
//! loads the source in its own service process when an app opens the camera,
//! and the source creates this user's shared-memory ring (named after the
//! user's SID; see `oak_mf_vcam::naming`). One instance per user may publish
//! (an instance lock file plus the ring's single-producer claim). Each
//! [`publish`](WinCamera::publish) writes the frame into an NV12 staging
//! buffer and copies that into the ring; the source converts and scales to the
//! consumer's chosen type. Stop removes the camera.
//!
//! Ported from stream-concern's `vcam/windows.rs`. Single camera for now:
// M4c: `VirtualCamera::create(name, sid, index)` per CameraId with a new
// ATTR_CAMERA_INDEX attribute, per-index ring names and instance locks, and
// per-index slot capacity (RGB 4K, mono 1280x800).

use std::time::{Duration, Instant};

use oak_mf_vcam::layout::{MAX_HEIGHT, MAX_WIDTH};
use oak_mf_vcam::ring::WriteError;
use oak_mf_vcam::win::host::{
    self, InstanceLock, Producer, ProducerError, Status, VirtualCamera as MfVirtualCamera,
};
use oak_mf_vcam::win::shm::RingConfig;
pub use oak_mf_vcam::{CLSID_STRING, DEVICE_NAME, DLL_NAME};

use crate::{
    CameraFormat, CameraId, DropReason, FrameSrc, Nv12Dest, Publish, VcamError, VcamStats,
    VirtualCamera, write_nv12,
};

/// How often a missing ring (nobody has opened the camera yet) is looked for.
const RING_RETRY: Duration = Duration::from_millis(500);

/// The script that builds, installs and registers the DLL.
pub const INSTALL_COMMAND: &str = r"scripts\vcam\build-windows.ps1 -Register";

/// The camera's state on this machine (Windows 11 API, registration).
#[must_use]
pub fn status() -> Status {
    host::status()
}

/// Whether the camera can be started: Windows 11 and the media source
/// registered for the machine.
#[must_use]
pub fn available() -> bool {
    matches!(status(), Status::Ready { .. })
}

/// The user-facing text for each state that stops the camera starting.
#[must_use]
pub fn missing_message() -> String {
    match status() {
        Status::Unsupported => "The virtual camera needs Windows 11 (build 22000 or later); \
             Windows 10 has no Media Foundation virtual camera."
            .to_string(),
        Status::NotRegistered { .. } => format!(
            "The oak-cam virtual camera is not installed. From an administrator PowerShell in \
             the oak-cam folder run {INSTALL_COMMAND} (once), then start again."
        ),
        Status::RegisteredFileMissing { dll } => format!(
            "The registered virtual camera component {} is missing. Run {INSTALL_COMMAND} \
             from an administrator PowerShell, then start again.",
            dll.display()
        ),
        Status::Ready { .. } => "The virtual camera is installed.".to_string(),
    }
}

/// One Windows virtual camera.
pub struct WinCamera {
    camera: CameraId,
    mf: Option<MfVirtualCamera>,
    producer: Option<Producer>,
    ring: Option<RingConfig>,
    lock: Option<InstanceLock>,
    /// Another producer holds the ring (or took it over): stop writing.
    refused: Option<u32>,
    last_ring_attempt: Option<Instant>,
    size: (u32, u32),
    fps: f32,
    staging: Vec<u8>,
    uv_neutral: bool,
    stats: VcamStats,
}

impl WinCamera {
    /// A camera for `camera`, created on [`start`](VirtualCamera::start).
    #[must_use]
    pub fn new(camera: CameraId) -> Self {
        Self {
            camera,
            mf: None,
            producer: None,
            ring: None,
            lock: None,
            refused: None,
            last_ring_attempt: None,
            size: (0, 0),
            fps: 0.0,
            staging: Vec::new(),
            uv_neutral: false,
            stats: VcamStats::default(),
        }
    }

    fn try_open_ring(&mut self) {
        if self.producer.is_some()
            || self.refused.is_some()
            || self
                .last_ring_attempt
                .is_some_and(|t| t.elapsed() < RING_RETRY)
        {
            return;
        }
        self.last_ring_attempt = Some(Instant::now());
        let Some(ring) = self.ring.as_ref() else {
            return;
        };
        match Producer::open(ring, self.fps) {
            Ok(p) => {
                tracing::info!(
                    "virtual camera: ring attached (created by {})",
                    if p.created() {
                        "the app"
                    } else {
                        "the camera source"
                    }
                );
                self.producer = Some(p);
            }
            Err(ProducerError::Busy { pid }) => {
                tracing::error!(
                    "virtual camera: process {pid} is already publishing this user's camera; \
                     not writing"
                );
                self.refused = Some(pid);
            }
            Err(e) => tracing::trace!("virtual camera: ring not open yet: {e}"),
        }
    }

    fn publish_inner(&mut self, src: &FrameSrc<'_>) -> Publish {
        if self.mf.is_none() {
            return Publish::Dropped(DropReason::NotStarted);
        }
        let (w, h) = self.size;
        let y_len = CameraFormat::Nv12.plane_stride(w, 0) * CameraFormat::Nv12.plane_rows(h, 0);
        let (y, uv) = self.staging.split_at_mut(y_len);
        let mut dst = Nv12Dest {
            width: w,
            height: h,
            y,
            y_stride: CameraFormat::Nv12.plane_stride(w, 0),
            uv,
            uv_stride: CameraFormat::Nv12.plane_stride(w, 1),
        };
        if write_nv12(src, &mut dst, !self.uv_neutral).is_err() {
            return Publish::Dropped(DropReason::FormatMismatch);
        }
        self.uv_neutral = !src.has_chroma();
        self.try_open_ring();
        if self.refused.is_some() {
            return Publish::Dropped(DropReason::Busy);
        }
        let Some(producer) = self.producer.as_mut() else {
            // Nobody has opened the camera yet, so the ring does not exist.
            return Publish::Dropped(DropReason::NoConsumer);
        };
        match producer.write_nv12(w, h, &self.staging) {
            Ok(_) => Publish::Published,
            Err(WriteError::Lost) => {
                tracing::error!("virtual camera: another producer took the ring over; stopped");
                self.producer = None;
                self.refused = Some(0);
                Publish::Dropped(DropReason::Busy)
            }
            Err(e) => {
                tracing::warn!("virtual camera: frame refused by the ring: {e:?}");
                Publish::Dropped(DropReason::Backend)
            }
        }
    }
}

impl VirtualCamera for WinCamera {
    fn start(&mut self, width: u32, height: u32, fps: f32) -> Result<(), VcamError> {
        // A restart (size change) starts from a clean state, releasing our own
        // instance lock first.
        self.stop();
        if width == 0 || height == 0 || width > MAX_WIDTH || height > MAX_HEIGHT {
            return Err(VcamError::InvalidSize {
                width,
                height,
                reason: "outside the ring's slot capacity",
            });
        }
        if !available() {
            return Err(VcamError::NotAvailable(missing_message()));
        }
        // M4c: one lock per camera index.
        let lock = InstanceLock::acquire().map_err(|pid| {
            VcamError::Backend(match pid {
                Some(pid) => format!(
                    "virtual camera: another oak-cam (process {pid}) is already publishing the \
                     virtual camera for this Windows user; stop it there first"
                ),
                None => format!(
                    "virtual camera: cannot open the instance lock {}",
                    InstanceLock::path().display()
                ),
            })
        })?;
        let ring = RingConfig::for_current_user().map_err(|e| {
            VcamError::Backend(format!("virtual camera: cannot read the user SID: {e}"))
        })?;
        // M4c: name each camera after its CameraId and pass its index.
        let mut mf = MfVirtualCamera::create(DEVICE_NAME, &ring.owner_sid).map_err(|e| {
            VcamError::Backend(format!(
                "virtual camera: MFCreateVirtualCamera failed (0x{:08X}: {})",
                e.code().0 as u32,
                e.message()
            ))
        })?;
        mf.start().map_err(|e| {
            VcamError::Backend(format!(
                "virtual camera: starting the camera failed (0x{:08X}: {}). Access denied \
                 usually means the Frame Server cannot read the registered DLL: install it \
                 with {INSTALL_COMMAND}.",
                e.code().0 as u32,
                e.message()
            ))
        })?;
        self.mf = Some(mf);
        self.lock = Some(lock);
        self.ring = Some(ring);
        self.refused = None;
        self.size = (width, height);
        self.fps = fps;
        self.staging = vec![0; CameraFormat::Nv12.frame_size(width, height)];
        self.uv_neutral = false;
        self.stats = VcamStats::default();
        self.producer = None;
        self.last_ring_attempt = None;
        self.try_open_ring();
        if let Some(pid) = self.refused {
            self.stop();
            return Err(VcamError::Backend(format!(
                "virtual camera: process {pid} is already publishing this user's camera"
            )));
        }
        tracing::info!(
            "virtual camera: started {DEVICE_NAME} for {} ({width}x{height} NV12)",
            self.camera
        );
        Ok(())
    }

    fn publish(&mut self, src: &FrameSrc<'_>) -> Publish {
        let outcome = self.publish_inner(src);
        self.stats.record(outcome, crate::monotonic_ns());
        outcome
    }

    fn stats(&self) -> VcamStats {
        self.stats
    }

    fn stop(&mut self) {
        self.producer = None;
        self.ring = None;
        if let Some(mut mf) = self.mf.take() {
            mf.remove();
            tracing::info!("virtual camera: removed {DEVICE_NAME}");
        }
        self.lock = None;
    }
}

impl Drop for WinCamera {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Status and message agree, and Windows 10 reports the Windows 11
    /// requirement.
    #[test]
    fn vcam_windows_status_and_message() {
        let st = status();
        let msg = missing_message();
        eprintln!(
            "vcam windows: status {st:?}; available {}; message: {msg}",
            available()
        );
        match st {
            Status::Unsupported => assert!(msg.contains("Windows 11"), "{msg}"),
            Status::NotRegistered { .. } | Status::RegisteredFileMissing { .. } => {
                assert!(msg.contains(INSTALL_COMMAND), "{msg}");
                assert!(!available());
            }
            Status::Ready { .. } => assert!(available()),
        }
    }

    /// Without a camera the backend refuses frames; an oversized start fails.
    #[test]
    fn vcam_windows_idle() {
        let mut cam = WinCamera::new(CameraId::Rgb);
        let y = vec![0u8; 16];
        let frame = FrameSrc::Gray8 {
            y: &y,
            stride: 4,
            w: 4,
            h: 4,
            ts_ns: 0,
        };
        assert_eq!(
            cam.publish(&frame),
            Publish::Dropped(DropReason::NotStarted)
        );
        cam.stop();
        assert!(matches!(
            cam.start(MAX_WIDTH + 1, 1080, 30.0),
            Err(VcamError::InvalidSize { .. })
        ));
    }
}
