//! Moving test patterns, so every virtual-camera backend can be verified
//! without an OAK attached (`oak-cam vcam test-pattern`).
//!
//! Ported from stream-concern's `sc-video` test pattern (RGBA colour bars with
//! a moving marker and a scrolling ramp) and rewritten to render straight into
//! the two formats oak-cam publishes:
//!
//! - [`Nv12Pattern`]: SMPTE-style colour bars (BT.709 video range) over the top
//!   70 %, a white marker moving right 8 px per frame, and a scrolling luma
//!   ramp across the bottom 30 %. Stands in for the RGB camera.
//! - [`GrayPattern`]: eight luma steps with the same moving marker and
//!   scrolling ramp, as GRAY8. Stands in for the mono cameras and exercises the
//!   Gray8 → NV12 path.
//!
//! Both build their row tables once and render allocation-free; neither has a
//! text overlay (the original had none). Consecutive frames always differ, so
//! a frozen consumer is obvious.

use crate::FrameSrc;

/// SMPTE-ish colour bars as RGB triples (75 % white, yellow, cyan, green,
/// magenta, red, blue).
pub const BARS: [[u8; 3]; 7] = [
    [191, 191, 191],
    [191, 191, 0],
    [0, 191, 191],
    [0, 191, 0],
    [191, 0, 191],
    [191, 0, 0],
    [0, 0, 191],
];

/// Width of the moving marker in pixels.
pub const MARKER_WIDTH: usize = 6;
/// Marker speed in pixels per frame.
pub const MARKER_SPEED: u64 = 8;

/// BT.709 video-range Y, Cb, Cr of an 8-bit RGB triple.
#[must_use]
pub fn rgb_to_ycbcr709(rgb: [u8; 3]) -> [u8; 3] {
    let [red, green, blue] = rgb.map(f32::from);
    let luma = 16.0 + 219.0 * (0.2126 * red + 0.7152 * green + 0.0722 * blue) / 255.0;
    let cb = 128.0 + 224.0 * (-0.114_572 * red - 0.385_428 * green + 0.5 * blue) / 255.0;
    let cr = 128.0 + 224.0 * (0.5 * red - 0.454_153 * green - 0.045_847 * blue) / 255.0;
    let quantise = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    [quantise(luma), quantise(cb), quantise(cr)]
}

/// Rows of the bars band (the rest is the ramp).
fn bars_rows(height: usize) -> usize {
    height * 7 / 10
}

/// The marker's first column in frame `index`.
fn marker_x(index: u64, width: usize) -> usize {
    ((index * MARKER_SPEED) % width as u64) as usize
}

/// A luma ramp two widths long (video range), so any `width` window is
/// continuous; the bottom band slides through it one pixel per frame.
fn luma_ramp(width: usize) -> Vec<u8> {
    (0..width * 2)
        .map(|x| (16 + (x % width) * (235 - 16) / width.max(1)) as u8)
        .collect()
}

/// Moving colour bars, NV12 (tightly packed), any even-or-odd size.
#[derive(Debug)]
pub struct Nv12Pattern {
    width: usize,
    height: usize,
    y: Vec<u8>,
    uv: Vec<u8>,
    bar_y: Vec<u8>,
    bar_uv: Vec<u8>,
    ramp: Vec<u8>,
    frame_index: u64,
}

