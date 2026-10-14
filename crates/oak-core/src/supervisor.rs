//! The supervisor thread: owns the [`Device`], the running pipeline, the
//! three control queues and the capture threads, and turns [`Command`]s
//! into device actions and [`Event`]s.
//!
//! - **Start / restart.** Each time a device opens, its link type
//!   ([`Transport`]: PoE, USB3 or USB2) picks a stream profile
//!   ([`Config::profile`]: the user's, else the built-in one) whose sizes
//!   and rates replace those in the config ([`Config::apply_transport`];
//!   reported as [`Event::Profile`] right after [`Event::Connected`]).
//!   A pipeline is then built from the current [`Config`]:
//!   one Camera per enabled sensor with its full control set as
//!   `initialControl`. A change that cannot apply live (stream size, fps,
//!   `enabled`, device, USB speed) stops the capture threads and the
//!   pipeline, reopens the device (depthai v3's `Pipeline::stop` closes it)
//!   and builds a new pipeline.
//!   The frame sinks (virtual cameras) survive both: capture threads hand
//!   them back when they end, so a virtual camera keeps showing its last
//!   frame and resizes itself when frames of the new size arrive.
//! - **Live controls.** On [`Command::ApplyConfig`] nothing is sent at
//!   once; every [`CONTROL_INTERVAL`] each sensor's applied settings are
//!   diffed against the latest config and the delta (if any) is sent, so a
//!   burst of edits coalesces to at most one send per sensor per interval,
//!   latest wins. IR dot / flood apply immediately through the device.
//! - **Reconnect.** When the device cannot be opened, or a stream is lost
//!   (queue closed, device gone), everything is dropped and the device is
//!   reopened by id after [`backoff`] (1, 2, 5, then every 10 s), until it
//!   comes back or [`Command::Shutdown`]. With automatic reconnect off
//!   ([`Options::auto_reconnect`], [`Command::SetAutoReconnect`]) it waits
//!   for a Rescan, a device or config change instead.
//! - **Sinks.** [`Command::ReplaceSinks`] swaps sensors' frame sinks (the
//!   GUI moves from null cameras to the OS ones once the camera extension
//!   is approved); a running pipeline restarts to hand them over.

use std::fmt::{self, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use depthai::{
    ControlParams, ControlQueue, Device, DeviceInfo, ImgType, OutputSpec, PipelineBuilder,
    Protocol, ResizeMode, SensorSpec, UsbSpeed,
};

use crate::capture::{
    CaptureHandle, CaptureQueue, FrameSink, SensorId, SensorStats, StatsSnapshot, spawn_capture,
};
use crate::config::{
    Config, DeviceSelection, MonoCam, MonoSettings, RgbSettings, StreamProfile, Transport,
};
use crate::controls::{
    af_trigger, device_delta, diff_mono, diff_rgb, mono_to_control, rgb_to_control, stream_restart,
};
use crate::thumb::{FrameTap, FrameTaps};
use crate::vcam_sink::VcamCounters;

/// Minimum time between two control sends to one sensor.
pub const CONTROL_INTERVAL: Duration = Duration::from_millis(33);

/// Device-side queue depth for live streams (latest wins).
const LIVE_QUEUE_SIZE: u32 = 2;

/// How often the supervisor wakes when idle (control coalescing tick).
const TICK: Duration = Duration::from_millis(10);

/// After a deliberate restart or reopen (depthai's `Pipeline::stop` closes
/// the device, which needs a moment before it opens again), this many
/// failed opens are retried after [`SETTLE`] before the device counts as
/// lost.
pub const SETTLE_ATTEMPTS: u32 = 3;

/// Wait between the [`SETTLE_ATTEMPTS`] reopen tries.
pub const SETTLE: Duration = Duration::from_millis(500);

/// A device-open helper thread the supervisor stopped waiting for (it
/// still holds, or may still open, the device).
type OrphanOpen = Arc<Mutex<Option<JoinHandle<()>>>>;

/// How often the device is polled for a silent disconnect.
const HEALTH_INTERVAL: Duration = Duration::from_secs(1);

/// Delay before reconnect attempt `attempt` (1-based): 1, 2, 5, then 10 s.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(match attempt {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 5,
        _ => 10,
    })
}

/// Requests to the supervisor.
#[derive(Debug)]
pub enum Command {
    /// A new configuration (validated by the supervisor).
    ApplyConfig(Box<Config>),
    /// One-shot RGB autofocus.
    TriggerAf,
    /// List devices ([`Event::Devices`]); while waiting to reconnect, also
    /// retry at once.
    Rescan,
    /// Switch device (reopens).
    SelectDevice(DeviceSelection),
    /// Replace the frame sinks (e.g. null cameras by the OS virtual
    /// cameras once they become available). The listed sensors' old sinks
    /// are dropped (stopping their cameras); others keep theirs. While
    /// streaming this restarts the pipeline (a reopen), since the capture
    /// threads own the sinks.
    ReplaceSinks(Vec<SensorSink>),
    /// Turn automatic reconnect on or off (see [`Options::auto_reconnect`]).
    SetAutoReconnect(bool),
    /// Stop everything and end the thread.
    Shutdown,
    /// Test hook: stop the running pipeline as if the device went away,
    /// exercising the lost-stream and reconnect path.
    SimulateStreamLoss,
}

/// What the supervisor reports.
#[derive(Clone, Debug)]
pub enum Event {
    /// A device is open and streaming.
    Connected(ConnectedInfo),
    /// The stream profile for the connected device's link was applied to
    /// the config (sent right after [`Event::Connected`]). A GUI applies
    /// the same [`Config::apply_transport`] to its own copy.
    Profile {
        /// The link type.
        transport: Transport,
        /// The sizes and rates now in use.
        profile: StreamProfile,
        /// Whether the config's streams changed.
        changed: bool,
    },
    /// The device was lost or could not be opened.
    Disconnected(String),
    /// Waiting `delay` before reconnect attempt `attempt`.
    Reconnecting {
        /// 1-based attempt number.
        attempt: u32,
        /// Wait before this attempt.
        delay: Duration,
    },
    /// The pipeline was rebuilt (stream settings changed).
    Restarted(String),
    /// Periodic statistics.
    Stats(StatsReport),
    /// One sensor's stream ended.
    StreamLost(SensorId),
    /// Something the user should know (validation fix, failed control, ...).
    Warning(String),
    /// Answer to [`Command::Rescan`].
    Devices(Vec<DeviceInfo>),
}

/// The open device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectedInfo {
    /// Device id (MxId).
    pub id: String,
    /// Product name.
    pub name: String,
    /// USB or TCP/IP.
    pub protocol: Protocol,
    /// Link speed (USB).
    pub usb_speed: UsbSpeed,
    /// IR drivers (0 on non-Pro models).
    pub ir_drivers: u32,
}

/// One sensor's line in a [`StatsReport`].
#[derive(Clone, Debug, PartialEq)]
pub struct SensorReport {
    /// Which sensor.
    pub sensor: SensorId,
    /// Capture-side stats since the last report.
    pub stats: StatsSnapshot,
    /// Virtual camera frames published / dropped, if publishing.
    pub vcam: Option<(u64, u64)>,
}

/// Periodic statistics for all streaming sensors.
#[derive(Clone, Debug, PartialEq)]
pub struct StatsReport {
    /// Enabled sensors, in [`SensorId::ALL`] order.
    pub sensors: Vec<SensorReport>,
    /// Link speed.
    pub usb: UsbSpeed,
    /// Chip temperature, °C.
    pub temp_c: Option<f32>,
}

impl fmt::Display for StatsReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut line = String::new();
        for s in &self.sensors {
            let _ = write!(line, "{} {}", s.sensor, s.stats);
            if let Some((p, d)) = s.vcam {
                let _ = write!(line, " vcam {p}/{d}");
            }
            line.push_str(" | ");
        }
        let temp = self
            .temp_c
            .map_or_else(|| "n/a".to_owned(), |t| format!("{t:.0}C"));
        write!(f, "{line}usb {} | temp {temp}", self.usb)
    }
}

/// One sensor's frame sink, and its publish counters when it is a
/// [`crate::vcam_sink::VcamSink`].
pub struct SensorSink {
    /// Which sensor.
    pub sensor: SensorId,
    /// Where frames go.
    pub sink: Box<dyn FrameSink>,
    /// Publish counters, for stats.
    pub vcam: Option<Arc<VcamCounters>>,
}

impl fmt::Debug for SensorSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SensorSink")
            .field("sensor", &self.sensor)
            .field("vcam", &self.vcam.is_some())
            .finish_non_exhaustive()
    }
}

