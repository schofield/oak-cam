//! Control widgets shared by the RGB and Mono tabs, plus the pure mode
//! switches behind them (unit-tested). Widgets only write to the config
//! when the user changes something, so drawing a config never alters it.

use egui::{Slider, Ui};
use oak_core::config::{AE_COMP, Antibanding, DETAIL, EXPOSURE_US, Exposure, ISO, TUNE};

/// Manual exposure a switch from auto starts at when the sensor has not
/// reported one yet.
pub(crate) const DEFAULT_MANUAL: (u32, u32) = (10_000, 400);

/// Switches `e` to manual (seeded with `seed`, the exposure/ISO the sensor
/// reports now, so the picture does not jump) or back to plain auto. A
/// switch to the mode `e` already has keeps it unchanged.
pub(crate) fn switch_exposure(e: &mut Exposure, manual: bool, seed: Option<(u32, u32)>) {
    match (*e, manual) {
        (Exposure::Auto { .. }, true) => {
            let (us, iso) = seed
                .filter(|&(us, iso)| us > 0 && iso > 0)
                .unwrap_or(DEFAULT_MANUAL);
            *e = Exposure::Manual {
                us: us.clamp(EXPOSURE_US.0, EXPOSURE_US.1),
                iso: iso.clamp(ISO.0, ISO.1),
            };
        }
        (Exposure::Manual { .. }, false) => *e = Exposure::default(),
        _ => {}
    }
}

/// IR ratio (0..=1) as the percent the slider shows.
pub(crate) fn ratio_to_percent(r: f32) -> f32 {
    (r * 100.0).round()
}

/// Slider percent back to the ratio the config stores.
pub(crate) fn percent_to_ratio(p: f32) -> f32 {
    (p.round() / 100.0).clamp(0.0, 1.0)
}

/// A labelled slider row; returns whether it changed.
pub(crate) fn slider<T: egui::emath::Numeric>(
    ui: &mut Ui,
    label: &str,
    v: &mut T,
    range: std::ops::RangeInclusive<T>,
) -> bool {
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 18.0], egui::Label::new(label));
        ui.add(Slider::new(v, range)).changed()
    })
    .inner
}

/// Exposure: auto (compensation, limit, lock) or manual (time, ISO).
pub(crate) fn exposure(ui: &mut Ui, id: &str, e: &mut Exposure, seed: Option<(u32, u32)>) {
    let mut manual = matches!(e, Exposure::Manual { .. });
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 18.0], egui::Label::new("Exposure"));
        let a = ui.selectable_value(&mut manual, false, "Auto");
        let m = ui.selectable_value(&mut manual, true, "Manual");
        if a.changed() || m.changed() {
            switch_exposure(e, manual, seed);
        }
        if let Some((us, iso)) = seed.filter(|s| s.0 > 0) {
            ui.weak(format!("now {us} µs · ISO {iso}"));
        }
    });
    ui.push_id(id, |ui| match e {
        Exposure::Auto {
            comp,
            limit_us,
            lock,
        } => {
            slider(ui, "Compensation", comp, AE_COMP.0..=AE_COMP.1);
            ui.horizontal(|ui| {
                ui.add_sized([96.0, 18.0], egui::Label::new("Limit"));
                let mut on = limit_us.is_some();
                if ui.checkbox(&mut on, "").changed() {
                    *limit_us = on.then_some(limit_us.unwrap_or(16_000));
                }
                if let Some(l) = limit_us {
                    ui.add(
                        Slider::new(l, 100..=EXPOSURE_US.1)
                            .logarithmic(true)
                            .suffix(" µs"),
                    );
                } else {
                    ui.weak("about one frame time");
                }
            });
            ui.horizontal(|ui| {
                ui.add_sized([96.0, 18.0], egui::Label::new(""));
                ui.checkbox(lock, "Lock auto exposure");
            });
        }
        Exposure::Manual { us, iso } => {
            ui.horizontal(|ui| {
                ui.add_sized([96.0, 18.0], egui::Label::new("Time"));
                ui.add(
                    Slider::new(us, EXPOSURE_US.0..=EXPOSURE_US.1)
                        .logarithmic(true)
                        .suffix(" µs"),
                );
            });
            ui.horizontal(|ui| {
                ui.add_sized([96.0, 18.0], egui::Label::new("ISO"));
                ui.add(Slider::new(iso, ISO.0..=ISO.1).logarithmic(true));
            });
        }
    });
}

