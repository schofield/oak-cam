//! [`FrameSrc`], the borrowed frame every backend publishes, and
//! [`write_nv12`], the one copy from it into an NV12 destination (a locked
//! IOSurface, a staging buffer, a ring slot).

/// A borrowed frame to publish. Planes may carry row padding (`*_stride` >=
/// the visible row bytes); the last row need not be padded.
#[derive(Clone, Copy, Debug)]
pub enum FrameSrc<'a> {
    /// NV12: full-resolution Y plane plus interleaved half-resolution UV.
    Nv12 {
        /// Y plane, `h` rows of `y_stride` bytes.
        y: &'a [u8],
        /// Bytes between Y rows.
        y_stride: usize,
        /// UV plane, `h.div_ceil(2)` rows of `uv_stride` bytes.
        uv: &'a [u8],
        /// Bytes between UV rows.
        uv_stride: usize,
        /// Width in pixels.
        w: u32,
        /// Height in pixels.
        h: u32,
        /// Capture timestamp, host monotonic nanoseconds (informational).
        ts_ns: i64,
    },
    /// 8-bit luma only (mono sensors). Published as NV12 with UV = 0x80.
    Gray8 {
        /// Luma plane, `h` rows of `stride` bytes.
        y: &'a [u8],
        /// Bytes between rows.
        stride: usize,
        /// Width in pixels.
        w: u32,
        /// Height in pixels.
        h: u32,
        /// Capture timestamp, host monotonic nanoseconds (informational).
        ts_ns: i64,
    },
}

/// Why [`write_nv12`] refused a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// Zero width or height.
    #[error("empty frame")]
    Empty,
    /// Source and destination sizes differ.
    #[error("frame is {src_w}x{src_h}, destination {dst_w}x{dst_h}")]
    SizeMismatch {
        /// Source width.
        src_w: u32,
        /// Source height.
        src_h: u32,
        /// Destination width.
        dst_w: u32,
        /// Destination height.
        dst_h: u32,
    },
    /// A source plane is shorter than its stride and height require.
    #[error("source plane {plane} too short")]
    SourceTooShort {
        /// 0 = Y, 1 = UV.
        plane: usize,
    },
    /// A destination plane is shorter than its stride and height require.
    #[error("destination plane {plane} too short")]
    DestTooShort {
        /// 0 = Y, 1 = UV.
        plane: usize,
    },
}

impl FrameSrc<'_> {
    /// Width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        match *self {
            FrameSrc::Nv12 { w, .. } | FrameSrc::Gray8 { w, .. } => w,
        }
    }

    /// Height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        match *self {
            FrameSrc::Nv12 { h, .. } | FrameSrc::Gray8 { h, .. } => h,
        }
    }

    /// Capture timestamp in nanoseconds.
    #[must_use]
    pub fn ts_ns(&self) -> i64 {
        match *self {
            FrameSrc::Nv12 { ts_ns, .. } | FrameSrc::Gray8 { ts_ns, .. } => ts_ns,
        }
    }

    /// Whether this frame carries its own chroma (NV12) rather than needing
    /// neutral chroma written (Gray8).
    #[must_use]
    pub fn has_chroma(&self) -> bool {
        matches!(self, FrameSrc::Nv12 { .. })
    }
}

/// Bytes a plane of `rows` rows of `row_bytes` visible bytes at `stride`
/// needs (the last row unpadded).
fn plane_len(stride: usize, rows: usize, row_bytes: usize) -> usize {
    if rows == 0 {
        0
    } else {
        stride * (rows - 1) + row_bytes
    }
}

/// Copy `rows` rows of `row_bytes` from `src` (stride `src_stride`) to `dst`
/// (stride `dst_stride`), as one copy when both are tightly packed alike.
fn copy_plane(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    rows: usize,
    row_bytes: usize,
) {
    if src_stride == dst_stride {
        let n = plane_len(src_stride, rows, row_bytes);
        dst[..n].copy_from_slice(&src[..n]);
        return;
    }
    for row in 0..rows {
        let s = row * src_stride;
        let d = row * dst_stride;
        dst[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
    }
}

/// A writable NV12 destination: a locked pixel buffer, a staging buffer, a
/// ring slot.
#[derive(Debug)]
pub struct Nv12Dest<'a> {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Y plane.
    pub y: &'a mut [u8],
    /// Bytes between Y rows.
    pub y_stride: usize,
    /// Interleaved UV plane.
    pub uv: &'a mut [u8],
    /// Bytes between UV rows.
    pub uv_stride: usize,
}

