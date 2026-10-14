//! Mono tab: the shared settings both OV9282s use, plus "Override Left" /
//! "Override Right" toggles that give one camera its own section. Turning
//! an override on starts it from a copy of the shared settings (nothing
//! changes on the device); turning it off returns that camera to shared.

use egui::Ui;
use oak_core::capture::SensorId;
use oak_core::config::{MonoCam, MonoGroup, MonoSettings};

use crate::gui::status::Status;
use crate::gui::theme;
use crate::gui::widgets;

/// Whose settings a section edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Shared,
    Own(MonoCam),
}

/// The sections to draw, in order, with their titles.
pub(crate) fn sections(g: &MonoGroup) -> Vec<(String, Target)> {
    let users: Vec<&str> = MonoCam::ALL
        .into_iter()
        .filter(|&c| !g.is_overridden(c))
        .map(cam_name)
        .collect();
    let shared = if users.is_empty() {
        "Shared (not used: both cameras overridden)".to_owned()
    } else {
        format!("Shared ({})", users.join(" + "))
    };
    let mut out = vec![(shared, Target::Shared)];
    for cam in MonoCam::ALL {
        if g.is_overridden(cam) {
            out.push((format!("{} override", cam_name(cam)), Target::Own(cam)));
        }
    }
    out
}

pub(crate) fn cam_name(cam: MonoCam) -> &'static str {
    match cam {
        MonoCam::Left => "Left",
        MonoCam::Right => "Right",
    }
}

fn settings_mut(g: &mut MonoGroup, t: Target) -> &mut MonoSettings {
    match t {
        Target::Shared => &mut g.shared,
        Target::Own(cam) => g.editable(cam),
    }
}

/// One section's controls (the stream size / fps / enable live on the
/// camera cards).
fn controls(ui: &mut Ui, id: &str, m: &mut MonoSettings, seed: Option<(u32, u32)>) {
    widgets::exposure(ui, &format!("{id}-exp"), &mut m.exposure, seed);
    widgets::antibanding(ui, &format!("{id}-ab"), &mut m.antibanding);
    widgets::tune(ui, &mut m.brightness, &mut m.contrast, None);
    widgets::detail(ui, "Sharpness", &mut m.sharpness);
    widgets::detail(ui, "Luma denoise", &mut m.luma_denoise);
}

fn seed(status: &Status, sensor: SensorId) -> Option<(u32, u32)> {
    status
        .sensor(sensor)
        .map(|r| (r.stats.exposure_us, r.stats.iso))
}

/// Draws the tab.
pub(crate) fn show(ui: &mut Ui, g: &mut MonoGroup, status: &Status) {
    ui.horizontal(|ui| {
        for cam in MonoCam::ALL {
            let mut on = g.is_overridden(cam);
            if ui
                .checkbox(&mut on, format!("Override {}", cam_name(cam)))
                .on_hover_text(format!(
                    "Give the {} camera its own settings (starting from a copy of the \
                     shared ones)",
                    cam_name(cam).to_lowercase()
                ))
                .changed()
            {
                g.set_override(cam, on);
            }
        }
    });
    for (title, target) in sections(g) {
        let (id, s) = match target {
            Target::Shared => ("mono-shared", seed(status, SensorId::Left)),
            Target::Own(MonoCam::Left) => ("mono-left", seed(status, SensorId::Left)),
            Target::Own(MonoCam::Right) => ("mono-right", seed(status, SensorId::Right)),
        };
        theme::section(ui, &title, |ui| {
            controls(ui, id, settings_mut(g, target), s);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oak_core::config::{Config, Exposure};

    #[test]
    fn sections_follow_the_override_toggles() {
        let mut g = MonoGroup::default();
        assert_eq!(
            sections(&g),
            [("Shared (Left + Right)".to_owned(), Target::Shared)]
        );
        g.set_override(MonoCam::Right, true);
        assert_eq!(
            sections(&g),
            [
                ("Shared (Left)".to_owned(), Target::Shared),
                ("Right override".to_owned(), Target::Own(MonoCam::Right)),
            ]
        );
        g.set_override(MonoCam::Left, true);
        assert_eq!(sections(&g).len(), 3);
        assert!(sections(&g)[0].0.contains("not used"));
    }

    /// Override on copies shared (no effective change), edits then diverge,
    /// and off returns the camera to shared.
    #[test]
    fn override_toggle_behaviour() {
        let mut cfg = Config::default();
        cfg.mono.shared.brightness = 2;
        cfg.mono.set_override(MonoCam::Left, true);
        assert_eq!(cfg.mono.left, Some(cfg.mono.shared));
        let before = *cfg.mono.effective(MonoCam::Left);
        // Editing the override leaves shared (and Right) alone.
        settings_mut(&mut cfg.mono, Target::Own(MonoCam::Left)).exposure =
            Exposure::Manual { us: 2000, iso: 400 };
        assert_eq!(cfg.mono.shared.exposure, Exposure::default());
        assert_eq!(
            cfg.mono.effective(MonoCam::Right).exposure,
            Exposure::default()
        );
        assert_ne!(*cfg.mono.effective(MonoCam::Left), before);
        // Editing shared no longer reaches Left.
        settings_mut(&mut cfg.mono, Target::Shared).contrast = 5;
        assert_eq!(cfg.mono.effective(MonoCam::Left).contrast, 0);
        assert_eq!(cfg.mono.effective(MonoCam::Right).contrast, 5);
        // Off: back to shared.
        cfg.mono.set_override(MonoCam::Left, false);
        assert_eq!(cfg.mono.left, None);
        assert_eq!(cfg.mono.effective(MonoCam::Left).contrast, 5);
    }
}
