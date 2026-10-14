//! Camera controls: [`ControlParams`] (a builder over `oak_control_params`)
//! and [`ControlQueue`] (a Camera node's `inputControl`).
//!
//! Only fields whose `set` bit is on are applied by the shim, so a
//! `ControlParams` is a *delta*: `ControlParams::new().brightness(2)` changes
//! brightness and nothing else.

use std::ptr::NonNull;
use std::sync::Arc;

use depthai_sys as sys;

use crate::pipeline::PipelineInner;
use crate::{Result, check};

/// Mains-flicker compensation (`dai::CameraControl::AntiBandingMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Antibanding {
    /// No compensation.
    Off,
    /// 50 Hz mains.
    Mains50Hz,
    /// 60 Hz mains.
    Mains60Hz,
    /// Detect automatically.
    Auto,
}

impl Antibanding {
    /// The `OAK_ANTIBANDING_*` constant.
    pub fn to_raw(self) -> i32 {
        match self {
            Self::Off => sys::OAK_ANTIBANDING_OFF,
            Self::Mains50Hz => sys::OAK_ANTIBANDING_50HZ,
            Self::Mains60Hz => sys::OAK_ANTIBANDING_60HZ,
            Self::Auto => sys::OAK_ANTIBANDING_AUTO,
        }
    }
}

/// Autofocus mode (`dai::CameraControl::AutoFocusMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AfMode {
    /// Autofocus off (manual lens position).
    Off,
    /// Single-shot AF on trigger.
    Auto,
    /// Single-shot AF, close range.
    Macro,
    /// Continuous AF tuned for video.
    ContinuousVideo,
    /// Continuous AF tuned for stills.
    ContinuousPicture,
    /// Extended depth of field.
    Edof,
}

impl AfMode {
    /// The `OAK_AF_*` constant.
    pub fn to_raw(self) -> i32 {
        match self {
            Self::Off => sys::OAK_AF_OFF,
            Self::Auto => sys::OAK_AF_AUTO,
            Self::Macro => sys::OAK_AF_MACRO,
            Self::ContinuousVideo => sys::OAK_AF_CONTINUOUS_VIDEO,
            Self::ContinuousPicture => sys::OAK_AF_CONTINUOUS_PICTURE,
            Self::Edof => sys::OAK_AF_EDOF,
        }
    }
}

/// Auto white balance mode (`dai::CameraControl::AutoWhiteBalanceMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AwbMode {
    /// AWB off (use [`ControlParams::manual_wb`]).
    Off,
    /// Automatic.
    Auto,
    /// Incandescent preset.
    Incandescent,
    /// Fluorescent preset.
    Fluorescent,
    /// Warm fluorescent preset.
    WarmFluorescent,
    /// Daylight preset.
    Daylight,
    /// Cloudy daylight preset.
    CloudyDaylight,
    /// Twilight preset.
    Twilight,
    /// Shade preset.
    Shade,
}

impl AwbMode {
    /// The `OAK_AWB_*` constant.
    pub fn to_raw(self) -> i32 {
        match self {
            Self::Off => sys::OAK_AWB_OFF,
            Self::Auto => sys::OAK_AWB_AUTO,
            Self::Incandescent => sys::OAK_AWB_INCANDESCENT,
            Self::Fluorescent => sys::OAK_AWB_FLUORESCENT,
            Self::WarmFluorescent => sys::OAK_AWB_WARM_FLUORESCENT,
            Self::Daylight => sys::OAK_AWB_DAYLIGHT,
            Self::CloudyDaylight => sys::OAK_AWB_CLOUDY_DAYLIGHT,
            Self::Twilight => sys::OAK_AWB_TWILIGHT,
            Self::Shade => sys::OAK_AWB_SHADE,
        }
    }
}

/// A set of camera-control changes. Each builder method sets one field and
/// its `set` bit; unset fields are left alone on the device.
///
/// Ranges are validated by the device / shim (e.g. lens position 0..255 is
/// checked in the shim; brightness -10..10 is clamped by the firmware).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[must_use]
pub struct ControlParams {
    raw: sys::oak_control_params,
}

