//! Config → camera controls: full parameter sets for `initialControl`, and
//! minimal deltas between two configs for the live control queues.
//!
//! Pure functions, no device access. A [`ControlParams`] only applies the
//! fields whose `set` bit is on, so a delta touches exactly what changed.
//!
//! Rules:
//! - Stream changes (size, fps, `enabled`) cannot apply live: they set
//!   [`Delta::restart`].
//! - Manual → Auto exposure emits `AE_AUTO` plus the whole Auto block:
//!   compensation, lock and always `AE_LIMIT` (the configured limit, else
//!   one frame time), because the firmware keeps the last limit it was sent
//!   across a manual period; Auto → Auto only the changed sub-fields; any
//!   change to Manual emits `MANUAL_EXPOSURE`.
//! - Removing an AE limit sends a limit of one frame time (depthai's
//!   default behaviour), since there is no "no limit" command.
//! - Manual focus / Kelvin white balance also switch AF / AWB off, since
//!   depthai's `setManualFocus` / `setManualWhiteBalance` do not.
//! - The AF trigger is a one-shot command ([`af_trigger`]), not state.

use depthai::ControlParams;

use crate::config::{
    Config, Exposure, Focus, MonoSettings, RgbSettings, StreamSettings, WhiteBalance,
};

/// What it takes to go from one sensor config to another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Delta {
    /// Controls to send live (`None`: nothing changed that a control can
    /// change).
    pub control: Option<ControlParams>,
    /// The stream changed: stop and rebuild the pipeline.
    pub restart: bool,
}

impl Delta {
    fn new(control: ControlParams, restart: bool) -> Self {
        Self {
            control: (!control.is_empty()).then_some(control),
            restart,
        }
    }

    /// Nothing to do.
    pub fn is_empty(&self) -> bool {
        self.control.is_none() && !self.restart
    }
}

/// Device-level changes between two configs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DeviceDelta {
    /// New IR dot projector intensity to apply live.
    pub ir_dot: Option<f32>,
    /// New IR flood intensity to apply live.
    pub ir_flood: Option<f32>,
    /// The device selection or USB speed cap changed: close and reopen the
    /// device (which restarts the pipeline).
    pub reopen: bool,
}

/// The one-shot autofocus trigger.
pub fn af_trigger() -> ControlParams {
    ControlParams::new().af_trigger()
}

/// AE limit used when a limit is removed: one frame time at `fps`.
fn frame_time_us(fps: f32) -> u32 {
    if fps > 0.0 {
        (1_000_000.0 / f64::from(fps)).round() as u32
    } else {
        33_333
    }
}

fn exposure_full(p: ControlParams, e: Exposure) -> ControlParams {
    match e {
        Exposure::Manual { us, iso } => p.manual_exposure(us, iso),
        Exposure::Auto {
            comp,
            limit_us,
            lock,
        } => {
            let p = p.auto_exposure().ae_compensation(comp).ae_lock(lock);
            match limit_us {
                Some(l) => p.ae_limit(l),
                None => p,
            }
        }
    }
}

fn exposure_diff(mut p: ControlParams, old: Exposure, new: Exposure, fps: f32) -> ControlParams {
    if old == new {
        return p;
    }
    match (old, new) {
        (
            Exposure::Auto {
                comp: c0,
                limit_us: l0,
                lock: k0,
            },
            Exposure::Auto {
                comp: c1,
                limit_us: l1,
                lock: k1,
            },
        ) => {
            if c0 != c1 {
                p = p.ae_compensation(c1);
            }
            if k0 != k1 {
                p = p.ae_lock(k1);
            }
            if l0 != l1 {
                p = p.ae_limit(l1.unwrap_or_else(|| frame_time_us(fps)));
            }
            p
        }
        (
            Exposure::Manual { .. },
            Exposure::Auto {
                comp,
                limit_us,
                lock,
            },
        ) => p
            .auto_exposure()
            .ae_compensation(comp)
            .ae_lock(lock)
            .ae_limit(limit_us.unwrap_or_else(|| frame_time_us(fps))),
        (_, new) => exposure_full(p, new),
    }
}

