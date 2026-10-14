//! One card per sensor: enable toggle, live thumbnail, resolution and
//! frame rate, and fps / latency / drops / virtual camera counters.
//!
//! The mono cards edit the settings their camera actually uses: its
//! override when it has one, else the shared block, which moves both mono
//! cameras together (the card says so).

use egui::{Color32, Ui, vec2};
use oak_core::capture::SensorId;
use oak_core::config::{
    Config, MONO_SIZES, MonoCam, RGB_SIZES, StreamSettings, mono_max_fps, rgb_max_fps,
};
use oak_core::supervisor::SensorReport;

use super::status::{Link, Status};
use super::theme;
use super::thumbs::ThumbView;

/// Widest a card's thumbnail is drawn, points (the texture is at most 320
/// pixels wide).
pub(crate) const THUMB_MAX_W: f32 = 400.0;
/// Narrowest a card is laid out before the grid drops a column.
pub(crate) const CARD_MIN_W: f32 = 250.0;
/// A card's frame (inner margin and stroke) on both sides, points.
pub(crate) const CARD_CHROME: f32 = 16.0;

/// Columns and card content width for three cards in `avail` points with
/// `spacing` between cards: 3 across when they fit at [`CARD_MIN_W`],
/// else 2 (2 + 1), else 1 (stacked). The width always derives from
/// `avail`, capped at [`THUMB_MAX_W`].
pub(crate) fn card_layout(avail: f32, spacing: f32) -> (usize, f32) {
    let fits = |cols: f32| (avail - (cols - 1.0) * spacing) / cols - CARD_CHROME;
    let cols = [3usize, 2]
        .into_iter()
        .find(|&c| fits(c as f32) >= CARD_MIN_W)
        .unwrap_or(1);
    let width = fits(cols as f32).clamp(80.0, THUMB_MAX_W);
    (cols, width)
}

const RGB_FPS: [f32; 10] = [5.0, 10.0, 15.0, 20.0, 24.0, 25.0, 30.0, 48.0, 50.0, 60.0];
const MONO_FPS: [f32; 11] = [
    5.0, 10.0, 15.0, 20.0, 24.0, 25.0, 30.0, 45.0, 60.0, 90.0, 120.0,
];

/// How the card's sensor publishes this session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Output {
    /// To its OS virtual camera.
    Platform,
    /// To a null camera (OS side unavailable).
    Null,
    /// Not published (disabled in the config, or unsupported here).
    Off,
}

pub(crate) fn mono_cam(sensor: SensorId) -> Option<MonoCam> {
    match sensor {
        SensorId::Rgb => None,
        SensorId::Left => Some(MonoCam::Left),
        SensorId::Right => Some(MonoCam::Right),
    }
}

/// The stream settings a card edits.
pub(crate) fn stream_mut(cfg: &mut Config, sensor: SensorId) -> &mut StreamSettings {
    match mono_cam(sensor) {
        None => &mut cfg.rgb.stream,
        Some(cam) => &mut cfg.mono.editable(cam).stream,
    }
}

/// Supported sizes for `sensor`.
pub(crate) fn sizes(sensor: SensorId) -> &'static [(u32, u32)] {
    if sensor == SensorId::Rgb {
        &RGB_SIZES
    } else {
        &MONO_SIZES
    }
}

/// Frame rates offered at `size` (the current one included even when it
/// is not a standard value).
pub(crate) fn fps_choices(sensor: SensorId, size: (u32, u32), current: f32) -> Vec<f32> {
    let (table, max): (&[f32], f32) = if sensor == SensorId::Rgb {
        (&RGB_FPS, rgb_max_fps(size))
    } else {
        (&MONO_FPS, mono_max_fps(size))
    };
    let mut v: Vec<f32> = table.iter().copied().filter(|&f| f <= max).collect();
    if !v.iter().any(|&f| (f - current).abs() < 0.01) {
        v.push(current);
        v.sort_by(f32::total_cmp);
    }
    v
}

