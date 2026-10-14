//! `--headless [--config PATH] [--device ID|IP] [--no-vcam] [--stats]
//! [--duration SECS]`: stream the enabled sensors until Ctrl-C (or for
//! SECS seconds), configured from the config file
//! and reloading it whenever it changes.
//!
//! The work happens in the oak-core [`Supervisor`]: this command loads the
//! config, starts the virtual cameras, hands both to the supervisor, feeds
//! it `ApplyConfig` on every (debounced) change of the file, and prints its
//! events. Each sensor's frames go to a `VcamSink` over its OS virtual
//! camera (`OAK RGB` / `OAK Left` / `OAK Right`), or to a `NullSink` with
//! `--no-vcam`; the hidden `--null-vcam` publishes into
//! [`vcam::NullCamera`]s. The virtual cameras start before the device is
//! opened, so a missing macOS extension fails fast.

use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use depthai::{Device, UsbSpeed};
use notify::Watcher as _;
use oak_core::capture::{NullSink, SensorId};
use oak_core::config::{Config, DeviceSelection, MonoCam, StreamSettings, Transport};
use oak_core::supervisor::{Command, Event, Options, SensorSink, Supervisor};
use oak_core::vcam_sink::VcamSink;
use vcam::{CameraId, VcamError, VirtualCamera};

use crate::cmd_config::{load_or_default, resolve};

/// Quiet time after the last file event before the config is reloaded.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// Opens `selector` (id or IP), or the first device found.
pub fn open_device(selector: Option<&str>) -> Result<Device> {
    let selector = selector.unwrap_or("");
    if selector.is_empty()
        && Device::list()
            .context("device discovery failed")?
            .is_empty()
    {
        bail!("no devices found");
    }
    Device::open(selector, UsbSpeed::SuperPlus).with_context(|| {
        format!(
            "cannot open device {}",
            if selector.is_empty() {
                "(any)"
            } else {
                selector
            }
        )
    })
}

/// Options for [`run`].
#[derive(Debug)]
pub struct HeadlessArgs<'a> {
    /// Device id or IP; overrides the config's `device`.
    pub device: Option<&'a str>,
    /// Where frames are published.
    pub vcam: VcamMode,
    /// Print a stats line every second.
    pub stats: bool,
    /// Stop cleanly after this long, as on Ctrl-C; `None` runs until Ctrl-C.
    pub duration: Option<Duration>,
    /// Config file; `None` for the per-user default.
    pub config: Option<&'a Path>,
    /// Test hook: simulate a lost stream after this many seconds.
    pub test_stream_loss_after: Option<f32>,
}

/// Where `--headless` publishes frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VcamMode {
    /// `--no-vcam`: capture and discard.
    Off,
    /// The OS virtual cameras (default).
    Platform,
    /// `--null-vcam`: [`vcam::NullCamera`]s, for testing the publish path
    /// without the OS side.
    Null,
}

/// `--device` as a selection: an IP address, else a device id.
fn selection(device: &str) -> DeviceSelection {
    if device.parse::<IpAddr>().is_ok() {
        DeviceSelection::ByIp(device.to_owned())
    } else {
        DeviceSelection::ById(device.to_owned())
    }
}