impl Nv12Pattern {
    /// Build the row tables and the frame buffer for `width` x `height`.
    ///
    /// # Panics
    /// When either dimension is zero.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        assert!(width > 0 && height > 0, "frame size must be non-zero");
        let (w, h) = (width as usize, height as usize);
        let uv_w = w.div_ceil(2) * 2;
        let mut bar_y = vec![0u8; w];
        let mut bar_uv = vec![0u8; uv_w];
        for x in 0..w {
            let [yy, cb, cr] = rgb_to_ycbcr709(BARS[x * BARS.len() / w]);
            bar_y[x] = yy;
            if x % 2 == 0 {
                bar_uv[x] = cb;
                bar_uv[x + 1] = cr;
            }
        }
        Self {
            width: w,
            height: h,
            y: vec![0; w * h],
            uv: vec![0; uv_w * h.div_ceil(2)],
            bar_y,
            bar_uv,
            ramp: luma_ramp(w),
            frame_index: 0,
        }
    }

    /// Frames rendered so far.
    #[must_use]
    pub fn frame_index(&self) -> u64 {
        self.frame_index
    }

    /// Render the next frame and return it, stamped `ts_ns`.
    pub fn next_frame(&mut self, ts_ns: i64) -> FrameSrc<'_> {
        let (w, h) = (self.width, self.height);
        let uv_w = self.bar_uv.len();
        let bars = bars_rows(h);
        let mx = marker_x(self.frame_index, w);
        let mend = (mx + MARKER_WIDTH).min(w);
        let ramp_off = (self.frame_index % w as u64) as usize;

        for (row, line) in self.y.chunks_exact_mut(w).enumerate() {
            if row < bars {
                line.copy_from_slice(&self.bar_y);
                line[mx..mend].fill(235);
            } else {
                line.copy_from_slice(&self.ramp[ramp_off..ramp_off + w]);
            }
        }
        let (umx, umend) = (mx / 2 * 2, mend.div_ceil(2) * 2);
        for (row, line) in self.uv.chunks_exact_mut(uv_w).enumerate() {
            if row * 2 < bars {
                line.copy_from_slice(&self.bar_uv);
                line[umx..umend.min(uv_w)].fill(0x80);
            } else {
                line.fill(0x80);
            }
        }
        self.frame_index += 1;
        FrameSrc::Nv12 {
            y: &self.y,
            y_stride: w,
            uv: &self.uv,
            uv_stride: uv_w,
            w: w as u32,
            h: h as u32,
            ts_ns,
        }
    }
}

/// Moving gray steps, GRAY8 (tightly packed), any size.
#[derive(Debug)]
pub struct GrayPattern {
    width: usize,
    height: usize,
    y: Vec<u8>,
    steps: Vec<u8>,
    ramp: Vec<u8>,
    frame_index: u64,
}

/// Luma steps across the gray pattern's top band.
pub const GRAY_STEPS: usize = 8;