/// `30` or `7.5`.
pub(crate) fn fmt_fps(f: f32) -> String {
    if (f - f.round()).abs() < 0.01 {
        format!("{f:.0}")
    } else {
        format!("{f:.1}")
    }
}

/// The card's stats line.
pub(crate) fn stats_line(r: Option<&SensorReport>, output: Output) -> String {
    let Some(r) = r else {
        return "no stats yet".into();
    };
    let s = &r.stats;
    let vcam = match (output, r.vcam) {
        (Output::Off, _) | (_, None) => "vcam off".to_owned(),
        (Output::Null, Some((p, d))) => format!("null vcam {p}/{d}"),
        (Output::Platform, Some((p, d))) => format!("vcam {p}/{d}"),
    };
    format!(
        "{:.1} fps · p50 {:.0} ms · drops {} · {vcam}",
        s.fps, s.p50_ms, s.drops
    )
}

fn thumbnail(
    ui: &mut Ui,
    width: f32,
    view: &ThumbView<'_>,
    stream: &StreamSettings,
    note: Option<&str>,
) {
    let aspect = if stream.width > 0 {
        stream.height as f32 / stream.width as f32
    } else {
        0.5625
    };
    let size = vec2(width, (width * aspect).round());
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, Color32::from_gray(12));
    let current = view.source == (stream.width, stream.height);
    if let Some(tex) = view.texture {
        let tint = if view.stale || !current || note.is_some() {
            Color32::from_gray(90)
        } else {
            Color32::WHITE
        };
        painter.image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            tint,
        );
    }
    let overlay = note.or(if view.texture.is_none() {
        Some("waiting for frames")
    } else if view.stale {
        Some("no frames")
    } else {
        None
    });
    if let Some(text) = overlay {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(15.0),
            Color32::from_gray(220),
        );
    }
}