/// Supervisor options.
#[derive(Clone, Debug)]
pub struct Options {
    /// Emit [`Event::Stats`] this often (`None`: never).
    pub stats_interval: Option<Duration>,
    /// Preview thumbnails for a GUI (`None`: none, and no cost).
    pub taps: Option<Arc<FrameTaps>>,
    /// Reopen a lost device on its own, with [`backoff`] (default). When
    /// off, a lost device stays closed until [`Command::Rescan`], a device
    /// or config change, or [`Command::SetAutoReconnect`]`(true)`.
    pub auto_reconnect: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            stats_interval: None,
            taps: None,
            auto_reconnect: true,
        }
    }
}

/// Handle to the supervisor thread. Dropping it sends
/// [`Command::Shutdown`] and joins.
pub struct Supervisor {
    commands: Sender<Command>,
    events: Receiver<Event>,
    handle: Option<JoinHandle<()>>,
    orphan: OrphanOpen,
}

impl Supervisor {
    /// Starts the supervisor with `config` (validated first; fixes are
    /// reported as [`Event::Warning`]) and one sink per sensor (sensors
    /// without a sink get a [`crate::capture::NullSink`]).
    pub fn spawn(
        config: Config,
        sinks: Vec<SensorSink>,
        options: Options,
    ) -> std::io::Result<Self> {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let mut slots: [Option<SensorSink>; 3] = [None, None, None];
        for s in sinks {
            let i = s.sensor.index();
            slots[i] = Some(s);
        }
        let orphan: OrphanOpen = Arc::default();
        let thread_orphan = Arc::clone(&orphan);
        let handle = std::thread::Builder::new()
            .name("supervisor".into())
            .spawn(move || {
                let mut state = State::new(config, slots, options, cmd_rx, ev_tx);
                state.orphan = thread_orphan;
                state.run();
                tracing::debug!("supervisor exiting");
            })?;
        Ok(Self {
            commands: cmd_tx,
            events: ev_rx,
            handle: Some(handle),
            orphan,
        })
    }