/// Write `src` into the NV12 destination `dst`.
///
/// NV12 sources copy both planes. Gray8 sources copy the Y plane and, when
/// `fill_uv` is true, fill the UV plane with 0x80 (neutral chroma); a backend
/// that reuses destination buffers passes `false` for a buffer whose UV it
/// already filled and has not overwritten since.
///
/// # Errors
/// [`FrameError`] when the sizes differ or a plane is too short; nothing is
/// written then.
pub fn write_nv12(
    src: &FrameSrc<'_>,
    dst: &mut Nv12Dest<'_>,
    fill_uv: bool,
) -> Result<(), FrameError> {
    let (dst_w, dst_h) = (dst.width, dst.height);
    let (dst_y_stride, dst_uv_stride) = (dst.y_stride, dst.uv_stride);
    let dst_y = &mut *dst.y;
    let dst_uv = &mut *dst.uv;
    let (w, h) = (src.width(), src.height());
    if w == 0 || h == 0 {
        return Err(FrameError::Empty);
    }
    if (w, h) != (dst_w, dst_h) {
        return Err(FrameError::SizeMismatch {
            src_w: w,
            src_h: h,
            dst_w,
            dst_h,
        });
    }
    let y_rows = h as usize;
    let y_bytes = w as usize;
    let uv_rows = y_rows.div_ceil(2);
    let uv_bytes = y_bytes.div_ceil(2) * 2;
    if dst_y_stride < y_bytes || dst_y.len() < plane_len(dst_y_stride, y_rows, y_bytes) {
        return Err(FrameError::DestTooShort { plane: 0 });
    }
    if dst_uv_stride < uv_bytes || dst_uv.len() < plane_len(dst_uv_stride, uv_rows, uv_bytes) {
        return Err(FrameError::DestTooShort { plane: 1 });
    }
    match *src {
        FrameSrc::Nv12 {
            y,
            y_stride,
            uv,
            uv_stride,
            ..
        } => {
            if y_stride < y_bytes || y.len() < plane_len(y_stride, y_rows, y_bytes) {
                return Err(FrameError::SourceTooShort { plane: 0 });
            }
            if uv_stride < uv_bytes || uv.len() < plane_len(uv_stride, uv_rows, uv_bytes) {
                return Err(FrameError::SourceTooShort { plane: 1 });
            }
            copy_plane(y, y_stride, dst_y, dst_y_stride, y_rows, y_bytes);
            copy_plane(uv, uv_stride, dst_uv, dst_uv_stride, uv_rows, uv_bytes);
        }
        FrameSrc::Gray8 { y, stride, .. } => {
            if stride < y_bytes || y.len() < plane_len(stride, y_rows, y_bytes) {
                return Err(FrameError::SourceTooShort { plane: 0 });
            }
            copy_plane(y, stride, dst_y, dst_y_stride, y_rows, y_bytes);
            if fill_uv {
                if dst_uv_stride == uv_bytes {
                    dst_uv[..uv_rows * uv_bytes].fill(0x80);
                } else {
                    for row in 0..uv_rows {
                        let d = row * dst_uv_stride;
                        dst_uv[d..d + uv_bytes].fill(0x80);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nv12(w: u32, h: u32, stride: usize) -> (Vec<u8>, Vec<u8>) {
        let rows = h as usize;
        let mut y = vec![0u8; stride * rows];
        let mut uv = vec![0u8; stride * rows.div_ceil(2)];
        for r in 0..rows {
            for c in 0..w as usize {
                y[r * stride + c] = (r * 7 + c) as u8;
            }
        }
        for r in 0..rows.div_ceil(2) {
            for c in 0..(w as usize).div_ceil(2) * 2 {
                uv[r * stride + c] = (r * 3 + c + 100) as u8;
            }
        }
        (y, uv)
    }

    #[test]
    fn nv12_copies_both_planes_across_strides() {
        let (w, h) = (6u32, 4u32);
        let (y, uv) = nv12(w, h, 8); // padded source
        let src = FrameSrc::Nv12 {
            y: &y,
            y_stride: 8,
            uv: &uv,
            uv_stride: 8,
            w,
            h,
            ts_ns: 0,
        };
        let mut dy = vec![0xEE; 16 * 4];
        let mut duv = vec![0xEE; 16 * 2];
        write_nv12(
            &src,
            &mut Nv12Dest {
                width: w,
                height: h,
                y: &mut dy,
                y_stride: 16,
                uv: &mut duv,
                uv_stride: 16,
            },
            true,
        )
        .unwrap();
        for r in 0..4 {
            assert_eq!(&dy[r * 16..r * 16 + 6], &y[r * 8..r * 8 + 6]);
            assert!(dy[r * 16 + 6..r * 16 + 16].iter().all(|&b| b == 0xEE));
        }
        for r in 0..2 {
            assert_eq!(&duv[r * 16..r * 16 + 6], &uv[r * 8..r * 8 + 6]);
        }
    }

    #[test]
    fn nv12_tight_to_tight_is_exact() {
        let (w, h) = (4u32, 2u32);
        let (y, uv) = nv12(w, h, 4);
        let src = FrameSrc::Nv12 {
            y: &y,
            y_stride: 4,
            uv: &uv,
            uv_stride: 4,
            w,
            h,
            ts_ns: 0,
        };
        let mut dy = vec![0; 8];
        let mut duv = vec![0; 4];
        write_nv12(
            &src,
            &mut Nv12Dest {
                width: w,
                height: h,
                y: &mut dy,
                y_stride: 4,
                uv: &mut duv,
                uv_stride: 4,
            },
            false,
        )
        .unwrap();
        assert_eq!(dy, y);
        assert_eq!(duv, uv);
    }

    #[test]
    fn gray8_writes_luma_and_neutral_chroma_once() {
        let (w, h) = (5u32, 3u32); // odd geometry: UV 6 bytes x 2 rows
        let luma: Vec<u8> = (0..(w * h) as u8).collect();
        let src = FrameSrc::Gray8 {
            y: &luma,
            stride: 5,
            w,
            h,
            ts_ns: 7,
        };
        let mut dy = vec![0; 8 * 3];
        let mut duv = vec![0x11; 8 * 2];
        write_nv12(
            &src,
            &mut Nv12Dest {
                width: w,
                height: h,
                y: &mut dy,
                y_stride: 8,
                uv: &mut duv,
                uv_stride: 8,
            },
            true,
        )
        .unwrap();
        for r in 0..3 {
            assert_eq!(&dy[r * 8..r * 8 + 5], &luma[r * 5..r * 5 + 5]);
        }
        for r in 0..2 {
            assert!(duv[r * 8..r * 8 + 6].iter().all(|&b| b == 0x80));
            assert!(duv[r * 8 + 6..r * 8 + 8].iter().all(|&b| b == 0x11));
        }
        // fill_uv = false leaves the chroma alone.
        duv.fill(0x22);
        write_nv12(
            &src,
            &mut Nv12Dest {
                width: w,
                height: h,
                y: &mut dy,
                y_stride: 8,
                uv: &mut duv,
                uv_stride: 8,
            },
            false,
        )
        .unwrap();
        assert!(duv.iter().all(|&b| b == 0x22));
        assert!(!src.has_chroma());
        assert_eq!(src.ts_ns(), 7);
    }

    #[test]
    fn refuses_mismatch_and_short_planes() {
        let luma = vec![0u8; 16];
        let src = FrameSrc::Gray8 {
            y: &luma,
            stride: 4,
            w: 4,
            h: 4,
            ts_ns: 0,
        };
        let mut dy = vec![0; 16];
        let mut duv = vec![0; 8];
        assert!(matches!(
            write_nv12(
                &src,
                &mut Nv12Dest {
                    width: 8,
                    height: 4,
                    y: &mut dy,
                    y_stride: 4,
                    uv: &mut duv,
                    uv_stride: 4
                },
                true
            ),
            Err(FrameError::SizeMismatch { .. })
        ));
        assert_eq!(
            write_nv12(
                &src,
                &mut Nv12Dest {
                    width: 4,
                    height: 4,
                    y: &mut dy[..15],
                    y_stride: 4,
                    uv: &mut duv,
                    uv_stride: 4
                },
                true
            ),
            Err(FrameError::DestTooShort { plane: 0 })
        );
        assert_eq!(
            write_nv12(
                &src,
                &mut Nv12Dest {
                    width: 4,
                    height: 4,
                    y: &mut dy,
                    y_stride: 4,
                    uv: &mut duv[..7],
                    uv_stride: 4
                },
                true
            ),
            Err(FrameError::DestTooShort { plane: 1 })
        );
        let short = FrameSrc::Gray8 {
            y: &luma[..15],
            stride: 4,
            w: 4,
            h: 4,
            ts_ns: 0,
        };
        assert_eq!(
            write_nv12(
                &short,
                &mut Nv12Dest {
                    width: 4,
                    height: 4,
                    y: &mut dy,
                    y_stride: 4,
                    uv: &mut duv,
                    uv_stride: 4
                },
                true
            ),
            Err(FrameError::SourceTooShort { plane: 0 })
        );
        let empty = FrameSrc::Gray8 {
            y: &[],
            stride: 0,
            w: 0,
            h: 0,
            ts_ns: 0,
        };
        assert_eq!(
            write_nv12(
                &empty,
                &mut Nv12Dest {
                    width: 0,
                    height: 0,
                    y: &mut dy,
                    y_stride: 4,
                    uv: &mut duv,
                    uv_stride: 4
                },
                true
            ),
            Err(FrameError::Empty)
        );
    }
}
