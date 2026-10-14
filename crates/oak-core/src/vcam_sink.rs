//! [`VcamSink`]: the [`FrameSink`] that publishes each received frame to an
//! OS virtual camera.
//!
//! RGB NV12 frames go through as-is (Y and UV planes with the device's row
//! stride); mono GRAY8 frames go as [`FrameSrc::Gray8`], which every backend
//! widens to NV12 with neutral chroma. Nothing is copied here: the one copy
//! happens inside the backend, into its destination buffer.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use depthai::{FrameView, ImgType};
use vcam::{CameraId, DropReason, FrameSrc, Publish, VcamError, VirtualCamera};

use crate::capture::FrameSink;

/// Every [`DropReason`], in [`reason_index`] order.
const DROP_REASONS: [DropReason; 7] = [
    DropReason::NotStarted,
    DropReason::FormatMismatch,
    DropReason::QueueFull,
    DropReason::PoolExhausted,
    DropReason::NoConsumer,
    DropReason::Busy,
    DropReason::Backend,
];

fn reason_index(reason: DropReason) -> usize {
    match reason {
        DropReason::NotStarted => 0,
        DropReason::FormatMismatch => 1,
        DropReason::QueueFull => 2,
        DropReason::PoolExhausted => 3,
        DropReason::NoConsumer => 4,
        DropReason::Busy => 5,
        DropReason::Backend => 6,
    }
}

/// Publish outcomes of one [`VcamSink`], written by its capture thread and
/// read by the stats printer. Cumulative over the sink's life (a camera
/// restart does not reset them, unlike [`VirtualCamera::stats`]).
#[derive(Debug, Default)]
pub struct VcamCounters {
    published: AtomicU64,
    dropped: [AtomicU64; DROP_REASONS.len()],
    restarts: AtomicU64,
}

impl VcamCounters {
    fn record(&self, outcome: Publish) {
        match outcome {
            Publish::Published => self.published.fetch_add(1, Ordering::Relaxed),
            Publish::Dropped(r) => self.dropped[reason_index(r)].fetch_add(1, Ordering::Relaxed),
        };
    }

    /// Frames handed to the OS.
    pub fn published(&self) -> u64 {
        self.published.load(Ordering::Relaxed)
    }

    /// Frames dropped for `reason`.
    pub fn dropped_by(&self, reason: DropReason) -> u64 {
        self.dropped[reason_index(reason)].load(Ordering::Relaxed)
    }

    /// Frames dropped for any reason.
    pub fn dropped(&self) -> u64 {
        DROP_REASONS.iter().map(|&r| self.dropped_by(r)).sum()
    }

    /// Camera restarts caused by a change of frame size (logged when they
    /// happen; not part of the stats line).
    #[cfg(test)]
    pub fn restarts(&self) -> u64 {
        self.restarts.load(Ordering::Relaxed)
    }

    /// The `--stats` field: `vcam P/D`, frames published / dropped by the
    /// virtual camera (distinct from the capture-side `drop`, which counts
    /// frames the device never delivered).
    pub fn summary(&self) -> String {
        format!("vcam {}/{}", self.published(), self.dropped())
    }
}

/// The borrowed [`FrameSrc`] for `v`: NV12 planes with their stride, or the
/// GRAY8 plane; `None` for any other format or a layout that does not fit.
/// `ts_ns` is the host-synchronised capture time.
pub fn frame_src<'a>(v: &FrameView<'a>) -> Option<FrameSrc<'a>> {
    let (w, h) = (v.width(), v.height());
    let stride = v.stride() as usize;
    let ts_ns = v.ts_host_ns();
    match v.img_type() {
        ImgType::Nv12 => v.nv12_planes().map(|(y, uv)| FrameSrc::Nv12 {
            y,
            y_stride: stride,
            uv,
            uv_stride: stride,
            w,
            h,
            ts_ns,
        }),
        ImgType::Gray8 => v.gray8().map(|y| FrameSrc::Gray8 {
            y,
            stride,
            w,
            h,
            ts_ns,
        }),
        _ => None,
    }
}

