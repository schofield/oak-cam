//! The Settings window (⌘, and oak-cam › Settings…): app defaults kept in
//! `[ui]` of the config file (theme, start streaming on launch, automatic
//! reconnect, thumbnail rate, stats on the cards) plus the default device
//! and USB speed cap, the config file's location, and the two resets.
//!
//! It is an in-window [`egui::Window`] rather than stream-concern's
//! separate OS window (`tool_window.rs`): it is small, modal-ish, and an
//! in-window window is part of the `--gui-script` screenshots.
//! Every change edits the config like any other widget, so it is applied
//! live and saved 500 ms later.

use egui::Ui;
use oak_core::config::{Config, DeviceSelection, THUMBNAIL_FPS, UsbSpeedPref};

use super::panels::{self, device::USB_PREFS};
use super::status::Status;
use super::theme;

/// Restores what this window sets to its defaults: the `[ui]` preferences,
/// the default device (Auto) and the USB speed cap. Camera controls, the
/// virtual camera outputs, remembered IPs, tab and window size are kept.
pub(crate) fn reset_settings(cfg: &mut Config) {
    cfg.ui.reset_preferences();
    cfg.device = DeviceSelection::Auto;
    cfg.device_settings.max_usb_speed = UsbSpeedPref::default();
}

/// What the window asks the app to do beyond editing the config.
#[derive(Debug, Default)]
pub(crate) struct SettingsOut {
    pub reveal_config: bool,
}

/// The window's own state.
#[derive(Debug, Default)]
pub(crate) struct SettingsWindow {
    pub open: bool,
    /// The "Reset camera controls" confirmation is showing.
    pub confirm_reset: bool,
}

impl SettingsWindow {
    /// Opens the window with the camera-controls reset confirmation
    /// showing (Camera › Reset Controls to Defaults).
    pub(crate) fn ask_reset_controls(&mut self) {
        self.open = true;
        self.confirm_reset = true;
    }

    /// Draws the window when open.
    pub(crate) fn show(
        &mut self,
        ctx: &egui::Context,
        cfg: &mut Config,
        status: &Status,
        config_path: &str,
    ) -> SettingsOut {
        let mut out = SettingsOut::default();
        if !self.open {
            return out;
        }
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                self.contents(ui, cfg, status, config_path, &mut out);
            });
        if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.open = false;
            self.confirm_reset = false;
        }
        out
    }

    #[allow(clippy::too_many_lines)] // one straight-line form
    fn contents(
        &mut self,
        ui: &mut Ui,
        cfg: &mut Config,
        status: &Status,
        config_path: &str,
        out: &mut SettingsOut,
    ) {
        ui.set_width(420.0);
        theme::section(ui, "Appearance", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Theme");
                if let Some(t) = theme::theme_choices(ui, cfg.ui.theme) {
                    cfg.ui.theme = t;
                }
            });
            ui.weak("Classic and Mac Native follow the system light / dark setting.");
        });
        theme::section(ui, "Device", |ui| {
            ui.checkbox(&mut cfg.ui.start_streaming, "Start streaming on launch")
                .on_hover_text(
                    "Off: oak-cam opens without touching the device; use Device › Connect",
                );
            ui.checkbox(&mut cfg.ui.auto_reconnect, "Reconnect automatically")
                .on_hover_text(
                    "Reopen a lost device on its own (after 1, 2, 5, then every 10 s); off: \
                     wait for Rescan or Device › Connect",
                );
            egui::Grid::new("settings-device")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Default device");
                    default_device(ui, cfg, status);
                    ui.end_row();
                    ui.label("Max USB speed");
                    let pref = &mut cfg.device_settings.max_usb_speed;
                    let current = USB_PREFS
                        .iter()
                        .find(|(v, _)| v == pref)
                        .map_or("?", |(_, l)| l);
                    egui::ComboBox::from_id_salt("settings-usb")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for (v, l) in USB_PREFS {
                                ui.selectable_value(pref, v, l);
                            }
                        });
                    ui.end_row();
                });
        });
        theme::section(ui, "Camera cards", |ui| {
            ui.horizontal(|ui| {
                ui.label("Thumbnail rate");
                for fps in THUMBNAIL_FPS {
                    ui.radio_value(&mut cfg.ui.thumbnail_fps, fps, format!("{fps} fps"));
                }
            });
            ui.checkbox(&mut cfg.ui.stats_overlay, "Show stats on the cards");
        });
        theme::section(ui, "Config file", |ui| {
            ui.horizontal(|ui| {
                let mut text = config_path.to_owned();
                ui.add(
                    egui::TextEdit::singleline(&mut text)
                        .interactive(false)
                        .desired_width(300.0),
                )
                .on_hover_text(config_path);
                if ui.button("Reveal").clicked() {
                    out.reveal_config = true;
                }
            });
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Reset settings to defaults")
                .on_hover_text("Restore the settings in this window (camera controls are kept)")
                .clicked()
            {
                reset_settings(cfg);
            }
            if ui
                .button("Reset camera controls to defaults…")
                .on_hover_text(
                    "Restore the RGB, mono and device controls (keeps the device choice \
                     and virtual camera outputs)",
                )
                .clicked()
            {
                self.confirm_reset = true;
            }
        });
        if self.confirm_reset {
            theme::group_frame(ui).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.colored_label(
                    theme::WARN,
                    "Reset every RGB, mono and device control to its default?",
                );
                ui.horizontal(|ui| {
                    if ui.button("Reset").clicked() {
                        panels::reset_to_defaults(cfg, status.transport);
                        self.confirm_reset = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_reset = false;
                    }
                });
            });
        }
    }
}

