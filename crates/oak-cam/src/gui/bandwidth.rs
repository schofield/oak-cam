//! Link load estimate: what the enabled streams cost the link against what
//! the transport can carry in practice. USB is limited by bytes (RGB NV12
//! 1.5 B/px, mono GRAY8 1 B/px); PoE by a per-frame cost plus bytes.

use depthai::{Protocol, UsbSpeed};
use oak_core::capture::SensorId;
use oak_core::config::Config;

/// Warn when the estimate exceeds this share of the budget.
pub(crate) const WARN_RATIO: f64 = 0.8;

/// PoE: seconds of the camera's network path each frame costs, across all
/// streams. The limit is the camera's per-frame cost on its network path,
/// not the gigabit wire. PoE load is
/// `frames/s * POE_FRAME_COST + bytes/s / POE_BYTES_PER_S`, fitted to
/// measurements taken 2026-09-28 on an OAK-D-PRO-W-POE over a gigabit LAN
/// (Mac Thunderbolt Ethernet, 2.5G switch), `--headless --no-vcam --stats`,
/// values steady after 60 s (rgb / both monos, bytes/s, frames/s, model load):
/// - 1280x720@30 / off, 41.5 MB/s (332 Mbps), 30: clean, p50 41 ms, 0 drops (0.53).
/// - 1280x720@20 / 640x400@20, 38 MB/s (303 Mbps), 60: clean, p50 45 ms,
///   0 drops (0.76).
/// - 1280x720@24 / 640x400@24, 45.5 MB/s (364 Mbps), 72: borderline, p50
///   49 ms, p99 spikes to 114 ms, 0 drops (0.91).
/// - 1920x1080@15 / 1280x800@15, 77.5 MB/s (620 Mbps), 45: clean-ish,
///   p50 76 ms, p99 114 ms, 0 drops (0.90).
/// - 1280x720@30 / 640x400@30, 57 MB/s (455 Mbps), 90: saturated, ~25 fps
///   each, p50 310 ms, ~260 drops/min (1.15).
/// - 1920x1080@20 / 1280x800@20, 104 MB/s (830 Mbps), 60: saturated,
///   ~16 fps each, p50 370-490 ms, ~270 drops/min (1.20).
const POE_FRAME_COST: f64 = 8.5e-3;
/// PoE: bytes per second the link carries on top of the per-frame cost
/// (about 1.2 Gbps, near the gigabit wire rate); see [`POE_FRAME_COST`].
const POE_BYTES_PER_S: f64 = 150e6;
/// Practical USB 3 bulk throughput of the RVC2 (about 400 MB/s).
const USB3_BPS: f64 = 3.2e9;
/// Practical USB 2 bulk throughput (about 40 MB/s).
const USB2_BPS: f64 = 320e6;
/// USB 1.
const USB1_BPS: f64 = 10e6;

/// Bits per second one sensor's stream needs (0 when disabled).
pub(crate) fn stream_bps(cfg: &Config, sensor: SensorId) -> f64 {
    let s = cfg.stream(sensor);
    if !s.enabled {
        return 0.0;
    }
    let bytes_per_px = if sensor == SensorId::Rgb { 1.5 } else { 1.0 };
    f64::from(s.width) * f64::from(s.height) * bytes_per_px * 8.0 * f64::from(s.fps)
}

/// Bits per second all enabled streams need.
pub(crate) fn estimate_bps(cfg: &Config) -> f64 {
    SensorId::ALL.into_iter().map(|s| stream_bps(cfg, s)).sum()
}

/// Frames per second all enabled streams send together.
pub(crate) fn frames_per_second_total(cfg: &Config) -> f64 {
    SensorId::ALL
        .into_iter()
        .map(|s| cfg.stream(s))
        .filter(|s| s.enabled)
        .map(|s| f64::from(s.fps))
        .sum()
}

/// What a USB link carries in practice. Unknown (not connected yet)
/// counts as USB 3.
fn usb_budget_bps(usb: UsbSpeed) -> f64 {
    match usb {
        UsbSpeed::High => USB2_BPS,
        UsbSpeed::Low | UsbSpeed::Full => USB1_BPS,
        _ => USB3_BPS,
    }
}

/// PoE load: the per-frame cost plus the bytes, as a share of one second
/// of the camera's network path (see [`POE_FRAME_COST`]).
fn poe_load(cfg: &Config) -> f64 {
    frames_per_second_total(cfg) * POE_FRAME_COST + estimate_bps(cfg) / 8.0 / POE_BYTES_PER_S
}

/// `used / budget`: for PoE the per-frame model, for USB (and an unknown
/// link, counted as USB 3) bits per second against the practical budget.
pub(crate) fn load(cfg: &Config, protocol: Option<Protocol>, usb: UsbSpeed) -> f64 {
    match protocol {
        Some(Protocol::TcpIp) => poe_load(cfg),
        _ => estimate_bps(cfg) / usb_budget_bps(usb),
    }
}