/// Publishes frames from one sensor into one [`VirtualCamera`].
///
/// Started at construction; stopped on drop. On a lost stream the camera
/// stays open (it holds the last frame until frames resume). A frame of a
/// different size than the camera was started with restarts the camera at
/// the new size.
pub struct VcamSink {
    id: CameraId,
    camera: Box<dyn VirtualCamera>,
    fps: f32,
    /// Size the camera was last (re)started with; `None` after a failed
    /// restart, so frames drop as `NotStarted` until the size changes again.
    started: Option<(u32, u32)>,
    /// Size of the last failed restart, so it is not retried every frame.
    failed: Option<(u32, u32)>,
    counters: Arc<VcamCounters>,
    warned_restart: bool,
    warned_format: bool,
}

impl VcamSink {
    /// Starts `camera` (standing in for `id`) at `width` x `height` @ `fps`.
    ///
    /// # Errors
    /// Whatever [`VirtualCamera::start`] returns, e.g.
    /// [`VcamError::NotAvailable`] when the OS side is not installed.
    pub fn start(
        id: CameraId,
        mut camera: Box<dyn VirtualCamera>,
        width: u32,
        height: u32,
        fps: f32,
    ) -> Result<Self, VcamError> {
        camera.start(width, height, fps)?;
        Ok(Self {
            id,
            camera,
            fps,
            started: Some((width, height)),
            failed: None,
            counters: Arc::new(VcamCounters::default()),
            warned_restart: false,
            warned_format: false,
        })
    }

    /// The counters, for the stats printer.
    pub fn counters(&self) -> Arc<VcamCounters> {
        Arc::clone(&self.counters)
    }

    /// Makes sure the camera runs at `size`, restarting it if needed.
    /// Returns `false` if it cannot publish at that size.
    fn ensure_size(&mut self, size: (u32, u32)) -> bool {
        if self.started == Some(size) {
            return true;
        }
        if self.failed == Some(size) {
            return false;
        }
        let (w, h) = size;
        if self.warned_restart {
            tracing::debug!("{}: frame size now {w}x{h}, restarting vcam", self.id);
        } else {
            self.warned_restart = true;
            tracing::warn!(
                "{}: frame size changed from {} to {w}x{h}; restarting the virtual camera",
                self.id,
                self.started
                    .map_or_else(|| "(not started)".to_owned(), |(a, b)| format!("{a}x{b}"))
            );
        }
        self.counters.restarts.fetch_add(1, Ordering::Relaxed);
        match self.camera.start(w, h, self.fps) {
            Ok(()) => {
                self.started = Some(size);
                self.failed = None;
                true
            }
            Err(e) => {
                tracing::error!("{}: cannot restart virtual camera at {w}x{h}: {e}", self.id);
                self.started = None;
                self.failed = Some(size);
                false
            }
        }
    }
}

impl FrameSink for VcamSink {
    fn publish(&mut self, frame: &FrameView<'_>) {
        let Some(src) = frame_src(frame) else {
            if !self.warned_format {
                self.warned_format = true;
                tracing::warn!(
                    "{}: cannot publish {} {}x{} (stride {}) frames to a virtual camera",
                    self.id,
                    frame.img_type(),
                    frame.width(),
                    frame.height(),
                    frame.stride()
                );
            }
            self.counters
                .record(Publish::Dropped(DropReason::FormatMismatch));
            return;
        };
        let outcome = if self.ensure_size((src.width(), src.height())) {
            self.camera.publish(&src)
        } else {
            Publish::Dropped(DropReason::NotStarted)
        };
        self.counters.record(outcome);
    }

    fn stream_lost(&mut self) {
        // Keep the camera open: consumers keep seeing the last frame until
        // the stream comes back.
        tracing::info!(
            "{}: stream lost; virtual camera holds its last frame",
            self.id
        );
    }
}

impl Drop for VcamSink {
    fn drop(&mut self) {
        self.camera.stop();
    }
}

/// A recording test double shared by the sink and capture-thread tests.
#[cfg(test)]
pub(crate) mod test_util {
    use std::sync::{Arc, Mutex};