/// Auto, or a remembered device (the configured one and the one open now).
fn default_device(ui: &mut Ui, cfg: &mut Config, status: &Status) {
    let mut choices = vec![(DeviceSelection::Auto, "Auto (first found)".to_owned())];
    let mut add = |sel: DeviceSelection, text: String| {
        if !choices.iter().any(|(s, _)| *s == sel) {
            choices.push((sel, text));
        }
    };
    match &cfg.device {
        DeviceSelection::Auto => {}
        DeviceSelection::ById(id) => add(cfg.device.clone(), format!("Id {id}")),
        DeviceSelection::ByIp(ip) => add(cfg.device.clone(), format!("IP {ip}")),
    }
    if let Some(i) = &status.info
        && !i.id.is_empty()
    {
        add(
            DeviceSelection::ById(i.id.clone()),
            format!("This device: {} · {}", i.name, i.id),
        );
    }
    let current = choices
        .iter()
        .find(|(s, _)| *s == cfg.device)
        .map_or_else(|| cfg.device.to_string(), |(_, t)| t.clone());
    egui::ComboBox::from_id_salt("settings-device")
        .width(260.0)
        .selected_text(current)
        .show_ui(ui, |ui| {
            for (sel, text) in choices {
                ui.selectable_value(&mut cfg.device, sel, text);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use oak_core::config::{Exposure, ThemePreset};

    #[test]
    fn reset_settings_restores_only_the_settings() {
        let mut cfg = Config::default();
        cfg.ui.theme = ThemePreset::FlatDark;
        cfg.ui.start_streaming = false;
        cfg.ui.auto_reconnect = false;
        cfg.ui.thumbnail_fps = 5;
        cfg.ui.stats_overlay = false;
        cfg.ui.recent_ips = vec!["198.51.100.2".into()];
        cfg.ui.tab = "mono".into();
        cfg.device = DeviceSelection::ById("X".into());
        cfg.device_settings.max_usb_speed = UsbSpeedPref::High;
        cfg.device_settings.ir_dot = 0.5;
        cfg.rgb.exposure = Exposure::Manual { us: 2000, iso: 400 };
        cfg.vcam.left.enabled = false;
        let before = cfg.clone();
        reset_settings(&mut cfg);
        let d = Config::default();
        assert_eq!(cfg.ui.theme, d.ui.theme);
        assert_eq!(
            cfg.ui.theme,
            ThemePreset::MacNative,
            "the M6 look is the default"
        );
        assert!(cfg.ui.start_streaming && cfg.ui.auto_reconnect && cfg.ui.stats_overlay);
        assert_eq!(cfg.ui.thumbnail_fps, d.ui.thumbnail_fps);
        assert_eq!(cfg.device, DeviceSelection::Auto);
        assert_eq!(cfg.device_settings.max_usb_speed, UsbSpeedPref::default());
        // Kept: camera controls, IR, outputs, remembered IPs and the tab.
        assert_eq!(cfg.rgb, before.rgb);
        assert!((cfg.device_settings.ir_dot - 0.5).abs() < f32::EPSILON);
        assert!(!cfg.vcam.left.enabled);
        assert_eq!(cfg.ui.recent_ips, before.ui.recent_ips);
        assert_eq!(cfg.ui.tab, "mono");
    }

    /// Drawing the window (no input) leaves the config as it was.
    #[test]
    fn drawing_the_window_round_trips_the_config() {
        let ctx = egui::Context::default();
        let mut cfg = Config::default();
        cfg.ui.theme = ThemePreset::SoftLight;
        cfg.ui.thumbnail_fps = 5;
        cfg.device = DeviceSelection::ByIp("198.51.100.2".into());
        let before = cfg.clone();
        let mut w = SettingsWindow {
            open: true,
            confirm_reset: true,
        };
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let out = w.show(ui.ctx(), &mut cfg, &Status::default(), "~/x.toml");
            assert!(!out.reveal_config);
        });
        out.drop_without_applying_deltas();
        assert_eq!(cfg, before);
        assert!(w.open);
    }
}