impl ControlParams {
    /// No changes.
    pub fn new() -> Self {
        Self::default()
    }

    /// True if no field is set.
    pub fn is_empty(&self) -> bool {
        self.raw.set == 0
    }

    /// The raw `set` bitmask (`OAK_CTL_*`).
    pub fn set_bits(&self) -> u64 {
        self.raw.set
    }

    /// The C struct passed to the shim.
    pub fn as_raw(&self) -> &sys::oak_control_params {
        &self.raw
    }

    fn with(mut self, bit: u64, f: impl FnOnce(&mut sys::oak_control_params)) -> Self {
        self.raw.set |= bit;
        f(&mut self.raw);
        self
    }

    /// Manual exposure: `exposure_us` microseconds at `iso` (100..1600).
    pub fn manual_exposure(self, exposure_us: u32, iso: u32) -> Self {
        self.with(sys::OAK_CTL_MANUAL_EXPOSURE, |r| {
            r.exposure_us = exposure_us;
            r.iso = iso;
        })
    }

    /// Enable auto exposure.
    pub fn auto_exposure(self) -> Self {
        self.with(sys::OAK_CTL_AE_AUTO, |_| {})
    }

    /// Auto-exposure compensation, -9..9.
    pub fn ae_compensation(self, comp: i8) -> Self {
        self.with(sys::OAK_CTL_AE_COMPENSATION, |r| {
            r.ae_compensation = i32::from(comp);
        })
    }

    /// Lock / unlock auto exposure.
    pub fn ae_lock(self, lock: bool) -> Self {
        self.with(sys::OAK_CTL_AE_LOCK, |r| r.ae_lock = i32::from(lock))
    }

    /// Upper limit for auto exposure time, in microseconds.
    pub fn ae_limit(self, max_exposure_us: u32) -> Self {
        self.with(sys::OAK_CTL_AE_LIMIT, |r| r.ae_limit_us = max_exposure_us)
    }

    /// Upper limit for auto-exposure ISO.
    pub fn ae_max_iso(self, max_iso: u32) -> Self {
        self.with(sys::OAK_CTL_AE_MAX_ISO, |r| r.ae_max_iso = max_iso)
    }

    /// Anti-banding (mains flicker) mode.
    pub fn antibanding(self, mode: Antibanding) -> Self {
        self.with(sys::OAK_CTL_ANTIBANDING, |r| r.antibanding = mode.to_raw())
    }

    /// Autofocus mode.
    pub fn af_mode(self, mode: AfMode) -> Self {
        self.with(sys::OAK_CTL_AF_MODE, |r| r.af_mode = mode.to_raw())
    }

    /// Manual lens position, 0 (infinity) ..255 (macro).
    pub fn manual_focus(self, lens_pos: u8) -> Self {
        self.with(sys::OAK_CTL_MANUAL_FOCUS, |r| {
            r.lens_pos = i32::from(lens_pos);
        })
    }

    /// One-shot autofocus trigger (a command, not state).
    pub fn af_trigger(self) -> Self {
        self.with(sys::OAK_CTL_AF_TRIGGER, |_| {})
    }

    /// Autofocus lens search range, `infinity`..`macro_` lens positions.
    pub fn af_lens_range(self, infinity: u8, macro_: u8) -> Self {
        self.with(sys::OAK_CTL_AF_LENS_RANGE, |r| {
            r.af_infinity = i32::from(infinity);
            r.af_macro = i32::from(macro_);
        })
    }

    /// Auto white balance mode.
    pub fn awb_mode(self, mode: AwbMode) -> Self {
        self.with(sys::OAK_CTL_AWB_MODE, |r| r.awb_mode = mode.to_raw())
    }

    /// Manual white balance, 1000..12000 K.
    pub fn manual_wb(self, kelvin: u32) -> Self {
        self.with(sys::OAK_CTL_MANUAL_WB, |r| {
            r.wb_kelvin = i32::try_from(kelvin).unwrap_or(i32::MAX);
        })
    }

    /// Lock / unlock auto white balance.
    pub fn awb_lock(self, lock: bool) -> Self {
        self.with(sys::OAK_CTL_AWB_LOCK, |r| r.awb_lock = i32::from(lock))
    }

