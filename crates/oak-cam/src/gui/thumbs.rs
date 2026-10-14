//! Camera thumbnails: each sensor's [`FrameTap`] copy converted to a
//! texture, at most once per new thumbnail (the capture thread keeps at
//! most 10 per second). NV12 is converted with BT.601 video-range integer
//! math; mono thumbnails show luma as grey.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use oak_core::capture::SensorId;
use oak_core::thumb::{FrameTaps, Thumb, ThumbFormat};

/// A thumbnail older than this is shown as stale.
const STALE_AFTER: Duration = Duration::from_millis(1500);

fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// BT.601 video range `Y'CbCr` to RGB.
#[allow(clippy::many_single_char_names)] // the textbook names
pub(crate) fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let c = 298 * (i32::from(y) - 16);
    let d = i32::from(u) - 128;
    let e = i32::from(v) - 128;
    [
        clamp8((c + 409 * e + 128) >> 8),
        clamp8((c - 100 * d - 208 * e + 128) >> 8),
        clamp8((c + 516 * d + 128) >> 8),
    ]
}

/// Converts `t` into `out` (cleared first), one opaque pixel per thumbnail
/// pixel.
#[allow(clippy::many_single_char_names)]
pub(crate) fn thumb_to_rgba(t: &Thumb, out: &mut Vec<Color32>) {
    out.clear();
    out.reserve(t.y.len());
    match t.format {
        ThumbFormat::Gray => out.extend(t.y.iter().map(|&y| Color32::from_gray(y))),
        ThumbFormat::Yuv => {
            for (i, &y) in t.y.iter().enumerate() {
                let (u, v) = (
                    t.uv.get(2 * i).copied().unwrap_or(128),
                    t.uv.get(2 * i + 1).copied().unwrap_or(128),
                );
                let [r, g, b] = yuv_to_rgb(y, u, v);
                out.push(Color32::from_rgb(r, g, b));
            }
        }
    }
}

/// One sensor's texture.
#[derive(Default)]
struct Slot {
    texture: Option<TextureHandle>,
    version: u64,
    updated: Option<Instant>,
    source: (u32, u32),
}

/// The three sensors' textures, refreshed from the taps.
pub(crate) struct Thumbs {
    taps: Arc<FrameTaps>,
    slots: [Slot; 3],
    scratch: Vec<Color32>,
}

/// What a card draws.
pub(crate) struct ThumbView<'a> {
    /// The texture, if any thumbnail arrived yet.
    pub texture: Option<&'a TextureHandle>,
    /// Size of the frames it came from.
    pub source: (u32, u32),
    /// No new thumbnail for a while.
    pub stale: bool,
}

impl Thumbs {
    pub(crate) fn new(taps: Arc<FrameTaps>) -> Self {
        Self {
            taps,
            slots: Default::default(),
            scratch: Vec::new(),
        }
    }

    /// Uploads every new thumbnail.
    pub(crate) fn refresh(&mut self, ctx: &egui::Context) {
        for sensor in SensorId::ALL {
            let tap = self.taps.get(sensor);
            let slot = &mut self.slots[sensor.index()];
            let version = tap.version();
            if version == slot.version {
                continue;
            }
            let (size, source) = {
                let t = tap.lock();
                thumb_to_rgba(&t, &mut self.scratch);
                ([t.width as usize, t.height as usize], t.source)
            };
            slot.version = version;
            if size[0] * size[1] != self.scratch.len() || self.scratch.is_empty() {
                continue;
            }
            let image = ColorImage::new(size, std::mem::take(&mut self.scratch));
            match &mut slot.texture {
                Some(tex) => tex.set(image, TextureOptions::LINEAR),
                None => {
                    slot.texture = Some(ctx.load_texture(
                        format!("thumb-{sensor}"),
                        image,
                        TextureOptions::LINEAR,
                    ));
                }
            }
            slot.source = source;
            slot.updated = Some(Instant::now());
        }
    }

    /// `sensor`'s thumbnail.
    pub(crate) fn view(&self, sensor: SensorId) -> ThumbView<'_> {
        let slot = &self.slots[sensor.index()];
        ThumbView {
            texture: slot.texture.as_ref(),
            source: slot.source,
            stale: slot.updated.is_none_or(|t| t.elapsed() > STALE_AFTER),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_range_extremes_and_primaries() {
        assert_eq!(yuv_to_rgb(16, 128, 128), [0, 0, 0]);
        assert_eq!(yuv_to_rgb(235, 128, 128), [255, 255, 255]);
        assert_eq!(yuv_to_rgb(126, 128, 128), [128, 128, 128]);
        // BT.601 video-range red, green, blue.
        let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 2);
        assert!(near(yuv_to_rgb(81, 90, 240), [255, 0, 0]));
        assert!(near(yuv_to_rgb(145, 54, 34), [0, 255, 0]));
        assert!(near(yuv_to_rgb(41, 240, 110), [0, 0, 255]));
    }

    /// NV12 frame -> decimated thumb (capture side) -> RGBA (GUI side).
    #[test]
    #[allow(clippy::many_single_char_names)]
    fn nv12_to_rgba_through_decimation() {
        // 8x4 NV12: left half red, right half blue (by 2x2 chroma blocks).
        let (w, h) = (8usize, 4usize);
        let mut y = vec![0u8; w * h];
        let mut uv = vec![0u8; w * h / 2];
        for r in 0..h {
            for c in 0..w {
                y[r * w + c] = if c < 4 { 81 } else { 41 };
            }
        }
        for r in 0..h / 2 {
            for b in 0..w / 2 {
                let (u, v) = if b < 2 { (90, 240) } else { (240, 110) };
                uv[r * w + 2 * b] = u;
                uv[r * w + 2 * b + 1] = v;
            }
        }
        let mut t = Thumb::default();
        t.fill_nv12(&y, w, &uv, w, w as u32, h as u32, 2);
        let mut px = Vec::new();
        thumb_to_rgba(&t, &mut px);
        assert_eq!(px.len(), 4 * 2);
        for row in px.chunks(4) {
            assert!(row[0].r() > 250 && row[0].b() < 5, "{:?}", row[0]);
            assert!(row[1].r() > 250);
            assert!(row[2].b() > 250 && row[2].r() < 5, "{:?}", row[2]);
            assert_eq!(row[0].a(), 255);
        }
    }

    #[test]
    fn gray_thumbs_show_luma() {
        let mut t = Thumb::default();
        t.fill_gray(&[0, 50, 100, 200], 2, 2, 2, 1);
        let mut px = vec![Color32::RED];
        thumb_to_rgba(&t, &mut px);
        assert_eq!(px, [0, 50, 100, 200].map(Color32::from_gray).to_vec());
    }
}