fn focus_full(p: ControlParams, f: Focus) -> ControlParams {
    match f {
        Focus::Auto(mode) => p.af_mode(mode.to_dai()),
        Focus::Manual(pos) => p.af_mode(depthai::AfMode::Off).manual_focus(pos),
    }
}

fn focus_diff(p: ControlParams, old: Focus, new: Focus) -> ControlParams {
    match (old, new) {
        _ if old == new => p,
        // Already manual: just move the lens.
        (Focus::Manual(_), Focus::Manual(pos)) => p.manual_focus(pos),
        (_, new) => focus_full(p, new),
    }
}

fn wb_full(p: ControlParams, wb: WhiteBalance) -> ControlParams {
    match wb {
        WhiteBalance::Auto { mode, lock } => p.awb_mode(mode.to_dai()).awb_lock(lock),
        WhiteBalance::Kelvin(k) => p.awb_mode(depthai::AwbMode::Off).manual_wb(k),
    }
}

fn wb_diff(mut p: ControlParams, old: WhiteBalance, new: WhiteBalance) -> ControlParams {
    match (old, new) {
        _ if old == new => p,
        (WhiteBalance::Auto { mode: m0, lock: l0 }, WhiteBalance::Auto { mode: m1, lock: l1 }) => {
            if m0 != m1 {
                p = p.awb_mode(m1.to_dai());
            }
            if l0 != l1 {
                p = p.awb_lock(l1);
            }
            p
        }
        (WhiteBalance::Kelvin(_), WhiteBalance::Kelvin(k)) => p.manual_wb(k),
        (_, new) => wb_full(p, new),
    }
}

/// Whether going from `old` to `new` needs a pipeline rebuild.
pub fn stream_restart(old: &StreamSettings, new: &StreamSettings) -> bool {
    old != new
}

/// Every RGB control, for `initialControl`.
pub fn rgb_to_control(s: &RgbSettings) -> ControlParams {
    let p = exposure_full(ControlParams::new(), s.exposure).antibanding(s.antibanding.to_dai());
    let p = wb_full(focus_full(p, s.focus), s.wb);
    p.brightness(s.brightness)
        .contrast(s.contrast)
        .saturation(s.saturation)
        .sharpness(s.sharpness)
        .luma_denoise(s.luma_denoise)
        .chroma_denoise(s.chroma_denoise)
}

/// Every mono control, for `initialControl`.
pub fn mono_to_control(s: &MonoSettings) -> ControlParams {
    exposure_full(ControlParams::new(), s.exposure)
        .antibanding(s.antibanding.to_dai())
        .brightness(s.brightness)
        .contrast(s.contrast)
        .sharpness(s.sharpness)
        .luma_denoise(s.luma_denoise)
}

/// The RGB changes from `old` to `new`.
pub fn diff_rgb(old: &RgbSettings, new: &RgbSettings) -> Delta {
    let mut p = exposure_diff(
        ControlParams::new(),
        old.exposure,
        new.exposure,
        new.stream.fps,
    );
    if old.antibanding != new.antibanding {
        p = p.antibanding(new.antibanding.to_dai());
    }
    p = focus_diff(p, old.focus, new.focus);
    p = wb_diff(p, old.wb, new.wb);
    if old.brightness != new.brightness {
        p = p.brightness(new.brightness);
    }
    if old.contrast != new.contrast {
        p = p.contrast(new.contrast);
    }
    if old.saturation != new.saturation {
        p = p.saturation(new.saturation);
    }
    if old.sharpness != new.sharpness {
        p = p.sharpness(new.sharpness);
    }
    if old.luma_denoise != new.luma_denoise {
        p = p.luma_denoise(new.luma_denoise);
    }
    if old.chroma_denoise != new.chroma_denoise {
        p = p.chroma_denoise(new.chroma_denoise);
    }
    Delta::new(p, stream_restart(&old.stream, &new.stream))
}

