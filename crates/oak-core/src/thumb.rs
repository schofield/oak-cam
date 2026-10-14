//! Preview thumbnails for a GUI: a [`FrameTap`] per sensor that the capture
//! thread offers every frame to, and that keeps a small decimated copy of
//! at most one frame per [`THUMB_INTERVAL_NS`].
//!
//! The capture thread never waits on the GUI: it checks the rate limit with
//! one atomic load, then `try_lock`s the slot and skips the frame if the GUI
//! is reading it. The copy is point-sampled (every `step`-th pixel of every
//! `step`-th row, `step` chosen so the thumbnail is at most
//! [`THUMB_MAX_WIDTH`] wide): about 58 K luma bytes plus the matching chroma
//! for a 1080p frame, into reused buffers. Colour conversion is left to the
//! reader. Without a GUI no tap exists and the capture path does not touch
//! any of this.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};

use depthai::{FrameView, ImgType};

use crate::capture::SensorId;

/// Widest thumbnail kept, pixels.
pub const THUMB_MAX_WIDTH: u32 = 320;

/// Minimum time between two thumbnails of one sensor, ns (at most 10 per
/// second).
pub const THUMB_INTERVAL_NS: i64 = 100_000_000;

/// Pixel layout of a [`Thumb`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThumbFormat {
    /// Luma only (mono sensors).
    #[default]
    Gray,
    /// Luma plus one (U, V) pair per thumbnail pixel.
    Yuv,
}

/// A point-sampled copy of one frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Thumb {
    /// Width, pixels.
    pub width: u32,
    /// Height, pixels.
    pub height: u32,
    /// Luma, `width * height` bytes, tightly packed.
    pub y: Vec<u8>,
    /// With [`ThumbFormat::Yuv`]: `(U, V)` per pixel, `2 * width * height`
    /// bytes; empty otherwise.
    pub uv: Vec<u8>,
    /// Layout.
    pub format: ThumbFormat,
    /// Size of the frame it was taken from.
    pub source: (u32, u32),
    /// The frame's sequence number.
    pub seq: i64,
}

/// The decimation step that brings `width` to at most `max_width`.
pub fn decimation_step(width: u32, max_width: u32) -> u32 {
    width.div_ceil(max_width.max(1)).max(1)
}

impl Thumb {
    /// Fills `self` from NV12 planes (`y` / `uv` with their row strides),
    /// keeping every `step`-th pixel. Each kept pixel takes the chroma pair
    /// of the 2x2 block it lies in.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_nv12(
        &mut self,
        y: &[u8],
        y_stride: usize,
        uv: &[u8],
        uv_stride: usize,
        width: u32,
        height: u32,
        step: u32,
    ) {
        self.fill_luma(y, y_stride, width, height, step);
        self.format = ThumbFormat::Yuv;
        let (tw, th, s) = (
            self.width as usize,
            self.height as usize,
            step.max(1) as usize,
        );
        self.uv.clear();
        self.uv.reserve(tw * th * 2);
        for ty in 0..th {
            let row = (ty * s / 2) * uv_stride;
            for tx in 0..tw {
                let at = row + (tx * s / 2) * 2;
                let pair = uv.get(at..at + 2).unwrap_or(&[128, 128]);
                self.uv.extend_from_slice(pair);
            }
        }
    }

    /// Fills `self` from one 8-bit plane, keeping every `step`-th pixel.
    pub fn fill_gray(&mut self, y: &[u8], stride: usize, width: u32, height: u32, step: u32) {
        self.fill_luma(y, stride, width, height, step);
        self.format = ThumbFormat::Gray;
        self.uv.clear();
    }

    fn fill_luma(&mut self, y: &[u8], stride: usize, width: u32, height: u32, step: u32) {
        let step = step.max(1);
        let (tw, th) = (width.div_ceil(step), height.div_ceil(step));
        self.width = tw;
        self.height = th;
        self.source = (width, height);
        let s = step as usize;
        self.y.clear();
        self.y.reserve(tw as usize * th as usize);
        for ty in 0..th as usize {
            let row = ty * s * stride;
            for tx in 0..tw as usize {
                self.y.push(y.get(row + tx * s).copied().unwrap_or(0));
            }
        }
    }

    /// Fills `self` from a frame (NV12 or GRAY8), at most `max_width` wide.
    /// Returns `false` (leaving `self` alone) for other formats or a layout
    /// that does not fit its buffer.
    pub fn fill_from(&mut self, v: &FrameView<'_>, max_width: u32) -> bool {
        let step = decimation_step(v.width(), max_width);
        let stride = v.stride() as usize;
        let ok = match v.img_type() {
            ImgType::Nv12 => v.nv12_planes().is_some_and(|(y, uv)| {
                self.fill_nv12(y, stride, uv, stride, v.width(), v.height(), step);
                true
            }),
            ImgType::Gray8 => v.gray8().is_some_and(|y| {
                self.fill_gray(y, stride, v.width(), v.height(), step);
                true
            }),
            _ => false,
        };
        if ok {
            self.seq = v.seq();
        }
        ok
    }
}

