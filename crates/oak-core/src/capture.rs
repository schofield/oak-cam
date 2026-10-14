//! Per-sensor capture threads and their statistics.
//!
//! One thread per enabled sensor owns its [`OutputQueue`], does a blocking
//! `get(100 ms)`, records stats (fps EWMA, capture→host latency, seq-gap
//! drops, the sensor's reported exposure) and hands each frame to a
//! [`FrameSink`]: [`NullSink`] with `--no-vcam`, otherwise
//! [`crate::vcam_sink::VcamSink`].
//!
//! The thread hands its sink back when it ends ([`CaptureHandle`]), so a
//! thread that exits on a lost stream does not drop (and so stop) its
//! virtual camera: the camera keeps showing its last frame until the owner
//! joins the thread at shutdown.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use depthai::{FrameView, ImgType, OutputQueue, OutputSpec, ResizeMode, Socket};

use crate::thumb::FrameTap;

/// Which of the three OAK-D Pro sensors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SensorId {
    /// `CAM_A`, IMX378 colour.
    Rgb,
    /// `CAM_B`, OV9282 mono.
    Left,
    /// `CAM_C`, OV9282 mono.
    Right,
}

impl SensorId {
    /// All sensors, in display order.
    pub const ALL: [Self; 3] = [Self::Rgb, Self::Left, Self::Right];

    /// Index into per-sensor arrays (`ALL` order).
    pub fn index(self) -> usize {
        match self {
            Self::Rgb => 0,
            Self::Left => 1,
            Self::Right => 2,
        }
    }

    /// Lower-case name used in output and file names.
    pub fn name(self) -> &'static str {
        match self {
            Self::Rgb => "rgb",
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    /// Board socket.
    pub fn socket(self) -> Socket {
        match self {
            Self::Rgb => Socket::CamA,
            Self::Left => Socket::CamB,
            Self::Right => Socket::CamC,
        }
    }

    /// The virtual camera this sensor publishes to.
    pub fn camera_id(self) -> vcam::CameraId {
        match self {
            Self::Rgb => vcam::CameraId::Rgb,
            Self::Left => vcam::CameraId::Left,
            Self::Right => vcam::CameraId::Right,
        }
    }

    /// Default output: RGB NV12 1920x1080@30, mono GRAY8 1280x800@30.
    pub fn default_output(self) -> OutputSpec {
        let (width, height, img_type) = match self {
            Self::Rgb => (1920, 1080, ImgType::Nv12),
            Self::Left | Self::Right => (1280, 800, ImgType::Gray8),
        };
        OutputSpec {
            width,
            height,
            img_type,
            resize: ResizeMode::Crop,
            fps: Some(30.0),
        }
    }
}

impl std::str::FromStr for SensorId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|id| id.name().eq_ignore_ascii_case(s))
            .ok_or_else(|| format!("unknown sensor {s:?} (expected rgb, left or right)"))
    }
}