/// Every anti-banding choice with its label.
pub(crate) const ANTIBANDING: [(Antibanding, &str); 4] = [
    (Antibanding::Off, "Off"),
    (Antibanding::Mains50Hz, "50 Hz"),
    (Antibanding::Mains60Hz, "60 Hz"),
    (Antibanding::Auto, "Auto"),
];

/// Anti-banding combo.
pub(crate) fn antibanding(ui: &mut Ui, id: &str, a: &mut Antibanding) {
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 18.0], egui::Label::new("Anti-banding"));
        let current = ANTIBANDING
            .iter()
            .find(|(v, _)| v == a)
            .map_or("?", |(_, l)| l);
        egui::ComboBox::from_id_salt(id)
            .selected_text(current)
            .show_ui(ui, |ui| {
                for (v, label) in ANTIBANDING {
                    ui.selectable_value(a, v, label);
                }
            });
    });
}

/// Brightness / contrast (and optionally saturation) sliders.
pub(crate) fn tune(
    ui: &mut Ui,
    brightness: &mut i8,
    contrast: &mut i8,
    saturation: Option<&mut i8>,
) {
    slider(ui, "Brightness", brightness, TUNE.0..=TUNE.1);
    slider(ui, "Contrast", contrast, TUNE.0..=TUNE.1);
    if let Some(s) = saturation {
        slider(ui, "Saturation", s, TUNE.0..=TUNE.1);
    }
}

/// Sharpness / denoise slider.
pub(crate) fn detail(ui: &mut Ui, label: &str, v: &mut u8) {
    slider(ui, label, v, DETAIL.0..=DETAIL.1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposure_switch_seeds_from_the_sensor_and_clamps() {
        let mut e = Exposure::default();
        switch_exposure(&mut e, true, Some((8_000, 250)));
        assert_eq!(
            e,
            Exposure::Manual {
                us: 8_000,
                iso: 250
            }
        );
        // Same mode: unchanged.
        switch_exposure(&mut e, true, Some((1, 1)));
        assert_eq!(
            e,
            Exposure::Manual {
                us: 8_000,
                iso: 250
            }
        );
        switch_exposure(&mut e, false, None);
        assert_eq!(e, Exposure::default());
        switch_exposure(&mut e, true, None);
        assert_eq!(
            e,
            Exposure::Manual {
                us: DEFAULT_MANUAL.0,
                iso: DEFAULT_MANUAL.1
            }
        );
        let mut e = Exposure::default();
        switch_exposure(&mut e, true, Some((99_000, 50)));
        assert_eq!(
            e,
            Exposure::Manual {
                us: 33_000,
                iso: 100
            }
        );
        // A zero report (unknown) falls back to the default.
        let mut e = Exposure::default();
        switch_exposure(&mut e, true, Some((0, 0)));
        assert!(matches!(e, Exposure::Manual { us: 10_000, .. }));
    }

    #[test]
    fn ir_percent_round_trips() {
        for p in 0..=100 {
            let p = p as f32;
            assert!((ratio_to_percent(percent_to_ratio(p)) - p).abs() < f32::EPSILON);
        }
        assert!((percent_to_ratio(50.0) - 0.5).abs() < f32::EPSILON);
        assert!((percent_to_ratio(140.0) - 1.0).abs() < f32::EPSILON);
    }
}