    use vcam::{CameraId, FrameSrc, NullCamera, Publish, VcamError, VcamStats, VirtualCamera};

    /// Geometry of one [`FrameSrc`] as the camera received it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) enum Seen {
        Nv12 {
            w: u32,
            h: u32,
            y_stride: usize,
            uv_stride: usize,
            y_len: usize,
            uv_len: usize,
            y0: u8,
            uv0: u8,
        },
        Gray8 {
            w: u32,
            h: u32,
            stride: usize,
            y_len: usize,
            y0: u8,
        },
    }

    #[derive(Debug, Default)]
    pub(crate) struct Log {
        pub(crate) starts: Vec<(u32, u32, u32)>,
        pub(crate) frames: Vec<Seen>,
        pub(crate) stops: usize,
    }

    /// A [`NullCamera`] that also records what it was asked to do.
    pub(crate) struct Recording {
        inner: NullCamera,
        log: Arc<Mutex<Log>>,
    }

    impl Recording {
        pub(crate) fn boxed(id: CameraId) -> (Box<dyn VirtualCamera>, Arc<Mutex<Log>>) {
            let log = Arc::new(Mutex::new(Log::default()));
            let cam = Self {
                inner: NullCamera::new(id),
                log: Arc::clone(&log),
            };
            (Box::new(cam), log)
        }
    }

    impl VirtualCamera for Recording {
        fn start(&mut self, width: u32, height: u32, fps: f32) -> Result<(), VcamError> {
            self.log
                .lock()
                .unwrap()
                .starts
                .push((width, height, fps.round() as u32));
            self.inner.start(width, height, fps)
        }

        fn publish(&mut self, src: &FrameSrc<'_>) -> Publish {
            let seen = match *src {
                FrameSrc::Nv12 {
                    y,
                    y_stride,
                    uv,
                    uv_stride,
                    w,
                    h,
                    ..
                } => Seen::Nv12 {
                    w,
                    h,
                    y_stride,
                    uv_stride,
                    y_len: y.len(),
                    uv_len: uv.len(),
                    y0: y[0],
                    uv0: uv[0],
                },
                FrameSrc::Gray8 {
                    y, stride, w, h, ..
                } => Seen::Gray8 {
                    w,
                    h,
                    stride,
                    y_len: y.len(),
                    y0: y[0],
                },
            };
            self.log.lock().unwrap().frames.push(seen);
            self.inner.publish(src)
        }

        fn stats(&self) -> VcamStats {
            self.inner.stats()
        }

        fn stop(&mut self) {
            self.log.lock().unwrap().stops += 1;
            self.inner.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use vcam::NullCamera;

    use super::test_util::{Recording, Seen};
    use super::*;

    /// NV12 `w`x`h` with row stride `s` and the UV plane at `s * h`; Y bytes
    /// are 0x10, UV 0x80, padding 0xEE.
    fn nv12(w: usize, h: usize, s: usize) -> Vec<u8> {
        let mut buf = vec![0xEE; s * h + s * h / 2];
        for r in 0..h {
            buf[r * s..r * s + w].fill(0x10);
        }
        for r in 0..h / 2 {
            buf[s * h + r * s..s * h + r * s + w].fill(0x80);
        }
        buf
    }

    #[test]
    fn nv12_frame_keeps_padded_stride() {
        let (cam, log) = Recording::boxed(CameraId::Rgb);
        let mut sink = VcamSink::start(CameraId::Rgb, cam, 8, 4, 30.0).unwrap();
        let buf = nv12(8, 4, 12);
        let v = FrameView::synthetic(&buf, ImgType::Nv12, 8, 4, 12, 48);
        sink.publish(&v);
        {
            let log = log.lock().unwrap();
            assert_eq!(log.starts, [(8, 4, 30)]);
            assert_eq!(
                log.frames,
                [Seen::Nv12 {
                    w: 8,
                    h: 4,
                    y_stride: 12,
                    uv_stride: 12,
                    y_len: 48,
                    uv_len: 24,
                    y0: 0x10,
                    uv0: 0x80,
                }]
            );
        }
        let c = sink.counters();
        assert_eq!((c.published(), c.dropped()), (1, 0));
        assert_eq!(c.summary(), "vcam 1/0");
        drop(sink);
        assert_eq!(log.lock().unwrap().stops, 1, "stopped on drop");
    }

    #[test]
    fn gray8_frame_is_published_as_gray8() {
        let (cam, log) = Recording::boxed(CameraId::Left);
        let mut sink = VcamSink::start(CameraId::Left, cam, 6, 4, 30.0).unwrap();
        let (w, h, s) = (6usize, 4usize, 8usize);
        let mut buf = vec![0xEE; s * h];
        for r in 0..h {
            buf[r * s..r * s + w].fill(0x42);
        }
        let v = FrameView::synthetic(&buf, ImgType::Gray8, 6, 4, 8, 0);
        sink.publish(&v);
        assert_eq!(
            log.lock().unwrap().frames,
            [Seen::Gray8 {
                w: 6,
                h: 4,
                stride: 8,
                y_len: 32,
                y0: 0x42,
            }]
        );
        assert_eq!(sink.counters().published(), 1);
    }

    #[test]
    fn size_change_restarts_exactly_once() {
        let (cam, log) = Recording::boxed(CameraId::Rgb);
        let mut sink = VcamSink::start(CameraId::Rgb, cam, 8, 4, 30.0).unwrap();
        let small = nv12(8, 4, 8);
        let big = nv12(16, 8, 16);
        sink.publish(&FrameView::synthetic(&small, ImgType::Nv12, 8, 4, 8, 0));
        for _ in 0..5 {
            sink.publish(&FrameView::synthetic(&big, ImgType::Nv12, 16, 8, 16, 0));
        }
        let c = sink.counters();
        assert_eq!(log.lock().unwrap().starts, [(8, 4, 30), (16, 8, 30)]);
        assert_eq!(c.restarts(), 1);
        assert_eq!((c.published(), c.dropped()), (6, 0));
        assert_eq!(c.summary(), "vcam 6/0");
    }

    #[test]
    fn unpublishable_frames_count_as_drops_and_lost_stream_keeps_camera() {
        let (cam, log) = Recording::boxed(CameraId::Rgb);
        let mut sink = VcamSink::start(CameraId::Rgb, cam, 2, 2, 30.0).unwrap();
        let buf = [0u8; 12];
        sink.publish(&FrameView::synthetic(&buf, ImgType::Rgb888i, 2, 2, 6, 0));
        // Too short for its NV12 geometry.
        sink.publish(&FrameView::synthetic(&buf[..3], ImgType::Nv12, 2, 2, 2, 0));
        let c = sink.counters();
        assert_eq!(c.dropped_by(DropReason::FormatMismatch), 2);
        assert_eq!((c.published(), c.dropped()), (0, 2));
        sink.stream_lost();
        assert_eq!(log.lock().unwrap().stops, 0);
        assert!(log.lock().unwrap().frames.is_empty());
    }

    #[test]
    fn failed_restart_is_not_retried_every_frame() {
        let (cam, log) = Recording::boxed(CameraId::Left);
        let mut sink = VcamSink::start(CameraId::Left, cam, 4, 2, 30.0).unwrap();
        // NullCamera refuses a zero-sized start; a 0-width GRAY8 view is not
        // convertible, so force the failure through ensure_size directly.
        assert!(!sink.ensure_size((0, 2)));
        assert!(!sink.ensure_size((0, 2)));
        assert_eq!(log.lock().unwrap().starts.len(), 2);
        assert_eq!(sink.counters().restarts(), 1);
        // Back at a valid size it restarts again and publishes.
        let buf = [7u8; 8];
        sink.publish(&FrameView::synthetic(&buf, ImgType::Gray8, 4, 2, 4, 0));
        assert_eq!(sink.counters().published(), 1);
        assert_eq!(log.lock().unwrap().starts.len(), 3);
    }

    #[test]
    fn start_error_propagates() {
        let err = VcamSink::start(
            CameraId::Rgb,
            Box::new(NullCamera::new(CameraId::Rgb)),
            0,
            0,
            30.0,
        );
        assert!(err.is_err());
    }
}