impl fmt::Display for SensorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Where a capture thread delivers frames. Called on the capture thread;
/// must not block for long (latest-wins: drop rather than wait).
pub trait FrameSink: Send {
    /// One received frame.
    fn publish(&mut self, frame: &FrameView<'_>);
    /// The stream ended (queue closed, device lost). Default: nothing.
    fn stream_lost(&mut self) {}
}

/// Discards frames (`--no-vcam`, and M2 before the vcam output exists).
#[derive(Debug, Default)]
pub struct NullSink;

impl FrameSink for NullSink {
    fn publish(&mut self, _frame: &FrameView<'_>) {}
}

/// EWMA weight of the newest frame interval.
const FPS_ALPHA: f64 = 0.1;

/// Latency samples kept per stats window. When nobody reads the stats (no
/// `--stats`) the window is a ring over the newest samples, so it never grows.
pub const LATENCY_WINDOW: usize = 256;

#[derive(Debug)]
struct StatsInner {
    last_seq: Option<i64>,
    last_ts_ns: Option<i64>,
    ewma_dt_ns: Option<f64>,
    frames: u64,
    drops: u64,
    /// Latencies (µs) since the last snapshot: at most [`LATENCY_WINDOW`]
    /// (the newest ones), in a buffer allocated once up front.
    window_lat_us: Vec<u32>,
    /// Next slot to overwrite once the window is full (oldest sample).
    window_pos: usize,
    /// Frames since the last snapshot (may exceed the samples kept).
    window_frames: usize,
    lost: bool,
    /// Sequence numbers per delivered frame when nothing is dropped (see
    /// [`SensorStats::with_seq_stride`]).
    seq_stride: i64,
    /// Exposure (µs) and ISO the sensor reported for the newest frame.
    exposure_us: u32,
    iso: u32,
    probe: Option<ExposureProbe>,
}

/// Watches the frames after a control send (see [`SensorStats::arm_probe`]).
#[derive(Clone, Copy, Debug)]
struct ExposureProbe {
    sent_ns: i64,
    target_us: Option<u32>,
    frames_seen: u32,
}

/// Frames a probe waits for its target exposure before giving up.
const PROBE_MAX_FRAMES: u32 = 90;

/// What a probe saw on one frame; logged by the capture thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeReport {
    /// The first frame captured after the send.
    First {
        /// Reported exposure, µs.
        exposure_us: u32,
        /// Reported ISO.
        iso: u32,
        /// Frame end-of-exposure time minus send time, ms.
        after_ms: i64,
    },
    /// The first frame whose exposure is within 5 % of the target.
    Reached {
        /// Reported exposure, µs.
        exposure_us: u32,
        /// Reported ISO.
        iso: u32,
        /// Frames since the send, this one included.
        frames: u32,
        /// Frame end-of-exposure time minus send time, ms.
        after_ms: i64,
    },
    /// The target was not reached within [`PROBE_MAX_FRAMES`] frames.
    Missed {
        /// Last reported exposure, µs.
        exposure_us: u32,
        /// The target, µs.
        target_us: u32,
    },
}

impl fmt::Display for ProbeReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::First {
                exposure_us,
                iso,
                after_ms,
            } => write!(
                f,
                "first frame after control: exposure {exposure_us} us iso {iso} \
                 ({after_ms} ms after send)"
            ),
            Self::Reached {
                exposure_us,
                iso,
                frames,
                after_ms,
            } => write!(
                f,
                "exposure reached {exposure_us} us iso {iso} after {frames} frame(s), \
                 {after_ms} ms after send"
            ),
            Self::Missed {
                exposure_us,
                target_us,
            } => write!(
                f,
                "exposure still {exposure_us} us after {PROBE_MAX_FRAMES} frames \
                 (target {target_us} us)"
            ),
        }
    }
}

/// Shared between one capture thread (writer) and the stats printer.
#[derive(Debug)]
pub struct SensorStats {
    inner: Mutex<StatsInner>,
}

impl Default for SensorStats {
    fn default() -> Self {
        Self::with_seq_stride(1)
    }
}

impl SensorStats {
    /// Stats for an output that normally advances the sequence number by
    /// `stride` per frame: a 30 fps output of a sensor running at 60 sees
    /// every other number. Only a gap beyond the stride counts as drops
    /// (rounded to whole frames, so jitter of one sensor frame is not a
    /// drop). `0` counts as 1.
    pub fn with_seq_stride(stride: u32) -> Self {
        Self {
            inner: Mutex::new(StatsInner {
                last_seq: None,
                last_ts_ns: None,
                ewma_dt_ns: None,
                frames: 0,
                drops: 0,
                window_lat_us: Vec::with_capacity(LATENCY_WINDOW),
                window_pos: 0,
                window_frames: 0,
                lost: false,
                seq_stride: i64::from(stride.max(1)),
                exposure_us: 0,
                iso: 0,
                probe: None,
            }),
        }
    }
}

/// A point-in-time summary; latency percentiles cover the frames since the
/// previous snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StatsSnapshot {
    /// Frame rate, EWMA over frame intervals (0 when no frame arrived in
    /// the window).
    pub fps: f64,
    /// Median capture→host latency in the window, ms.
    pub p50_ms: f64,
    /// 99th-percentile latency in the window, ms.
    pub p99_ms: f64,
    /// Frames in the window.
    pub window_frames: usize,
    /// Total frames received.
    pub frames: u64,
    /// Total frames missing from the sequence (seq gaps).
    pub drops: u64,
    /// The stream ended.
    pub lost: bool,
    /// Exposure (µs) of the newest frame; 0 if unknown.
    pub exposure_us: u32,
    /// ISO of the newest frame; 0 if unknown.
    pub iso: u32,
}

