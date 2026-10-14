//! UI theme presets (ported from stream-concern's `src/app/theme.rs`: the
//! same four presets, names and palettes) and the shared drawing helpers.
//!
//! A [`ThemePreset`] (stored in the config as `[ui] theme`) maps to a pair
//! of complete [`egui::Style`]s, one per system appearance, built from
//! egui's own dark or light base by [`style_for`]. [`sync`] installs them
//! on the context **only when the preset changes** (startup and a switch);
//! the draw loop never rebuilds a style. `classic` and `mac-native` follow
//! the macOS light/dark setting ([`ThemePreference::System`]); `flat-dark`
//! and `soft-light` are single-appearance.
//!
//! Grouping is part of a preset too: [`group_frame`] and [`section`] read
//! the active preset's [`Grouping`] from context memory, so panels change
//! their grouping without per-panel special cases.

use std::sync::Arc;

use egui::{Color32, CornerRadius, Shadow, Stroke, Style, ThemePreference, Visuals, vec2};

pub(crate) use oak_core::config::ThemePreset;

/// The macOS system accent blue in dark appearance.
pub(crate) const MAC_ACCENT_DARK: Color32 = Color32::from_rgb(0x0A, 0x84, 0xFF);
/// The macOS system accent blue in light appearance.
pub(crate) const MAC_ACCENT_LIGHT: Color32 = Color32::from_rgb(0x00, 0x7A, 0xFF);

/// The flat-dark focus-ring accent (egui's dark hyperlink blue).
const FLAT_DARK_ACCENT: Color32 = Color32::from_rgb(90, 170, 255);

/// Good / warning / error colours for chips and badges.
pub(crate) const OK: Color32 = Color32::from_rgb(0x30, 0xB0, 0x50);
pub(crate) const WARN: Color32 = Color32::from_rgb(0xE0, 0x9A, 0x10);
pub(crate) const ERROR: Color32 = Color32::from_rgb(0xE0, 0x40, 0x40);
pub(crate) const IDLE: Color32 = Color32::from_gray(128);

/// Slider length the side panel is laid out for.
const SLIDER_WIDTH: f32 = 170.0;

/// How a preset groups related controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Grouping {
    /// Wrap sections in group frames. Off for `classic`, which draws a
    /// section as a bold title over its controls.
    pub sections: bool,
    /// Fill group frames with `visuals.extreme_bg_color` instead of drawing
    /// an outline only.
    pub filled: bool,
}

/// The grouping a preset asks for.
#[must_use]
pub(crate) fn grouping(preset: ThemePreset) -> Grouping {
    match preset {
        ThemePreset::Classic => Grouping {
            sections: false,
            filled: false,
        },
        ThemePreset::MacNative | ThemePreset::FlatDark | ThemePreset::SoftLight => Grouping {
            sections: true,
            filled: true,
        },
    }
}

/// Which system appearance(s) a preset follows. A preset with both a dark
/// and a light variant follows the system setting.
#[must_use]
pub(crate) fn theme_preference(preset: ThemePreset) -> ThemePreference {
    match preset {
        ThemePreset::Classic | ThemePreset::MacNative => ThemePreference::System,
        ThemePreset::FlatDark => ThemePreference::Dark,
        ThemePreset::SoftLight => ThemePreference::Light,
    }
}

/// Build the complete style for `preset` in dark (`dark = true`) or light
/// appearance. A single-appearance preset (`flat-dark`, `soft-light`)
/// returns its one style whatever `dark` says.
#[must_use]
pub(crate) fn style_for(preset: ThemePreset, dark: bool) -> Style {
    match preset {
        ThemePreset::Classic => classic(dark),
        ThemePreset::MacNative => mac_native(dark),
        ThemePreset::FlatDark => flat_dark(),
        ThemePreset::SoftLight => soft_light(),
    }
}

/// Intent: egui's stock look, untouched.
fn classic(dark: bool) -> Style {
    egui::Theme::from_dark_mode(dark).default_style()
}