/// Draws the card for `sensor`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn show(
    ui: &mut Ui,
    sensor: SensorId,
    cfg: &mut Config,
    status: &Status,
    view: &ThumbView<'_>,
    output: Output,
    width: f32,
    show_stats: bool,
) {
    let shared = mono_cam(sensor).is_some_and(|c| !cfg.mono.is_overridden(c));
    theme::group_frame(ui).show(ui, |ui| {
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.horizontal(|ui| {
                let title = match sensor {
                    SensorId::Rgb => "RGB",
                    SensorId::Left => "Left",
                    SensorId::Right => "Right",
                };
                let stream = stream_mut(cfg, sensor);
                ui.checkbox(&mut stream.enabled, egui::RichText::new(title).strong())
                    .on_hover_text(if shared {
                        "Shared with the other mono camera (turn on its override in the \
                     Mono tab to set it separately)"
                    } else {
                        "Stream this sensor"
                    });
                ui.weak(sensor.camera_id().name());
                if shared {
                    theme::chip(ui, "shared", theme::IDLE).on_hover_text(
                        "Left and Right share these settings; use Override in the Mono tab to \
                     set them separately",
                    );
                }
            });
            let stream = *cfg.stream(sensor);
            let note = if !stream.enabled {
                Some("off")
            } else if status.lost[sensor.index()] {
                Some("stream lost")
            } else if status.link == Link::Stopped {
                Some("disconnected")
            } else if matches!(status.link, Link::Booting | Link::Reconnecting(_)) {
                Some("starting…")
            } else {
                None
            };
            thumbnail(ui, width, view, &stream, note);
            if show_stats {
                let line = stats_line(status.sensor(sensor), output);
                ui.add(egui::Label::new(egui::RichText::new(&line).monospace().small()).truncate())
                    .on_hover_text(line);
            }
            ui.horizontal(|ui| {
                let s = stream_mut(cfg, sensor);
                let mut size = (s.width, s.height);
                egui::ComboBox::from_id_salt(("res", sensor.name()))
                    .width(110.0)
                    .selected_text(format!("{}x{}", size.0, size.1))
                    .show_ui(ui, |ui| {
                        for &sz in sizes(sensor) {
                            ui.selectable_value(&mut size, sz, format!("{}x{}", sz.0, sz.1));
                        }
                    });
                if size != (s.width, s.height) {
                    (s.width, s.height) = size;
                    let max = if sensor == SensorId::Rgb {
                        rgb_max_fps(size)
                    } else {
                        mono_max_fps(size)
                    };
                    s.fps = s.fps.min(max);
                }
                egui::ComboBox::from_id_salt(("fps", sensor.name()))
                    .width(60.0)
                    .selected_text(format!("{} fps", fmt_fps(s.fps)))
                    .show_ui(ui, |ui| {
                        for f in fps_choices(sensor, size, s.fps) {
                            ui.selectable_value(&mut s.fps, f, fmt_fps(f));
                        }
                    });
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use oak_core::capture::StatsSnapshot;

    #[test]
    fn cards_wrap_to_the_available_width() {
        // Every layout fits: cols * (width + chrome) + gaps <= avail.
        for avail in [300.0f32, 500.0, 620.0, 800.0, 1020.0, 1540.0, 2400.0] {
            let (cols, w) = card_layout(avail, 8.0);
            let used = cols as f32 * (w + CARD_CHROME) + (cols as f32 - 1.0) * 8.0;
            assert!(used <= avail + 0.01, "{avail}: {cols} x {w} uses {used}");
        }
        assert_eq!(card_layout(1020.0, 8.0).0, 3);
        assert_eq!(card_layout(620.0, 8.0).0, 2);
        assert_eq!(card_layout(400.0, 8.0).0, 1);
        assert!((card_layout(2400.0, 8.0).1 - THUMB_MAX_W).abs() < f32::EPSILON);
    }

    #[test]
    fn fps_choices_respect_the_sensor_limits() {
        let hd = fps_choices(SensorId::Rgb, (1920, 1080), 30.0);
        assert!((hd.last().unwrap() - 60.0).abs() < f32::EPSILON);
        let uhd = fps_choices(SensorId::Rgb, (3840, 2160), 30.0);
        assert!((uhd.last().unwrap() - 30.0).abs() < f32::EPSILON);
        let mono = fps_choices(SensorId::Left, (640, 400), 30.0);
        assert!((mono.last().unwrap() - 120.0).abs() < f32::EPSILON);
        let odd = fps_choices(SensorId::Rgb, (1920, 1080), 12.5);
        assert!(odd.contains(&12.5));
        assert!(odd.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn mono_cards_edit_shared_until_overridden() {
        let mut cfg = Config::default();
        stream_mut(&mut cfg, SensorId::Left).width = 640;
        assert_eq!(cfg.stream(SensorId::Right).width, 640, "shared moves both");
        cfg.mono.set_override(MonoCam::Left, true);
        stream_mut(&mut cfg, SensorId::Left).height = 400;
        assert_eq!(cfg.stream(SensorId::Left).height, 400);
        assert_eq!(cfg.stream(SensorId::Right).height, 800);
        stream_mut(&mut cfg, SensorId::Rgb).enabled = false;
        assert!(!cfg.stream(SensorId::Rgb).enabled);
    }

    #[test]
    fn stats_lines() {
        assert_eq!(stats_line(None, Output::Platform), "no stats yet");
        let r = SensorReport {
            sensor: SensorId::Rgb,
            stats: StatsSnapshot {
                fps: 29.96,
                p50_ms: 40.2,
                drops: 1,
                ..StatsSnapshot::default()
            },
            vcam: Some((900, 2)),
        };
        assert_eq!(
            stats_line(Some(&r), Output::Platform),
            "30.0 fps · p50 40 ms · drops 1 · vcam 900/2"
        );
        assert!(stats_line(Some(&r), Output::Null).contains("null vcam 900/2"));
        assert!(stats_line(Some(&r), Output::Off).ends_with("vcam off"));
    }
}