    /// Brightness, -10..10.
    pub fn brightness(self, v: i8) -> Self {
        self.with(sys::OAK_CTL_BRIGHTNESS, |r| r.brightness = i32::from(v))
    }

    /// Contrast, -10..10.
    pub fn contrast(self, v: i8) -> Self {
        self.with(sys::OAK_CTL_CONTRAST, |r| r.contrast = i32::from(v))
    }

    /// Saturation, -10..10.
    pub fn saturation(self, v: i8) -> Self {
        self.with(sys::OAK_CTL_SATURATION, |r| r.saturation = i32::from(v))
    }

    /// Sharpness, 0..4.
    pub fn sharpness(self, v: u8) -> Self {
        self.with(sys::OAK_CTL_SHARPNESS, |r| r.sharpness = i32::from(v))
    }

    /// Luma denoise, 0..4.
    pub fn luma_denoise(self, v: u8) -> Self {
        self.with(sys::OAK_CTL_LUMA_DENOISE, |r| r.luma_denoise = i32::from(v))
    }

    /// Chroma denoise, 0..4.
    pub fn chroma_denoise(self, v: u8) -> Self {
        self.with(sys::OAK_CTL_CHROMA_DENOISE, |r| {
            r.chroma_denoise = i32::from(v);
        })
    }
}

/// A Camera node's control input. Controls sent here apply live.
///
/// `Send + Sync`: depthai serialises sends on its input queue. Holds the
/// pipeline alive through an `Arc` (and the C++ pipeline holds the device
/// connection), so it may outlive the `RunningPipeline` and the `Device`
/// handle; once the pipeline has stopped, [`Self::send`] returns an error
/// (`"MessageQueue was closed"`).
pub struct ControlQueue {
    raw: NonNull<sys::oak_control_queue>,
    _pipeline: Arc<PipelineInner>,
}

// SAFETY: oak_control_send is serialised by depthai (oak_dai.h, "Threading");
// the handle is freed once, in Drop.
unsafe impl Send for ControlQueue {}
// SAFETY: as above; `send` is the only operation.
unsafe impl Sync for ControlQueue {}

impl ControlQueue {
    pub(crate) fn new(raw: NonNull<sys::oak_control_queue>, pipeline: Arc<PipelineInner>) -> Self {
        Self {
            raw,
            _pipeline: pipeline,
        }
    }

    /// Sends a control. An empty [`ControlParams`] is a no-op (not sent).
    pub fn send(&self, params: &ControlParams) -> Result<()> {
        if params.is_empty() {
            return Ok(());
        }
        // SAFETY: handle is live; params points at a valid struct.
        check(unsafe { sys::oak_control_send(self.raw.as_ptr(), params.as_raw()) })
    }
}

impl Drop for ControlQueue {
    fn drop(&mut self) {
        // SAFETY: we own the handle; freed exactly once, before the pipeline
        // (we hold an Arc to it).
        unsafe { sys::oak_control_queue_free(self.raw.as_ptr()) };
    }
}