/// The changes for one mono camera from `old` to `new` (pass each
/// camera's [`crate::config::MonoGroup::effective`] settings).
pub fn diff_mono(old: &MonoSettings, new: &MonoSettings) -> Delta {
    let mut p = exposure_diff(
        ControlParams::new(),
        old.exposure,
        new.exposure,
        new.stream.fps,
    );
    if old.antibanding != new.antibanding {
        p = p.antibanding(new.antibanding.to_dai());
    }
    if old.brightness != new.brightness {
        p = p.brightness(new.brightness);
    }
    if old.contrast != new.contrast {
        p = p.contrast(new.contrast);
    }
    if old.sharpness != new.sharpness {
        p = p.sharpness(new.sharpness);
    }
    if old.luma_denoise != new.luma_denoise {
        p = p.luma_denoise(new.luma_denoise);
    }
    Delta::new(p, stream_restart(&old.stream, &new.stream))
}

/// Device-level changes from `old` to `new`.
pub fn device_delta(old: &Config, new: &Config) -> DeviceDelta {
    let (o, n) = (&old.device_settings, &new.device_settings);
    DeviceDelta {
        ir_dot: (o.ir_dot.to_bits() != n.ir_dot.to_bits()).then_some(n.ir_dot),
        ir_flood: (o.ir_flood.to_bits() != n.ir_flood.to_bits()).then_some(n.ir_flood),
        reopen: o.max_usb_speed != n.max_usb_speed || old.device != new.device,
    }
}

#[cfg(test)]
#[allow(clippy::type_complexity)]
mod tests {
    use depthai::{AfMode as DaiAf, Antibanding as DaiAb, AwbMode as DaiAwb};
    use depthai_sys as sys;

    use super::*;
    use crate::config::{
        AfMode, Antibanding, AwbMode, DeviceSelection, MonoCam, MonoGroup, UsbSpeedPref,
    };

    fn bits(d: Delta) -> u64 {
        d.control.map_or(0, |c| c.set_bits())
    }

    fn auto(comp: i8, limit_us: Option<u32>, lock: bool) -> Exposure {
        Exposure::Auto {
            comp,
            limit_us,
            lock,
        }
    }

    // --- full params -----------------------------------------------------

    #[test]
    fn rgb_full_params_default() {
        let p = rgb_to_control(&RgbSettings::default());
        assert_eq!(
            p.set_bits(),
            sys::OAK_CTL_AE_AUTO
                | sys::OAK_CTL_AE_COMPENSATION
                | sys::OAK_CTL_AE_LOCK
                | sys::OAK_CTL_ANTIBANDING
                | sys::OAK_CTL_AF_MODE
                | sys::OAK_CTL_AWB_MODE
                | sys::OAK_CTL_AWB_LOCK
                | sys::OAK_CTL_BRIGHTNESS
                | sys::OAK_CTL_CONTRAST
                | sys::OAK_CTL_SATURATION
                | sys::OAK_CTL_SHARPNESS
                | sys::OAK_CTL_LUMA_DENOISE
                | sys::OAK_CTL_CHROMA_DENOISE
        );
        let r = p.as_raw();
        assert_eq!(r.af_mode, DaiAf::ContinuousVideo.to_raw());
        assert_eq!(r.awb_mode, DaiAwb::Auto.to_raw());
        assert_eq!(r.antibanding, DaiAb::Auto.to_raw());
    }

