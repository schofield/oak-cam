//! The media types the virtual camera advertises, in preference order: NV12
//! 1080p30 and 720p30 first (the ring's native format), then YUY2 and BGRA
//! (Media Foundation's `RGB32`) at the same sizes for consumers that insist on
//! a packed format. The source converts and scales the ring's NV12 frame into
//! whichever one the consumer selects.

/// A pixel format the source can emit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    /// 4:2:0, Y plane then interleaved UV plane (BT.709 video range).
    Nv12,
    /// Packed 4:2:2, `Y0 U Y1 V` (BT.709 video range).
    Yuy2,
    /// Packed 8-bit `B G R X` (Media Foundation `RGB32`), full range.
    Rgb32,
}

impl OutputFormat {
    /// Bytes per row at `width` for plane 0 (NV12's UV plane has the same
    /// row length).
    #[must_use]
    pub const fn row_bytes(self, width: u32) -> usize {
        let w = width as usize;
        match self {
            OutputFormat::Nv12 => w.div_ceil(2) * 2,
            OutputFormat::Yuy2 => w.div_ceil(2) * 4,
            OutputFormat::Rgb32 => w * 4,
        }
    }

    /// Tightly packed bytes of a `width` x `height` frame.
    #[must_use]
    pub const fn frame_bytes(self, width: u32, height: u32) -> usize {
        let h = height as usize;
        match self {
            OutputFormat::Nv12 => crate::layout::nv12_size(width, height),
            OutputFormat::Yuy2 | OutputFormat::Rgb32 => self.row_bytes(width) * h,
        }
    }

    /// The Media Foundation subtype GUID's first 32 bits (the four-cc, or
    /// `D3DFMT_X8R8G8B8` = 22 for `RGB32`); the remaining bits are the common
    /// `-0000-0010-8000-00AA00389B71` suffix.
    #[must_use]
    pub const fn mf_subtype_data1(self) -> u32 {
        match self {
            OutputFormat::Nv12 => u32::from_le_bytes(*b"NV12"),
            OutputFormat::Yuy2 => u32::from_le_bytes(*b"YUY2"),
            OutputFormat::Rgb32 => 22,
        }
    }

    /// Recognise a subtype by its first 32 bits.
    #[must_use]
    pub fn from_mf_subtype_data1(data1: u32) -> Option<Self> {
        [OutputFormat::Nv12, OutputFormat::Yuy2, OutputFormat::Rgb32]
            .into_iter()
            .find(|f| f.mf_subtype_data1() == data1)
    }
}

/// One advertised media type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaFormat {
    /// Pixel format.
    pub format: OutputFormat,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Frame-rate numerator.
    pub fps_num: u32,
    /// Frame-rate denominator.
    pub fps_den: u32,
}

impl MediaFormat {
    const fn new(format: OutputFormat, width: u32, height: u32) -> Self {
        Self {
            format,
            width,
            height,
            fps_num: 30,
            fps_den: 1,
        }
    }

    /// Frame duration in 100 ns units.
    #[must_use]
    pub const fn frame_duration_100ns(&self) -> u64 {
        10_000_000 * self.fps_den as u64 / self.fps_num as u64
    }

    /// Tightly packed frame size in bytes (`MF_MT_SAMPLE_SIZE`).
    #[must_use]
    pub const fn sample_size(&self) -> usize {
        self.format.frame_bytes(self.width, self.height)
    }

    /// Positive default stride (`MF_MT_DEFAULT_STRIDE`): every format here is
    /// top-down.
    #[must_use]
    pub const fn default_stride(&self) -> usize {
        self.format.row_bytes(self.width)
    }

    /// Average bit rate (`MF_MT_AVG_BITRATE`) of the uncompressed stream.
    #[must_use]
    pub const fn avg_bitrate(&self) -> u64 {
        self.sample_size() as u64 * 8 * self.fps_num as u64 / self.fps_den as u64
    }
}

/// The advertised media types, in preference order. Index 0 is the default
/// when a consumer starts the stream without choosing one.
pub const MEDIA_FORMATS: [MediaFormat; 6] = [
    MediaFormat::new(OutputFormat::Nv12, 1920, 1080),
    MediaFormat::new(OutputFormat::Nv12, 1280, 720),
    MediaFormat::new(OutputFormat::Yuy2, 1920, 1080),
    MediaFormat::new(OutputFormat::Yuy2, 1280, 720),
    MediaFormat::new(OutputFormat::Rgb32, 1920, 1080),
    MediaFormat::new(OutputFormat::Rgb32, 1280, 720),
];

/// Find the advertised type matching a consumer's choice.
#[must_use]
pub fn find_format(format: OutputFormat, width: u32, height: u32) -> Option<MediaFormat> {
    MEDIA_FORMATS
        .iter()
        .copied()
        .find(|m| m.format == format && m.width == width && m.height == height)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NV12 first (1080p then 720p), then YUY2, then RGB32, all at 30 fps.
    #[test]
    fn vcam_format_table_order() {
        let order: Vec<(OutputFormat, u32, u32)> = MEDIA_FORMATS
            .iter()
            .map(|m| (m.format, m.width, m.height))
            .collect();
        assert_eq!(
            order,
            [
                (OutputFormat::Nv12, 1920, 1080),
                (OutputFormat::Nv12, 1280, 720),
                (OutputFormat::Yuy2, 1920, 1080),
                (OutputFormat::Yuy2, 1280, 720),
                (OutputFormat::Rgb32, 1920, 1080),
                (OutputFormat::Rgb32, 1280, 720),
            ]
        );
        for m in MEDIA_FORMATS {
            assert_eq!((m.fps_num, m.fps_den), (30, 1));
            assert_eq!(m.frame_duration_100ns(), 333_333);
        }
    }

    /// Sizes, strides and subtype codes match Media Foundation's definitions.
    #[test]
    fn vcam_format_geometry_and_subtypes() {
        let nv12 = MEDIA_FORMATS[0];
        assert_eq!(nv12.sample_size(), 3_110_400);
        assert_eq!(nv12.default_stride(), 1920);
        let yuy2 = MEDIA_FORMATS[2];
        assert_eq!(yuy2.sample_size(), 4_147_200);
        assert_eq!(yuy2.default_stride(), 3840);
        let rgb = MEDIA_FORMATS[5];
        assert_eq!(rgb.sample_size(), 1280 * 720 * 4);
        assert_eq!(rgb.avg_bitrate(), 1280 * 720 * 4 * 8 * 30);
        // MFVideoFormat_NV12 = {3231564E-...}, YUY2 = {32595559-...}, RGB32 = {00000016-...}.
        assert_eq!(OutputFormat::Nv12.mf_subtype_data1(), 0x3231_564E);
        assert_eq!(OutputFormat::Yuy2.mf_subtype_data1(), 0x3259_5559);
        assert_eq!(OutputFormat::Rgb32.mf_subtype_data1(), 0x16);
        assert_eq!(
            OutputFormat::from_mf_subtype_data1(0x3259_5559),
            Some(OutputFormat::Yuy2)
        );
        assert_eq!(OutputFormat::from_mf_subtype_data1(0), None);
        assert_eq!(
            find_format(OutputFormat::Rgb32, 1920, 1080),
            Some(MEDIA_FORMATS[4])
        );
        assert_eq!(find_format(OutputFormat::Nv12, 640, 480), None);
    }
}
