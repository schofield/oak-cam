//! RGB tab: the colour camera's full control set.

use egui::{Slider, Ui};
use oak_core::capture::SensorId;
use oak_core::config::{AfMode, AwbMode, Focus, KELVIN, RgbSettings, WhiteBalance};

use crate::gui::status::Status;
use crate::gui::theme;
use crate::gui::widgets;

/// Lens position a switch to manual focus starts at.
pub(crate) const DEFAULT_LENS: u8 = 128;
/// Colour temperature a switch to manual white balance starts at.
pub(crate) const DEFAULT_KELVIN: u32 = 5000;

pub(crate) const AF_MODES: [(AfMode, &str); 5] = [
    (AfMode::Auto, "Auto (on trigger)"),
    (AfMode::Macro, "Macro"),
    (AfMode::ContinuousVideo, "Continuous video"),
    (AfMode::ContinuousPicture, "Continuous picture"),
    (AfMode::Edof, "EDOF"),
];

pub(crate) const AWB_MODES: [(AwbMode, &str); 8] = [
    (AwbMode::Auto, "Auto"),
    (AwbMode::Incandescent, "Incandescent"),
    (AwbMode::Fluorescent, "Fluorescent"),
    (AwbMode::WarmFluorescent, "Warm fluorescent"),
    (AwbMode::Daylight, "Daylight"),
    (AwbMode::CloudyDaylight, "Cloudy daylight"),
    (AwbMode::Twilight, "Twilight"),
    (AwbMode::Shade, "Shade"),
];

/// Switches focus between auto (previous mode kept if it was auto) and
/// manual.
pub(crate) fn switch_focus(f: &mut Focus, manual: bool) {
    match (*f, manual) {
        (Focus::Auto(_), true) => *f = Focus::Manual(DEFAULT_LENS),
        (Focus::Manual(_), false) => *f = Focus::default(),
        _ => {}
    }
}

/// Switches white balance between auto and a fixed Kelvin value.
pub(crate) fn switch_wb(wb: &mut WhiteBalance, kelvin: bool) {
    match (*wb, kelvin) {
        (WhiteBalance::Auto { .. }, true) => *wb = WhiteBalance::Kelvin(DEFAULT_KELVIN),
        (WhiteBalance::Kelvin(_), false) => *wb = WhiteBalance::default(),
        _ => {}
    }
}

fn label_of<T: PartialEq + Copy>(table: &[(T, &'static str)], v: T) -> &'static str {
    table.iter().find(|(x, _)| *x == v).map_or("?", |(_, l)| l)
}

fn mode_row(ui: &mut Ui, label: &str, manual: &mut bool, names: (&str, &str)) -> bool {
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 18.0], egui::Label::new(label));
        let a = ui.selectable_value(manual, false, names.0);
        let b = ui.selectable_value(manual, true, names.1);
        a.changed() || b.changed()
    })
    .inner
}

/// Draws the tab; returns whether "Trigger AF" was clicked.
pub(crate) fn show(ui: &mut Ui, rgb: &mut RgbSettings, status: &Status) -> bool {
    let seed = status
        .sensor(SensorId::Rgb)
        .map(|r| (r.stats.exposure_us, r.stats.iso));
    let mut trigger = false;
    theme::section(ui, "Exposure", |ui| {
        widgets::exposure(ui, "rgb-exp", &mut rgb.exposure, seed);
        widgets::antibanding(ui, "rgb-ab", &mut rgb.antibanding);
    });
    theme::section(ui, "Focus", |ui| {
        let mut manual = matches!(rgb.focus, Focus::Manual(_));
        if mode_row(ui, "Focus", &mut manual, ("Auto", "Manual")) {
            switch_focus(&mut rgb.focus, manual);
        }
        match &mut rgb.focus {
            Focus::Auto(mode) => {
                ui.horizontal(|ui| {
                    ui.add_sized([96.0, 18.0], egui::Label::new("Mode"));
                    egui::ComboBox::from_id_salt("rgb-af")
                        .selected_text(label_of(&AF_MODES, *mode))
                        .show_ui(ui, |ui| {
                            for (v, l) in AF_MODES {
                                ui.selectable_value(mode, v, l);
                            }
                        });
                    trigger = ui
                        .button("Trigger AF")
                        .on_hover_text("Run one autofocus sweep now")
                        .clicked();
                });
            }
            Focus::Manual(lens) => {
                ui.horizontal(|ui| {
                    ui.add_sized([96.0, 18.0], egui::Label::new("Lens"));
                    ui.add(Slider::new(lens, 0..=255).text("far → near"));
                });
            }
        }
    });
    theme::section(ui, "White balance", |ui| {
        let mut kelvin = matches!(rgb.wb, WhiteBalance::Kelvin(_));
        if mode_row(ui, "White balance", &mut kelvin, ("Auto", "Kelvin")) {
            switch_wb(&mut rgb.wb, kelvin);
        }
        match &mut rgb.wb {
            WhiteBalance::Auto { mode, lock } => {
                ui.horizontal(|ui| {
                    ui.add_sized([96.0, 18.0], egui::Label::new("Mode"));
                    egui::ComboBox::from_id_salt("rgb-awb")
                        .selected_text(label_of(&AWB_MODES, *mode))
                        .show_ui(ui, |ui| {
                            for (v, l) in AWB_MODES {
                                ui.selectable_value(mode, v, l);
                            }
                        });
                    ui.checkbox(lock, "Lock");
                });
            }
            WhiteBalance::Kelvin(k) => {
                ui.horizontal(|ui| {
                    ui.add_sized([96.0, 18.0], egui::Label::new("Temperature"));
                    ui.add(Slider::new(k, KELVIN.0..=KELVIN.1).suffix(" K"));
                });
            }
        }
    });
    theme::section(ui, "Image", |ui| {
        widgets::tune(
            ui,
            &mut rgb.brightness,
            &mut rgb.contrast,
            Some(&mut rgb.saturation),
        );
        widgets::detail(ui, "Sharpness", &mut rgb.sharpness);
        widgets::detail(ui, "Luma denoise", &mut rgb.luma_denoise);
        widgets::detail(ui, "Chroma denoise", &mut rgb.chroma_denoise);
    });
    trigger
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_and_wb_switches() {
        let mut f = Focus::Auto(AfMode::Macro);
        switch_focus(&mut f, false);
        assert_eq!(f, Focus::Auto(AfMode::Macro), "same mode keeps the AF mode");
        switch_focus(&mut f, true);
        assert_eq!(f, Focus::Manual(DEFAULT_LENS));
        switch_focus(&mut f, true);
        assert_eq!(f, Focus::Manual(DEFAULT_LENS));
        switch_focus(&mut f, false);
        assert_eq!(f, Focus::default());

        let mut wb = WhiteBalance::default();
        switch_wb(&mut wb, true);
        assert_eq!(wb, WhiteBalance::Kelvin(DEFAULT_KELVIN));
        switch_wb(&mut wb, false);
        assert_eq!(wb, WhiteBalance::default());
    }

    #[test]
    fn every_mode_has_a_label() {
        for (m, l) in AF_MODES {
            assert_eq!(label_of(&AF_MODES, m), l);
        }
        assert_eq!(AWB_MODES.len(), 8);
    }
}
