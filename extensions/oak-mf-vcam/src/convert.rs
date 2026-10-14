//! Pixel work the media source does per frame, allocation-free: nearest-
//! neighbour scale from the ring's NV12 frame to the consumer's size, with a
//! conversion to YUY2 or BGRX (BT.709 video range in, full range out) when the
//! consumer chose a packed format; and the placeholder card.

use crate::formats::OutputFormat;
use crate::layout::nv12_size;

/// A tightly packed NV12 frame (Y plane `width` bytes per row, then the UV
/// plane `ceil(width / 2) * 2` bytes per row).
#[derive(Clone, Copy, Debug)]
pub struct Nv12Frame<'a> {
    /// The pixel bytes, at least [`nv12_size`] long.
    pub data: &'a [u8],
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Nv12Frame<'_> {
    fn valid(&self) -> bool {
        self.width > 0 && self.height > 0 && self.data.len() >= nv12_size(self.width, self.height)
    }

    fn luma_row(&self, y: usize) -> &[u8] {
        let w = self.width as usize;
        &self.data[y * w..(y + 1) * w]
    }

    /// The UV row covering luma row `y`.
    fn chroma_row(&self, y: usize) -> &[u8] {
        let base = self.width as usize * self.height as usize;
        let stride = (self.width as usize).div_ceil(2) * 2;
        let start = base + (y / 2) * stride;
        &self.data[start..start + stride]
    }
}

/// Maps destination coordinates to source coordinates (16.16 fixed point).
#[derive(Clone, Copy)]
struct Axis {
    step: u64,
    max: usize,
}

impl Axis {
    fn new(src: u32, dst: u32) -> Self {
        Self {
            step: (u64::from(src) << 16) / u64::from(dst.max(1)),
            max: src.max(1) as usize - 1,
        }
    }

    fn map(self, d: usize) -> usize {
        (((d as u64) * self.step) >> 16).min(self.max as u64) as usize
    }
}

/// Bytes a destination buffer needs for `format` at `height` rows of `pitch`.
#[must_use]
pub const fn required_len(format: OutputFormat, height: u32, pitch: usize) -> usize {
    let h = height as usize;
    match format {
        OutputFormat::Nv12 => pitch * h + pitch * h.div_ceil(2),
        OutputFormat::Yuy2 | OutputFormat::Rgb32 => pitch * h,
    }
}

/// Render `src` into `dst` as `format` at `width` x `height`, rows `pitch`
/// bytes apart (NV12's UV plane follows the Y plane at `pitch * height`).
/// Returns `false`, leaving `dst` untouched, when `src` is malformed or `dst`
/// is too small.
pub fn render(
    src: &Nv12Frame<'_>,
    format: OutputFormat,
    width: u32,
    height: u32,
    dst: &mut [u8],
    pitch: usize,
) -> bool {
    if !src.valid()
        || width == 0
        || height == 0
        || pitch < format.row_bytes(width)
        || dst.len() < required_len(format, height, pitch)
    {
        return false;
    }
    let (dw, dh) = (width as usize, height as usize);
    let xs = Axis::new(src.width, width);
    let ys = Axis::new(src.height, height);
    match format {
        OutputFormat::Nv12 => {
            let same_width = src.width == width;
            for y in 0..dh {
                let s = src.luma_row(ys.map(y));
                let d = &mut dst[y * pitch..y * pitch + dw];
                if same_width {
                    d.copy_from_slice(s);
                } else {
                    for (x, px) in d.iter_mut().enumerate() {
                        *px = s[xs.map(x)];
                    }
                }
            }
            let uv_base = pitch * dh;
            let cw = dw.div_ceil(2);
            let cxs = Axis::new(src.width.div_ceil(2), width.div_ceil(2));
            for cy in 0..dh.div_ceil(2) {
                // Chroma row cy covers luma rows 2cy and 2cy+1.
                let s = src.chroma_row(ys.map(cy * 2));
                let start = uv_base + cy * pitch;
                let d = &mut dst[start..start + cw * 2];
                if same_width {
                    d.copy_from_slice(&s[..cw * 2]);
                } else {
                    for (cx, pair) in d.chunks_mut(2).enumerate() {
                        let sx = cxs.map(cx) * 2;
                        pair[0] = s[sx];
                        pair[1] = s[sx + 1];
                    }
                }
            }
        }
        OutputFormat::Yuy2 => {
            for y in 0..dh {
                let sy = ys.map(y);
                let luma = src.luma_row(sy);
                let chroma = src.chroma_row(sy);
                let d = &mut dst[y * pitch..y * pitch + dw.div_ceil(2) * 4];
                for (px, quad) in d.chunks_mut(4).enumerate() {
                    let x0 = xs.map(px * 2);
                    let x1 = xs.map((px * 2 + 1).min(dw - 1));
                    let c = (x0 / 2) * 2;
                    quad[0] = luma[x0];
                    quad[1] = chroma[c];
                    quad[2] = luma[x1];
                    quad[3] = chroma[c + 1];
                }
            }
        }
        OutputFormat::Rgb32 => {
            for y in 0..dh {
                let sy = ys.map(y);
                let luma = src.luma_row(sy);
                let chroma = src.chroma_row(sy);
                let d = &mut dst[y * pitch..y * pitch + dw * 4];
                for (x, bgrx) in d.chunks_mut(4).enumerate() {
                    let sx = xs.map(x);
                    let c = (sx / 2) * 2;
                    let [r, g, b] = bt709_to_rgb(luma[sx], chroma[c], chroma[c + 1]);
                    bgrx[0] = b;
                    bgrx[1] = g;
                    bgrx[2] = r;
                    bgrx[3] = 255;
                }
            }
        }
    }
    true
}

