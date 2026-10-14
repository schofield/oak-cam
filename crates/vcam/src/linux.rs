//! v4l2loopback virtual camera (Linux): publishes NV12 frames to a
//! `/dev/videoN` loopback device with `write(2)`.
//!
//! Uses raw `libc` ioctls (no v4l2 crate). Ported from stream-concern's
//! `vcam/linux.rs`: the compositor pack negotiation (NV12, then UYVY, then
//! BGRA) is trimmed to NV12, the only format oak-cam produces; Gray8 frames
//! are widened into the NV12 staging buffer (UV filled once per start).
//!
//! Single device for now: every [`CameraId`] opens the first loopback device.
// M4b: `enumerate()` all `/dev/video0..63` loopback outputs and map each
// CameraId to one (config `linux_device` override, then `card_label` match,
// then unassigned in order), and add VIDIOC_S_PARM for the frame rate.

#![allow(unsafe_code)]

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

use crate::{
    CameraFormat, CameraId, DropReason, FrameSrc, Nv12Dest, Publish, VcamError, VcamStats,
    VirtualCamera, write_nv12,
};

// v4l2 ioctl constants and structures.
const VIDIOC_QUERYCAP: u64 = 0x8068_5600;
const VIDIOC_S_FMT: u64 = 0xC0D0_5605;

const V4L2_CAP_VIDEO_OUTPUT: u32 = 0x0000_2000;
const V4L2_FIELD_NONE: u32 = 0x0001;
const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;

/// `v4l2_capability`.
#[repr(C)]
struct V4l2Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    _reserved: [u32; 3],
}

/// `v4l2_format` with the `fmt` union padded to its kernel size (200 bytes).
#[repr(C)]
struct V4l2Format {
    type_: u32,
    fmt: V4l2FormatUnion,
}

#[repr(C)]
union V4l2FormatUnion {
    pix: V4l2PixFormat,
    raw: [u8; 200],
    // The kernel union contains pointers, so it is 8-byte aligned on 64-bit.
    _align: [u64; 25],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct V4l2PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

/// The shell command that loads v4l2loopback for oak-cam.
pub const SETUP_HINT: &str = "sudo modprobe v4l2loopback devices=3 video_nr=10,11,12 \
     card_label=\"OAK RGB\",\"OAK Left\",\"OAK Right\" exclusive_caps=1,1,1";

/// A v4l2loopback virtual camera.
pub struct V4l2LoopbackCamera {
    camera: CameraId,
    device: Option<File>,
    path: String,
    size: (u32, u32),
    /// Tightly packed NV12 staging frame (`write(2)` takes one buffer).
    staging: Vec<u8>,
    /// The staging UV plane holds neutral chroma from a Gray8 publish.
    uv_neutral: bool,
    stats: VcamStats,
}

impl V4l2LoopbackCamera {
    /// A camera that publishes `camera` to the first loopback device.
    #[must_use]
    pub fn new(camera: CameraId) -> Self {
        Self {
            camera,
            device: None,
            path: String::new(),
            size: (0, 0),
            staging: Vec::new(),
            uv_neutral: false,
            stats: VcamStats::default(),
        }
    }