/// One sensor's thumbnail slot, shared by its capture thread (writer) and
/// the GUI (reader).
#[derive(Debug)]
pub struct FrameTap {
    slot: Mutex<Thumb>,
    /// Bumped on every new thumbnail; 0 = none yet.
    version: AtomicU64,
    /// Steady-clock time of the last thumbnail, ns (`i64::MIN` = never).
    last_ns: AtomicI64,
    /// Minimum time between thumbnails, ns ([`THUMB_INTERVAL_NS`] unless
    /// [`FrameTap::set_rate`] changed it).
    interval_ns: AtomicI64,
}

impl Default for FrameTap {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameTap {
    /// An empty tap.
    pub fn new() -> Self {
        Self {
            slot: Mutex::new(Thumb::default()),
            version: AtomicU64::new(0),
            last_ns: AtomicI64::new(i64::MIN),
            interval_ns: AtomicI64::new(THUMB_INTERVAL_NS),
        }
    }

    /// Keeps at most `fps` thumbnails per second (clamped to 1..=10, the
    /// [`THUMB_INTERVAL_NS`] ceiling). Takes effect on the next frame.
    pub fn set_rate(&self, fps: u32) {
        let fps = i64::from(fps.clamp(1, 10));
        self.interval_ns
            .store(1_000_000_000 / fps, Ordering::Relaxed);
    }

    /// Called by the capture thread for every frame, at steady time
    /// `now_ns`: keeps a thumbnail if the tap's interval (default
    /// [`THUMB_INTERVAL_NS`]) has passed since the last one and the reader
    /// is not holding the slot. Never blocks. Returns whether a thumbnail
    /// was taken.
    pub fn offer(&self, v: &FrameView<'_>, now_ns: i64) -> bool {
        let last = self.last_ns.load(Ordering::Relaxed);
        let interval = self.interval_ns.load(Ordering::Relaxed);
        if last != i64::MIN && now_ns.saturating_sub(last) < interval {
            return false;
        }
        let mut slot = match self.slot.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return false,
        };
        if !slot.fill_from(v, THUMB_MAX_WIDTH) {
            return false;
        }
        drop(slot);
        self.last_ns.store(now_ns, Ordering::Relaxed);
        self.version.fetch_add(1, Ordering::Release);
        true
    }

    /// Bumped on every new thumbnail (0 = none yet); cheap to poll.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Locks the slot for reading. Keep it short: while it is held the
    /// capture thread skips thumbnails (it never waits).
    pub fn lock(&self) -> MutexGuard<'_, Thumb> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The three sensors' taps.
#[derive(Debug)]
pub struct FrameTaps {
    taps: [Arc<FrameTap>; 3],
}

impl Default for FrameTaps {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameTaps {
    /// Three empty taps.
    pub fn new() -> Self {
        Self {
            taps: std::array::from_fn(|_| Arc::new(FrameTap::new())),
        }
    }

    /// `sensor`'s tap.
    pub fn get(&self, sensor: SensorId) -> &Arc<FrameTap> {
        &self.taps[sensor.index()]
    }