impl std::fmt::Debug for ControlQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlQueue")
            .field("raw", &self.raw)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_by_default() {
        let p = ControlParams::new();
        assert!(p.is_empty());
        assert_eq!(*p.as_raw(), sys::oak_control_params::default());
    }

    #[test]
    fn exposure_bits_and_fields() {
        let p = ControlParams::new().manual_exposure(8000, 400);
        assert_eq!(p.set_bits(), sys::OAK_CTL_MANUAL_EXPOSURE);
        assert_eq!(p.as_raw().exposure_us, 8000);
        assert_eq!(p.as_raw().iso, 400);

        let p = ControlParams::new()
            .auto_exposure()
            .ae_compensation(-3)
            .ae_lock(true)
            .ae_limit(20_000)
            .ae_max_iso(800)
            .antibanding(Antibanding::Mains60Hz);
        assert_eq!(
            p.set_bits(),
            sys::OAK_CTL_AE_AUTO
                | sys::OAK_CTL_AE_COMPENSATION
                | sys::OAK_CTL_AE_LOCK
                | sys::OAK_CTL_AE_LIMIT
                | sys::OAK_CTL_AE_MAX_ISO
                | sys::OAK_CTL_ANTIBANDING
        );
        let r = p.as_raw();
        assert_eq!(r.ae_compensation, -3);
        assert_eq!(r.ae_lock, 1);
        assert_eq!(r.ae_limit_us, 20_000);
        assert_eq!(r.ae_max_iso, 800);
        assert_eq!(r.antibanding, sys::OAK_ANTIBANDING_60HZ);
    }

    #[test]
    fn focus_and_wb_bits_and_fields() {
        let p = ControlParams::new()
            .af_mode(AfMode::ContinuousVideo)
            .manual_focus(130)
            .af_trigger()
            .af_lens_range(10, 200)
            .awb_mode(AwbMode::Daylight)
            .manual_wb(5600)
            .awb_lock(false);
        assert_eq!(
            p.set_bits(),
            sys::OAK_CTL_AF_MODE
                | sys::OAK_CTL_MANUAL_FOCUS
                | sys::OAK_CTL_AF_TRIGGER
                | sys::OAK_CTL_AF_LENS_RANGE
                | sys::OAK_CTL_AWB_MODE
                | sys::OAK_CTL_MANUAL_WB
                | sys::OAK_CTL_AWB_LOCK
        );
        let r = p.as_raw();
        assert_eq!(r.af_mode, sys::OAK_AF_CONTINUOUS_VIDEO);
        assert_eq!(r.lens_pos, 130);
        assert_eq!((r.af_infinity, r.af_macro), (10, 200));
        assert_eq!(r.awb_mode, sys::OAK_AWB_DAYLIGHT);
        assert_eq!(r.wb_kelvin, 5600);
        assert_eq!(r.awb_lock, 0);
    }

    #[test]
    fn image_tuning_bits_and_fields() {
        let p = ControlParams::new()
            .brightness(-10)
            .contrast(10)
            .saturation(-2)
            .sharpness(4)
            .luma_denoise(1)
            .chroma_denoise(3);
        assert_eq!(
            p.set_bits(),
            sys::OAK_CTL_BRIGHTNESS
                | sys::OAK_CTL_CONTRAST
                | sys::OAK_CTL_SATURATION
                | sys::OAK_CTL_SHARPNESS
                | sys::OAK_CTL_LUMA_DENOISE
                | sys::OAK_CTL_CHROMA_DENOISE
        );
        let r = p.as_raw();
        assert_eq!(
            (
                r.brightness,
                r.contrast,
                r.saturation,
                r.sharpness,
                r.luma_denoise,
                r.chroma_denoise
            ),
            (-10, 10, -2, 4, 1, 3)
        );
        // Untouched fields stay zero.
        assert_eq!(r.exposure_us, 0);
        assert_eq!(r.lens_pos, 0);
    }

    #[test]
    fn every_bit_is_distinct_and_known() {
        let all = ControlParams::new()
            .auto_exposure()
            .manual_exposure(1, 100)
            .ae_compensation(0)
            .ae_lock(false)
            .ae_limit(1)
            .ae_max_iso(100)
            .antibanding(Antibanding::Off)
            .af_mode(AfMode::Off)
            .manual_focus(0)
            .af_trigger()
            .awb_mode(AwbMode::Off)
            .manual_wb(1000)
            .awb_lock(false)
            .brightness(0)
            .contrast(0)
            .saturation(0)
            .sharpness(0)
            .luma_denoise(0)
            .chroma_denoise(0)
            .af_lens_range(0, 0);
        // 20 builders, 20 distinct bits, all within the shim's known mask.
        assert_eq!(all.set_bits(), (1u64 << 20) - 1);
    }

    #[test]
    fn enum_raw_values() {
        assert_eq!(Antibanding::Auto.to_raw(), 3);
        assert_eq!(AfMode::Edof.to_raw(), 5);
        assert_eq!(AwbMode::Shade.to_raw(), 8);
    }

    const fn assert_send_sync<T: Send + Sync>() {}
    const _: () = assert_send_sync::<ControlQueue>();
}
