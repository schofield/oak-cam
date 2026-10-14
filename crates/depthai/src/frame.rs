//! Received frames: [`Frame`] (owns an `ImgFrame`) and [`FrameView`] (a
//! borrowed, typed view of its pixels and metadata).

use std::ptr::NonNull;
use std::time::Duration;

use depthai_sys as sys;

use crate::{Error, ImgType, Result, check};

/// One image received from an [`crate::OutputQueue`]. Owns the underlying
/// `std::shared_ptr<dai::ImgFrame>`; released on drop.
///
/// `Send + Sync`: the frame is immutable once received and the shim allows
/// reads and release from any thread.
pub struct Frame {
    raw: NonNull<sys::oak_frame>,
    view: sys::oak_frame_view,
}

// SAFETY: oak_frame_* are read-only and callable from any thread; release
// happens once, in Drop (oak_dai.h, "Threading").
unsafe impl Send for Frame {}
// SAFETY: the pixel buffer is never written after the frame is received.
unsafe impl Sync for Frame {}

impl Frame {
    /// Takes ownership of a frame handle from the shim.
    pub(crate) fn from_raw(raw: NonNull<sys::oak_frame>) -> Result<Self> {
        let mut view = sys::oak_frame_view::default();
        // SAFETY: handle is live; view is a valid out-param.
        let rc = unsafe { sys::oak_frame_get_view(raw.as_ptr(), &raw mut view) };
        if let Err(e) = check(rc) {
            // SAFETY: we own the handle and have not wrapped it yet.
            unsafe { sys::oak_frame_release(raw.as_ptr()) };
            return Err(e);
        }
        if view.data.is_null() && view.len != 0 {
            // SAFETY: as above.
            unsafe { sys::oak_frame_release(raw.as_ptr()) };
            return Err(Error::Depthai("frame view has NULL data".into()));
        }
        Ok(Self { raw, view })
    }

    /// Pixels and metadata. The view borrows the frame.
    pub fn view(&self) -> FrameView<'_> {
        let data: &[u8] = if self.view.data.is_null() {
            &[]
        } else {
            // SAFETY: the shim guarantees `data` points at `len` bytes that
            // stay valid and unmodified until oak_frame_release (our Drop),
            // and the returned view borrows `self`.
            unsafe { std::slice::from_raw_parts(self.view.data, self.view.len) }
        };
        FrameView::from_raw(data, &self.view)
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        // SAFETY: we own the handle; released exactly once.
        unsafe { sys::oak_frame_release(self.raw.as_ptr()) };
    }
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.view().fmt(f)
    }
}

/// A borrowed view of a frame: pixel bytes (starting at plane 0) plus
/// metadata. Plane offsets are relative to [`FrameView::data`].
#[derive(Clone, Copy)]
pub struct FrameView<'a> {
    data: &'a [u8],
    width: u32,
    height: u32,
    stride: u32,
    img_type: ImgType,
    plane2_offset: u32,
    plane3_offset: u32,
    instance_num: i32,
    ts_host_ns: i64,
    ts_device_ns: i64,
    seq: i64,
    exposure_us: u32,
    iso: u32,
    lens_pos: i32,
    color_temp_k: i32,
}

impl<'a> FrameView<'a> {
    fn from_raw(data: &'a [u8], v: &sys::oak_frame_view) -> Self {
        Self {
            data,
            width: v.width,
            height: v.height,
            stride: v.stride,
            img_type: ImgType::from_raw(v.type_),
            plane2_offset: v.plane2_offset,
            plane3_offset: v.plane3_offset,
            instance_num: v.instance_num,
            ts_host_ns: v.ts_host_ns,
            ts_device_ns: v.ts_device_ns,
            seq: v.seq,
            exposure_us: v.exposure_us,
            iso: v.iso,
            lens_pos: v.lens_pos,
            color_temp_k: v.color_temp_k,
        }
    }

    /// A view over caller-owned bytes, for tests and synthetic frames.
    /// Metadata other than the layout is zero.
    pub fn synthetic(
        data: &'a [u8],
        img_type: ImgType,
        width: u32,
        height: u32,
        stride: u32,
        plane2_offset: u32,
    ) -> Self {
        let v = sys::oak_frame_view {
            width,
            height,
            stride,
            plane2_offset,
            type_: img_type.to_raw(),
            len: data.len(),
            ..sys::oak_frame_view::default()
        };
        Self::from_raw(data, &v)
    }