    #[test]
    fn rgb_full_params_manual_everything() {
        let s = RgbSettings {
            exposure: Exposure::Manual { us: 2000, iso: 400 },
            focus: Focus::Manual(130),
            wb: WhiteBalance::Kelvin(5600),
            antibanding: Antibanding::Mains50Hz,
            brightness: -3,
            contrast: 2,
            saturation: 1,
            sharpness: 4,
            luma_denoise: 1,
            chroma_denoise: 2,
            ..RgbSettings::default()
        };
        let p = rgb_to_control(&s);
        assert_eq!(p.set_bits() & sys::OAK_CTL_AE_AUTO, 0);
        assert_ne!(p.set_bits() & sys::OAK_CTL_MANUAL_EXPOSURE, 0);
        assert_ne!(p.set_bits() & sys::OAK_CTL_MANUAL_FOCUS, 0);
        assert_ne!(p.set_bits() & sys::OAK_CTL_MANUAL_WB, 0);
        assert_eq!(p.set_bits() & sys::OAK_CTL_AWB_LOCK, 0);
        let r = p.as_raw();
        assert_eq!((r.exposure_us, r.iso), (2000, 400));
        assert_eq!((r.af_mode, r.lens_pos), (DaiAf::Off.to_raw(), 130));
        assert_eq!((r.awb_mode, r.wb_kelvin), (DaiAwb::Off.to_raw(), 5600));
        assert_eq!(r.antibanding, DaiAb::Mains50Hz.to_raw());
        assert_eq!(
            (
                r.brightness,
                r.contrast,
                r.saturation,
                r.sharpness,
                r.luma_denoise,
                r.chroma_denoise
            ),
            (-3, 2, 1, 4, 1, 2)
        );
    }

    #[test]
    fn auto_exposure_limit_only_when_set() {
        let mut s = RgbSettings::default();
        assert_eq!(rgb_to_control(&s).set_bits() & sys::OAK_CTL_AE_LIMIT, 0);
        s.exposure = auto(-2, Some(20_000), true);
        let p = rgb_to_control(&s);
        assert_ne!(p.set_bits() & sys::OAK_CTL_AE_LIMIT, 0);
        let r = p.as_raw();
        assert_eq!(
            (r.ae_compensation, r.ae_lock, r.ae_limit_us),
            (-2, 1, 20_000)
        );
    }

    #[test]
    fn mono_full_params_have_no_colour_or_focus_bits() {
        let p = mono_to_control(&MonoSettings::default());
        assert_eq!(
            p.set_bits(),
            sys::OAK_CTL_AE_AUTO
                | sys::OAK_CTL_AE_COMPENSATION
                | sys::OAK_CTL_AE_LOCK
                | sys::OAK_CTL_ANTIBANDING
                | sys::OAK_CTL_BRIGHTNESS
                | sys::OAK_CTL_CONTRAST
                | sys::OAK_CTL_SHARPNESS
                | sys::OAK_CTL_LUMA_DENOISE
        );
        let m = MonoSettings {
            exposure: Exposure::Manual { us: 8000, iso: 800 },
            ..MonoSettings::default()
        };
        let r = *mono_to_control(&m).as_raw();
        assert_eq!((r.exposure_us, r.iso), (8000, 800));
    }

    // --- diff_rgb --------------------------------------------------------

    #[test]
    fn identical_configs_diff_to_nothing() {
        let s = RgbSettings::default();
        assert!(diff_rgb(&s, &s).is_empty());
        let m = MonoSettings::default();
        assert!(diff_mono(&m, &m).is_empty());
        assert_eq!(diff_rgb(&s, &s), Delta::default());
    }

    #[test]
    fn single_scalar_changes_set_one_bit() {
        let base = RgbSettings::default();
        let cases: [(fn(&mut RgbSettings), u64); 6] = [
            (|s| s.brightness = 3, sys::OAK_CTL_BRIGHTNESS),
            (|s| s.contrast = -1, sys::OAK_CTL_CONTRAST),
            (|s| s.saturation = 7, sys::OAK_CTL_SATURATION),
            (|s| s.sharpness = 2, sys::OAK_CTL_SHARPNESS),
            (|s| s.luma_denoise = 3, sys::OAK_CTL_LUMA_DENOISE),
            (|s| s.chroma_denoise = 1, sys::OAK_CTL_CHROMA_DENOISE),
        ];
        for (edit, bit) in cases {
            let mut new = base;
            edit(&mut new);
            let d = diff_rgb(&base, &new);
            assert!(!d.restart);
            assert_eq!(bits(d), bit);
        }
        let mut new = base;
        new.brightness = 3;
        new.contrast = -1;
        let d = diff_rgb(&base, &new);
        assert_eq!(bits(d), sys::OAK_CTL_BRIGHTNESS | sys::OAK_CTL_CONTRAST);
        let r = d.control.unwrap();
        assert_eq!((r.as_raw().brightness, r.as_raw().contrast), (3, -1));
    }