/// Starts one virtual camera per sensor whose `[vcam]` entry is enabled
/// (none with [`VcamMode::Off`]), at the sensor's configured size and rate.
fn start_sinks(out: &mut impl Write, mode: VcamMode, cfg: &Config) -> Result<Vec<SensorSink>> {
    let null = |sensor| SensorSink {
        sensor,
        sink: Box::new(NullSink),
        vcam: None,
    };
    let mut sinks = Vec::new();
    for sensor in SensorId::ALL {
        let id = sensor.camera_id();
        if mode != VcamMode::Off && !cfg.vcam.get(sensor).enabled {
            writeln!(out, "{sensor}: virtual camera disabled in the config")?;
            sinks.push(null(sensor));
            continue;
        }
        let camera: Box<dyn VirtualCamera> = match mode {
            VcamMode::Off => {
                sinks.push(null(sensor));
                continue;
            }
            VcamMode::Null => Box::new(vcam::NullCamera::new(id)),
            VcamMode::Platform if !crate::cmd_vcam::MULTI_DEVICE && id != CameraId::Rgb => {
                tracing::warn!(
                    "{sensor}: this platform's virtual camera backend publishes RGB only \
                     until M4b/M4c; {sensor} frames are captured and discarded"
                );
                sinks.push(null(sensor));
                continue;
            }
            VcamMode::Platform => vcam::platform_camera(id),
        };
        let st = cfg.stream(sensor);
        match VcamSink::start(id, camera, st.width, st.height, st.fps) {
            Ok(sink) => {
                let counters = sink.counters();
                sinks.push(SensorSink {
                    sensor,
                    sink: Box::new(sink),
                    vcam: Some(counters),
                });
            }
            Err(VcamError::NotAvailable(msg)) => {
                writeln!(out, "{}: {msg}", id.name())?;
                #[cfg(target_os = "macos")]
                crate::cmd_vcam::write_macos_steps(out)?;
                writeln!(
                    out,
                    "\nOr run without virtual cameras: oak-cam --headless --no-vcam"
                )?;
                out.flush()?;
                bail!("{} is not available", id.name());
            }
            Err(e) => bail!("{}: cannot start virtual camera: {e}", id.name()),
        }
    }
    Ok(sinks)
}

/// Watches one file (through its directory, so editors that save by
/// rename are seen) and reports when it changed and then stayed quiet for
/// [`DEBOUNCE`].
struct ConfigWatcher {
    _watcher: notify::RecommendedWatcher,
    events: Receiver<notify::Result<notify::Event>>,
    file_name: std::ffi::OsString,
    due: Option<Instant>,
}

impl ConfigWatcher {
    fn new(path: &Path) -> Result<Self> {
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let file_name = path
            .file_name()
            .context("config path has no file name")?
            .to_owned();
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })
        .context("cannot create file watcher")?;
        watcher
            .watch(dir, notify::RecursiveMode::NonRecursive)
            .with_context(|| format!("cannot watch {}", dir.display()))?;
        Ok(Self {
            _watcher: watcher,
            events: rx,
            file_name,
            due: None,
        })
    }

    /// Drains file events; returns `true` once per debounced change.
    fn poll(&mut self) -> bool {
        while let Ok(res) = self.events.try_recv() {
            match res {
                Ok(ev) => {
                    let ours = ev
                        .paths
                        .iter()
                        .any(|p| p.file_name() == Some(self.file_name.as_os_str()));
                    if ours && !matches!(ev.kind, notify::EventKind::Access(_)) {
                        self.due = Some(Instant::now() + DEBOUNCE);
                    }
                }
                Err(e) => tracing::warn!("config watcher: {e}"),
            }
        }
        if self.due.is_some_and(|d| Instant::now() >= d) {
            self.due = None;
            return true;
        }
        false
    }
}