    /// Sets every tap's rate (see [`FrameTap::set_rate`]).
    pub fn set_rate(&self, fps: u32) {
        for t in &self.taps {
            t.set_rate(fps);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;

    #[test]
    fn step_keeps_width_under_the_limit() {
        assert_eq!(decimation_step(1920, 320), 6);
        assert_eq!(decimation_step(3840, 320), 12);
        assert_eq!(decimation_step(1280, 320), 4);
        assert_eq!(decimation_step(640, 320), 2);
        assert_eq!(decimation_step(100, 320), 1);
        assert_eq!(decimation_step(321, 320), 2);
        assert_eq!(decimation_step(5, 0), 5);
    }

    /// NV12 8x4 with stride 10: luma = x + 10*y; the chroma pair of 2x2
    /// block (bx, by) is (i, 200 + i) with i = 4*by + bx.
    fn nv12_8x4() -> (Vec<u8>, Vec<u8>) {
        let stride = 10;
        let mut y = vec![0u8; stride * 4];
        for r in 0..4 {
            for c in 0..8 {
                y[r * stride + c] = (c + 10 * r) as u8;
            }
        }
        let mut uv = vec![0u8; stride * 2];
        for r in 0..2 {
            for b in 0..4 {
                let idx = (r * 4 + b) as u8;
                uv[r * stride + 2 * b] = idx;
                uv[r * stride + 2 * b + 1] = 200 + idx;
            }
        }
        (y, uv)
    }

    #[test]
    fn nv12_decimation_samples_luma_and_block_chroma() {
        let (y, uv) = nv12_8x4();
        let mut t = Thumb::default();
        t.fill_nv12(&y, 10, &uv, 10, 8, 4, 2);
        assert_eq!((t.width, t.height), (4, 2));
        assert_eq!(t.source, (8, 4));
        assert_eq!(t.format, ThumbFormat::Yuv);
        assert_eq!(t.y, [0, 2, 4, 6, 20, 22, 24, 26]);
        // Pixel (tx, ty) comes from source (2tx, 2ty): block (tx, ty).
        assert_eq!(
            t.uv,
            [
                0, 200, 1, 201, 2, 202, 3, 203, 4, 204, 5, 205, 6, 206, 7, 207
            ]
        );

        // Step 3: 3x2 thumb from columns 0,3,6 and rows 0,3.
        t.fill_nv12(&y, 10, &uv, 10, 8, 4, 3);
        assert_eq!((t.width, t.height), (3, 2));
        assert_eq!(t.y, [0, 3, 6, 30, 33, 36]);
        // (3,0) lies in block (1,0); (6,3) in block (3,1) = index 7.
        assert_eq!(&t.uv[..6], &[0, 200, 1, 201, 3, 203]);
        assert_eq!(&t.uv[6..], &[4, 204, 5, 205, 7, 207]);
    }

    #[test]
    fn gray_decimation_and_short_planes() {
        let y: Vec<u8> = (0..16).collect();
        let mut t = Thumb::default();
        t.fill_gray(&y, 4, 4, 4, 2);
        assert_eq!((t.width, t.height, t.format), (2, 2, ThumbFormat::Gray));
        assert_eq!(t.y, [0, 2, 8, 10]);
        assert!(t.uv.is_empty());
        // A plane shorter than its geometry reads as black, never panics.
        t.fill_gray(&y[..5], 4, 4, 4, 2);
        assert_eq!(t.y, [0, 2, 0, 0]);
    }

    #[test]
    fn fill_from_a_frame_view() {
        let (y, uv) = nv12_8x4();
        let mut buf = y.clone();
        buf.extend_from_slice(&uv);
        let v = FrameView::synthetic(&buf, ImgType::Nv12, 8, 4, 10, 40);
        let mut t = Thumb::default();
        assert!(t.fill_from(&v, 4));
        assert_eq!((t.width, t.height), (4, 2));
        assert_eq!(t.y, [0, 2, 4, 6, 20, 22, 24, 26]);
        assert_eq!(&t.uv[..4], &[0, 200, 1, 201]);
        let raw = FrameView::synthetic(&buf, ImgType::Raw8, 8, 4, 10, 0);
        assert!(!t.fill_from(&raw, 4));
    }

    #[test]
    fn tap_is_rate_limited_and_never_blocks() {
        let y = vec![7u8; 640 * 400];
        let v = FrameView::synthetic(&y, ImgType::Gray8, 640, 400, 640, 0);
        let tap = FrameTap::new();
        assert_eq!(tap.version(), 0);
        assert!(tap.offer(&v, 0));
        assert_eq!(tap.version(), 1);
        {
            let t = tap.lock();
            assert_eq!((t.width, t.height), (320, 200));
            assert!(t.y.iter().all(|&p| p == 7));
        }
        // Within 100 ms: skipped.
        assert!(!tap.offer(&v, 50 * MS));
        assert!(tap.offer(&v, 100 * MS));
        // The reader holds the slot: the writer skips instead of waiting.
        let guard = tap.lock();
        assert!(!tap.offer(&v, 300 * MS));
        drop(guard);
        assert!(tap.offer(&v, 300 * MS));
        assert_eq!(tap.version(), 3);
    }

    #[test]
    fn tap_rate_can_be_lowered_and_is_capped_at_ten() {
        let y = vec![7u8; 64 * 40];
        let v = FrameView::synthetic(&y, ImgType::Gray8, 64, 40, 64, 0);
        let taps = FrameTaps::new();
        taps.set_rate(5);
        let tap = taps.get(SensorId::Rgb);
        assert!(tap.offer(&v, 0));
        assert!(!tap.offer(&v, 150 * MS), "5 fps: 200 ms apart");
        assert!(tap.offer(&v, 200 * MS));
        tap.set_rate(60);
        assert!(!tap.offer(&v, 250 * MS), "never faster than 10 fps");
        assert!(tap.offer(&v, 300 * MS));
    }

    #[test]
    fn taps_per_sensor() {
        let taps = FrameTaps::new();
        assert!(!Arc::ptr_eq(
            taps.get(SensorId::Left),
            taps.get(SensorId::Right)
        ));
    }
}