    #[test]
    fn antibanding_change() {
        let base = RgbSettings::default();
        let new = RgbSettings {
            antibanding: Antibanding::Mains60Hz,
            ..base
        };
        let d = diff_rgb(&base, &new);
        assert_eq!(bits(d), sys::OAK_CTL_ANTIBANDING);
        assert_eq!(
            d.control.unwrap().as_raw().antibanding,
            DaiAb::Mains60Hz.to_raw()
        );
    }

    #[test]
    fn auto_to_manual_exposure() {
        let base = RgbSettings::default();
        let new = RgbSettings {
            exposure: Exposure::Manual { us: 2000, iso: 400 },
            ..base
        };
        let d = diff_rgb(&base, &new);
        assert_eq!(bits(d), sys::OAK_CTL_MANUAL_EXPOSURE);
        let r = *d.control.unwrap().as_raw();
        assert_eq!((r.exposure_us, r.iso), (2000, 400));
    }

    #[test]
    fn manual_to_manual_exposure_resends_both_values() {
        let old = RgbSettings {
            exposure: Exposure::Manual { us: 2000, iso: 400 },
            ..RgbSettings::default()
        };
        let new = RgbSettings {
            exposure: Exposure::Manual { us: 2000, iso: 800 },
            ..old
        };
        let d = diff_rgb(&old, &new);
        assert_eq!(bits(d), sys::OAK_CTL_MANUAL_EXPOSURE);
        assert_eq!(d.control.unwrap().as_raw().iso, 800);
        assert_eq!(d.control.unwrap().as_raw().exposure_us, 2000);
    }

    #[test]
    fn manual_to_auto_emits_ae_auto_and_the_auto_block() {
        let old = RgbSettings {
            exposure: Exposure::Manual { us: 2000, iso: 400 },
            ..RgbSettings::default()
        };
        let new = RgbSettings {
            exposure: auto(0, None, false),
            ..old
        };
        let d = diff_rgb(&old, &new);
        assert_eq!(
            bits(d),
            sys::OAK_CTL_AE_AUTO
                | sys::OAK_CTL_AE_COMPENSATION
                | sys::OAK_CTL_AE_LOCK
                | sys::OAK_CTL_AE_LIMIT
        );
        // No limit configured: one frame time, clearing any earlier limit.
        assert_eq!(d.control.unwrap().as_raw().ae_limit_us, 33_333);
        let new = RgbSettings {
            exposure: auto(3, Some(10_000), true),
            ..old
        };
        let d = diff_rgb(&old, &new);
        assert_eq!(
            bits(d),
            sys::OAK_CTL_AE_AUTO
                | sys::OAK_CTL_AE_COMPENSATION
                | sys::OAK_CTL_AE_LOCK
                | sys::OAK_CTL_AE_LIMIT
        );
        let r = *d.control.unwrap().as_raw();
        assert_eq!(
            (r.ae_compensation, r.ae_lock, r.ae_limit_us),
            (3, 1, 10_000)
        );
    }

    /// Auto (10 ms limit) → Manual → Auto (no limit): the last step must
    /// reset the limit, or the firmware keeps the 10 ms cap.
    #[test]
    fn auto_limit_manual_auto_sequence_resets_limit() {
        let a = RgbSettings {
            exposure: auto(0, Some(10_000), false),
            ..RgbSettings::default()
        };
        let m = RgbSettings {
            exposure: Exposure::Manual { us: 2000, iso: 400 },
            ..a
        };
        let b = RgbSettings {
            exposure: auto(0, None, false),
            ..a
        };
        assert_eq!(bits(diff_rgb(&a, &m)), sys::OAK_CTL_MANUAL_EXPOSURE);
        let d = diff_rgb(&m, &b);
        assert_ne!(bits(d) & sys::OAK_CTL_AE_AUTO, 0);
        assert_ne!(bits(d) & sys::OAK_CTL_AE_LIMIT, 0);
        assert_eq!(d.control.unwrap().as_raw().ae_limit_us, 33_333);
        // Same for mono, at the mono frame rate.
        let mut mb = MonoSettings {
            exposure: auto(0, None, false),
            ..MonoSettings::default()
        };
        mb.stream.fps = 60.0;
        let mm = MonoSettings {
            exposure: Exposure::Manual { us: 2000, iso: 400 },
            ..mb
        };
        let d = diff_mono(&mm, &mb);
        assert_ne!(bits(d) & sys::OAK_CTL_AE_LIMIT, 0);
        assert_eq!(d.control.unwrap().as_raw().ae_limit_us, 16_667);
    }