/// Intent: feel at home next to native macOS apps: pill-shaped controls,
/// filled rounded groups, roomier rows and the system accent blue, in both
/// the system's light and dark appearance.
fn mac_native(dark: bool) -> Style {
    let mut style = Style {
        visuals: if dark {
            Visuals::dark()
        } else {
            Visuals::light()
        },
        ..Style::default()
    };
    let v = &mut style.visuals;
    let accent = if dark {
        MAC_ACCENT_DARK
    } else {
        MAC_ACCENT_LIGHT
    };

    for w in [
        &mut v.widgets.active,
        &mut v.widgets.hovered,
        &mut v.widgets.inactive,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(30);
        w.bg_stroke = Stroke::NONE;
    }
    v.widgets.noninteractive.corner_radius = CornerRadius::same(8);
    style.spacing.button_padding = vec2(5.0, 2.5);

    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(10);
    let (window, panel, outline) = if dark { (30, 38, 52) } else { (240, 248, 218) };
    v.window_fill = Color32::from_gray(window);
    v.panel_fill = Color32::from_gray(panel);
    v.widgets.noninteractive.bg_fill = v.window_fill;
    v.widgets.noninteractive.weak_bg_fill = v.window_fill;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_gray(outline));
    // Text fields sit one step off the filled group they live in, with a
    // faint resting outline and a 1 px accent ring when focused or pressed.
    v.text_edit_bg_color = Some(Color32::from_gray(if dark { 22 } else { 250 }));
    let (rest, hover) = if dark { (58, 80) } else { (208, 180) };
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_gray(rest));
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_gray(hover));
    v.widgets.active.bg_stroke = Stroke::new(1.0, accent);
    v.selection.bg_fill = accent;
    // `selection.stroke` colours both selected text (on the accent) and the
    // focused text-field outline (on the field): white in dark, black in
    // light (white vanishes on a white field).
    v.selection.stroke = Stroke::new(1.0, if dark { Color32::WHITE } else { Color32::BLACK });
    v.faint_bg_color = if dark {
        Color32::from_additive_luminance(10)
    } else {
        Color32::from_black_alpha(10)
    };
    v.hyperlink_color = accent;
    v.slider_trailing_fill = true;
    style.spacing.item_spacing = vec2(8.0, 6.0);
    style.spacing.interact_size.y = 24.0;
    style
}

/// Intent: a dark "studio console": square-ish controls, no widget
/// outlines, brighter text on deeper greys and tight spacing.
fn flat_dark() -> Style {
    let mut style = Style::default();
    let v = &mut style.visuals;
    let radius = CornerRadius::same(4);
    for (w, fill, text) in [
        (&mut v.widgets.noninteractive, 22, 215),
        (&mut v.widgets.inactive, 44, 235),
        (&mut v.widgets.hovered, 58, 255),
        (&mut v.widgets.active, 72, 255),
        (&mut v.widgets.open, 36, 235),
    ] {
        w.corner_radius = radius;
        w.bg_fill = Color32::from_gray(fill);
        w.weak_bg_fill = Color32::from_gray(fill);
        w.bg_stroke = Stroke::NONE;
        w.fg_stroke.color = Color32::from_gray(text);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_gray(40));
    v.text_edit_bg_color = Some(Color32::from_gray(20));
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_gray(50));
    v.widgets.active.bg_stroke = Stroke::new(1.0, FLAT_DARK_ACCENT);
    v.window_corner_radius = radius;
    v.menu_corner_radius = radius;
    v.window_fill = Color32::from_gray(22);
    v.panel_fill = Color32::from_gray(18);
    v.extreme_bg_color = Color32::from_gray(8);
    v.faint_bg_color = Color32::from_additive_luminance(8);
    v.window_stroke = Stroke::new(1.0, Color32::from_gray(40));
    v.window_shadow = Shadow {
        offset: [0, 4],
        blur: 10,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    v.popup_shadow = Shadow {
        offset: [0, 2],
        blur: 6,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    v.weak_text_alpha = 0.7;
    style.spacing.item_spacing = vec2(6.0, 2.0);
    style.spacing.button_padding = vec2(4.0, 1.0);
    style.spacing.window_margin = egui::Margin::same(4);
    style.spacing.menu_margin = egui::Margin::same(4);
    style
}

/// Intent: a calm light workspace: generously rounded controls, soft
/// diffuse shadows, white groups on a pale panel and generous spacing.
fn soft_light() -> Style {
    let mut style = Style {
        visuals: Visuals::light(),
        ..Style::default()
    };
    let v = &mut style.visuals;
    let radius = CornerRadius::same(12);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = radius;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_gray(222));
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_gray(215));
    v.widgets.inactive.bg_fill = Color32::from_gray(236);
    v.widgets.inactive.weak_bg_fill = Color32::from_gray(236);
    v.window_corner_radius = radius;
    v.menu_corner_radius = radius;
    v.window_fill = Color32::WHITE;
    v.panel_fill = Color32::from_rgb(245, 245, 247);
    v.widgets.noninteractive.bg_fill = v.window_fill;
    v.widgets.noninteractive.weak_bg_fill = v.window_fill;
    v.window_stroke = Stroke::new(1.0, Color32::from_gray(222));
    v.faint_bg_color = Color32::from_rgb(240, 240, 244);
    v.text_edit_bg_color = Some(Color32::from_rgb(248, 248, 250));
    // One step stronger than a pastel so the slider trailing fill (drawn in
    // the selection colour) still reads on the pale rail.
    v.selection.bg_fill = Color32::from_rgb(160, 200, 255);
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(22),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(20),
    };
    v.slider_trailing_fill = true;
    style.spacing.item_spacing = vec2(10.0, 8.0);
    style.spacing.button_padding = vec2(10.0, 5.0);
    style.spacing.interact_size.y = 28.0;
    style.spacing.window_margin = egui::Margin::same(12);
    style.spacing.menu_margin = egui::Margin::same(10);
    style.spacing.indent = 22.0;
    style
}