/// BT.709 video-range Y'CbCr to full-range R'G'B' (8.8 fixed point).
#[must_use]
pub fn bt709_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let c = (i32::from(y) - 16) * 298;
    let d = i32::from(u) - 128;
    let e = i32::from(v) - 128;
    let clamp = |x: i32| ((x + 128) >> 8).clamp(0, 255) as u8;
    [
        clamp(c + 459 * e),
        clamp(c - 55 * d - 136 * e),
        clamp(c + 541 * d),
    ]
}

/// Luma of the placeholder's grey background (video range).
pub const PLACEHOLDER_GREY: u8 = 32;
/// Number of luma steps in the placeholder's middle band.
pub const PLACEHOLDER_BARS: usize = 8;

/// Paint the "no signal" card into a tightly packed NV12 buffer: dark grey,
/// with a band of eight luma steps (16 to 235) across the middle third and
/// neutral chroma. The same look as the macOS camera extension's placeholder,
/// so a consumer can tell "camera alive, no programme" from black or a frozen
/// picture. Returns `false` when `dst` is too small.
pub fn paint_placeholder_nv12(dst: &mut [u8], width: u32, height: u32) -> bool {
    let len = nv12_size(width, height);
    if width == 0 || height == 0 || dst.len() < len {
        return false;
    }
    let (w, h) = (width as usize, height as usize);
    let (top, bottom) = (h / 3, h * 2 / 3);
    for (y, row) in dst[..w * h].chunks_mut(w).enumerate() {
        if y < top || y >= bottom {
            row.fill(PLACEHOLDER_GREY);
            continue;
        }
        for (x, px) in row.iter_mut().enumerate() {
            let bar = x * PLACEHOLDER_BARS / w;
            *px = (16 + bar * (235 - 16) / (PLACEHOLDER_BARS - 1)) as u8;
        }
    }
    dst[w * h..len].fill(128);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::OutputFormat;

    fn frame(width: u32, height: u32, luma: impl Fn(usize, usize) -> u8, uv: (u8, u8)) -> Vec<u8> {
        let (w, h) = (width as usize, height as usize);
        let mut v = vec![0u8; nv12_size(width, height)];
        for y in 0..h {
            for x in 0..w {
                v[y * w + x] = luma(x, y);
            }
        }
        for pair in v[w * h..].chunks_mut(2) {
            pair[0] = uv.0;
            pair[1] = uv.1;
        }
        v
    }

    /// The placeholder: grey above and below, eight rising luma steps across
    /// the middle third, neutral chroma.
    #[test]
    fn vcam_placeholder_painter() {
        let (w, h) = (1280u32, 720u32);
        let mut buf = vec![0u8; nv12_size(w, h)];
        assert!(paint_placeholder_nv12(&mut buf, w, h));
        let row = |y: usize| &buf[y * 1280..(y + 1) * 1280];
        assert!(row(0).iter().all(|&p| p == PLACEHOLDER_GREY));
        assert!(row(719).iter().all(|&p| p == PLACEHOLDER_GREY));
        let mid = row(360);
        assert_eq!(mid[0], 16);
        assert_eq!(mid[1279], 235);
        let steps: std::collections::BTreeSet<u8> = mid.iter().copied().collect();
        assert_eq!(steps.len(), PLACEHOLDER_BARS);
        assert!(mid.windows(2).all(|p| p[0] <= p[1]), "monotonic band");
        assert!(buf[1280 * 720..].iter().all(|&c| c == 128));
        assert!(!paint_placeholder_nv12(&mut buf[..10], w, h));
    }

    /// Same-size NV12 with a padded pitch copies the planes row by row.
    #[test]
    fn vcam_convert_nv12_identity_with_pitch() {
        let (w, h) = (64u32, 36u32);
        let src = frame(w, h, |x, y| (x + y * 3) as u8, (90, 200));
        let pitch = 128;
        let mut dst = vec![0xAAu8; required_len(OutputFormat::Nv12, h, pitch)];
        let f = Nv12Frame {
            data: &src,
            width: w,
            height: h,
        };
        assert!(render(&f, OutputFormat::Nv12, w, h, &mut dst, pitch));
        for y in 0..36 {
            assert_eq!(&dst[y * pitch..y * pitch + 64], &src[y * 64..(y + 1) * 64]);
            assert!(
                dst[y * pitch + 64..(y + 1) * pitch]
                    .iter()
                    .all(|&b| b == 0xAA)
            );
        }
        let uv = pitch * 36;
        assert_eq!(dst[uv], 90);
        assert_eq!(dst[uv + 1], 200);
        assert_eq!(dst[uv + 17 * pitch + 62], 90);
        assert_eq!(dst[uv + 17 * pitch + 63], 200);
    }

    /// Scaling 2:1 picks every other source pixel; scaling up repeats them.
    #[test]
    fn vcam_convert_nv12_scales() {
        let src = frame(8, 4, |x, _| (x * 10) as u8, (128, 128));
        let f = Nv12Frame {
            data: &src,
            width: 8,
            height: 4,
        };
        let mut down = vec![0u8; nv12_size(4, 2)];
        assert!(render(&f, OutputFormat::Nv12, 4, 2, &mut down, 4));
        assert_eq!(&down[..4], &[0, 20, 40, 60]);
        let mut up = vec![0u8; nv12_size(16, 8)];
        assert!(render(&f, OutputFormat::Nv12, 16, 8, &mut up, 16));
        assert_eq!(&up[..8], &[0, 0, 10, 10, 20, 20, 30, 30]);
        assert!(up[16 * 8..].iter().all(|&c| c == 128));
    }

    /// YUY2 interleaves Y0 U Y1 V from the NV12 source.
    #[test]
    fn vcam_convert_yuy2() {
        let src = frame(4, 2, |x, _| (100 + x) as u8, (60, 190));
        let f = Nv12Frame {
            data: &src,
            width: 4,
            height: 2,
        };
        let mut dst = vec![0u8; 8 * 2];
        assert!(render(&f, OutputFormat::Yuy2, 4, 2, &mut dst, 8));
        assert_eq!(&dst[..8], &[100, 60, 101, 190, 102, 60, 103, 190]);
        assert_eq!(&dst[8..], &dst[..8].to_vec()[..]);
    }

    /// BGRX conversion: video-range white, black and BT.709 red land on the
    /// full-range values within rounding.
    #[test]
    fn vcam_convert_rgb32_bt709() {
        assert_eq!(bt709_to_rgb(235, 128, 128), [255, 255, 255]);
        assert_eq!(bt709_to_rgb(16, 128, 128), [0, 0, 0]);
        let [r, g, b] = bt709_to_rgb(63, 102, 240);
        assert!(r >= 250 && g <= 5 && b <= 5, "red -> {r},{g},{b}");
        let src = frame(2, 2, |_, _| 235, (128, 128));
        let f = Nv12Frame {
            data: &src,
            width: 2,
            height: 2,
        };
        let mut dst = vec![0u8; 16];
        assert!(render(&f, OutputFormat::Rgb32, 2, 2, &mut dst, 8));
        assert!(dst.iter().all(|&b| b == 255));
    }

    /// Malformed input or a short destination is refused, not written.
    #[test]
    fn vcam_convert_rejects_bad_buffers() {
        let src = frame(4, 4, |_, _| 50, (128, 128));
        let f = Nv12Frame {
            data: &src,
            width: 4,
            height: 4,
        };
        let mut small = vec![7u8; 10];
        assert!(!render(&f, OutputFormat::Nv12, 4, 4, &mut small, 4));
        assert!(small.iter().all(|&b| b == 7));
        let short = Nv12Frame {
            data: &src[..5],
            width: 4,
            height: 4,
        };
        let mut dst = vec![0u8; 64];
        assert!(!render(&short, OutputFormat::Rgb32, 4, 4, &mut dst, 16));
        assert!(
            !render(&f, OutputFormat::Rgb32, 4, 4, &mut dst, 8),
            "pitch too small"
        );
    }
}