    /// All pixel bytes, from the start of plane 0.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }
    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }
    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }
    /// Bytes per row of plane 0 (and of the NV12 UV plane).
    pub fn stride(&self) -> u32 {
        self.stride
    }
    /// Pixel format.
    pub fn img_type(&self) -> ImgType {
        self.img_type
    }
    /// Byte offset of plane 1 (NV12 UV); 0 for single-plane formats.
    pub fn plane2_offset(&self) -> u32 {
        self.plane2_offset
    }
    /// Byte offset of plane 2 (`YUV420p` V); 0 otherwise.
    pub fn plane3_offset(&self) -> u32 {
        self.plane3_offset
    }
    /// Camera instance (socket number) the frame came from.
    pub fn instance_num(&self) -> i32 {
        self.instance_num
    }
    /// Host-synchronised capture timestamp, `steady_clock` ns (compare with
    /// [`crate::steady_now_ns`]).
    pub fn ts_host_ns(&self) -> i64 {
        self.ts_host_ns
    }
    /// [`Self::ts_host_ns`] as a `Duration` since the steady-clock epoch.
    pub fn ts_host(&self) -> Duration {
        Duration::from_nanos(u64::try_from(self.ts_host_ns).unwrap_or(0))
    }
    /// Device monotonic timestamp, ns.
    pub fn ts_device_ns(&self) -> i64 {
        self.ts_device_ns
    }
    /// Sequence number (per camera; gaps mean dropped frames).
    pub fn seq(&self) -> i64 {
        self.seq
    }
    /// Exposure time in microseconds.
    pub fn exposure_us(&self) -> u32 {
        self.exposure_us
    }
    /// Sensitivity (ISO).
    pub fn iso(&self) -> u32 {
        self.iso
    }
    /// Lens position (0 on fixed-focus cameras).
    pub fn lens_pos(&self) -> i32 {
        self.lens_pos
    }
    /// White-balance colour temperature in K (0 on mono cameras).
    pub fn color_temp(&self) -> i32 {
        self.color_temp_k
    }

    fn dims(&self) -> Option<(usize, usize, usize)> {
        let (w, h, s) = (
            self.width as usize,
            self.height as usize,
            self.stride as usize,
        );
        (w > 0 && h > 0 && s >= w).then_some((w, h, s))
    }

    /// NV12 planes `(y, uv)`, each `stride` bytes per row (`height` and
    /// `height / 2` rows). The last row of each slice may be shorter than
    /// `stride` (at least `width` bytes). `None` if this is not NV12, the
    /// width or height is odd (4:2:0 needs even dimensions), or the layout
    /// does not fit the buffer.
    pub fn nv12_planes(&self) -> Option<(&'a [u8], &'a [u8])> {
        if self.img_type != ImgType::Nv12 {
            return None;
        }
        let (w, h, s) = self.nv12_dims()?;
        let len = self.data.len();
        let y_full = s.checked_mul(h)?;
        let p2 = if self.plane2_offset == 0 {
            y_full
        } else {
            self.plane2_offset as usize
        };
        let y_min = s * (h - 1) + w;
        if p2 < y_min {
            return None;
        }
        let uv_rows = h / 2;
        let uv_min = p2.checked_add(s * (uv_rows - 1) + w)?;
        if uv_min > len {
            return None;
        }
        let y_end = y_full.min(p2);
        let uv_end = p2.checked_add(s * uv_rows)?.min(len);
        Some((&self.data[..y_end], &self.data[p2..uv_end]))
    }

    /// The GRAY8 plane, `stride` bytes per row, `height` rows (the last row
    /// may be shorter than `stride`). `None` if not GRAY8 or too short.
    pub fn gray8(&self) -> Option<&'a [u8]> {
        if self.img_type != ImgType::Gray8 {
            return None;
        }
        let (w, h, s) = self.dims()?;
        let min = s * (h - 1) + w;
        if self.data.len() < min {
            return None;
        }
        Some(&self.data[..(s * h).min(self.data.len())])
    }

    /// [`Self::dims`] with even width and height, as NV12 requires.
    fn nv12_dims(&self) -> Option<(usize, usize, usize)> {
        let (w, h, s) = self.dims()?;
        (w % 2 == 0 && h % 2 == 0).then_some((w, h, s))
    }

    /// Bytes of a tightly packed copy (no row padding): `w*h*3/2` for NV12
    /// (even dimensions only), `w*h` for GRAY8; `None` for other formats or
    /// odd NV12 dimensions. Always equals what [`Self::copy_packed`] writes
    /// when that succeeds.
    pub fn packed_len(&self) -> Option<usize> {
        let (w, h) = (self.width as usize, self.height as usize);
        match self.img_type {
            ImgType::Nv12 => self.nv12_dims().map(|(w, h, _)| w * h * 3 / 2),
            ImgType::Gray8 => Some(w * h),
            _ => None,
        }
    }

    /// Replaces `out` with a tightly packed copy (rows of exactly `width`
    /// bytes; NV12 is Y then interleaved UV). Returns the byte count, or
    /// `None` for formats other than NV12 / GRAY8 or a bad layout.
    pub fn copy_packed(&self, out: &mut Vec<u8>) -> Option<usize> {
        out.clear();
        let (w, h, s) = self.dims()?;
        let push_rows = |out: &mut Vec<u8>, plane: &[u8], rows: usize, row_len: usize| {
            for r in 0..rows {
                out.extend_from_slice(&plane[r * s..r * s + row_len]);
            }
        };
        match self.img_type {
            ImgType::Nv12 => {
                let (y, uv) = self.nv12_planes()?;
                out.reserve(self.packed_len()?);
                push_rows(out, y, h, w);
                push_rows(out, uv, h / 2, w);
            }
            ImgType::Gray8 => {
                let g = self.gray8()?;
                out.reserve(w * h);
                push_rows(out, g, h, w);
            }
            _ => return None,
        }
        Some(out.len())
    }
}