fn grouping_id() -> egui::Id {
    egui::Id::new("oak-theme-grouping")
}

/// Install `preset` on `ctx`: both appearance styles, the theme preference
/// and the preset's grouping. Call only when the preset changes.
pub(crate) fn apply(ctx: &egui::Context, preset: ThemePreset) {
    // oak-cam's side panel is laid out for 170 px sliders under every preset.
    let build = |dark| {
        let mut s = style_for(preset, dark);
        s.spacing.slider_width = SLIDER_WIDTH;
        Arc::new(s)
    };
    let dark = build(true);
    let light = build(false);
    ctx.options_mut(|o| {
        o.dark_style = dark;
        o.light_style = light;
        o.theme_preference = theme_preference(preset);
    });
    ctx.data_mut(|d| d.insert_temp(grouping_id(), grouping(preset)));
}

/// Apply `wanted` if it differs from `applied`, recording it. Returns
/// whether the style changed. Allocation-free when nothing changed, so it
/// runs every frame.
pub(crate) fn sync(
    ctx: &egui::Context,
    applied: &mut Option<ThemePreset>,
    wanted: ThemePreset,
) -> bool {
    if *applied == Some(wanted) {
        return false;
    }
    apply(ctx, wanted);
    *applied = Some(wanted);
    true
}

fn current_grouping(ui: &egui::Ui) -> Grouping {
    ui.ctx()
        .data(|d| d.get_temp::<Grouping>(grouping_id()))
        .unwrap_or_default()
}

/// The frame for a group of related controls under the active preset: a
/// group filled with `extreme_bg_color` when the preset asks for filled
/// groups, a plain [`egui::Frame::group`] otherwise.
pub(crate) fn group_frame(ui: &egui::Ui) -> egui::Frame {
    let frame = egui::Frame::group(ui.style());
    if current_grouping(ui).filled {
        frame.fill(ui.visuals().extreme_bg_color)
    } else {
        frame
    }
}

/// A titled group spanning the available width: inside [`group_frame`]
/// when the preset groups sections, else a bold title over the controls.
pub(crate) fn section<R>(
    ui: &mut egui::Ui,
    title: &str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    if !current_grouping(ui).sections {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(title).strong());
        let r = add(ui);
        ui.add_space(4.0);
        return r;
    }
    group_frame(ui)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new(title).strong());
            add(ui)
        })
        .inner
}

/// A small coloured pill with `text`.
pub(crate) fn chip(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.22))
        .stroke(Stroke::new(1.0, color))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| ui.colored_label(color, text))
        .response
}