    #[test]
    fn auto_to_auto_sends_only_changed_subfields() {
        let old = RgbSettings::default();
        let comp = RgbSettings {
            exposure: auto(-4, None, false),
            ..old
        };
        let d = diff_rgb(&old, &comp);
        assert_eq!(bits(d), sys::OAK_CTL_AE_COMPENSATION);
        assert_eq!(d.control.unwrap().as_raw().ae_compensation, -4);

        let lock = RgbSettings {
            exposure: auto(0, None, true),
            ..old
        };
        assert_eq!(bits(diff_rgb(&old, &lock)), sys::OAK_CTL_AE_LOCK);

        let limit = RgbSettings {
            exposure: auto(0, Some(15_000), false),
            ..old
        };
        let d = diff_rgb(&old, &limit);
        assert_eq!(bits(d), sys::OAK_CTL_AE_LIMIT);
        assert_eq!(d.control.unwrap().as_raw().ae_limit_us, 15_000);
    }

    #[test]
    fn removing_ae_limit_sends_one_frame_time() {
        let old = RgbSettings {
            exposure: auto(0, Some(15_000), false),
            ..RgbSettings::default()
        };
        let new = RgbSettings {
            exposure: auto(0, None, false),
            ..old
        };
        let d = diff_rgb(&old, &new);
        assert_eq!(bits(d), sys::OAK_CTL_AE_LIMIT);
        assert_eq!(d.control.unwrap().as_raw().ae_limit_us, 33_333);
        let mut new15 = new;
        new15.stream.fps = 15.0;
        // fps also changed ⇒ restart, and the limit follows the new rate.
        let d = diff_rgb(&old, &new15);
        assert!(d.restart);
        assert_eq!(d.control.unwrap().as_raw().ae_limit_us, 66_667);
    }

    #[test]
    fn focus_transitions() {
        let auto_cv = RgbSettings::default();
        let manual = RgbSettings {
            focus: Focus::Manual(100),
            ..auto_cv
        };
        // Auto → Manual: AF off + lens.
        let d = diff_rgb(&auto_cv, &manual);
        assert_eq!(bits(d), sys::OAK_CTL_AF_MODE | sys::OAK_CTL_MANUAL_FOCUS);
        let r = *d.control.unwrap().as_raw();
        assert_eq!((r.af_mode, r.lens_pos), (DaiAf::Off.to_raw(), 100));
        // Manual → Manual: lens only.
        let manual2 = RgbSettings {
            focus: Focus::Manual(200),
            ..auto_cv
        };
        let d = diff_rgb(&manual, &manual2);
        assert_eq!(bits(d), sys::OAK_CTL_MANUAL_FOCUS);
        assert_eq!(d.control.unwrap().as_raw().lens_pos, 200);
        // Manual → Auto: AF mode only.
        let d = diff_rgb(&manual, &auto_cv);
        assert_eq!(bits(d), sys::OAK_CTL_AF_MODE);
        assert_eq!(
            d.control.unwrap().as_raw().af_mode,
            DaiAf::ContinuousVideo.to_raw()
        );
        // Auto → other Auto mode.
        let macro_ = RgbSettings {
            focus: Focus::Auto(AfMode::Macro),
            ..auto_cv
        };
        let d = diff_rgb(&auto_cv, &macro_);
        assert_eq!(bits(d), sys::OAK_CTL_AF_MODE);
        assert_eq!(d.control.unwrap().as_raw().af_mode, DaiAf::Macro.to_raw());
    }