impl SensorStats {
    fn lock(&self) -> std::sync::MutexGuard<'_, StatsInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a frame with sequence `seq`, host timestamp `ts_host_ns`,
    /// received at `now_ns` (same steady clock).
    pub fn record(&self, seq: i64, ts_host_ns: i64, now_ns: i64) {
        let mut s = self.lock();
        s.frames += 1;
        if let Some(prev) = s.last_seq {
            let (gap, stride) = (seq - prev, s.seq_stride);
            // Frames the gap spans, rounded half down (a gap of 1.5 strides
            // is jitter, not a drop); all but one were dropped.
            let spans = (2 * gap + stride - 1) / (2 * stride);
            if spans > 1 {
                s.drops += u64::try_from(spans - 1).unwrap_or(0);
            }
        }
        s.last_seq = Some(seq);
        if let Some(prev) = s.last_ts_ns {
            let dt = (ts_host_ns - prev) as f64;
            if dt > 0.0 {
                s.ewma_dt_ns = Some(match s.ewma_dt_ns {
                    Some(e) => e + FPS_ALPHA * (dt - e),
                    None => dt,
                });
            }
        }
        s.last_ts_ns = Some(ts_host_ns);
        let lat_us = ((now_ns - ts_host_ns).max(0) / 1_000).min(i64::from(u32::MAX));
        // Never allocates: capacity is reserved up front and never exceeded.
        s.window_frames += 1;
        if s.window_lat_us.len() < LATENCY_WINDOW {
            s.window_lat_us.push(lat_us as u32);
        } else {
            let pos = s.window_pos;
            s.window_lat_us[pos] = lat_us as u32;
            s.window_pos = (pos + 1) % LATENCY_WINDOW;
        }
    }

    /// Records the exposure the sensor reported for the frame with host
    /// timestamp `ts_host_ns`, and advances an armed probe.
    pub fn record_exposure(&self, exposure_us: u32, iso: u32, ts_host_ns: i64) -> Vec<ProbeReport> {
        let mut s = self.lock();
        s.exposure_us = exposure_us;
        s.iso = iso;
        let Some(mut p) = s.probe else {
            return Vec::new();
        };
        if ts_host_ns < p.sent_ns {
            return Vec::new();
        }
        let mut out = Vec::new();
        let after_ms = (ts_host_ns - p.sent_ns) / 1_000_000;
        p.frames_seen += 1;
        if p.frames_seen == 1 {
            out.push(ProbeReport::First {
                exposure_us,
                iso,
                after_ms,
            });
        }
        let done = match p.target_us {
            None => true,
            Some(t) if exposure_us.abs_diff(t) * 20 <= t => {
                out.push(ProbeReport::Reached {
                    exposure_us,
                    iso,
                    frames: p.frames_seen,
                    after_ms,
                });
                true
            }
            Some(t) if p.frames_seen >= PROBE_MAX_FRAMES => {
                out.push(ProbeReport::Missed {
                    exposure_us,
                    target_us: t,
                });
                true
            }
            Some(_) => false,
        };
        s.probe = if done { None } else { Some(p) };
        out
    }

    /// Watches the frames captured after `sent_ns` (steady clock): the
    /// capture thread logs the first one's exposure and, with a target,
    /// the first one that reaches it. Replaces any armed probe.
    pub fn arm_probe(&self, sent_ns: i64, target_us: Option<u32>) {
        self.lock().probe = Some(ExposureProbe {
            sent_ns,
            target_us,
            frames_seen: 0,
        });
    }

    /// Marks the stream as ended.
    pub fn mark_lost(&self) {
        self.lock().lost = true;
    }

    /// Frames received so far.
    pub fn frames(&self) -> u64 {
        self.lock().frames
    }

