//! Helpers shared by the Windows integration tests (included with `mod`).

#![allow(dead_code)]

use oak_mf_vcam::layout::nv12_size;

/// Luma of the pattern's left half (the placeholder never shows it at x < w/4
/// outside its band, and inside the band the first bars are dark).
pub const LEFT_Y: u8 = 200;
/// Luma of the pattern's right half.
pub const RIGHT_Y: u8 = 60;
/// Side of the top-left marker block that carries the frame counter.
pub const MARKER: usize = 64;

/// Fill `buf` (tight NV12, `w` x `h`) with the test pattern for frame `n`:
/// left half LEFT_Y, right half RIGHT_Y, neutral chroma, and a top-left
/// MARKER x MARKER block whose luma is `16 + n % 200`.
pub fn paint_pattern(buf: &mut [u8], w: u32, h: u32, n: u64) {
    let (wu, hu) = (w as usize, h as usize);
    assert!(buf.len() >= nv12_size(w, h));
    for (y, row) in buf[..wu * hu].chunks_mut(wu).enumerate() {
        for (x, px) in row.iter_mut().enumerate() {
            *px = if x < MARKER && y < MARKER {
                16 + (n % 200) as u8
            } else if x < wu / 2 {
                LEFT_Y
            } else {
                RIGHT_Y
            };
        }
    }
    buf[wu * hu..nv12_size(w, h)].fill(128);
}

/// Whether a luma value is within `tol` of `want`.
pub fn near(got: u8, want: u8, tol: u8) -> bool {
    got.abs_diff(want) <= tol
}
