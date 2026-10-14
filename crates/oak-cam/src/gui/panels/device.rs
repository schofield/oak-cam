//! Device tab: IR dot projector and flood LED (Pro models), USB speed cap,
//! the stream profile per link type, and what the open device reports.

use egui::{Slider, Ui};
use oak_core::config::{Config, Transport, UsbSpeedPref};

use crate::gui::status::{Status, shapes_text};
use crate::gui::theme;
use crate::gui::widgets::{percent_to_ratio, ratio_to_percent};

pub(crate) const USB_PREFS: [(UsbSpeedPref, &str); 3] = [
    (UsbSpeedPref::High, "USB2 (480 Mbps)"),
    (UsbSpeedPref::Super, "USB3 (5 Gbps)"),
    (UsbSpeedPref::SuperPlus, "No limit"),
];

/// An IR intensity slider shown in percent; writes the ratio only when
/// the user moves it.
fn ir_slider(ui: &mut Ui, label: &str, ratio: &mut f32) {
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 18.0], egui::Label::new(label));
        let mut pct = ratio_to_percent(*ratio);
        if ui
            .add(
                Slider::new(&mut pct, 0.0..=100.0)
                    .step_by(1.0)
                    .fixed_decimals(0)
                    .suffix(" %"),
            )
            .changed()
        {
            *ratio = percent_to_ratio(pct);
        }
    });
}

/// Draws the tab.
pub(crate) fn show(ui: &mut Ui, cfg: &mut Config, status: &Status) {
    let ir_drivers = status.info.as_ref().map(|i| i.ir_drivers);
    theme::section(ui, "Infrared", |ui| {
        let enabled = ir_drivers != Some(0);
        ui.add_enabled_ui(enabled, |ui| {
            let d = &mut cfg.device_settings;
            ir_slider(ui, "Dot projector", &mut d.ir_dot);
            ir_slider(ui, "Flood LED", &mut d.ir_flood);
        })
        .response
        .on_disabled_hover_text(
            "This device has no IR drivers (only OAK-D Pro models have the dot projector \
             and flood LED)",
        );
        if ir_drivers.is_none() {
            ui.weak("Applied when a device connects.");
        } else {
            ui.weak("Applied live, no restart.");
        }
    });
    theme::section(ui, "USB", |ui| {
        ui.horizontal(|ui| {
            ui.add_sized([96.0, 18.0], egui::Label::new("Max speed"));
            let pref = &mut cfg.device_settings.max_usb_speed;
            let current = USB_PREFS
                .iter()
                .find(|(v, _)| v == pref)
                .map_or("?", |(_, l)| l);
            egui::ComboBox::from_id_salt("usb-speed")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (v, l) in USB_PREFS {
                        ui.selectable_value(pref, v, l);
                    }
                });
        });
        ui.weak("Changing it reopens the device.");
    });
    theme::section(ui, "Stream profiles", |ui| {
        egui::Grid::new("stream-profiles")
            .num_columns(4)
            .show(ui, |ui| {
                for t in Transport::ALL {
                    let custom = cfg.profiles.get(t).is_some();
                    let p = cfg.profile(t);
                    let active = status.transport == Some(t);
                    let name = egui::RichText::new(t.label());
                    ui.label(if active { name.strong() } else { name });
                    ui.monospace(shapes_text(&p, "\n"));
                    ui.weak(if custom { "(custom)" } else { "(defaults)" });
                    if ui
                        .add_enabled(custom, egui::Button::new("Reset to defaults").small())
                        .clicked()
                    {
                        if active {
                            cfg.reset_profile(t);
                        } else {
                            *cfg.profiles.get_mut(t) = None;
                        }
                    }
                    ui.end_row();
                }
            });
        ui.weak(
            "Applied when a device connects over that link; size and fps edits made while \
             connected are remembered for it.",
        );
    });
    theme::section(ui, "Connected device", |ui| match &status.info {
        Some(i) => {
            egui::Grid::new("device-info")
                .num_columns(2)
                .show(ui, |ui| {
                    let temp = status
                        .temp_c
                        .map_or_else(|| "n/a".to_owned(), |t| format!("{t:.1} °C"));
                    for (k, v) in [
                        ("Name", i.name.clone()),
                        ("Id", i.id.clone()),
                        ("Transport", i.protocol.to_string()),
                        ("USB speed", status.usb.to_string()),
                        ("IR drivers", i.ir_drivers.to_string()),
                        ("Chip temp", temp),
                    ] {
                        ui.label(k);
                        ui.monospace(v);
                        ui.end_row();
                    }
                });
        }
        None => {
            ui.weak("No device open.");
        }
    });
    if let Some(path) = crate::gui::log_path() {
        let text = format!("Log: {}", crate::gui::display_path(path));
        ui.add(egui::Label::new(egui::RichText::new(&text).weak().small()).truncate())
            .on_hover_text(path.display().to_string());
    }
}