impl std::fmt::Debug for FrameView<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameView")
            .field("len", &self.data.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("img_type", &self.img_type)
            .field("plane2_offset", &self.plane2_offset)
            .field("plane3_offset", &self.plane3_offset)
            .field("instance_num", &self.instance_num)
            .field("ts_host_ns", &self.ts_host_ns)
            .field("ts_device_ns", &self.ts_device_ns)
            .field("seq", &self.seq)
            .field("exposure_us", &self.exposure_us)
            .field("iso", &self.iso)
            .field("lens_pos", &self.lens_pos)
            .field("color_temp", &self.color_temp_k)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn assert_send_sync<T: Send + Sync>() {}
    const _: () = assert_send_sync::<Frame>();

    /// NV12 4x4 with stride 6 and a 2-byte gap before the UV plane:
    /// Y rows are 10+r*10+c, UV rows 200+r*10+c, padding 0xEE.
    fn nv12_padded() -> Vec<u8> {
        let (w, h, s) = (4usize, 4usize, 6usize);
        let mut buf = vec![0xEE; s * h + 2 + s * (h / 2)];
        for r in 0..h {
            for c in 0..w {
                buf[r * s + c] = (10 + r * 10 + c) as u8;
            }
        }
        let p2 = s * h + 2;
        for r in 0..h / 2 {
            for c in 0..w {
                buf[p2 + r * s + c] = (200 + r * 10 + c) as u8;
            }
        }
        buf
    }

    #[test]
    fn nv12_planes_respect_stride_and_offset() {
        let buf = nv12_padded();
        let v = FrameView::synthetic(&buf, ImgType::Nv12, 4, 4, 6, 26);
        let (y, uv) = v.nv12_planes().unwrap();
        assert_eq!(y.len(), 24);
        assert_eq!(&y[6..10], &[20, 21, 22, 23]);
        assert_eq!(uv.len(), 12);
        assert_eq!(&uv[..4], &[200, 201, 202, 203]);
        assert_eq!(&uv[6..10], &[210, 211, 212, 213]);

        let mut packed = Vec::new();
        assert_eq!(v.copy_packed(&mut packed), Some(24));
        assert_eq!(v.packed_len(), Some(24));
        assert_eq!(
            packed,
            [
                10, 11, 12, 13, 20, 21, 22, 23, 30, 31, 32, 33, 40, 41, 42, 43, //
                200, 201, 202, 203, 210, 211, 212, 213
            ]
        );
    }

    #[test]
    fn nv12_unset_plane2_means_stride_times_height() {
        let buf = vec![7u8; 4 * 2 * 3 / 2];
        let v = FrameView::synthetic(&buf, ImgType::Nv12, 4, 2, 4, 0);
        let (y, uv) = v.nv12_planes().unwrap();
        assert_eq!((y.len(), uv.len()), (8, 4));
    }

    #[test]
    fn nv12_accepts_short_last_row() {
        // Buffer ends right after the last UV row's `width` bytes.
        let mut buf = nv12_padded();
        buf.truncate(26 + 6 + 4);
        let v = FrameView::synthetic(&buf, ImgType::Nv12, 4, 4, 6, 26);
        let (_, uv) = v.nv12_planes().unwrap();
        assert_eq!(uv.len(), 10);
        let mut packed = Vec::new();
        assert_eq!(v.copy_packed(&mut packed), Some(24));
    }

    #[test]
    fn nv12_rejects_odd_dimensions() {
        // 5x4 and 4x3 with stride == width: packed_len and copy_packed must
        // agree, and both refuse.
        let buf = vec![0u8; 64];
        for (w, h) in [(5u32, 4u32), (4, 3), (5, 3)] {
            let v = FrameView::synthetic(&buf, ImgType::Nv12, w, h, w, 0);
            assert_eq!(v.nv12_planes(), None, "{w}x{h}");
            assert_eq!(v.packed_len(), None, "{w}x{h}");
            assert_eq!(v.copy_packed(&mut Vec::new()), None, "{w}x{h}");
        }
        // Odd GRAY8 is fine.
        let v = FrameView::synthetic(&buf, ImgType::Gray8, 5, 3, 5, 0);
        assert_eq!(v.packed_len(), Some(15));
        assert_eq!(v.copy_packed(&mut Vec::new()), Some(15));
    }

    #[test]
    fn nv12_rejects_bad_layouts() {
        let buf = nv12_padded();
        // Too short for the UV plane.
        let short = &buf[..30];
        assert!(
            FrameView::synthetic(short, ImgType::Nv12, 4, 4, 6, 26)
                .nv12_planes()
                .is_none()
        );
        // UV plane overlapping Y.
        assert!(
            FrameView::synthetic(&buf, ImgType::Nv12, 4, 4, 6, 10)
                .nv12_planes()
                .is_none()
        );
        // Stride narrower than width.
        assert!(
            FrameView::synthetic(&buf, ImgType::Nv12, 8, 4, 6, 26)
                .nv12_planes()
                .is_none()
        );
        // Wrong type.
        assert!(
            FrameView::synthetic(&buf, ImgType::Gray8, 4, 4, 6, 26)
                .nv12_planes()
                .is_none()
        );
    }

    #[test]
    fn gray8_respects_stride() {
        let (w, h, s) = (3usize, 2usize, 5usize);
        let mut buf = vec![0xEE; s * h];
        for r in 0..h {
            for c in 0..w {
                buf[r * s + c] = (r * 10 + c) as u8;
            }
        }
        let v = FrameView::synthetic(&buf, ImgType::Gray8, 3, 2, 5, 0);
        assert_eq!(v.gray8().unwrap().len(), 10);
        let mut packed = Vec::new();
        assert_eq!(v.copy_packed(&mut packed), Some(6));
        assert_eq!(packed, [0, 1, 2, 10, 11, 12]);
        assert_eq!(v.packed_len(), Some(6));
        assert!(v.nv12_planes().is_none());

        // Short last row is fine; one byte less is not.
        assert!(
            FrameView::synthetic(&buf[..8], ImgType::Gray8, 3, 2, 5, 0)
                .gray8()
                .is_some()
        );
        assert!(
            FrameView::synthetic(&buf[..7], ImgType::Gray8, 3, 2, 5, 0)
                .gray8()
                .is_none()
        );
    }

    #[test]
    fn other_formats_have_no_packed_copy() {
        let buf = [0u8; 12];
        let v = FrameView::synthetic(&buf, ImgType::Rgb888i, 2, 2, 6, 0);
        assert_eq!(v.packed_len(), None);
        assert_eq!(v.copy_packed(&mut Vec::new()), None);
    }
}