/// Runs until Ctrl-C, or until `args.duration` has passed.
pub fn run(out: &mut impl Write, args: &HeadlessArgs<'_>) -> Result<()> {
    let stop = crate::signal::install();
    let path = resolve(args.config)?;
    let (mut cfg, existed) = load_or_default(&path)?;
    if existed {
        writeln!(out, "config: {}", path.display())?;
    } else {
        writeln!(
            out,
            "config: {} not found; using defaults (create it with `oak-cam config init`)",
            path.display()
        )?;
    }
    let device_override = args.device.map(selection);
    if let Some(sel) = &device_override {
        cfg.device = sel.clone();
    }
    // Validate here too so the virtual cameras start at a supported size;
    // the supervisor reports the warnings.
    let sinks = start_sinks(out, args.vcam, &{
        let mut c = cfg.clone();
        c.validate();
        c
    })?;

    // Watching needs the directory; create it on a fresh machine so a
    // config written later is still picked up. On failure the watcher
    // below reports why.
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && !dir.exists()
    {
        let _ = std::fs::create_dir_all(dir);
    }
    let watcher = match ConfigWatcher::new(&path) {
        Ok(w) => Some(w),
        Err(e) => {
            writeln!(out, "note: config changes will not be picked up: {e:#}")?;
            None
        }
    };
    let options = Options {
        stats_interval: args.stats.then_some(Duration::from_secs(1)),
        taps: None,
        ..Options::default()
    };
    let file_cfg = cfg.clone();
    let sup = Supervisor::spawn(cfg, sinks, options).context("cannot start supervisor")?;
    out.flush()?;

    let outputs = match args.vcam {
        VcamMode::Off => "no virtual cameras",
        VcamMode::Null => "null virtual cameras",
        VcamMode::Platform => "virtual cameras",
    };
    let result = event_loop(
        out,
        args,
        &sup,
        watcher,
        &path,
        device_override.as_ref(),
        outputs,
        &stop,
        file_cfg,
    );
    sup.shutdown();
    result?;
    writeln!(out, "stopped")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn event_loop(
    out: &mut impl Write,
    args: &HeadlessArgs<'_>,
    sup: &Supervisor,
    mut watcher: Option<ConfigWatcher>,
    path: &Path,
    device_override: Option<&DeviceSelection>,
    outputs: &str,
    stop: &std::sync::atomic::AtomicBool,
    mut file_cfg: Config,
) -> Result<()> {
    let started = Instant::now();
    let mut transport: Option<Transport> = None;
    // What the supervisor has now: the last config sent, with the link's
    // profile applied on connect (the same pure step it runs).
    let mut applied = file_cfg.clone();
    let mut loss_hook = args.test_stream_loss_after.map(Duration::from_secs_f32);
    let deadline = args.duration.map(|d| started + d);
    while !stop.load(Ordering::SeqCst) && deadline.is_none_or(|d| Instant::now() < d) {
        match sup.events().recv_timeout(Duration::from_millis(50)) {
            Ok(ev) => {
                match &ev {
                    Event::Profile { transport: t, .. } => {
                        transport = Some(*t);
                        applied.apply_transport(*t);
                    }
                    Event::Disconnected(_) | Event::Reconnecting { .. } => transport = None,
                    _ => {}
                }
                print_event(out, &ev, outputs)?;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("supervisor ended unexpectedly"),
        }
        if sup.is_finished() {
            bail!("supervisor ended unexpectedly");
        }
        if let Some(w) = &mut watcher
            && w.poll()
        {
            match Config::load(path) {
                Ok(mut c) => {
                    if let Some(sel) = device_override {
                        c.device = sel.clone();
                    }
                    writeln!(out, "config reloaded")?;
                    out.flush()?;
                    let mut apply = c.clone();
                    follow_transport(&mut apply, &file_cfg, &applied, transport);
                    file_cfg = c;
                    applied = apply.clone();
                    sup.send(Command::ApplyConfig(Box::new(apply)));
                }
                Err(e) => {
                    writeln!(
                        out,
                        "config reload failed, keeping the current settings: {e}"
                    )?;
                    out.flush()?;
                }
            }
        }
        if let Some(after) = loss_hook
            && started.elapsed() >= after
        {
            loss_hook = None;
            writeln!(out, "test hook: simulating a lost stream")?;
            sup.send(Command::SimulateStreamLoss);
        }
    }
    Ok(())
}

fn print_event(out: &mut impl Write, ev: &Event, outputs: &str) -> Result<()> {
    match ev {
        Event::Connected(i) => writeln!(
            out,
            "streaming from {} ({}, {} {}) to {outputs}; Ctrl-C to stop",
            i.id, i.name, i.protocol, i.usb_speed
        )?,
        Event::Profile {
            transport, profile, ..
        } => writeln!(
            out,
            "stream profile: {transport} ({}): {profile}",
            if *profile == transport.defaults() {
                "defaults"
            } else {
                "custom"
            }
        )?,
        Event::Disconnected(r) => writeln!(out, "disconnected: {r}")?,
        Event::Reconnecting { attempt, delay } => writeln!(
            out,
            "reconnecting in {} s (attempt {attempt})",
            delay.as_secs()
        )?,
        Event::Restarted(r) => writeln!(out, "pipeline restarted: {r}")?,
        Event::Stats(r) => writeln!(out, "{r}")?,
        Event::StreamLost(s) => writeln!(out, "{s}: stream lost")?,
        Event::Warning(w) => writeln!(out, "warning: {w}")?,
        Event::Devices(list) => {
            for d in list {
                writeln!(out, "{}  {}  {}  {}", d.id, d.protocol, d.state, d.name)?;
            }
        }
    }
    out.flush()?;
    Ok(())
}

/// Reconciles a reloaded file (`new`; `previous` is the file as last
/// loaded) with what the supervisor runs (`applied`) and the connected
/// link `t`:
///
/// - `[profiles]` unchanged in the file: the in-memory profiles (edits
///   remembered by earlier reloads) are carried forward.
/// - Stream sizes/rates: only the fields the file changed since the last
///   load are taken from it; the rest keep their applied values. If any
///   changed, the result becomes `t`'s profile; if none did, `t`'s profile
///   is applied, so editing an unrelated key does not undo it.
///
/// Not connected (`t` is `None`): the file's streams are sent as they are.
fn follow_transport(new: &mut Config, previous: &Config, applied: &Config, t: Option<Transport>) {
    if new.profiles == previous.profiles {
        new.profiles = applied.profiles;
    }
    let Some(t) = t else { return };
    let mut edited = overlay(
        &mut new.rgb.stream,
        &previous.rgb.stream,
        &applied.rgb.stream,
    );
    edited |= overlay(
        &mut new.mono.shared.stream,
        &previous.mono.shared.stream,
        &applied.mono.shared.stream,
    );
    for (cam, slot) in [
        (MonoCam::Left, &mut new.mono.left),
        (MonoCam::Right, &mut new.mono.right),
    ] {
        if let Some(m) = slot {
            edited |= overlay(
                &mut m.stream,
                &previous.mono.effective(cam).stream,
                &applied.mono.effective(cam).stream,
            );
        }
    }
    if edited {
        new.remember_profile(t);
    } else {
        new.apply_transport(t);
    }
}

/// Per field (width, height, fps): keeps `new`'s value where the file
/// changed it (differs from `previous`), else takes `applied`'s. Returns
/// whether the file changed any.
fn overlay(new: &mut StreamSettings, previous: &StreamSettings, applied: &StreamSettings) -> bool {
    let mut edited = false;
    if new.width == previous.width {
        new.width = applied.width;
    } else {
        edited = true;
    }
    if new.height == previous.height {
        new.height = applied.height;
    } else {
        edited = true;
    }
    if new.fps.to_bits() == previous.fps.to_bits() {
        new.fps = applied.fps;
    } else {
        edited = true;
    }
    edited
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_flag_is_ip_or_id() {
        assert_eq!(
            selection("192.0.2.44"),
            DeviceSelection::ByIp("192.0.2.44".into())
        );
        assert_eq!(
            selection("14442C10514ACDD600"),
            DeviceSelection::ById("14442C10514ACDD600".into())
        );
    }

    /// A file with the USB3 sizes, running on PoE defaults.
    fn on_poe() -> (Config, Config) {
        let file = Config::default();
        let mut applied = file.clone();
        applied.apply_transport(Transport::Poe);
        (file, applied)
    }

    #[test]
    fn reload_keeps_the_profile_unless_streams_were_edited() {
        let (file, applied) = on_poe();
        // Unrelated edit on PoE: the PoE profile stays.
        let mut new = file.clone();
        new.rgb.brightness = 3;
        follow_transport(&mut new, &file, &applied, Some(Transport::Poe));
        assert_eq!(new.active_profile(), Transport::Poe.defaults());
        assert_eq!(new.profiles.poe, None);
        assert_eq!(new.rgb.brightness, 3);
        // Not connected: the file as is.
        let mut new = file.clone();
        new.rgb.brightness = 1;
        follow_transport(&mut new, &file, &file, None);
        assert_eq!(new.active_profile(), file.active_profile());
    }

    /// Editing one field of a file that still holds the USB3 sizes changes
    /// only that field of the running PoE profile.
    #[test]
    fn reload_takes_only_the_edited_stream_fields() {
        let (file, applied) = on_poe();
        let mut new = file.clone();
        new.rgb.stream.fps = 15.0;
        follow_transport(&mut new, &file, &applied, Some(Transport::Poe));
        assert_eq!(
            new.active_profile().to_string(),
            "rgb 1280x720@15, mono 640x400@20"
        );
        assert_eq!(new.profiles.poe, Some(new.active_profile()));
    }

    /// A stream edit remembered by one reload survives a later reload that
    /// edits something else.
    #[test]
    fn stream_edit_survives_an_unrelated_reload() {
        let (file0, applied0) = on_poe();
        let mut file1 = file0.clone();
        file1.rgb.stream.fps = 15.0;
        let mut sent1 = file1.clone();
        follow_transport(&mut sent1, &file0, &applied0, Some(Transport::Poe));
        let mut file2 = file1.clone();
        file2.rgb.brightness = 4;
        let mut sent2 = file2.clone();
        follow_transport(&mut sent2, &file1, &sent1, Some(Transport::Poe));
        assert_eq!(
            sent2.active_profile().to_string(),
            "rgb 1280x720@15, mono 640x400@20"
        );
        assert_eq!(sent2.profiles, sent1.profiles);
        assert_eq!(sent2.rgb.brightness, 4);
        // A restart re-applies the remembered profile: nothing changes.
        let mut after_restart = sent2.clone();
        assert!(!after_restart.apply_transport(Transport::Poe));
    }

    /// A per-camera override size edited in the file is kept, and kept
    /// across a restart.
    #[test]
    fn reload_keeps_an_override_size() {
        let (file0, applied0) = on_poe();
        let mut file1 = file0.clone();
        file1.mono.set_override(MonoCam::Left, true);
        file1.mono.left.as_mut().unwrap().stream.fps = 15.0;
        let mut sent = file1.clone();
        follow_transport(&mut sent, &file0, &applied0, Some(Transport::Poe));
        assert_eq!(
            sent.active_profile().to_string(),
            "rgb 1280x720@20, mono 640x400@20, left 640x400@15"
        );
        assert!(!sent.apply_transport(Transport::Poe));
    }

    #[test]
    fn profile_event_line() {
        let mut out = Vec::new();
        let ev = Event::Profile {
            transport: Transport::Poe,
            profile: Transport::Poe.defaults(),
            changed: true,
        };
        print_event(&mut out, &ev, "").unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "stream profile: PoE (defaults): rgb 1280x720@20, mono 640x400@20\n"
        );
    }

    #[test]
    fn watcher_debounces_file_changes() {
        let dir = std::env::temp_dir().join(format!("oak-cam-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let mut w = ConfigWatcher::new(&path).unwrap();
        // Let the watcher settle, then drop anything it saw from setup.
        std::thread::sleep(Duration::from_millis(300));
        while w.poll() {}
        w.due = None;
        let _ = w.poll();

        // Several writes in a burst: one reload, after the quiet period.
        for i in 0..3 {
            std::fs::write(&path, format!("version = 1\n# {i}\n")).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::write(dir.join("other.toml"), "x").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut fired = 0;
        let mut first = None;
        while Instant::now() < deadline && fired == 0 {
            if w.poll() {
                fired += 1;
                first = Some(Instant::now());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fired, 1, "no reload after writing the file");
        // Nothing more once quiet.
        let quiet_until = first.unwrap() + Duration::from_millis(600);
        while Instant::now() < quiet_until {
            assert!(!w.poll(), "reloaded twice for one burst");
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