/// The theme choices as radio items, for the View menu and Settings.
/// Returns the newly chosen preset, if any.
pub(crate) fn theme_choices(ui: &mut egui::Ui, current: ThemePreset) -> Option<ThemePreset> {
    let mut chosen = None;
    for preset in ThemePreset::ALL {
        if ui.radio(current == preset, preset.label()).clicked() && current != preset {
            chosen = Some(preset);
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Style`'s `PartialEq` compares its number formatter by pointer; align
    /// that one field so the comparison covers everything else.
    fn same_style(mut a: Style, b: &Style) -> bool {
        a.number_formatter = b.number_formatter.clone();
        a == *b
    }

    #[test]
    fn classic_is_egui_default_in_both_appearances() {
        let fresh = egui::Options::default();
        assert!(same_style(
            style_for(ThemePreset::Classic, true),
            &fresh.dark_style
        ));
        assert!(same_style(
            style_for(ThemePreset::Classic, false),
            &fresh.light_style
        ));
        assert_eq!(
            theme_preference(ThemePreset::Classic),
            fresh.theme_preference
        );
    }

    #[test]
    fn mac_native_has_the_stream_concern_values() {
        for dark in [true, false] {
            let s = style_for(ThemePreset::MacNative, dark);
            let v = &s.visuals;
            for w in [
                &v.widgets.active,
                &v.widgets.hovered,
                &v.widgets.inactive,
                &v.widgets.open,
            ] {
                assert_eq!(w.corner_radius, CornerRadius::same(30));
            }
            assert_eq!(s.spacing.button_padding, vec2(5.0, 2.5));
            assert_eq!(v.dark_mode, dark);
            let accent = if dark {
                MAC_ACCENT_DARK
            } else {
                MAC_ACCENT_LIGHT
            };
            assert_eq!(v.selection.bg_fill, accent);
            assert_eq!(v.widgets.active.bg_stroke, Stroke::new(1.0, accent));
            assert!(v.panel_fill.r() > v.window_fill.r());
        }
        assert_eq!(
            theme_preference(ThemePreset::MacNative),
            ThemePreference::System
        );
    }

    #[test]
    fn single_appearance_presets() {
        let flat = style_for(ThemePreset::FlatDark, false);
        assert!(flat.visuals.dark_mode);
        assert_eq!(
            flat.visuals.widgets.inactive.corner_radius,
            CornerRadius::same(4)
        );
        let soft = style_for(ThemePreset::SoftLight, true);
        assert!(!soft.visuals.dark_mode);
        assert_eq!(soft.visuals.window_corner_radius, CornerRadius::same(12));
        assert_eq!(
            theme_preference(ThemePreset::FlatDark),
            ThemePreference::Dark
        );
        assert_eq!(
            theme_preference(ThemePreset::SoftLight),
            ThemePreference::Light
        );
    }

    /// Every preset keeps a keyboard focus ring (egui draws focus with
    /// `widgets.active`).
    #[test]
    fn every_preset_keeps_a_focus_ring() {
        for preset in ThemePreset::ALL {
            for dark in [true, false] {
                let v = style_for(preset, dark).visuals;
                assert!(v.widgets.active.bg_stroke.width > 0.0, "{preset:?}");
            }
        }
    }

    #[test]
    fn sync_applies_only_on_change_and_group_frame_follows() {
        let ctx = egui::Context::default();
        let mut applied = None;
        for preset in ThemePreset::ALL {
            assert!(sync(&ctx, &mut applied, preset));
            assert!(!sync(&ctx, &mut applied, preset));
            assert_eq!(
                ctx.options(|o| o.theme_preference),
                theme_preference(preset)
            );
            let out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let frame = group_frame(ui);
                if grouping(preset).filled {
                    assert_eq!(frame.fill, ui.visuals().extreme_bg_color);
                } else {
                    assert_eq!(frame.fill, Color32::TRANSPARENT);
                }
            });
            out.drop_without_applying_deltas();
        }
    }

    /// Every preset appears among the theme radios, under its label.
    #[test]
    fn every_preset_appears_in_the_theme_choices() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = theme_choices(ui, ThemePreset::MacNative);
        });
        let update = out
            .platform_output
            .accesskit_update
            .clone()
            .expect("accesskit update");
        out.drop_without_applying_deltas();
        let radios: Vec<String> = update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == egui::accesskit::Role::RadioButton)
            .map(|(_, n)| n.label().unwrap_or_default().to_string())
            .collect();
        for preset in ThemePreset::ALL {
            assert!(radios.iter().any(|l| l == preset.label()), "{radios:?}");
        }
    }
}
