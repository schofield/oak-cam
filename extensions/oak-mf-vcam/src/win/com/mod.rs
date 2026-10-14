//! The COM server: [`factory::ClassFactory`] hands out
//! [`activate::SourceActivate`] objects (the `IMFActivate` the Frame Server
//! asks the CLSID for), whose `ActivateObject` builds a
//! [`source::MediaSource`] with one [`stream::MediaStream`].

// The `*_Impl` trait methods receive raw pointers from COM callers and cannot
// be `unsafe fn` (the `windows` crate fixes their signatures); the COM
// contract makes the caller responsible for their validity.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod activate;
pub mod factory;
pub mod source;
pub mod stream;

use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE,
    MF_MT_FIXED_SIZE_SAMPLES, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SAMPLE_SIZE, MF_MT_SUBTYPE,
    MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES, MF_MT_YUV_MATRIX, MFCreateMediaType,
    MFMediaType_Video, MFNominalRange_0_255, MFNominalRange_16_235, MFVideoFormat_NV12,
    MFVideoFormat_RGB32, MFVideoFormat_YUY2, MFVideoInterlace_Progressive, MFVideoPrimaries_BT709,
    MFVideoTransferMatrix_BT709,
};
use windows_core::GUID;

use crate::formats::{MediaFormat, OutputFormat, find_format};

/// The Media Foundation subtype GUID of `format`.
#[must_use]
pub fn subtype(format: OutputFormat) -> GUID {
    match format {
        OutputFormat::Nv12 => MFVideoFormat_NV12,
        OutputFormat::Yuy2 => MFVideoFormat_YUY2,
        OutputFormat::Rgb32 => MFVideoFormat_RGB32,
    }
}

const fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

/// Build the `IMFMediaType` advertising `m`.
///
/// # Errors
/// Any failing Media Foundation call.
pub fn media_type(m: &MediaFormat) -> windows_core::Result<IMFMediaType> {
    // SAFETY: plain Media Foundation attribute calls on a type we own; every
    // GUID pointer is to a constant.
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &subtype(m.format))?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(m.width, m.height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(m.fps_num, m.fps_den))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        t.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
        t.SetUINT32(&MF_MT_SAMPLE_SIZE, m.sample_size() as u32)?;
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, m.default_stride() as u32)?;
        t.SetUINT32(
            &MF_MT_AVG_BITRATE,
            m.avg_bitrate().min(u64::from(u32::MAX)) as u32,
        )?;
        t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        match m.format {
            OutputFormat::Nv12 | OutputFormat::Yuy2 => {
                t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
                t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
            }
            OutputFormat::Rgb32 => {
                t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_0_255.0 as u32)?;
            }
        }
        Ok(t)
    }
}

/// Recognise a consumer's media type as one of the advertised formats.
#[must_use]
pub fn parse_media_type(t: &IMFMediaType) -> Option<MediaFormat> {
    // SAFETY: attribute reads on a live media type.
    let (sub, size) = unsafe {
        (
            t.GetGUID(&MF_MT_SUBTYPE).ok()?,
            t.GetUINT64(&MF_MT_FRAME_SIZE).ok()?,
        )
    };
    let format = [OutputFormat::Nv12, OutputFormat::Yuy2, OutputFormat::Rgb32]
        .into_iter()
        .find(|f| subtype(*f) == sub)?;
    find_format(format, (size >> 32) as u32, size as u32)
}