    #[test]
    fn white_balance_transitions() {
        let auto_wb = RgbSettings::default();
        let kelvin = RgbSettings {
            wb: WhiteBalance::Kelvin(4000),
            ..auto_wb
        };
        let d = diff_rgb(&auto_wb, &kelvin);
        assert_eq!(bits(d), sys::OAK_CTL_AWB_MODE | sys::OAK_CTL_MANUAL_WB);
        let r = *d.control.unwrap().as_raw();
        assert_eq!((r.awb_mode, r.wb_kelvin), (DaiAwb::Off.to_raw(), 4000));
        // Kelvin → Kelvin: temperature only.
        let kelvin2 = RgbSettings {
            wb: WhiteBalance::Kelvin(6500),
            ..auto_wb
        };
        assert_eq!(bits(diff_rgb(&kelvin, &kelvin2)), sys::OAK_CTL_MANUAL_WB);
        // Kelvin → Auto: mode and lock.
        let d = diff_rgb(&kelvin, &auto_wb);
        assert_eq!(bits(d), sys::OAK_CTL_AWB_MODE | sys::OAK_CTL_AWB_LOCK);
        assert_eq!(d.control.unwrap().as_raw().awb_mode, DaiAwb::Auto.to_raw());
        // Auto → Auto: only what changed.
        let daylight = RgbSettings {
            wb: WhiteBalance::Auto {
                mode: AwbMode::Daylight,
                lock: false,
            },
            ..auto_wb
        };
        let d = diff_rgb(&auto_wb, &daylight);
        assert_eq!(bits(d), sys::OAK_CTL_AWB_MODE);
        assert_eq!(
            d.control.unwrap().as_raw().awb_mode,
            DaiAwb::Daylight.to_raw()
        );
        let locked = RgbSettings {
            wb: WhiteBalance::Auto {
                mode: AwbMode::Auto,
                lock: true,
            },
            ..auto_wb
        };
        let d = diff_rgb(&auto_wb, &locked);
        assert_eq!(bits(d), sys::OAK_CTL_AWB_LOCK);
        assert_eq!(d.control.unwrap().as_raw().awb_lock, 1);
    }

    #[test]
    fn stream_changes_restart() {
        let base = RgbSettings::default();
        let edits: [fn(&mut StreamSettings); 4] = [
            |s| s.width = 1280,
            |s| s.height = 720,
            |s| s.fps = 15.0,
            |s| s.enabled = false,
        ];
        for edit in edits {
            let mut new = base;
            edit(&mut new.stream);
            let d = diff_rgb(&base, &new);
            assert!(d.restart, "{new:?}");
            assert_eq!(d.control, None);
        }
        // A stream change plus a control change reports both.
        let mut new = base;
        new.stream.width = 1280;
        new.brightness = 1;
        let d = diff_rgb(&base, &new);
        assert!(d.restart);
        assert_eq!(bits(d), sys::OAK_CTL_BRIGHTNESS);
    }

    // --- diff_mono -------------------------------------------------------

    #[test]
    fn mono_scalar_and_exposure_changes() {
        let base = MonoSettings::default();
        let cases: [(fn(&mut MonoSettings), u64); 5] = [
            (|s| s.brightness = 3, sys::OAK_CTL_BRIGHTNESS),
            (|s| s.contrast = 2, sys::OAK_CTL_CONTRAST),
            (|s| s.sharpness = 1, sys::OAK_CTL_SHARPNESS),
            (|s| s.luma_denoise = 4, sys::OAK_CTL_LUMA_DENOISE),
            (
                |s| s.antibanding = Antibanding::Off,
                sys::OAK_CTL_ANTIBANDING,
            ),
        ];
        for (edit, bit) in cases {
            let mut new = base;
            edit(&mut new);
            assert_eq!(bits(diff_mono(&base, &new)), bit);
        }
        let manual = MonoSettings {
            exposure: Exposure::Manual { us: 5000, iso: 200 },
            ..base
        };
        assert_eq!(
            bits(diff_mono(&base, &manual)),
            sys::OAK_CTL_MANUAL_EXPOSURE
        );
        let d = diff_mono(&manual, &base);
        assert_eq!(
            bits(d),
            sys::OAK_CTL_AE_AUTO
                | sys::OAK_CTL_AE_COMPENSATION
                | sys::OAK_CTL_AE_LOCK
                | sys::OAK_CTL_AE_LIMIT
        );
    }