    /// Whether the stream ended.
    pub fn is_lost(&self) -> bool {
        self.lock().lost
    }

    /// Summarises and starts a new latency window.
    pub fn snapshot(&self) -> StatsSnapshot {
        // Allocate before taking the lock; clear() keeps the capture side's
        // buffer capacity.
        let mut lat = Vec::with_capacity(LATENCY_WINDOW);
        let mut s = self.lock();
        lat.extend_from_slice(&s.window_lat_us);
        s.window_lat_us.clear();
        s.window_pos = 0;
        let window_frames = std::mem::take(&mut s.window_frames);
        let snap_frames = s.frames;
        let drops = s.drops;
        let lost = s.lost;
        let (exposure_us, iso) = (s.exposure_us, s.iso);
        let ewma_dt_ns = s.ewma_dt_ns;
        drop(s);
        lat.sort_unstable();
        let fps = match (lat.is_empty(), ewma_dt_ns) {
            (false, Some(dt)) if dt > 0.0 => 1e9 / dt,
            _ => 0.0,
        };
        StatsSnapshot {
            fps,
            p50_ms: f64::from(percentile(&lat, 50)) / 1000.0,
            p99_ms: f64::from(percentile(&lat, 99)) / 1000.0,
            window_frames,
            frames: snap_frames,
            drops,
            lost,
            exposure_us,
            iso,
        }
    }
}

/// Nearest-rank percentile of sorted `v` (0 when empty).
fn percentile(sorted: &[u32], pct: usize) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (pct * sorted.len()).div_ceil(100).max(1);
    sorted[rank.min(sorted.len()) - 1]
}

impl fmt::Display for StatsSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.lost {
            return write!(f, "LOST drop {}", self.drops);
        }
        write!(
            f,
            "{:.1}fps lat p50 {:.0}ms p99 {:.0}ms drop {}",
            self.fps, self.p50_ms, self.p99_ms, self.drops
        )?;
        if self.exposure_us > 0 {
            write!(f, " exp {}us iso {}", self.exposure_us, self.iso)?;
        }
        Ok(())
    }
}

/// How long one blocking `get` waits before re-checking the stop flag.
const GET_TIMEOUT: Duration = Duration::from_millis(100);

/// What a capture thread reads frames from: an [`OutputQueue`], or a
/// scripted queue in tests.
pub trait CaptureQueue: Send + 'static {
    /// Waits up to `timeout` for one frame and calls `f` with its view.
    /// `Ok(false)` on timeout.
    ///
    /// # Errors
    /// The queue failed (or closed; see [`Self::is_closed`]).
    fn next_frame(
        &self,
        timeout: Duration,
        f: &mut dyn FnMut(&FrameView<'_>),
    ) -> depthai::Result<bool>;
    /// Whether the queue has closed for good (errors count as closed).
    fn is_closed(&self) -> bool;
}

