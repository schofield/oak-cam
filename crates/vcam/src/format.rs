//! [`CameraFormat`]: the byte geometry and platform four-cc codes every
//! backend shares (ported from stream-concern's `vcam/mod.rs`).

/// A pixel format a virtual camera can publish, shared by every platform
/// backend. [`PREFERRED`](Self::PREFERRED) is the negotiation order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraFormat {
    /// 8-bit 4:2:0, Y plane plus interleaved UV plane (video range).
    Nv12,
    /// 8-bit packed 4:2:2, `Y0 U Y1 V`.
    Yuy2,
    /// 8-bit packed 4:2:2, `U Y0 V Y1`.
    Uyvy,
    /// 8-bit packed BGRA.
    Bgra,
}

impl CameraFormat {
    /// Negotiation order: NV12 first, then YUY2/UYVY, then BGRA. oak-cam only
    /// produces NV12 today (RGB is NV12 from the device, mono GRAY8 is widened
    /// to NV12), so backends publish NV12; the others are what the macOS
    /// extension and the Windows source advertise to consumers.
    pub const PREFERRED: [CameraFormat; 4] = [
        CameraFormat::Nv12,
        CameraFormat::Yuy2,
        CameraFormat::Uyvy,
        CameraFormat::Bgra,
    ];

    /// The CoreVideo pixel-format code (`kCVPixelFormatType_*`) for this format:
    /// `420v`, `yuvs`, `2vuy`, `BGRA`.
    #[must_use]
    pub const fn cv_pixel_format(self) -> u32 {
        match self {
            CameraFormat::Nv12 => fourcc(*b"420v"),
            CameraFormat::Yuy2 => fourcc(*b"yuvs"),
            CameraFormat::Uyvy => fourcc(*b"2vuy"),
            CameraFormat::Bgra => fourcc(*b"BGRA"),
        }
    }

    /// The V4L2 pixel-format code (`V4L2_PIX_FMT_*`, little-endian four-cc) for
    /// this format: `NV12`, `YUYV`, `UYVY`, `BGRA`.
    #[must_use]
    pub const fn v4l2_pixel_format(self) -> u32 {
        match self {
            CameraFormat::Nv12 => fourcc_le(*b"NV12"),
            CameraFormat::Yuy2 => fourcc_le(*b"YUYV"),
            CameraFormat::Uyvy => fourcc_le(*b"UYVY"),
            CameraFormat::Bgra => fourcc_le(*b"BGRA"),
        }
    }

    /// Number of planes a frame in this format has.
    #[must_use]
    pub const fn plane_count(self) -> usize {
        match self {
            CameraFormat::Nv12 => 2,
            CameraFormat::Yuy2 | CameraFormat::Uyvy | CameraFormat::Bgra => 1,
        }
    }

    /// Tightly packed bytes per row of `plane` at `width`. NV12's UV plane is
    /// `width` bytes (half the samples, two bytes each); 4:2:2 is two bytes per
    /// pixel; BGRA four.
    #[must_use]
    pub const fn plane_stride(self, width: u32, plane: usize) -> usize {
        let w = width as usize;
        match self {
            CameraFormat::Nv12 => {
                if plane == 0 {
                    w
                } else {
                    w.div_ceil(2) * 2
                }
            }
            CameraFormat::Yuy2 | CameraFormat::Uyvy => w.div_ceil(2) * 4,
            CameraFormat::Bgra => w * 4,
        }
    }

    /// Rows in `plane` at `height`: NV12's UV plane has half the rows.
    #[must_use]
    pub const fn plane_rows(self, height: u32, plane: usize) -> usize {
        let h = height as usize;
        match self {
            CameraFormat::Nv12 if plane == 1 => h.div_ceil(2),
            _ => h,
        }
    }

    /// Total tightly packed bytes of a `width` x `height` frame.
    #[must_use]
    pub const fn frame_size(self, width: u32, height: u32) -> usize {
        let mut total = 0;
        let mut plane = 0;
        while plane < self.plane_count() {
            total += self.plane_stride(width, plane) * self.plane_rows(height, plane);
            plane += 1;
        }
        total
    }
}