    #[test]
    fn mono_stream_change_restarts() {
        let base = MonoSettings::default();
        let mut new = base;
        new.stream.width = 640;
        new.stream.height = 400;
        let d = diff_mono(&base, &new);
        assert!(d.restart);
        assert!(d.control.is_none());
    }

    #[test]
    fn mono_group_override_diffs_per_camera() {
        let old = MonoGroup::default();
        // Turning an override on changes nothing (it copies shared).
        let mut new = old;
        new.set_override(MonoCam::Left, true);
        for cam in MonoCam::ALL {
            assert!(diff_mono(old.effective(cam), new.effective(cam)).is_empty());
        }
        // Editing the override touches only that camera.
        let mut edited = new;
        edited.editable(MonoCam::Left).brightness = 4;
        let l = diff_mono(
            new.effective(MonoCam::Left),
            edited.effective(MonoCam::Left),
        );
        let r = diff_mono(
            new.effective(MonoCam::Right),
            edited.effective(MonoCam::Right),
        );
        assert_eq!(bits(l), sys::OAK_CTL_BRIGHTNESS);
        assert!(r.is_empty());
        // Editing shared touches only the camera that follows it.
        let mut shared = edited;
        shared.shared.contrast = -2;
        let l = diff_mono(
            edited.effective(MonoCam::Left),
            shared.effective(MonoCam::Left),
        );
        let r = diff_mono(
            edited.effective(MonoCam::Right),
            shared.effective(MonoCam::Right),
        );
        assert!(l.is_empty());
        assert_eq!(bits(r), sys::OAK_CTL_CONTRAST);
        // Turning the override off reverts the left camera to shared.
        let mut off = shared;
        off.set_override(MonoCam::Left, false);
        let l = diff_mono(
            shared.effective(MonoCam::Left),
            off.effective(MonoCam::Left),
        );
        assert_eq!(bits(l), sys::OAK_CTL_BRIGHTNESS | sys::OAK_CTL_CONTRAST);
        assert_eq!(l.control.unwrap().as_raw().brightness, 0);
        assert_eq!(l.control.unwrap().as_raw().contrast, -2);
    }

    // --- device_delta ----------------------------------------------------

    #[test]
    fn ir_is_live_and_device_or_usb_changes_reopen() {
        let old = Config::default();
        assert_eq!(device_delta(&old, &old), DeviceDelta::default());

        let mut ir = old.clone();
        ir.device_settings.ir_dot = 0.8;
        let d = device_delta(&old, &ir);
        assert_eq!(d.ir_dot, Some(0.8));
        assert_eq!(d.ir_flood, None);
        assert!(!d.reopen);
        ir.device_settings.ir_flood = 0.25;
        assert_eq!(device_delta(&old, &ir).ir_flood, Some(0.25));

        let mut usb = old.clone();
        usb.device_settings.max_usb_speed = UsbSpeedPref::High;
        assert!(device_delta(&old, &usb).reopen);

        let mut dev = old.clone();
        dev.device = DeviceSelection::ById("14442C10514ACDD600".into());
        let d = device_delta(&old, &dev);
        assert!(d.reopen);
        assert_eq!((d.ir_dot, d.ir_flood), (None, None));
    }

    #[test]
    fn af_trigger_is_one_shot_command() {
        assert_eq!(af_trigger().set_bits(), sys::OAK_CTL_AF_TRIGGER);
        // Never part of state-derived params.
        let s = RgbSettings::default();
        assert_eq!(rgb_to_control(&s).set_bits() & sys::OAK_CTL_AF_TRIGGER, 0);
    }
}