/// The top bar's load readout.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LinkLoad {
    /// `used / budget`, as [`load`].
    pub load: f64,
    /// `PoE load 76 %` / `303 Mbps / 3.2 Gbps (9 %)`.
    pub label: String,
    /// What the figure means.
    pub hover: String,
}

/// The load of `cfg` on the link, with its top-bar text.
pub(crate) fn link_load(cfg: &Config, protocol: Option<Protocol>, usb: UsbSpeed) -> LinkLoad {
    let load = load(cfg, protocol, usb);
    let used = estimate_bps(cfg);
    if protocol == Some(Protocol::TcpIp) {
        LinkLoad {
            load,
            label: format!("PoE load {:.0} %", load * 100.0),
            hover: format!(
                "Frames cost ~{:.1} ms each on the camera's network path, plus bytes at \
                 ~{:.1} Gbps: {:.0} frames/s + {}",
                POE_FRAME_COST * 1e3,
                POE_BYTES_PER_S * 8.0 / 1e9,
                frames_per_second_total(cfg),
                format_bps(used)
            ),
        }
    } else {
        LinkLoad {
            load,
            label: format!(
                "{} / {} ({:.0} %)",
                format_bps(used),
                format_bps(usb_budget_bps(usb)),
                load * 100.0
            ),
            hover: "Estimated link use of the enabled streams (RGB NV12 1.5 B/px, mono 1 B/px) \
                    against the practical budget of this transport; above 80 % frames may drop"
                .into(),
        }
    }
}