impl CaptureQueue for OutputQueue {
    fn next_frame(
        &self,
        timeout: Duration,
        f: &mut dyn FnMut(&FrameView<'_>),
    ) -> depthai::Result<bool> {
        Ok(self.get(timeout)?.is_some_and(|frame| {
            f(&frame.view());
            true
        }))
    }

    fn is_closed(&self) -> bool {
        OutputQueue::is_closed(self).unwrap_or(true)
    }
}

/// A capture thread; joining it returns its sink.
pub type CaptureHandle = JoinHandle<Box<dyn FrameSink>>;

/// Starts the capture thread for `sensor`. It runs until `stop` is set or
/// the queue closes, then returns `sink` through the join handle, so the
/// sink lives until whoever joins the thread drops it. With a `tap` (a GUI
/// preview), every frame is also offered to it before the sink sees it;
/// the tap is rate-limited and never blocks.
pub fn spawn_capture(
    sensor: SensorId,
    queue: impl CaptureQueue,
    mut sink: Box<dyn FrameSink>,
    stats: Arc<SensorStats>,
    stop: Arc<AtomicBool>,
    tap: Option<Arc<FrameTap>>,
) -> std::io::Result<CaptureHandle> {
    std::thread::Builder::new()
        .name(format!("capture-{sensor}"))
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let mut on_frame = |v: &FrameView<'_>| {
                    let now_ns = depthai::steady_now_ns();
                    stats.record(v.seq(), v.ts_host_ns(), now_ns);
                    for r in stats.record_exposure(v.exposure_us(), v.iso(), v.ts_host_ns()) {
                        tracing::info!("{sensor}: {r}");
                    }
                    if let Some(tap) = &tap {
                        tap.offer(v, now_ns);
                    }
                    sink.publish(v);
                };
                match queue.next_frame(GET_TIMEOUT, &mut on_frame) {
                    Ok(_) => {}
                    Err(e) => {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        if queue.is_closed() {
                            tracing::warn!("{sensor}: stream lost: {e}");
                            stats.mark_lost();
                            sink.stream_lost();
                            break;
                        }
                        tracing::warn!("{sensor}: get failed: {e}");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
            tracing::debug!("{sensor}: capture thread exiting");
            sink
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;

    #[test]
    fn seq_gaps_count_as_drops() {
        let s = SensorStats::default();
        for (i, seq) in [0, 1, 2, 5, 6, 10].into_iter().enumerate() {
            let ts = i64::try_from(i).unwrap() * 33 * MS;
            s.record(seq, ts, ts + 5 * MS);
        }
        let snap = s.snapshot();
        assert_eq!(snap.frames, 6);
        assert_eq!(snap.drops, 2 + 3);
        assert_eq!(snap.window_frames, 6);
        assert!(!snap.lost);
    }

    #[test]
    fn strided_sequences_count_only_real_gaps() {
        let s = SensorStats::with_seq_stride(2);
        // Every other number: nothing dropped. A gap of 3 (jitter) is one
        // frame; 4 is one dropped frame; 8 is three.
        for (i, seq) in [0, 2, 4, 6, 9, 11, 15, 23].into_iter().enumerate() {
            let ts = i64::try_from(i).unwrap() * 33 * MS;
            s.record(seq, ts, ts);
        }
        assert_eq!(s.snapshot().drops, 1 + 3);
        // Stride 1 keeps the old counting; 0 behaves as 1.
        let s = SensorStats::with_seq_stride(0);
        for seq in [0, 1, 3] {
            s.record(seq, 0, 0);
        }
        assert_eq!(s.snapshot().drops, 1);
    }

    #[test]
    fn fps_is_ewma_of_intervals() {
        let s = SensorStats::default();
        for i in 0..100 {
            let ts = i * 33_333_333;
            s.record(i, ts, ts);
        }
        let snap = s.snapshot();
        assert!((snap.fps - 30.0).abs() < 0.01, "{}", snap.fps);
        // No frames in the next window: 0 fps.
        assert!(s.snapshot().fps.abs() < f64::EPSILON);
    }

    #[test]
    fn latency_percentiles_per_window() {
        let s = SensorStats::default();
        // Latencies 1..=100 ms.
        for i in 1..=100i64 {
            let ts = i * 33 * MS;
            s.record(i, ts, ts + i * MS);
        }
        let snap = s.snapshot();
        assert!((snap.p50_ms - 50.0).abs() < 1e-9, "{}", snap.p50_ms);
        assert!((snap.p99_ms - 99.0).abs() < 1e-9, "{}", snap.p99_ms);
        let next = s.snapshot();
        assert_eq!(next.window_frames, 0);
        assert!(next.p50_ms.abs() < f64::EPSILON);
        assert_eq!(next.frames, 100);
    }

    #[test]
    fn window_is_capped_without_readers() {
        let s = SensorStats::default();
        // 1000 frames, latency = index ms; nobody snapshots in between.
        for i in 0..1000i64 {
            let ts = i * 33 * MS;
            s.record(i, ts, ts + i * MS);
        }
        {
            let inner = s.lock();
            assert_eq!(inner.window_lat_us.len(), LATENCY_WINDOW);
            assert_eq!(inner.window_lat_us.capacity(), LATENCY_WINDOW);
        }
        let snap = s.snapshot();
        assert_eq!(snap.window_frames, 1000);
        // Only the newest 256 (744..=999 ms) are kept.
        assert!((snap.p50_ms - 871.0).abs() < 1e-9, "{}", snap.p50_ms);
        assert!((snap.p99_ms - 997.0).abs() < 1e-9, "{}", snap.p99_ms);
        assert_eq!(s.lock().window_lat_us.capacity(), LATENCY_WINDOW);
    }

    #[test]
    fn percentile_edges() {
        assert_eq!(percentile(&[], 50), 0);
        assert_eq!(percentile(&[7], 99), 7);
        assert_eq!(percentile(&[1, 2], 50), 1);
        assert_eq!(percentile(&[1, 2], 99), 2);
    }

    #[test]
    fn display_format() {
        let snap = StatsSnapshot {
            fps: 29.97,
            p50_ms: 9.2,
            p99_ms: 17.6,
            drops: 3,
            ..StatsSnapshot::default()
        };
        assert_eq!(snap.to_string(), "30.0fps lat p50 9ms p99 18ms drop 3");
        let lost = StatsSnapshot {
            lost: true,
            drops: 1,
            ..StatsSnapshot::default()
        };
        assert_eq!(lost.to_string(), "LOST drop 1");
        let exp = StatsSnapshot {
            exposure_us: 2000,
            iso: 400,
            ..snap
        };
        assert_eq!(
            exp.to_string(),
            "30.0fps lat p50 9ms p99 18ms drop 3 exp 2000us iso 400"
        );
    }

    #[test]
    fn probe_reports_first_frame_and_target() {
        let s = SensorStats::default();
        // Nothing armed: no reports.
        assert!(s.record_exposure(8000, 100, 10 * MS).is_empty());
        s.arm_probe(100 * MS, Some(2000));
        // A frame captured before the send is ignored.
        assert!(s.record_exposure(8000, 100, 90 * MS).is_empty());
        assert_eq!(
            s.record_exposure(8000, 100, 120 * MS),
            [ProbeReport::First {
                exposure_us: 8000,
                iso: 100,
                after_ms: 20
            }]
        );
        assert!(s.record_exposure(5000, 200, 153 * MS).is_empty());
        assert_eq!(
            s.record_exposure(2050, 400, 186 * MS),
            [ProbeReport::Reached {
                exposure_us: 2050,
                iso: 400,
                frames: 3,
                after_ms: 86
            }]
        );
        // Disarmed after reaching the target.
        assert!(s.record_exposure(2000, 400, 220 * MS).is_empty());
        let snap = s.snapshot();
        assert_eq!((snap.exposure_us, snap.iso), (2000, 400));
    }

    #[test]
    fn probe_without_target_reports_once_and_gives_up_on_miss() {
        let s = SensorStats::default();
        s.arm_probe(0, None);
        assert_eq!(s.record_exposure(100, 100, MS).len(), 1);
        assert!(s.record_exposure(100, 100, 2 * MS).is_empty());
        s.arm_probe(0, Some(2000));
        let mut last = Vec::new();
        for i in 0..PROBE_MAX_FRAMES {
            last = s.record_exposure(9000, 100, i64::from(i + 1) * MS);
        }
        assert_eq!(
            last,
            [ProbeReport::Missed {
                exposure_us: 9000,
                target_us: 2000
            }]
        );
        assert!(s.record_exposure(9000, 100, 999 * MS).is_empty());
    }

    #[test]
    fn sensor_id_parses_and_indexes() {
        assert_eq!("LEFT".parse::<SensorId>(), Ok(SensorId::Left));
        assert!("mono".parse::<SensorId>().is_err());
        for (i, s) in SensorId::ALL.into_iter().enumerate() {
            assert_eq!(s.index(), i);
        }
    }

    #[test]
    fn sensor_defaults() {
        assert_eq!(SensorId::Rgb.socket(), Socket::CamA);
        assert_eq!(SensorId::Right.socket(), Socket::CamC);
        let o = SensorId::Left.default_output();
        assert_eq!((o.width, o.height, o.img_type), (1280, 800, ImgType::Gray8));
        let o = SensorId::Rgb.default_output();
        assert_eq!((o.width, o.height, o.img_type), (1920, 1080, ImgType::Nv12));
    }
}