    /// Sends a command (ignored once the thread has ended).
    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }

    /// The event stream.
    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Whether the thread has ended.
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Shuts down and joins: capture threads, pipeline, device, then the
    /// sinks (virtual cameras).
    pub fn shutdown(mut self) {
        self.stop_and_join();
    }

    /// [`Self::shutdown`], then also waits (up to `max`) for a device open
    /// that was still in flight, so the device is really closed when this
    /// returns and a new supervisor can open it. Returns `false` if the
    /// open was still running after `max`.
    pub fn shutdown_and_release(mut self, max: Duration) -> bool {
        self.stop_and_join();
        let pending = self
            .orphan
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(h) = pending else { return true };
        let deadline = Instant::now() + max;
        while !h.is_finished() {
            if Instant::now() >= deadline {
                tracing::warn!("a device open was still running after {max:?}");
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = h.join();
        true
    }

    fn stop_and_join(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(h) = self.handle.take()
            && h.join().is_err()
        {
            tracing::error!("supervisor thread panicked");
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

/// Retries of one control delta before it is given up (with a warning).
pub const CONTROL_MAX_ATTEMPTS: u32 = 3;

/// One sensor's settings as far as controls go.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Rgb(RgbSettings),
    Mono(MonoSettings),
}

impl Target {
    fn of(cfg: &Config, sensor: SensorId) -> Self {
        match sensor {
            SensorId::Rgb => Self::Rgb(cfg.rgb),
            SensorId::Left => Self::Mono(*cfg.mono.effective(MonoCam::Left)),
            SensorId::Right => Self::Mono(*cfg.mono.effective(MonoCam::Right)),
        }
    }

    /// The control that takes the device from `self` to `to`.
    fn diff(&self, to: &Self) -> Option<ControlParams> {
        match (self, to) {
            (Self::Rgb(a), Self::Rgb(b)) => diff_rgb(a, b).control,
            (Self::Mono(a), Self::Mono(b)) => diff_mono(a, b).control,
            _ => unreachable!("a sensor never changes kind"),
        }
    }
}

/// Per-sensor control bookkeeping.
#[derive(Clone, Debug)]
struct SensorCtl {
    /// What the device has (confirmed sends only).
    applied: Target,
    /// What the last send tried to apply, until [`ControlState::sent`].
    in_flight: Option<Target>,
    /// Failed sends of the current target.
    failures: u32,
    /// Target of those failures (a new target starts a new count).
    failing: Option<Target>,
    last_send: Option<Instant>,
}

/// The settings each sensor's device has, and when each was last sent a
/// control. Drives the latest-wins coalescing: a change is applied only
/// once its send succeeds; a failing one is retried at most
/// [`CONTROL_MAX_ATTEMPTS`] times.
#[derive(Clone, Debug)]
pub(crate) struct ControlState {
    sensors: [SensorCtl; 3],
}

impl ControlState {
    /// State right after a pipeline started with `cfg` as `initialControl`.
    pub(crate) fn new(cfg: &Config) -> Self {
        let ctl = |sensor| SensorCtl {
            applied: Target::of(cfg, sensor),
            in_flight: None,
            failures: 0,
            failing: None,
            last_send: None,
        };
        Self {
            sensors: SensorId::ALL.map(ctl),
        }
    }

    /// The controls to send now to go from the applied settings to
    /// `desired`, for sensors whose last send is at least
    /// [`CONTROL_INTERVAL`] old. Each returned control must be reported
    /// with [`Self::sent`]. Sensors not due keep their pending changes.
    pub(crate) fn due(&mut self, desired: &Config, now: Instant) -> Vec<(SensorId, ControlParams)> {
        let mut out = Vec::new();
        for sensor in SensorId::ALL {
            let c = &mut self.sensors[sensor.index()];
            if c.last_send
                .is_some_and(|t| now.duration_since(t) < CONTROL_INTERVAL)
            {
                continue;
            }
            let target = Target::of(desired, sensor);
            if let Some(params) = c.applied.diff(&target) {
                c.in_flight = Some(target);
                c.last_send = Some(now);
                out.push((sensor, params));
            }
        }
        out
    }

    /// Reports the outcome of the send [`Self::due`] returned for
    /// `sensor`. On failure the change stays pending (it is retried on a
    /// later [`Self::due`]) until it has failed [`CONTROL_MAX_ATTEMPTS`]
    /// times; then it is dropped and `Some(attempts)` is returned so the
    /// caller can warn.
    pub(crate) fn sent(&mut self, sensor: SensorId, ok: bool) -> Option<u32> {
        let c = &mut self.sensors[sensor.index()];
        let target = c.in_flight.take()?;
        if ok {
            c.applied = target;
            c.failures = 0;
            c.failing = None;
            return None;
        }
        if c.failing != Some(target) {
            c.failing = Some(target);
            c.failures = 0;
        }
        c.failures += 1;
        if c.failures < CONTROL_MAX_ATTEMPTS {
            return None;
        }
        // Give up on this change: treat it as applied so it is not resent.
        let n = c.failures;
        c.applied = target;
        c.failures = 0;
        c.failing = None;
        Some(n)
    }
}

/// Whether any enabled/size/fps setting differs between the two configs.
fn streams_differ(a: &Config, b: &Config) -> bool {
    SensorId::ALL
        .into_iter()
        .any(|s| stream_restart(a.stream(s), b.stream(s)))
}

/// Why a pipeline or device session ended.
enum Outcome {
    /// Shutdown requested.
    Shutdown,
    /// Stream settings changed: rebuild the pipeline. depthai v3 closes the
    /// device when a pipeline stops, so this reopens the device (at once,
    /// by id) too.
    Restart(String),
    /// Device selection / USB speed changed: reopen at once.
    Reopen,
    /// The device or a stream was lost: reconnect with backoff.
    Lost(String),
    /// The pipeline could not be built or started with this config: close
    /// the device and wait for a config change.
    ConfigError(String),
}

struct Worker {
    sensor: SensorId,
    stats: Arc<SensorStats>,
    vcam: Option<Arc<VcamCounters>>,
    handle: Option<CaptureHandle>,
}

/// The capture threads of one pipeline. [`Workers::join_all`] (also run
/// on drop) sets the stop flag, joins every thread and returns the sinks.
struct Workers {
    stop: Arc<AtomicBool>,
    list: Vec<Worker>,
}

impl Workers {
    fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            list: Vec::new(),
        }
    }

    fn spawn(
        &mut self,
        sensor: SensorId,
        queue: impl CaptureQueue,
        sink: SensorSink,
        tap: Option<Arc<FrameTap>>,
        seq_stride: u32,
    ) -> std::io::Result<()> {
        let stats = Arc::new(SensorStats::with_seq_stride(seq_stride));
        let SensorSink { sink, vcam, .. } = sink;
        let handle = spawn_capture(
            sensor,
            queue,
            sink,
            Arc::clone(&stats),
            Arc::clone(&self.stop),
            tap,
        )?;
        self.list.push(Worker {
            sensor,
            stats,
            vcam,
            handle: Some(handle),
        });
        Ok(())
    }

    /// Sensors whose thread ended on its own (stream lost).
    fn finished(&self) -> Vec<SensorId> {
        self.list
            .iter()
            .filter(|w| w.handle.as_ref().is_some_and(JoinHandle::is_finished))
            .map(|w| w.sensor)
            .collect()
    }

    fn stats(&self, sensor: SensorId) -> Option<&Arc<SensorStats>> {
        self.list
            .iter()
            .find(|w| w.sensor == sensor)
            .map(|w| &w.stats)
    }

    /// Stops and joins every thread; returns the sinks they hand back.
    fn join_all(&mut self) -> Vec<SensorSink> {
        self.stop.store(true, Ordering::SeqCst);
        let mut sinks = Vec::new();
        for w in &mut self.list {
            if let Some(h) = w.handle.take() {
                if let Ok(sink) = h.join() {
                    sinks.push(SensorSink {
                        sensor: w.sensor,
                        sink,
                        vcam: w.vcam.clone(),
                    });
                } else {
                    tracing::error!("{}: capture thread panicked", w.sensor);
                }
            }
        }
        sinks
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        drop(self.join_all());
    }
}

/// The supervisor thread's state.
struct State {
    /// Latest desired config (validated).
    config: Config,
    sinks: [Option<SensorSink>; 3],
    options: Options,
    commands: Receiver<Command>,
    events: Sender<Event>,
    /// Id of the last device connected, for reconnecting to the same one.
    known_id: Option<String>,
    /// Transport of the last device opened (a PoE close needs to settle).
    last_protocol: Option<Protocol>,
    /// The current pipeline has delivered frames (resets the backoff).
    streamed: bool,
    /// See [`Options::auto_reconnect`].
    auto_reconnect: bool,
    /// Sinks from [`Command::ReplaceSinks`] waiting for the running
    /// pipeline's capture threads to hand the old ones back.
    pending_sinks: Option<Vec<SensorSink>>,
    /// Where an abandoned device-open helper is left for
    /// [`Supervisor::shutdown_and_release`].
    orphan: OrphanOpen,
}

/// How long a closed PoE link is left alone before the device is opened
/// again. XLink tears a TCP link down on its own thread after
/// `Device::close` returns (`dispatcherClean` waiting in `XLink_sem_destroy`);
/// opening the next connection while that runs jumps through a cleared
/// function pointer in XLink's scheduler state (crash in `device-open` at
/// pc 0, seen 2026-09-28 on every stream-setting change over PoE). USB
/// tears down synchronously enough that it never showed.
pub const POE_CLOSE_SETTLE: Duration = Duration::from_secs(3);

impl State {
    fn new(
        mut config: Config,
        sinks: [Option<SensorSink>; 3],
        options: Options,
        commands: Receiver<Command>,
        events: Sender<Event>,
    ) -> Self {
        let warnings = config.validate();
        let state = Self {
            config,
            sinks,
            auto_reconnect: options.auto_reconnect,
            options,
            commands,
            events,
            known_id: None,
            last_protocol: None,
            streamed: false,
            pending_sinks: None,
            orphan: Arc::default(),
        };
        for w in warnings {
            state.emit(Event::Warning(format!("config: {w}")));
        }
        state
    }

    fn emit(&self, ev: Event) {
        let _ = self.events.send(ev);
    }

    /// Takes a sensor's sink, or a `NullSink` if it has none.
    fn take_sink(&mut self, sensor: SensorId) -> SensorSink {
        self.sinks[sensor.index()]
            .take()
            .unwrap_or_else(|| SensorSink {
                sensor,
                sink: Box::new(crate::capture::NullSink),
                vcam: None,
            })
    }

    fn put_sinks(&mut self, sinks: Vec<SensorSink>) {
        for s in sinks {
            let i = s.sensor.index();
            self.sinks[i] = Some(s);
        }
    }

    /// Installs replacement sinks (no pipeline running): the old ones are
    /// dropped here.
    fn replace_sinks(&mut self, sinks: Vec<SensorSink>) {
        let names: Vec<&str> = sinks.iter().map(|s| s.sensor.name()).collect();
        tracing::info!("frame sinks replaced: {}", names.join(", "));
        self.put_sinks(sinks);
    }

    /// Validates `new`, reports fixes, and makes it the desired config.
    /// Returns the previous config.
    fn accept(&mut self, mut new: Config) -> Config {
        for w in new.validate() {
            self.emit(Event::Warning(format!("config: {w}")));
        }
        if new.vcam != self.config.vcam {
            self.emit(Event::Warning(
                "config: [vcam] changes take effect when oak-cam restarts".into(),
            ));
        }
        std::mem::replace(&mut self.config, new)
    }

    /// The selector to open: the remembered id when reconnecting to an
    /// automatically chosen device, else the configured selection.
    fn selector(&self) -> String {
        match (&self.config.device, &self.known_id) {
            (DeviceSelection::Auto, Some(id)) => id.clone(),
            (sel, _) => sel.selector().to_owned(),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn run(&mut self) {
        let mut attempt = 0u32;
        // Quiet retries left after a deliberate restart / reopen.
        let mut settle = Settle::default();
        loop {
            if attempt > 0 {
                let delay = if self.auto_reconnect {
                    let delay = backoff(attempt);
                    self.emit(Event::Reconnecting { attempt, delay });
                    Some(delay)
                } else {
                    self.emit(Event::Warning(
                        "automatic reconnect is off; Rescan to retry".into(),
                    ));
                    None
                };
                match self.wait(delay) {
                    Wait::Shutdown => return,
                    Wait::Elapsed => {}
                    Wait::Retry => attempt = 0,
                }
            }
            let dev = match self.open_interruptible() {
                Opened::Device(d) => {
                    settle.clear();
                    d
                }
                Opened::Failed(e) => {
                    if settle.take() {
                        tracing::info!(
                            "reopen failed ({e}); retrying in {} ms",
                            SETTLE.as_millis()
                        );
                        match self.wait(Some(SETTLE)) {
                            Wait::Shutdown => return,
                            Wait::Elapsed | Wait::Retry => {}
                        }
                        continue;
                    }
                    self.emit(Event::Disconnected(e));
                    attempt = attempt.saturating_add(1);
                    continue;
                }
                Opened::Retry => {
                    attempt = 0;
                    continue;
                }
                Opened::Shutdown => return,
            };
            self.streamed = false;
            let outcome = self.device_session(&dev);
            // Close before reopening (a stopped pipeline has closed it on the
            // device side already).
            drop(dev);
            if self.last_protocol == Some(Protocol::TcpIp) && !self.settle(POE_CLOSE_SETTLE) {
                return;
            }
            // Only a pipeline that delivered frames counts as a recovery; a
            // device that opens but never streams keeps backing off.
            if self.streamed {
                attempt = 0;
            }
            match outcome {
                Outcome::Shutdown => return,
                Outcome::Reopen => {
                    attempt = 0;
                    settle.arm();
                }
                Outcome::Restart(reason) => {
                    tracing::info!("restarting pipeline: {reason}");
                    self.emit(Event::Restarted(reason));
                    attempt = 0;
                    settle.arm();
                }
                Outcome::Lost(reason) => {
                    self.emit(Event::Disconnected(reason));
                    attempt = attempt.saturating_add(1);
                }
                Outcome::ConfigError(msg) => {
                    self.emit(Event::Warning(format!(
                        "{msg}; device closed, waiting for a config change"
                    )));
                    match self.wait_for_config_change() {
                        Wait::Shutdown => return,
                        Wait::Elapsed | Wait::Retry => attempt = 0,
                    }
                }
            }
        }
    }

    /// Opens the selected device on a helper thread, handling commands
    /// meanwhile, so Shutdown is not stuck behind a device search (which
    /// can take ~10 s). If the open completes after we stopped waiting, the
    /// helper drops (closes) the device it opened.
    fn open_interruptible(&mut self) -> Opened {
        let selector = self.selector();
        let usb = self.config.device_settings.max_usb_speed.to_dai();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("device-open".into())
            .spawn(move || {
                let result = open(&selector, usb);
                if let Err(unwanted) = tx.send(result) {
                    tracing::debug!(
                        "device opened after the supervisor stopped waiting; closing it"
                    );
                    drop(unwanted);
                }
            });
        let helper = match spawned {
            Ok(h) => h,
            Err(e) => return Opened::Failed(format!("cannot spawn the device-open thread: {e}")),
        };
        // Set when the selection changed during the open: its result is
        // discarded and the caller retries with the new selection.
        let mut stale = false;
        loop {
            crossbeam_channel::select! {
                recv(rx) -> r => {
                    return match r {
                        Ok(_) if stale => Opened::Retry,
                        Ok(Ok(dev)) => Opened::Device(dev),
                        Ok(Err(e)) => Opened::Failed(e),
                        Err(_) => Opened::Failed("device-open thread ended".into()),
                    };
                }
                recv(self.commands) -> cmd => match cmd {
                    Err(_) | Ok(Command::Shutdown) => {
                        // The helper may still open the device; leave it
                        // for `shutdown_and_release` to wait on.
                        *self.orphan.lock().unwrap_or_else(PoisonError::into_inner) =
                            Some(helper);
                        return Opened::Shutdown;
                    }
                    Ok(Command::ApplyConfig(c)) => {
                        let old = self.accept(*c);
                        if device_delta(&old, &self.config).reopen {
                            self.known_id = None;
                            stale = true;
                        }
                    }
                    Ok(Command::SelectDevice(sel)) => {
                        if sel != self.config.device {
                            self.config.device = sel;
                            self.known_id = None;
                            stale = true;
                        }
                    }
                    Ok(Command::Rescan) => self.rescan(),
                    Ok(Command::ReplaceSinks(sinks)) => self.replace_sinks(sinks),
                    Ok(Command::SetAutoReconnect(on)) => self.auto_reconnect = on,
                    Ok(Command::TriggerAf | Command::SimulateStreamLoss) => {}
                },
            }
        }
    }

    /// No device open: waits until the config (streams, device or USB
    /// speed) changes, a device is selected, or a rescan is asked for.
    fn wait_for_config_change(&mut self) -> Wait {
        loop {
            match self.commands.recv() {
                Err(_) | Ok(Command::Shutdown) => return Wait::Shutdown,
                Ok(Command::ApplyConfig(c)) => {
                    let old = self.accept(*c);
                    if streams_differ(&old, &self.config) || device_delta(&old, &self.config).reopen
                    {
                        return Wait::Retry;
                    }
                }
                Ok(Command::SelectDevice(sel)) => {
                    self.config.device = sel;
                    self.known_id = None;
                    return Wait::Retry;
                }
                Ok(Command::Rescan) => {
                    self.rescan();
                    return Wait::Retry;
                }
                Ok(Command::ReplaceSinks(sinks)) => self.replace_sinks(sinks),
                Ok(Command::SetAutoReconnect(on)) => self.auto_reconnect = on,
                Ok(Command::TriggerAf | Command::SimulateStreamLoss) => {}
            }
        }
    }

    /// Waits `delay` while handling commands; unlike [`Self::wait`] a
    /// command that would retry does not cut the wait short. `false` on
    /// Shutdown.
    fn settle(&mut self, delay: Duration) -> bool {
        let deadline = Instant::now() + delay;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            match self.wait(Some(left)) {
                Wait::Shutdown => return false,
                Wait::Elapsed => return true,
                Wait::Retry => {}
            }
        }
    }

    /// Handles commands for up to `delay` while disconnected (`None`: until
    /// something asks for a retry, automatic reconnect being off).
    fn wait(&mut self, delay: Option<Duration>) -> Wait {
        let deadline = delay.map(|d| Instant::now() + d);
        loop {
            let got = match deadline {
                Some(t) => self
                    .commands
                    .recv_timeout(t.saturating_duration_since(Instant::now())),
                None => self
                    .commands
                    .recv()
                    .map_err(|_| RecvTimeoutError::Disconnected),
            };
            match got {
                Err(RecvTimeoutError::Timeout) => return Wait::Elapsed,
                Err(RecvTimeoutError::Disconnected) | Ok(Command::Shutdown) => {
                    return Wait::Shutdown;
                }
                Ok(Command::ApplyConfig(c)) => {
                    let old = self.accept(*c);
                    if device_delta(&old, &self.config).reopen {
                        self.known_id = None;
                        return Wait::Retry;
                    }
                }
                Ok(Command::SelectDevice(sel)) => {
                    self.config.device = sel;
                    self.known_id = None;
                    return Wait::Retry;
                }
                Ok(Command::Rescan) => {
                    self.rescan();
                    return Wait::Retry;
                }
                Ok(Command::ReplaceSinks(sinks)) => self.replace_sinks(sinks),
                Ok(Command::SetAutoReconnect(on)) => {
                    self.auto_reconnect = on;
                    if on && deadline.is_none() {
                        return Wait::Retry;
                    }
                }
                Ok(Command::TriggerAf | Command::SimulateStreamLoss) => {}
            }
        }
    }

    fn rescan(&self) {
        match Device::list() {
            Ok(list) => self.emit(Event::Devices(list)),
            Err(e) => self.emit(Event::Warning(format!("device discovery failed: {e}"))),
        }
    }

    /// Applies `t`'s stream profile to the desired config and reports it.
    fn apply_transport(&mut self, t: Transport) {
        let changed = self.config.apply_transport(t);
        self.emit(Event::Profile {
            transport: t,
            profile: self.config.profile(t),
            changed,
        });
    }

    /// Runs pipelines on `dev` until it must be closed.
    fn device_session(&mut self, dev: &Device) -> Outcome {
        let info = ConnectedInfo {
            id: dev.id().unwrap_or_default(),
            name: dev.name().unwrap_or_default(),
            protocol: dev.protocol().unwrap_or(Protocol::Unknown),
            usb_speed: dev.usb_speed().unwrap_or(UsbSpeed::Unknown),
            ir_drivers: dev.ir_drivers().unwrap_or(0),
        };
        if !info.id.is_empty() {
            self.known_id = Some(info.id.clone());
        }
        self.last_protocol = Some(info.protocol);
        let usb = info.usb_speed;
        let ir_drivers = info.ir_drivers;
        let transport = Transport::of(info.protocol, info.usb_speed);
        self.emit(Event::Connected(info));
        self.apply_transport(transport);
        let mut ir = IrState::default();
        ir.apply(dev, ir_drivers, &self.config, &self.events, true);
        // One pipeline per open: depthai v3's `Pipeline::stop` closes the
        // device, so a restart is always a reopen.
        self.pipeline_session(dev, usb, ir_drivers, &mut ir)
    }

    /// Builds and runs one pipeline; returns when it must stop.
    #[allow(clippy::too_many_lines)]
    fn pipeline_session(
        &mut self,
        dev: &Device,
        usb: UsbSpeed,
        ir_drivers: u32,
        ir: &mut IrState,
    ) -> Outcome {
        let cfg = self.config.clone();
        let enabled: Vec<SensorId> = SensorId::ALL
            .into_iter()
            .filter(|&s| cfg.stream(s).enabled)
            .collect();
        if enabled.is_empty() {
            self.emit(Event::Warning(
                "every stream is disabled; enable one in the config".into(),
            ));
            return self.idle(&cfg, dev, ir_drivers, ir);
        }

        let builder = match PipelineBuilder::new(dev) {
            Ok(b) => b,
            Err(e) => return Outcome::Lost(format!("cannot create pipeline: {e}")),
        };
        let mut queues = Vec::new();
        let mut controls: [Option<ControlQueue>; 3] = [None, None, None];
        for &sensor in &enabled {
            match build_camera(&builder, &cfg, sensor) {
                Ok((q, c)) => {
                    queues.push((sensor, q));
                    controls[sensor.index()] = Some(c);
                }
                Err(e) => {
                    return Outcome::ConfigError(format!(
                        "cannot build the pipeline for this config ({sensor}: {e})"
                    ));
                }
            }
        }
        let pipeline = match builder.start() {
            Ok(p) => p,
            // Each pipeline starts on a freshly opened device, so a start
            // failure is taken to be the config's fault (e.g. a mode the
            // sensor cannot do), not a lost device: retrying would reboot
            // the device forever.
            Err(e) => {
                return Outcome::ConfigError(format!(
                    "cannot start the pipeline with this config ({e})"
                ));
            }
        };
        let started = Instant::now();

        let mut workers = Workers::new();
        for (sensor, queue) in queues {
            let sink = self.take_sink(sensor);
            let tap = self
                .options
                .taps
                .as_ref()
                .map(|t| Arc::clone(t.get(sensor)));
            if let Err(e) = workers.spawn(sensor, queue, sink, tap, seq_stride(&cfg, sensor)) {
                self.emit(Event::Warning(format!(
                    "{sensor}: cannot spawn capture thread: {e}"
                )));
            }
        }
        tracing::info!(
            "pipeline started: {}",
            enabled
                .iter()
                .map(|&s| {
                    let st = cfg.stream(s);
                    format!("{s} {}x{}@{}", st.width, st.height, st.fps)
                })
                .collect::<Vec<_>>()
                .join(", ")
        );

        let mut applied = ControlState::new(&cfg);
        let mut next_health = Instant::now() + HEALTH_INTERVAL;
        let mut next_stats = self.options.stats_interval.map(|i| Instant::now() + i);
        let outcome = loop {
            // 1. Commands (waits up to one tick).
            match self.commands.recv_timeout(TICK) {
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) | Ok(Command::Shutdown) => {
                    break Outcome::Shutdown;
                }
                Ok(Command::ApplyConfig(c)) => {
                    let old = self.accept(*c);
                    let dd = device_delta(&old, &self.config);
                    if dd.reopen {
                        break Outcome::Reopen;
                    }
                    ir.apply(dev, ir_drivers, &self.config, &self.events, false);
                    if streams_differ(&cfg, &self.config) {
                        break Outcome::Restart(describe_stream_change(&cfg, &self.config));
                    }
                }
                Ok(Command::SelectDevice(sel)) => {
                    if sel != self.config.device {
                        self.config.device = sel;
                        self.known_id = None;
                        break Outcome::Reopen;
                    }
                }
                Ok(Command::TriggerAf) => {
                    if let Some(q) = &controls[SensorId::Rgb.index()] {
                        match q.send(&af_trigger()) {
                            Ok(()) => tracing::info!("rgb: autofocus triggered"),
                            Err(e) => self.emit(Event::Warning(format!("rgb: AF trigger: {e}"))),
                        }
                    }
                }
                Ok(Command::Rescan) => self.rescan(),
                Ok(Command::ReplaceSinks(sinks)) => {
                    // The capture threads own the current sinks: swap after
                    // they hand them back, then start a fresh pipeline.
                    self.pending_sinks = Some(sinks);
                    break Outcome::Restart("virtual camera outputs changed".into());
                }
                Ok(Command::SetAutoReconnect(on)) => self.auto_reconnect = on,
                Ok(Command::SimulateStreamLoss) => {
                    tracing::warn!("test hook: stopping the pipeline to simulate a lost stream");
                    let _ = pipeline.stop();
                }
            }

            // 2. Coalesced live controls.
            let now = Instant::now();
            for (sensor, params) in applied.due(&self.config, now) {
                let Some(q) = &controls[sensor.index()] else {
                    continue;
                };
                let target = (params.set_bits() & depthai_sys::OAK_CTL_MANUAL_EXPOSURE != 0)
                    .then_some(params.as_raw().exposure_us);
                let sent_ns = depthai::steady_now_ns();
                let result = q.send(&params);
                if let Some(n) = applied.sent(sensor, result.is_ok()) {
                    self.emit(Event::Warning(format!(
                        "{sensor}: control change dropped after {n} failed attempts"
                    )));
                }
                match result {
                    Ok(()) => {
                        tracing::info!(
                            "{sensor}: control sent (set bits {:#x}){}",
                            params.set_bits(),
                            target.map_or_else(String::new, |t| format!(
                                ", manual exposure {t} us iso {}",
                                params.as_raw().iso
                            ))
                        );
                        if let Some(stats) = workers.stats(sensor) {
                            stats.arm_probe(sent_ns, target);
                        }
                    }
                    Err(e) => self.emit(Event::Warning(format!("{sensor}: control failed: {e}"))),
                }
            }

            if !self.streamed && workers.list.iter().any(|w| w.stats.frames() > 0) {
                self.streamed = true;
            }

            // 3. Lost streams (reported after the join below, once every
            // thread has seen its queue close).
            if !workers.finished().is_empty() {
                break Outcome::Lost(String::new());
            }
            if now >= next_health {
                next_health = now + HEALTH_INTERVAL;
                if dev.is_closed().unwrap_or(true) {
                    break Outcome::Lost("device closed".into());
                }
            }

            // 4. Stats.
            if let (Some(at), Some(every)) = (next_stats, self.options.stats_interval)
                && now >= at
            {
                next_stats = Some(next_tick(at, every, now));
                self.emit(Event::Stats(stats_report(&workers, dev, usb)));
            }
        };

        // Capture threads first (they hand the sinks back), then the control
        // queues and the pipeline.
        let sinks = workers.join_all();
        self.put_sinks(sinks);
        if let Some(new) = self.pending_sinks.take() {
            self.replace_sinks(new);
        }
        let outcome = match outcome {
            Outcome::Lost(reason) if reason.is_empty() => {
                let lost: Vec<SensorId> = workers
                    .list
                    .iter()
                    .filter(|w| w.stats.is_lost())
                    .map(|w| w.sensor)
                    .collect();
                for &s in &lost {
                    self.emit(Event::StreamLost(s));
                }
                let names: Vec<&str> = lost.iter().map(|s| s.name()).collect();
                Outcome::Lost(format!("stream lost: {}", names.join(", ")))
            }
            other => other,
        };
        drop(controls);
        drop(pipeline);
        tracing::debug!("pipeline ran {:.1} s", started.elapsed().as_secs_f32());
        outcome
    }

    /// No pipeline (nothing enabled, or it cannot be built): wait for a
    /// config change.
    fn idle(&mut self, cfg: &Config, dev: &Device, ir_drivers: u32, ir: &mut IrState) -> Outcome {
        loop {
            match self.commands.recv_timeout(HEALTH_INTERVAL) {
                Err(RecvTimeoutError::Timeout) => {
                    if dev.is_closed().unwrap_or(true) {
                        return Outcome::Lost("device closed".into());
                    }
                }
                Err(RecvTimeoutError::Disconnected) | Ok(Command::Shutdown) => {
                    return Outcome::Shutdown;
                }
                Ok(Command::ApplyConfig(c)) => {
                    let old = self.accept(*c);
                    if device_delta(&old, &self.config).reopen {
                        return Outcome::Reopen;
                    }
                    ir.apply(dev, ir_drivers, &self.config, &self.events, false);
                    if streams_differ(cfg, &self.config) {
                        return Outcome::Restart(describe_stream_change(cfg, &self.config));
                    }
                }
                Ok(Command::SelectDevice(sel)) => {
                    if sel != self.config.device {
                        self.config.device = sel;
                        self.known_id = None;
                        return Outcome::Reopen;
                    }
                }
                Ok(Command::Rescan) => self.rescan(),
                Ok(Command::ReplaceSinks(sinks)) => self.replace_sinks(sinks),
                Ok(Command::SetAutoReconnect(on)) => self.auto_reconnect = on,
                Ok(Command::TriggerAf | Command::SimulateStreamLoss) => {}
            }
        }
    }
}

/// Quiet reopen retries after a deliberate restart / reopen (see
/// [`SETTLE_ATTEMPTS`]).
#[derive(Debug, Default)]
struct Settle {
    left: u32,
}

impl Settle {
    /// A deliberate restart / reopen: the next opens may fail briefly.
    fn arm(&mut self) {
        self.left = SETTLE_ATTEMPTS;
    }

    /// The device opened.
    fn clear(&mut self) {
        self.left = 0;
    }

    /// An open failed: whether to retry quietly (using up one retry).
    fn take(&mut self) -> bool {
        if self.left == 0 {
            return false;
        }
        self.left -= 1;
        true
    }
}

enum Wait {
    Shutdown,
    Elapsed,
    Retry,
}

/// Result of [`State::open_interruptible`].
enum Opened {
    Device(Device),
    Failed(String),
    /// The selection changed while opening: try again at once.
    Retry,
    Shutdown,
}

/// What the IR drivers were last set to.
#[derive(Default)]
struct IrState {
    dot: Option<f32>,
    flood: Option<f32>,
    warned: bool,
}

impl IrState {
    /// Applies the config's IR levels that differ from what was last set
    /// (all of them when `force`).
    fn apply(
        &mut self,
        dev: &Device,
        drivers: u32,
        cfg: &Config,
        events: &Sender<Event>,
        force: bool,
    ) {
        let (dot, flood) = (cfg.device_settings.ir_dot, cfg.device_settings.ir_flood);
        if drivers == 0 {
            if (dot > 0.0 || flood > 0.0) && !self.warned {
                self.warned = true;
                let _ = events.send(Event::Warning(
                    "this device has no IR drivers; ir_dot / ir_flood ignored".into(),
                ));
            }
            return;
        }
        if force || self.dot != Some(dot) {
            match dev.set_ir_dot(dot) {
                Ok(()) => {
                    tracing::info!(
                        "IR dot projector set to {dot:.2}{}",
                        if force { "" } else { " (live, no restart)" }
                    );
                    self.dot = Some(dot);
                }
                Err(e) => {
                    let _ = events.send(Event::Warning(format!("set_ir_dot({dot}): {e}")));
                }
            }
        }
        if force || self.flood != Some(flood) {
            match dev.set_ir_flood(flood) {
                Ok(()) => {
                    tracing::info!(
                        "IR flood set to {flood:.2}{}",
                        if force { "" } else { " (live, no restart)" }
                    );
                    self.flood = Some(flood);
                }
                Err(e) => {
                    let _ = events.send(Event::Warning(format!("set_ir_flood({flood}): {e}")));
                }
            }
        }
    }
}

/// The next periodic deadline after `at` fired at `now`: one period on,
/// or one period from now if the loop fell behind (no bursts).
fn next_tick(at: Instant, every: Duration, now: Instant) -> Instant {
    let next = at + every;
    if next > now { next } else { now + every }
}

/// Opens `selector` (id, IP, or `""` for the first found).
fn open(selector: &str, usb: UsbSpeed) -> Result<Device, String> {
    if selector.is_empty() {
        match Device::list() {
            Ok(l) if l.is_empty() => return Err("no devices found".into()),
            Ok(_) => {}
            Err(e) => return Err(format!("device discovery failed: {e}")),
        }
    }
    Device::open(selector, usb).map_err(|e| {
        format!(
            "cannot open device {}: {e}",
            if selector.is_empty() {
                "(any)"
            } else {
                selector
            }
        )
    })
}

/// The output spec for `sensor` under `cfg`: RGB NV12, mono GRAY8.
pub fn output_spec(cfg: &Config, sensor: SensorId) -> OutputSpec {
    let s = cfg.stream(sensor);
    OutputSpec {
        width: s.width,
        height: s.height,
        img_type: if sensor == SensorId::Rgb {
            ImgType::Nv12
        } else {
            ImgType::Gray8
        },
        resize: ResizeMode::Crop,
        fps: Some(s.fps),
    }
}

/// The full control set `sensor` starts with.
pub fn initial_control(cfg: &Config, sensor: SensorId) -> ControlParams {
    match sensor {
        SensorId::Rgb => rgb_to_control(&cfg.rgb),
        SensorId::Left => mono_to_control(cfg.mono.effective(MonoCam::Left)),
        SensorId::Right => mono_to_control(cfg.mono.effective(MonoCam::Right)),
    }
}

/// The sensor mode to pin both mono cameras to, when their outputs differ
/// in size or rate (`None`: let depthai choose from the requested outputs).
///
/// The OV9282 pair runs as one synchronised sensor mode. Left to itself,
/// depthai picks each camera's mode from its own output (640x400 gets the
/// binned 640x400 mode), but the pair then runs at the larger camera's
/// mode, so the smaller camera receives full-size frames that do not fit
/// the output pool sized for its request ("Output image is bigger
/// (1024000B) than maximum frame size ... (256000B)") and publishes
/// nothing. Rates behave the same way: with equal sizes but 30 and 60 fps
/// both cameras ran at about 37 fps with ~190 ms latency and steady drops.
/// Pinning both to the larger size and the higher rate makes depthai plan
/// the downscale and the frame skipping for the smaller / slower output.
pub fn mono_sensor_spec(cfg: &Config) -> Option<SensorSpec> {
    let (l, r) = (cfg.stream(SensorId::Left), cfg.stream(SensorId::Right));
    let same = (l.width, l.height) == (r.width, r.height) && (l.fps - r.fps).abs() < 0.01;
    if !(l.enabled && r.enabled) || same {
        return None;
    }
    let big = if l.width * l.height >= r.width * r.height {
        l
    } else {
        r
    };
    Some(SensorSpec {
        width: big.width,
        height: big.height,
        fps: l.fps.max(r.fps),
    })
}

/// How many sensor frames one output frame of `sensor` spans: the pinned
/// mono sensor rate over this output's rate, rounded (1 when not pinned).
/// Sequence numbers count sensor frames, so a 30 fps output of a sensor
/// pinned at 60 sees every other number without dropping anything.
pub fn seq_stride(cfg: &Config, sensor: SensorId) -> u32 {
    let spec = match sensor {
        SensorId::Rgb => None,
        SensorId::Left | SensorId::Right => mono_sensor_spec(cfg),
    };
    let out = cfg.stream(sensor).fps;
    match spec {
        Some(s) if out > 0.0 => ((s.fps / out).round() as u32).max(1),
        _ => 1,
    }
}

fn build_camera(
    builder: &PipelineBuilder<'_>,
    cfg: &Config,
    sensor: SensorId,
) -> depthai::Result<(depthai::OutputQueue, ControlQueue)> {
    let spec = match sensor {
        SensorId::Rgb => None,
        SensorId::Left | SensorId::Right => mono_sensor_spec(cfg),
    };
    let cam = builder.camera(sensor.socket(), spec)?;
    cam.set_initial_control(&initial_control(cfg, sensor))?;
    let spec = output_spec(cfg, sensor);
    let q = cam
        .request_output(spec)?
        .create_queue(LIVE_QUEUE_SIZE, false)?;
    let c = cam.control_queue()?;
    Ok((q, c))
}

fn describe_stream_change(old: &Config, new: &Config) -> String {
    let parts: Vec<String> = SensorId::ALL
        .into_iter()
        .filter(|&s| stream_restart(old.stream(s), new.stream(s)))
        .map(|s| {
            let (a, b) = (old.stream(s), new.stream(s));
            let fmt = |x: &crate::config::StreamSettings| {
                if x.enabled {
                    format!("{}x{}@{}", x.width, x.height, x.fps)
                } else {
                    "off".to_owned()
                }
            };
            format!("{s} {} -> {}", fmt(a), fmt(b))
        })
        .collect();
    parts.join(", ")
}

fn stats_report(workers: &Workers, dev: &Device, usb: UsbSpeed) -> StatsReport {
    StatsReport {
        sensors: workers
            .list
            .iter()
            .map(|w| SensorReport {
                sensor: w.sensor,
                stats: w.stats.snapshot(),
                vcam: w.vcam.as_ref().map(|v| (v.published(), v.dropped())),
            })
            .collect(),
        usb,
        temp_c: dev.chip_temp().ok(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use depthai::FrameView;
    use depthai_sys as sys;
    use vcam::CameraId;

    use super::*;
    use crate::config::Exposure;
    use crate::vcam_sink::VcamSink;
    use crate::vcam_sink::test_util::Recording;

    /// Shutdown while `Device::open` searches for an id that does not exist
    /// returns at once instead of after the search times out.
    #[test]
    #[ignore = "runs depthai device discovery; run with --ignored"]
    fn shutdown_is_prompt_while_opening() {
        let cfg = Config {
            device: DeviceSelection::ById("BOGUS00000000000000".into()),
            ..Config::default()
        };
        let sup = Supervisor::spawn(cfg, Vec::new(), Options::default()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(!sup.is_finished(), "still opening");
        let t = Instant::now();
        sup.shutdown();
        let took = t.elapsed();
        assert!(took < Duration::from_millis(500), "shutdown took {took:?}");
    }

    #[test]
    fn mono_sensor_is_pinned_only_when_sizes_differ() {
        let mut cfg = Config::default();
        assert_eq!(mono_sensor_spec(&cfg), None);
        cfg.mono.shared.stream.width = 640;
        cfg.mono.shared.stream.height = 400;
        assert_eq!(mono_sensor_spec(&cfg), None, "same size: depthai chooses");
        cfg.mono.set_override(MonoCam::Right, true);
        let right = cfg.mono.right.as_mut().unwrap();
        (right.stream.width, right.stream.height, right.stream.fps) = (1280, 800, 60.0);
        assert_eq!(
            mono_sensor_spec(&cfg),
            Some(SensorSpec {
                width: 1280,
                height: 800,
                fps: 60.0
            })
        );
        cfg.mono.right.as_mut().unwrap().stream.enabled = false;
        assert_eq!(mono_sensor_spec(&cfg), None, "one camera: nothing to pair");

        // Equal sizes, different rates: pinned at the higher rate.
        let mut cfg = Config::default();
        cfg.mono.set_override(MonoCam::Right, true);
        cfg.mono.right.as_mut().unwrap().stream.fps = 60.0;
        assert_eq!(
            mono_sensor_spec(&cfg),
            Some(SensorSpec {
                width: 1280,
                height: 800,
                fps: 60.0
            })
        );
        assert_eq!(seq_stride(&cfg, SensorId::Left), 2);
        assert_eq!(seq_stride(&cfg, SensorId::Right), 1);
        assert_eq!(seq_stride(&cfg, SensorId::Rgb), 1);
        cfg.mono.right.as_mut().unwrap().stream.fps = 30.0;
        assert_eq!(mono_sensor_spec(&cfg), None);
        assert_eq!(seq_stride(&cfg, SensorId::Left), 1);
    }

    /// The transport step rewrites the stream shapes (keeping `enabled`)
    /// and reports what it applied; a second connect on the same link
    /// changes nothing, and an unchanged config does not count as a
    /// stream change (no restart).
    #[test]
    fn connect_applies_the_transport_profile() {
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let mut cfg = Config::default();
        cfg.rgb.stream.enabled = false;
        let mut state = State::new(cfg, [None, None, None], Options::default(), cmd_rx, ev_tx);
        let before = state.config.clone();
        state.apply_transport(Transport::of(Protocol::TcpIp, UsbSpeed::Unknown));
        match ev_rx.try_recv().unwrap() {
            Event::Profile {
                transport,
                profile,
                changed,
            } => {
                assert_eq!(transport, Transport::Poe);
                assert_eq!(profile, Transport::Poe.defaults());
                assert!(changed);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(state.config.active_profile(), Transport::Poe.defaults());
        assert!(!state.config.rgb.stream.enabled);
        assert!(streams_differ(&before, &state.config));
        let after = state.config.clone();
        state.apply_transport(Transport::Poe);
        assert!(matches!(
            ev_rx.try_recv().unwrap(),
            Event::Profile { changed: false, .. }
        ));
        assert!(!streams_differ(&after, &state.config));
        // A GUI echoing the same streams back (with its own profiles) is
        // not a restart.
        let mut echo = after.clone();
        echo.remember_profile(Transport::Poe);
        echo.rgb.brightness = 2;
        assert!(!streams_differ(&after, &echo));
    }

    #[test]
    fn backoff_schedule() {
        let secs: Vec<u64> = (0..=6).map(|a| backoff(a).as_secs()).collect();
        assert_eq!(secs, [0, 1, 2, 5, 10, 10, 10]);
        assert_eq!(backoff(u32::MAX), Duration::from_secs(10));
    }

    #[test]
    fn stats_tick_never_bursts() {
        let t0 = Instant::now();
        let s = Duration::from_secs(1);
        assert_eq!(next_tick(t0, s, t0), t0 + s);
        assert_eq!(next_tick(t0, s, t0 + Duration::from_millis(30)), t0 + s);
        // Fell 3.5 periods behind: the next one is a full period away.
        let late = t0 + Duration::from_millis(3500);
        assert_eq!(next_tick(t0, s, late), late + s);
    }

    #[test]
    fn controls_coalesce_latest_wins_per_sensor() {
        let base = Config::default();
        let mut st = ControlState::new(&base);
        let t0 = Instant::now();
        // Nothing changed: nothing to send.
        assert!(st.due(&base, t0).is_empty());

        let mut a = base.clone();
        a.rgb.brightness = 1;
        let sent = st.due(&a, t0);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, SensorId::Rgb);
        assert_eq!(sent[0].1.set_bits(), sys::OAK_CTL_BRIGHTNESS);
        assert_eq!(st.sent(SensorId::Rgb, true), None);

        // Two more edits within 33 ms: held back...
        let mut b = a.clone();
        b.rgb.brightness = 2;
        assert!(st.due(&b, t0 + Duration::from_millis(10)).is_empty());
        let mut c = b.clone();
        c.rgb.brightness = 5;
        c.rgb.contrast = -1;
        assert!(st.due(&c, t0 + Duration::from_millis(20)).is_empty());
        // ...then sent once, with the latest values of every changed field.
        let sent = st.due(&c, t0 + CONTROL_INTERVAL);
        assert_eq!(sent.len(), 1);
        st.sent(SensorId::Rgb, true);
        let r = sent[0].1;
        assert_eq!(
            r.set_bits(),
            sys::OAK_CTL_BRIGHTNESS | sys::OAK_CTL_CONTRAST
        );
        assert_eq!((r.as_raw().brightness, r.as_raw().contrast), (5, -1));
        assert!(st.due(&c, t0 + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn mono_throttle_is_independent_of_rgb() {
        let base = Config::default();
        let mut st = ControlState::new(&base);
        let t0 = Instant::now();
        let mut a = base.clone();
        a.rgb.brightness = 1;
        assert_eq!(st.due(&a, t0).len(), 1);
        st.sent(SensorId::Rgb, true);
        // A shared mono edit right after is not held back by the RGB send,
        // and goes to both cameras.
        let mut b = a.clone();
        b.mono.shared.exposure = Exposure::Manual { us: 2000, iso: 400 };
        let sent = st.due(&b, t0 + Duration::from_millis(1));
        let sensors: Vec<SensorId> = sent.iter().map(|(s, _)| *s).collect();
        assert_eq!(sensors, [SensorId::Left, SensorId::Right]);
        for &s in &sensors {
            st.sent(s, true);
        }
        assert!(
            sent.iter()
                .all(|(_, p)| p.set_bits() == sys::OAK_CTL_MANUAL_EXPOSURE)
        );
        // An override edit goes to that camera only.
        let mut c = b.clone();
        c.mono.set_override(MonoCam::Right, true);
        c.mono.editable(MonoCam::Right).brightness = 3;
        let sent = st.due(&c, t0 + Duration::from_millis(100));
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, SensorId::Right);
    }

    #[test]
    fn failed_send_stays_pending_then_gives_up_after_three() {
        let base = Config::default();
        let mut st = ControlState::new(&base);
        let t0 = Instant::now();
        let mut a = base.clone();
        a.rgb.brightness = 4;
        let at = |n: u32| t0 + CONTROL_INTERVAL * n;
        // Attempts 1 and 2 fail: the change stays pending and is resent.
        for n in 0..2 {
            let sent = st.due(&a, at(n));
            assert_eq!(sent.len(), 1, "attempt {}", n + 1);
            assert_eq!(sent[0].1.set_bits(), sys::OAK_CTL_BRIGHTNESS);
            assert_eq!(st.sent(SensorId::Rgb, false), None);
        }
        // Attempt 3 fails: given up, reported once, not resent.
        assert_eq!(st.due(&a, at(2)).len(), 1);
        assert_eq!(st.sent(SensorId::Rgb, false), Some(3));
        assert!(st.due(&a, at(3)).is_empty());
        // A later change is sent afresh (full count again) and a success
        // applies it.
        let mut b = a.clone();
        b.rgb.contrast = 2;
        let sent = st.due(&b, at(4));
        assert_eq!(sent[0].1.set_bits(), sys::OAK_CTL_CONTRAST);
        assert_eq!(st.sent(SensorId::Rgb, false), None);
        let sent = st.due(&b, at(5));
        assert_eq!(sent[0].1.set_bits(), sys::OAK_CTL_CONTRAST);
        assert_eq!(st.sent(SensorId::Rgb, true), None);
        assert!(st.due(&b, at(6)).is_empty());
        // A success after a failure also applies (no reset needed).
        let mut c = b.clone();
        c.rgb.saturation = -3;
        assert_eq!(st.due(&c, at(7)).len(), 1);
        assert_eq!(st.sent(SensorId::Rgb, false), None);
        assert_eq!(st.due(&c, at(8)).len(), 1);
        assert_eq!(st.sent(SensorId::Rgb, true), None);
        assert!(st.due(&c, at(9)).is_empty());
        // `sent` without a pending send is a no-op.
        assert_eq!(st.sent(SensorId::Left, false), None);
    }

    #[test]
    fn stream_change_detection() {
        let a = Config::default();
        let mut b = a.clone();
        assert!(!streams_differ(&a, &b));
        b.rgb.brightness = 3;
        assert!(!streams_differ(&a, &b));
        b.mono.shared.stream.width = 640;
        b.mono.shared.stream.height = 400;
        assert!(streams_differ(&a, &b));
        assert_eq!(
            describe_stream_change(&a, &b),
            "left 1280x800@30 -> 640x400@30, right 1280x800@30 -> 640x400@30"
        );
        let mut c = a.clone();
        c.mono.set_override(MonoCam::Left, true);
        c.mono.editable(MonoCam::Left).stream.enabled = false;
        assert_eq!(describe_stream_change(&a, &c), "left 1280x800@30 -> off");
    }

    #[test]
    fn output_specs_and_initial_controls_follow_config() {
        let mut cfg = Config::default();
        cfg.mono.shared.stream.width = 640;
        cfg.mono.shared.stream.height = 400;
        cfg.rgb.exposure = Exposure::Manual { us: 2000, iso: 400 };
        let o = output_spec(&cfg, SensorId::Rgb);
        assert_eq!(
            (o.width, o.height, o.img_type, o.fps),
            (1920, 1080, ImgType::Nv12, Some(30.0))
        );
        let o = output_spec(&cfg, SensorId::Right);
        assert_eq!((o.width, o.height, o.img_type), (640, 400, ImgType::Gray8));
        let p = initial_control(&cfg, SensorId::Rgb);
        assert_ne!(p.set_bits() & sys::OAK_CTL_MANUAL_EXPOSURE, 0);
        assert_eq!(p.as_raw().exposure_us, 2000);
        let p = initial_control(&cfg, SensorId::Left);
        assert_ne!(p.set_bits() & sys::OAK_CTL_AE_AUTO, 0);
    }

    #[test]
    fn stats_report_formats_like_the_m2_line() {
        let r = StatsReport {
            sensors: vec![SensorReport {
                sensor: SensorId::Rgb,
                stats: StatsSnapshot {
                    fps: 30.0,
                    p50_ms: 40.0,
                    p99_ms: 50.0,
                    ..StatsSnapshot::default()
                },
                vcam: Some((10, 1)),
            }],
            usb: UsbSpeed::SuperPlus,
            temp_c: Some(47.6),
        };
        assert_eq!(
            r.to_string(),
            "rgb 30.0fps lat p50 40ms p99 50ms drop 0 vcam 10/1 | usb SUPER_PLUS | temp 48C"
        );
    }

    /// Hands out GRAY8 4x2 frames, then reports the queue closed.
    struct ScriptedQueue {
        frames: Mutex<VecDeque<Vec<u8>>>,
    }

    impl CaptureQueue for ScriptedQueue {
        fn next_frame(
            &self,
            _timeout: Duration,
            f: &mut dyn FnMut(&FrameView<'_>),
        ) -> depthai::Result<bool> {
            let next = self.frames.lock().unwrap().pop_front();
            match next {
                Some(buf) => {
                    f(&FrameView::synthetic(&buf, ImgType::Gray8, 4, 2, 4, 0));
                    Ok(true)
                }
                None => Err(depthai::Error::Depthai("queue closed".into())),
            }
        }

        fn is_closed(&self) -> bool {
            self.frames.lock().unwrap().is_empty()
        }
    }

    #[test]
    fn settle_retries_only_after_a_deliberate_reopen() {
        let mut s = Settle::default();
        assert!(
            !s.take(),
            "a first failure or a lost device is not retried quietly"
        );
        s.arm();
        for _ in 0..SETTLE_ATTEMPTS {
            assert!(s.take());
        }
        assert!(!s.take(), "then the device counts as lost");
        s.arm();
        assert!(s.take());
        s.clear();
        assert!(!s.take(), "a successful open ends the quiet retries");
    }

    /// `shutdown_and_release` waits for an open still in flight, bounded.
    #[test]
    fn shutdown_and_release_waits_for_an_open_in_flight() {
        let orphan: OrphanOpen = Arc::default();
        *orphan.lock().unwrap() = Some(std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(50));
        }));
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<Command>();
        drop(cmd_rx);
        let (_ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let sup = Supervisor {
            commands: cmd_tx,
            events: ev_rx,
            handle: None,
            orphan: Arc::clone(&orphan),
        };
        let t = Instant::now();
        assert!(sup.shutdown_and_release(Duration::from_secs(5)));
        assert!(
            t.elapsed() >= Duration::from_millis(40),
            "waited for the open helper"
        );
        assert!(orphan.lock().unwrap().is_none());
        let (cmd_tx, _cmd_rx) = crossbeam_channel::unbounded::<Command>();
        let (_ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let stuck: OrphanOpen = Arc::default();
        *stuck.lock().unwrap() = Some(std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(400));
        }));
        let sup = Supervisor {
            commands: cmd_tx,
            events: ev_rx,
            handle: None,
            orphan: stuck,
        };
        assert!(
            !sup.shutdown_and_release(Duration::from_millis(50)),
            "bounded"
        );
    }

    /// Replacing a sensor's sink drops (stops) its old camera and leaves
    /// the other sensors' sinks alone.
    #[test]
    fn replace_sinks_drops_only_the_replaced_ones() {
        let (old_left, old_log) = Recording::boxed(CameraId::Left);
        let (rgb, rgb_log) = Recording::boxed(CameraId::Rgb);
        let (new_left, new_log) = Recording::boxed(CameraId::Left);
        let sink = |sensor, cam| {
            let s = VcamSink::start(SensorId::camera_id(sensor), cam, 4, 2, 30.0).unwrap();
            SensorSink {
                sensor,
                vcam: Some(s.counters()),
                sink: Box::new(s),
            }
        };
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let mut state = State::new(
            Config::default(),
            [
                Some(sink(SensorId::Rgb, rgb)),
                Some(sink(SensorId::Left, old_left)),
                None,
            ],
            Options::default(),
            cmd_rx,
            ev_tx,
        );
        assert!(state.auto_reconnect, "on by default");
        state.replace_sinks(vec![sink(SensorId::Left, new_left)]);
        assert_eq!(old_log.lock().unwrap().stops, 1, "old camera stopped");
        assert_eq!(new_log.lock().unwrap().stops, 0);
        assert_eq!(rgb_log.lock().unwrap().stops, 0, "rgb untouched");
        assert!(state.sinks[SensorId::Left.index()].is_some());
        assert!(state.sinks[SensorId::Right.index()].is_none());
        assert!(format!("{:?}", Command::ReplaceSinks(Vec::new())).contains("ReplaceSinks"));
    }

    /// A capture thread that ends on a lost stream keeps its virtual camera
    /// open (holding the last frame); joining hands the sink back so the
    /// next pipeline reuses it, and only dropping it stops the camera.
    #[test]
    fn lost_stream_keeps_vcam_open_and_sink_is_reusable() {
        let (cam, log) = Recording::boxed(CameraId::Left);
        let sink = VcamSink::start(CameraId::Left, cam, 4, 2, 30.0).unwrap();
        let counters = sink.counters();
        let queue = ScriptedQueue {
            frames: Mutex::new(VecDeque::from([vec![1u8; 8], vec![2u8; 8]])),
        };
        let mut workers = Workers::new();
        workers
            .spawn(
                SensorId::Left,
                queue,
                SensorSink {
                    sensor: SensorId::Left,
                    sink: Box::new(sink),
                    vcam: Some(Arc::clone(&counters)),
                },
                None,
                1,
            )
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while workers.finished().is_empty() {
            assert!(Instant::now() < deadline, "capture thread did not end");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(workers.finished(), [SensorId::Left]);
        assert!(workers.stats(SensorId::Left).unwrap().snapshot().lost);
        assert_eq!(counters.published(), 2);
        assert_eq!(
            log.lock().unwrap().stops,
            0,
            "camera stopped on a lost stream"
        );

        let mut sinks = workers.join_all();
        assert_eq!(sinks.len(), 1);
        assert_eq!(
            log.lock().unwrap().stops,
            0,
            "joining must not stop the camera"
        );

        // The next pipeline reuses the sink (and its camera).
        let queue = ScriptedQueue {
            frames: Mutex::new(VecDeque::from([vec![3u8; 8]])),
        };
        let mut next = Workers::new();
        next.spawn(SensorId::Left, queue, sinks.pop().unwrap(), None, 1)
            .unwrap();
        while next.finished().is_empty() {
            assert!(
                Instant::now() < deadline,
                "second capture thread did not end"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(counters.published(), 3);
        // Fresh stats for the new pipeline.
        assert_eq!(next.stats(SensorId::Left).unwrap().snapshot().frames, 1);
        assert_eq!(log.lock().unwrap().starts.len(), 1, "camera started once");

        drop(next);
        assert_eq!(
            log.lock().unwrap().stops,
            1,
            "dropping the sink stops the camera"
        );
    }
}