/// Big-endian four-character code, the Apple (`OSType`) convention.
const fn fourcc(code: [u8; 4]) -> u32 {
    u32::from_be_bytes(code)
}

/// Little-endian four-character code, the V4L2 convention.
const fn fourcc_le(code: [u8; 4]) -> u32 {
    u32::from_le_bytes(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Negotiation order: NV12, then YUY2, then UYVY, then BGRA.
    #[test]
    fn format_negotiation_order_nv12_first() {
        assert_eq!(
            CameraFormat::PREFERRED,
            [
                CameraFormat::Nv12,
                CameraFormat::Yuy2,
                CameraFormat::Uyvy,
                CameraFormat::Bgra
            ]
        );
    }

    /// NV12 stride/size maths: at width W, the Y plane is W bytes per row x H
    /// rows and the UV plane W bytes per row x H/2 rows (interleaved).
    #[test]
    fn nv12_stride_and_size() {
        let f = CameraFormat::Nv12;
        assert_eq!(f.plane_count(), 2);
        assert_eq!(f.plane_stride(1920, 0), 1920);
        assert_eq!(f.plane_stride(1920, 1), 1920);
        assert_eq!(f.plane_rows(1080, 0), 1080);
        assert_eq!(f.plane_rows(1080, 1), 540);
        assert_eq!(f.frame_size(1920, 1080), 1920 * 1080 + 1920 * 540);
        assert_eq!(f.frame_size(1920, 1080), 3_110_400);
        // The mono default and 4K.
        assert_eq!(f.frame_size(1280, 800), 1_536_000);
        assert_eq!(f.frame_size(3840, 2160), 12_441_600);
        // Odd geometry rounds the chroma plane up.
        assert_eq!(f.plane_stride(641, 1), 642);
        assert_eq!(f.plane_rows(481, 1), 241);
    }

    /// Packed 4:2:2 (YUY2 and UYVY): two bytes per pixel, one plane.
    #[test]
    fn packed_422_stride() {
        for f in [CameraFormat::Yuy2, CameraFormat::Uyvy] {
            assert_eq!(f.plane_count(), 1);
            assert_eq!(f.plane_stride(1920, 0), 3840);
            assert_eq!(f.frame_size(1920, 1080), 4_147_200);
            // An odd width pads to a whole macropixel.
            assert_eq!(f.plane_stride(641, 0), 1284);
        }
    }

    /// BGRA: four bytes per pixel, one plane.
    #[test]
    fn bgra_stride() {
        let f = CameraFormat::Bgra;
        assert_eq!(f.plane_count(), 1);
        assert_eq!(f.plane_stride(1920, 0), 7680);
        assert_eq!(f.frame_size(1280, 720), 3_686_400);
    }

    /// The platform four-cc codes match the documented constants: Apple codes
    /// are big-endian `OSType`s, V4L2 codes little-endian.
    #[test]
    fn fourcc_codes() {
        assert_eq!(CameraFormat::Nv12.cv_pixel_format(), 0x3432_3076); // '420v'
        assert_eq!(CameraFormat::Yuy2.cv_pixel_format(), 0x7975_7673); // 'yuvs'
        assert_eq!(CameraFormat::Uyvy.cv_pixel_format(), 0x3276_7579); // '2vuy'
        assert_eq!(CameraFormat::Bgra.cv_pixel_format(), 0x4247_5241); // 'BGRA'
        assert_eq!(CameraFormat::Nv12.v4l2_pixel_format(), 0x3231_564e); // NV12
        assert_eq!(CameraFormat::Yuy2.v4l2_pixel_format(), 0x5659_5559); // YUYV
        assert_eq!(CameraFormat::Uyvy.v4l2_pixel_format(), 0x5956_5955); // UYVY
        assert_eq!(CameraFormat::Bgra.v4l2_pixel_format(), 0x4152_4742); // BGRA
    }
}
