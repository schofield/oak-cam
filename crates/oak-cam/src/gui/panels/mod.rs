//! The side panel: RGB, Mono, Device and Virtual cameras tabs. (Reset to
//! defaults lives in the Settings window and the Camera menu.)

pub(crate) mod device;
pub(crate) mod mono;
pub(crate) mod rgb;
pub(crate) mod vcam;

use oak_core::config::{Config, Transport};

use super::status::Status;
use super::vcam_setup::VcamSetup;

/// Side panel tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    Rgb,
    Mono,
    Device,
    Vcam,
}

impl Tab {
    pub(crate) const ALL: [Self; 4] = [Self::Rgb, Self::Mono, Self::Device, Self::Vcam];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Rgb => "RGB",
            Self::Mono => "Mono",
            Self::Device => "Device",
            Self::Vcam => "Virtual cameras",
        }
    }

    /// The short name stored in `[ui] tab` and used in script steps.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Rgb => "rgb",
            Self::Mono => "mono",
            Self::Device => "device",
            Self::Vcam => "vcam",
        }
    }

    /// Parses a `--gui-script` (or `[ui] tab`) tab name.
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "rgb" => Some(Self::Rgb),
            "mono" => Some(Self::Mono),
            "device" => Some(Self::Device),
            "vcam" | "virtual cameras" => Some(Self::Vcam),
            _ => None,
        }
    }
}

/// What the panel asks the app to do beyond editing the config.
#[derive(Debug, Default)]
pub(crate) struct PanelOut {
    pub trigger_af: bool,
}

/// The camera settings "Reset camera controls to defaults" restores: RGB
/// and mono settings and the IR levels. The device choice, the USB speed
/// cap (a Settings item), virtual camera outputs and `[ui]` are kept. While
/// connected over `transport`, that link's stream profile is reset too and
/// its default sizes applied (otherwise the USB3 sizes of `Config::default`
/// would be remembered as a custom profile).
pub(crate) fn reset_to_defaults(cfg: &mut Config, transport: Option<Transport>) {
    let d = Config::default();
    cfg.rgb = d.rgb;
    cfg.mono = d.mono;
    cfg.device_settings.ir_dot = d.device_settings.ir_dot;
    cfg.device_settings.ir_flood = d.device_settings.ir_flood;
    if let Some(t) = transport {
        cfg.reset_profile(t);
    }
}

/// Draws the tab strip and the selected tab.
pub(crate) fn show(
    ui: &mut egui::Ui,
    tab: &mut Tab,
    cfg: &mut Config,
    status: &Status,
    vcam: &mut vcam::VcamPanel,
    setup: &mut VcamSetup,
    config_path: &str,
) -> PanelOut {
    let mut out = PanelOut::default();
    ui.horizontal_wrapped(|ui| {
        for t in Tab::ALL {
            ui.selectable_value(tab, t, t.label());
        }
    });
    ui.separator();
    egui::Panel::bottom("side_bottom")
        .resizable(false)
        .show_separator_line(true)
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(egui::RichText::new(config_path).weak()).truncate())
                    .on_hover_text(config_path);
            });
            ui.add_space(2.0);
        });
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| match tab {
            Tab::Rgb => {
                out.trigger_af = rgb::show(ui, &mut cfg.rgb, status);
            }
            Tab::Mono => mono::show(ui, &mut cfg.mono, status),
            Tab::Device => device::show(ui, cfg, status),
            Tab::Vcam => vcam.show(ui, cfg, setup),
        });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use oak_core::config::{DeviceSelection, Exposure};

    #[test]
    fn reset_keeps_device_and_outputs() {
        let mut cfg = Config {
            device: DeviceSelection::ByIp("198.51.100.2".into()),
            ..Config::default()
        };
        cfg.vcam.left.enabled = false;
        cfg.ui.recent_ips = vec!["198.51.100.2".into()];
        cfg.rgb.exposure = Exposure::Manual { us: 2000, iso: 400 };
        cfg.mono.set_override(oak_core::config::MonoCam::Left, true);
        cfg.device_settings.ir_dot = 0.5;
        cfg.device_settings.ir_flood = 0.25;
        cfg.device_settings.max_usb_speed = oak_core::config::UsbSpeedPref::High;
        let mut on_poe = cfg.clone();
        reset_to_defaults(&mut cfg, None);
        assert_eq!(cfg.rgb, Config::default().rgb);
        assert_eq!(cfg.mono, Config::default().mono);
        assert_eq!(
            cfg.device_settings,
            oak_core::config::DeviceSettings {
                max_usb_speed: oak_core::config::UsbSpeedPref::High,
                ..Config::default().device_settings
            },
            "IR reset, USB speed cap kept"
        );
        assert_eq!(cfg.device, DeviceSelection::ByIp("198.51.100.2".into()));
        assert!(!cfg.vcam.left.enabled);
        assert_eq!(cfg.ui.recent_ips, ["198.51.100.2"]);
        // Connected over PoE: that link's profile is reset and applied.
        on_poe.rgb.stream.fps = 15.0;
        on_poe.remember_profile(Transport::Poe);
        on_poe.profiles.usb2 = Some(Transport::Usb2.defaults());
        reset_to_defaults(&mut on_poe, Some(Transport::Poe));
        assert_eq!(on_poe.profiles.poe, None);
        assert_eq!(on_poe.active_profile(), Transport::Poe.defaults());
        assert!(on_poe.profiles.usb2.is_some(), "other links untouched");
    }

    #[test]
    fn tab_names_parse() {
        for t in Tab::ALL {
            assert_eq!(Tab::parse(t.label()), Some(t));
        }
        assert_eq!(Tab::parse("vcam"), Some(Tab::Vcam));
        assert_eq!(Tab::parse("x"), None);
    }

    /// Drawing every tab over a config (no input) leaves the config
    /// exactly as it was: config -> widgets -> config is the identity.
    #[test]
    fn drawing_every_tab_round_trips_the_config() {
        let mut cfg = Config::default();
        cfg.rgb.exposure = Exposure::Manual { us: 2345, iso: 321 };
        cfg.rgb.focus = oak_core::config::Focus::Manual(77);
        cfg.rgb.wb = oak_core::config::WhiteBalance::Kelvin(4321);
        cfg.rgb.brightness = -3;
        cfg.rgb.sharpness = 2;
        cfg.mono.shared.exposure = Exposure::Auto {
            comp: -2,
            limit_us: Some(12_345),
            lock: true,
        };
        cfg.mono
            .set_override(oak_core::config::MonoCam::Right, true);
        cfg.mono.right.as_mut().unwrap().contrast = 4;
        cfg.device_settings.ir_dot = 0.37;
        cfg.device_settings.ir_flood = 0.05;
        let mut poe = Transport::Poe.defaults();
        poe.rgb.fps = 20.0;
        cfg.profiles.poe = Some(poe);
        let before = cfg.clone();
        let status = Status::default();
        let mut vcam = vcam::VcamPanel::new();
        let mut setup = VcamSetup::new([super::super::camera_card::Output::Platform; 3], None);
        let ctx = egui::Context::default();
        for t in Tab::ALL {
            let mut tab = t;
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let _ = show(ui, &mut tab, &mut cfg, &status, &mut vcam, &mut setup, "x");
            });
            out.textures_delta.clear();
            assert_eq!(cfg, before, "tab {t:?} changed the config");
        }
    }
}
