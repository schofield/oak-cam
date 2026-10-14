//! `--dump-frames N --sensor rgb|left|right --out DIR`: capture N frames
//! from one sensor and write each as a tightly packed raw file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use depthai::{FrameView, ImgType, PipelineBuilder};
use oak_core::capture::SensorId;

use crate::cmd_headless::open_device;

/// Options for [`run`].
#[derive(Debug)]
pub struct DumpArgs<'a> {
    /// Number of frames to write.
    pub count: u32,
    /// Which sensor.
    pub sensor: SensorId,
    /// Output directory (created if missing).
    pub out_dir: &'a Path,
    /// Output width override.
    pub width: Option<u32>,
    /// Output height override.
    pub height: Option<u32>,
    /// Frame rate override.
    pub fps: Option<f32>,
    /// Device id or IP; `None` for the first found.
    pub device: Option<&'a str>,
}

/// How long to wait for any one frame before giving up.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// File extension and ffplay `-pixel_format` for a pixel format.
fn format_names(t: ImgType) -> Result<(&'static str, &'static str)> {
    match t {
        ImgType::Nv12 => Ok(("nv12", "nv12")),
        ImgType::Gray8 => Ok(("gray8", "gray")),
        other => bail!("cannot dump {other} frames"),
    }
}

/// `DIR/<sensor>_<index:04>.<ext>`.
fn frame_path(dir: &Path, sensor: SensorId, index: u32, ext: &str) -> PathBuf {
    dir.join(format!("{}_{index:04}.{ext}", sensor.name()))
}

/// Runs the dump, printing one line per frame and a summary to `out`.
pub fn run(out: &mut impl Write, args: &DumpArgs<'_>) -> Result<()> {
    if args.count == 0 {
        bail!("--dump-frames must be at least 1");
    }
    let mut spec = args.sensor.default_output();
    if let Some(w) = args.width {
        spec.width = w;
    }
    if let Some(h) = args.height {
        spec.height = h;
    }
    if let Some(f) = args.fps {
        spec.fps = Some(f);
    }
    let (ext, pix_fmt) = format_names(spec.img_type)?;
    std::fs::create_dir_all(args.out_dir)
        .with_context(|| format!("cannot create {}", args.out_dir.display()))?;

    let dev = open_device(args.device)?;
    let builder = PipelineBuilder::new(&dev)?;
    let cam = builder
        .camera(args.sensor.socket(), None)
        .with_context(|| format!("cannot add camera {}", args.sensor.socket()))?;
    // A deeper queue than the live path: file writes must not drop frames.
    let queue = cam
        .request_output(spec)
        .with_context(|| {
            format!(
                "cannot request {}x{} {} from {}",
                spec.width,
                spec.height,
                spec.img_type,
                args.sensor.socket()
            )
        })?
        .create_queue(8, false)?;
    let pipeline = builder.start().context("cannot start pipeline")?;

    writeln!(
        out,
        "dumping {} {} frames ({}x{} {} @ {} fps) to {}",
        args.count,
        args.sensor,
        spec.width,
        spec.height,
        spec.img_type,
        spec.fps.unwrap_or(0.0),
        args.out_dir.display()
    )?;
    out.flush()?;

    let mut buf = Vec::new();
    let mut summary = Summary::default();
    for i in 0..args.count {
        let frame = queue
            .get(FRAME_TIMEOUT)?
            .with_context(|| format!("no frame within {FRAME_TIMEOUT:?} (frame {i})"))?;
        let now = depthai::steady_now_ns();
        let v = frame.view();
        let lat_ms = (now - v.ts_host_ns()) as f64 / 1e6;
        let size = v.copy_packed(&mut buf).with_context(|| {
            format!(
                "frame {i}: unexpected layout {}x{} {} stride {}",
                v.width(),
                v.height(),
                v.img_type(),
                v.stride()
            )
        })?;
        let path = frame_path(args.out_dir, args.sensor, i, ext);
        std::fs::write(&path, &buf).with_context(|| format!("cannot write {}", path.display()))?;
        writeln!(
            out,
            "{}  seq {:>5}  {}x{} {}  {size} B  ts {:.6}s  lat {lat_ms:.1} ms  exp {} us  iso {}",
            path.display(),
            v.seq(),
            v.width(),
            v.height(),
            v.img_type(),
            v.ts_host_ns() as f64 / 1e9,
            v.exposure_us(),
            v.iso(),
        )?;
        summary.add(&v, lat_ms, size, path);
    }
    drop(pipeline);
    summary.write(out, pix_fmt)
}

/// Per-run totals for the summary line.
#[derive(Debug, Default)]
struct Summary {
    latencies_ms: Vec<f64>,
    first: Option<(i64, i64, PathBuf, u32, u32)>, // seq, ts, path, w, h
    last_seq: i64,
    last_ts: i64,
    drops: i64,
    size: usize,
}

impl Summary {
    fn add(&mut self, v: &FrameView<'_>, lat_ms: f64, size: usize, path: PathBuf) {
        if self.first.is_none() {
            self.first = Some((v.seq(), v.ts_host_ns(), path, v.width(), v.height()));
        } else {
            self.drops += (v.seq() - self.last_seq - 1).max(0);
        }
        self.last_seq = v.seq();
        self.last_ts = v.ts_host_ns();
        self.latencies_ms.push(lat_ms);
        self.size = size;
    }

    fn write(mut self, out: &mut impl Write, pix_fmt: &str) -> Result<()> {
        let Some((first_seq, first_ts, path, w, h)) = self.first else {
            return Ok(());
        };
        let n = self.latencies_ms.len();
        self.latencies_ms.sort_by(f64::total_cmp);
        let span_s = (self.last_ts - first_ts) as f64 / 1e9;
        let fps = if span_s > 0.0 {
            (n - 1) as f64 / span_s
        } else {
            0.0
        };
        writeln!(
            out,
            "summary: {n} frames, {} B each, seq {first_seq}..{}, {} dropped, {fps:.1} fps, \
             latency p50 {:.1} ms max {:.1} ms",
            self.size,
            self.last_seq,
            self.drops,
            self.latencies_ms[n / 2],
            self.latencies_ms[n - 1],
        )?;
        writeln!(
            out,
            "play: ffplay -f rawvideo -pixel_format {pix_fmt} -video_size {w}x{h} {}",
            path.display()
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_paths() {
        assert_eq!(format_names(ImgType::Nv12).unwrap(), ("nv12", "nv12"));
        assert_eq!(format_names(ImgType::Gray8).unwrap(), ("gray8", "gray"));
        assert!(format_names(ImgType::Rgb888i).is_err());
        assert_eq!(
            frame_path(Path::new("/tmp/x"), SensorId::Left, 7, "gray8"),
            Path::new("/tmp/x/left_0007.gray8")
        );
    }
}