    /// The device path after a successful start.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl VirtualCamera for V4l2LoopbackCamera {
    fn start(&mut self, width: u32, height: u32, _fps: f32) -> Result<(), VcamError> {
        if width == 0 || height == 0 {
            return Err(VcamError::InvalidSize {
                width,
                height,
                reason: "empty frame",
            });
        }
        self.stop();
        // M4b: pick this CameraId's device instead of the first loopback.
        let path = find_loopback_device().ok_or_else(|| {
            VcamError::NotAvailable(format!("v4l2loopback is not loaded: {SETUP_HINT}"))
        })?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .map_err(|e| VcamError::Backend(format!("open {path}: {e}")))?;
        set_format(file.as_raw_fd(), (width, height), CameraFormat::Nv12)
            .map_err(VcamError::Backend)?;
        self.staging = vec![0; CameraFormat::Nv12.frame_size(width, height)];
        self.uv_neutral = false;
        self.size = (width, height);
        self.device = Some(file);
        self.stats = VcamStats::default();
        tracing::info!(
            "virtual camera: {} on {path} as NV12 {width}x{height}",
            self.camera
        );
        self.path = path;
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
        self.device = None;
    }
}

impl V4l2LoopbackCamera {
    fn publish_inner(&mut self, src: &FrameSrc<'_>) -> Publish {
        let Some(device) = self.device.as_mut() else {
            return Publish::Dropped(DropReason::NotStarted);
        };
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
        match device.write(&self.staging) {
            Ok(n) if n == self.staging.len() => Publish::Published,
            Ok(n) => {
                tracing::warn!("v4l2loopback short write {n}/{}", self.staging.len());
                Publish::Dropped(DropReason::Backend)
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                Publish::Dropped(DropReason::QueueFull)
            }
            Err(e) => {
                tracing::warn!("v4l2loopback write: {e}");
                Publish::Dropped(DropReason::Backend)
            }
        }
    }
}

impl Drop for V4l2LoopbackCamera {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Find the first `/dev/videoN` device whose driver is `v4l2 loopback` and
/// which accepts output.
#[must_use]
pub fn find_loopback_device() -> Option<String> {
    // M4b: scan 0..64 and return every match (with its card label).
    for n in 0..16 {
        let path = format!("/dev/video{n}");
        let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
            continue;
        };
        // SAFETY: an all-zero v4l2_capability is a valid value.
        let mut cap: V4l2Capability = unsafe { std::mem::zeroed() };
        // SAFETY: VIDIOC_QUERYCAP on a valid fd with a correctly sized struct;
        // the kernel fills it.
        let ret = unsafe { libc::ioctl(file.as_raw_fd(), VIDIOC_QUERYCAP as _, &raw mut cap) };
        if ret == 0 {
            let driver = std::str::from_utf8(&cap.driver)
                .unwrap_or("")
                .trim_end_matches('\0');
            if driver == "v4l2 loopback" && (cap.device_caps & V4L2_CAP_VIDEO_OUTPUT) != 0 {
                return Some(path);
            }
        }
    }
    None
}

/// Set the output format via `VIDIOC_S_FMT` and check the driver kept it.
fn set_format(fd: i32, size: (u32, u32), format: CameraFormat) -> Result<(), String> {
    let pixelformat = format.v4l2_pixel_format();
    let mut fmt = V4l2Format {
        type_: V4L2_BUF_TYPE_VIDEO_OUTPUT,
        fmt: V4l2FormatUnion { raw: [0; 200] },
    };
    fmt.fmt.pix = V4l2PixFormat {
        width: size.0,
        height: size.1,
        pixelformat,
        field: V4L2_FIELD_NONE,
        bytesperline: format.plane_stride(size.0, 0) as u32,
        sizeimage: format.frame_size(size.0, size.1) as u32,
        colorspace: 0,
        priv_: 0,
        flags: 0,
        ycbcr_enc: 0,
        quantization: 0,
        xfer_func: 0,
    };
    // SAFETY: VIDIOC_S_FMT on a valid fd with a correctly sized struct.
    let ret = unsafe { libc::ioctl(fd, VIDIOC_S_FMT as _, &raw mut fmt) };
    if ret != 0 {
        return Err(format!(
            "VIDIOC_S_FMT {format:?} {}x{} failed: {}",
            size.0,
            size.1,
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: the kernel filled the pix member of the union.
    let got = unsafe { fmt.fmt.pix.pixelformat };
    if got != pixelformat {
        return Err(format!(
            "driver chose pixelformat 0x{got:08x} instead of 0x{pixelformat:08x}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `struct v4l2_format` is 208 bytes on 64-bit Linux (the ioctl number
    /// 0xC0D05605 encodes 0xD0 = 208).
    #[test]
    fn v4l2_format_size_matches_ioctl() {
        assert_eq!(size_of::<V4l2Format>(), 208);
        assert_eq!(size_of::<V4l2Capability>(), 0x68);
    }

    /// Without a started device every publish drops as NotStarted.
    #[test]
    fn publish_before_start_drops() {
        let mut cam = V4l2LoopbackCamera::new(CameraId::Rgb);
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
        assert_eq!(cam.stats().dropped, 1);
    }
}