impl GrayPattern {
    /// Build the row tables and the frame buffer for `width` x `height`.
    ///
    /// # Panics
    /// When either dimension is zero.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        assert!(width > 0 && height > 0, "frame size must be non-zero");
        let (w, h) = (width as usize, height as usize);
        let steps = (0..w)
            .map(|x| {
                let step = x * GRAY_STEPS / w;
                (16 + step * (235 - 16) / (GRAY_STEPS - 1)) as u8
            })
            .collect();
        Self {
            width: w,
            height: h,
            y: vec![0; w * h],
            steps,
            ramp: luma_ramp(w),
            frame_index: 0,
        }
    }

    /// Frames rendered so far.
    #[must_use]
    pub fn frame_index(&self) -> u64 {
        self.frame_index
    }

    /// Render the next frame and return it, stamped `ts_ns`.
    pub fn next_frame(&mut self, ts_ns: i64) -> FrameSrc<'_> {
        let (w, h) = (self.width, self.height);
        let bars = bars_rows(h);
        let mx = marker_x(self.frame_index, w);
        let mend = (mx + MARKER_WIDTH).min(w);
        let ramp_off = (self.frame_index % w as u64) as usize;
        // The marker alternates black/white each frame on the brightest step so
        // it stays visible over every step.
        let marker = if self.frame_index.is_multiple_of(2) {
            255
        } else {
            0
        };
        for (row, line) in self.y.chunks_exact_mut(w).enumerate() {
            if row < bars {
                line.copy_from_slice(&self.steps);
                line[mx..mend].fill(marker);
            } else {
                line.copy_from_slice(&self.ramp[ramp_off..ramp_off + w]);
            }
        }
        self.frame_index += 1;
        FrameSrc::Gray8 {
            y: &self.y,
            stride: w,
            w: w as u32,
            h: h as u32,
            ts_ns,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CameraFormat, Nv12Dest, write_nv12};

    #[test]
    fn bt709_reference_values() {
        // White/black at video range, neutral chroma.
        assert_eq!(rgb_to_ycbcr709([255, 255, 255]), [235, 128, 128]);
        assert_eq!(rgb_to_ycbcr709([0, 0, 0]), [16, 128, 128]);
        // Pure red: Y 63, Cb 102, Cr 240 (BT.709 video range).
        assert_eq!(rgb_to_ycbcr709([255, 0, 0]), [63, 102, 240]);
        // Pure blue: Cb at the top of the range.
        assert_eq!(rgb_to_ycbcr709([0, 0, 255])[1], 240);
    }

    #[test]
    fn nv12_bars_fill_every_byte_and_move() {
        let mut p = Nv12Pattern::new(64, 20);
        let first = match p.next_frame(1) {
            FrameSrc::Nv12 {
                y, uv, w, h, ts_ns, ..
            } => {
                assert_eq!((w, h, ts_ns), (64, 20, 1));
                assert_eq!(y.len() + uv.len(), CameraFormat::Nv12.frame_size(64, 20));
                // Past the marker, the first column is the first bar.
                let [by, bcb, bcr] = rgb_to_ycbcr709(BARS[0]);
                assert_eq!(y[MARKER_WIDTH], by);
                assert_eq!(&uv[MARKER_WIDTH..MARKER_WIDTH + 2], &[bcb, bcr]);
                // The last column is the last bar.
                assert_eq!(y[63], rgb_to_ycbcr709(BARS[6])[0]);
                // The marker is white with neutral chroma.
                assert!(y[..MARKER_WIDTH].iter().all(|&v| v == 235));
                assert!(uv[..MARKER_WIDTH].iter().all(|&v| v == 0x80));
                // The bottom band is the ramp: video-range luma, neutral chroma.
                let last = &y[64 * 19..];
                assert!(last.iter().all(|&v| (16..=235).contains(&v)));
                assert!(uv[64 * 9..].iter().all(|&v| v == 0x80));
                [y.to_vec(), uv.to_vec()].concat()
            }
            FrameSrc::Gray8 { .. } => unreachable!(),
        };
        let FrameSrc::Nv12 { y, uv, .. } = p.next_frame(2) else {
            unreachable!()
        };
        assert_ne!(first, [y.to_vec(), uv.to_vec()].concat(), "frames move");
        assert_eq!(p.frame_index(), 2);
    }

    #[test]
    fn nv12_handles_odd_sizes_and_marker_wrap() {
        let mut p = Nv12Pattern::new(17, 5);
        for i in 0..40 {
            let FrameSrc::Nv12 {
                y,
                uv,
                y_stride,
                uv_stride,
                ..
            } = p.next_frame(i)
            else {
                unreachable!()
            };
            assert_eq!((y_stride, uv_stride), (17, 18));
            assert_eq!(y.len(), 17 * 5);
            assert_eq!(uv.len(), 18 * 3);
        }
    }

    #[test]
    fn gray_steps_move_and_publish_as_nv12() {
        let mut p = GrayPattern::new(1280, 800);
        let a = match p.next_frame(0) {
            FrameSrc::Gray8 {
                y, stride, w, h, ..
            } => {
                assert_eq!((stride, w, h), (1280, 1280, 800));
                // Eight steps from 16 to 235, left to right (after the marker).
                assert_eq!(y[100], 16);
                assert_eq!(y[200], 47);
                assert_eq!(y[1279], 235);
                y.to_vec()
            }
            FrameSrc::Nv12 { .. } => unreachable!(),
        };
        let frame = p.next_frame(1);
        let FrameSrc::Gray8 { y, .. } = frame else {
            unreachable!()
        };
        assert_ne!(a, y, "frames move");

        // A Gray8 frame widens to NV12 with neutral chroma.
        let mut dy = vec![0u8; 1280 * 800];
        let mut duv = vec![0u8; 1280 * 400];
        write_nv12(
            &frame,
            &mut Nv12Dest {
                width: 1280,
                height: 800,
                y: &mut dy,
                y_stride: 1280,
                uv: &mut duv,
                uv_stride: 1280,
            },
            true,
        )
        .unwrap();
        assert_eq!(dy, y);
        assert!(duv.iter().all(|&v| v == 0x80));
    }

    #[test]
    #[should_panic(expected = "non-zero")]
    fn rejects_empty_size() {
        let _ = GrayPattern::new(0, 4);
    }
}