/// `1.24 Gbps` / `455 Mbps`.
pub(crate) fn format_bps(bps: f64) -> String {
    if bps >= 1e9 {
        format!("{:.2} Gbps", bps / 1e9)
    } else {
        format!("{:.0} Mbps", bps / 1e6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oak_core::config::{MonoCam, Transport};

    fn set(cfg: &mut Config, sensor: SensorId, w: u32, h: u32, fps: f32) {
        let s = match sensor {
            SensorId::Rgb => &mut cfg.rgb.stream,
            SensorId::Left => &mut cfg.mono.editable(MonoCam::Left).stream,
            SensorId::Right => &mut cfg.mono.editable(MonoCam::Right).stream,
        };
        (s.width, s.height, s.fps) = (w, h, fps);
    }

    #[test]
    fn default_matches_the_plan_table() {
        let cfg = Config::default();
        // Budget: RGB 1080p30 NV12 ~747 Mbps, mono 800p30 ~246 Mbps each.
        assert!((stream_bps(&cfg, SensorId::Rgb) / 1e6 - 746.5).abs() < 0.1);
        assert!((stream_bps(&cfg, SensorId::Left) / 1e6 - 245.8).abs() < 0.1);
        assert!((estimate_bps(&cfg) / 1e9 - 1.238).abs() < 0.001);
        assert_eq!(format_bps(estimate_bps(&cfg)), "1.24 Gbps");
        // Fine on USB3, too much for PoE.
        assert!(load(&cfg, Some(Protocol::Usb), UsbSpeed::SuperPlus) < WARN_RATIO);
        assert!(load(&cfg, Some(Protocol::TcpIp), UsbSpeed::Unknown) > 1.0);
    }

    #[test]
    fn disabled_streams_cost_nothing() {
        let mut cfg = Config::default();
        cfg.rgb.stream.enabled = false;
        assert!(stream_bps(&cfg, SensorId::Rgb).abs() < f64::EPSILON);
        assert!((estimate_bps(&cfg) / 1e6 - 491.5).abs() < 0.1);
    }

    /// A representative (protocol, USB speed) for `t`.
    fn link(t: Transport) -> (Option<Protocol>, UsbSpeed) {
        match t {
            Transport::Poe => (Some(Protocol::TcpIp), UsbSpeed::Unknown),
            Transport::Usb3 => (Some(Protocol::Usb), UsbSpeed::Super),
            Transport::Usb2 => (Some(Protocol::Usb), UsbSpeed::High),
        }
    }

    #[test]
    fn transport_defaults_fit_their_link() {
        for t in Transport::ALL {
            let mut cfg = Config::default();
            cfg.apply_transport(t);
            let (protocol, usb) = link(t);
            assert_eq!(Transport::of(protocol.unwrap(), usb), t);
            let l = load(&cfg, protocol, usb);
            assert!(l <= WARN_RATIO, "{t}: {:.0} %", l * 100.0);
        }
    }

    /// rgb `r` and both monos `m` (`None`: disabled), as (w, h, fps).
    fn poe_cfg(r: Option<(u32, u32, f32)>, m: Option<(u32, u32, f32)>) -> Config {
        let mut cfg = Config::default();
        match r {
            Some((w, h, fps)) => set(&mut cfg, SensorId::Rgb, w, h, fps),
            None => cfg.rgb.stream.enabled = false,
        }
        match m {
            Some((w, h, fps)) => set(&mut cfg, SensorId::Left, w, h, fps),
            None => cfg.mono.editable(MonoCam::Left).stream.enabled = false,
        }
        cfg
    }

    fn poe(cfg: &Config) -> f64 {
        load(cfg, Some(Protocol::TcpIp), UsbSpeed::Unknown)
    }

    #[test]
    fn plan_profiles_against_their_budgets() {
        // USB3: 4K30 + both monos warns.
        let mut cfg = Config::default();
        set(&mut cfg, SensorId::Rgb, 3840, 2160, 30.0);
        assert!(load(&cfg, Some(Protocol::Usb), UsbSpeed::Super) > WARN_RATIO);
        // 720p15 + 400p15 fits USB2 (the default profile is @12).
        let usb2 = poe_cfg(Some((1280, 720, 15.0)), Some((640, 400, 15.0)));
        assert!(load(&usb2, Some(Protocol::Usb), UsbSpeed::High) < WARN_RATIO);
        assert!(load(&Config::default(), Some(Protocol::Usb), UsbSpeed::High) > 1.0);
    }

    #[test]
    fn poe_model_matches_the_measurements() {
        // Clean: 720p30 alone, and 720p20 + 400p20.
        let alone = poe_cfg(Some((1280, 720, 30.0)), None);
        assert_eq!(format_bps(estimate_bps(&alone)), "332 Mbps");
        assert!(poe(&alone) < WARN_RATIO, "{}", poe(&alone));
        let p20 = poe_cfg(Some((1280, 720, 20.0)), Some((640, 400, 20.0)));
        assert_eq!(format_bps(estimate_bps(&p20)), "303 Mbps");
        assert!(poe(&p20) < WARN_RATIO, "{}", poe(&p20));
        // Borderline / clean-ish: 24/24/24 and 1080p15 + 800p15 warn but fit.
        for cfg in [
            poe_cfg(Some((1280, 720, 24.0)), Some((640, 400, 24.0))),
            poe_cfg(Some((1920, 1080, 15.0)), Some((1280, 800, 15.0))),
        ] {
            let l = poe(&cfg);
            assert!(l > WARN_RATIO && l < 1.0, "{l}");
        }
        // Saturated: 30/30/30 and 1080p20 + 800p20.
        let p30 = poe_cfg(Some((1280, 720, 30.0)), Some((640, 400, 30.0)));
        assert_eq!(format_bps(estimate_bps(&p30)), "455 Mbps");
        let big = poe_cfg(Some((1920, 1080, 20.0)), Some((1280, 800, 20.0)));
        // Measured 104 MB/s (830 Mbps) on the wire; the payload is 825.
        assert_eq!(format_bps(estimate_bps(&big)), "825 Mbps");
        for cfg in [p30, big] {
            let l = poe(&cfg);
            assert!(l > 1.0, "{l}");
        }
    }

    #[test]
    fn frames_count_only_enabled_streams() {
        let rgb_only = poe_cfg(Some((1280, 720, 30.0)), None);
        assert!((frames_per_second_total(&rgb_only) - 30.0).abs() < 1e-9);
        let mono_only = poe_cfg(None, Some((640, 400, 20.0)));
        assert!((frames_per_second_total(&mono_only) - 40.0).abs() < 1e-9);
        let expected = 40.0 * POE_FRAME_COST + 2.0 * 640.0 * 400.0 * 20.0 / POE_BYTES_PER_S;
        assert!((poe(&mono_only) - expected).abs() < 1e-9);
        let mut none = mono_only.clone();
        none.mono.editable(MonoCam::Left).stream.enabled = false;
        assert!(frames_per_second_total(&none).abs() < f64::EPSILON);
        assert!(poe(&none).abs() < f64::EPSILON);
    }

    #[test]
    fn link_load_text() {
        let p20 = poe_cfg(Some((1280, 720, 20.0)), Some((640, 400, 20.0)));
        let l = link_load(&p20, Some(Protocol::TcpIp), UsbSpeed::Unknown);
        assert_eq!(l.label, "PoE load 76 %");
        assert_eq!(
            l.hover,
            "Frames cost ~8.5 ms each on the camera's network path, plus bytes at \
             ~1.2 Gbps: 60 frames/s + 303 Mbps"
        );
        let u = link_load(&p20, Some(Protocol::Usb), UsbSpeed::Super);
        assert_eq!(u.label, "303 Mbps / 3.20 Gbps (9 %)");
        assert!((u.load - load(&p20, Some(Protocol::Usb), UsbSpeed::Super)).abs() < f64::EPSILON);
    }
}
