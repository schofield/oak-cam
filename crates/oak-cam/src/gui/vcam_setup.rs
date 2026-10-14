//! Getting the OS virtual cameras going from inside the GUI: the frame
//! sinks, the "virtual cameras unavailable" banner with its Install /
//! Enable button, and switching from null cameras to the OS ones without a
//! restart once the camera extension is approved.
//!
//! Every OS probe and the activation request run on helper threads (a
//! `CoreMediaIO` call can spin the calling thread's run loop, which must
//! never happen inside winit's event handler). The replacement sinks are
//! built on a helper thread too and handed to the supervisor with
//! [`Command::ReplaceSinks`](oak_core::supervisor::Command::ReplaceSinks).

use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use oak_core::capture::{NullSink, SensorId};
use oak_core::config::Config;
use oak_core::supervisor::SensorSink;
use oak_core::vcam_sink::VcamSink;
use vcam::{CameraId, VirtualCamera};

use super::camera_card::Output;
use super::theme;

/// While waiting for the user to approve the extension, how often the
/// camera list is checked.
const WATCH_INTERVAL: Duration = Duration::from_secs(3);

/// Why the null cameras are used when `--force-null-vcam` is given.
pub(crate) const FORCED_REASON: &str =
    "Null cameras were forced with --force-null-vcam (test hook).";

/// The sinks for one session, what each sensor publishes to, and why the
/// OS cameras are not used (if they are not).
#[allow(clippy::struct_field_names)]
pub(crate) struct Sinks {
    pub sinks: Vec<SensorSink>,
    pub outputs: [Output; 3],
    pub fallback: Option<String>,
}

/// The frame sinks: each sensor's OS virtual camera, a null camera when
/// the OS side is unavailable (or `force_null`), or nothing when the
/// output is disabled.
pub(crate) fn start_sinks(cfg: &Config, force_null: bool) -> Sinks {
    start_sinks_for(cfg, force_null, [true; 3])
}

/// [`start_sinks`] for the sensors marked in `which` only (the others get
/// no sink and [`Output::Off`] here).
pub(crate) fn start_sinks_for(cfg: &Config, force_null: bool, which: [bool; 3]) -> Sinks {
    let mut sinks = Vec::new();
    let mut outputs = [Output::Off; 3];
    let mut fallback: Option<String> = None;
    for sensor in SensorId::ALL {
        if !which[sensor.index()] {
            continue;
        }
        let id = sensor.camera_id();
        // A single-device backend publishes RGB only; null cameras touch
        // no OS device, so forced null covers every sensor (as
        // `--headless --null-vcam` does).
        let off = !cfg.vcam.get(sensor).enabled
            || (!force_null && !crate::cmd_vcam::MULTI_DEVICE && id != CameraId::Rgb);
        if off {
            sinks.push(SensorSink {
                sensor,
                sink: Box::new(NullSink),
                vcam: None,
            });
            continue;
        }
        let st = cfg.stream(sensor);
        let (w, h, fps) = (st.width, st.height, st.fps);
        let platform = if force_null {
            Err(FORCED_REASON.to_owned())
        } else {
            VcamSink::start(id, vcam::platform_camera(id), w, h, fps).map_err(|e| e.to_string())
        };
        let (sink, output) = match platform {
            Ok(s) => (s, Output::Platform),
            Err(e) => {
                tracing::warn!("{}: virtual camera unavailable: {e}", id.name());
                fallback.get_or_insert(e);
                let null: Box<dyn VirtualCamera> = Box::new(vcam::NullCamera::new(id));
                match VcamSink::start(id, null, w, h, fps) {
                    Ok(s) => (s, Output::Null),
                    Err(e) => {
                        tracing::error!("{}: null camera failed to start: {e}", id.name());
                        sinks.push(SensorSink {
                            sensor,
                            sink: Box::new(NullSink),
                            vcam: None,
                        });
                        continue;
                    }
                }
            }
        };
        outputs[sensor.index()] = output;
        let counters = sink.counters();
        sinks.push(SensorSink {
            sensor,
            sink: Box::new(sink),
            vcam: Some(counters),
        });
    }
    Sinks {
        sinks,
        outputs,
        fallback,
    }
}

/// The camera extension as `systemextensionsctl` lists it (macOS).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Extension {
    /// Not probed yet (or not macOS).
    Unknown,
    #[cfg(target_os = "macos")]
    NotInstalled,
    AwaitingApproval,
    #[cfg(target_os = "macos")]
    Enabled,
    /// Another state (disabled, terminated…), as listed.
    #[cfg(target_os = "macos")]
    Other(String),
}

/// What the banner offers after an install attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Offer {
    None,
    /// Not running from /Applications/oak-cam.app: offer to open it (when
    /// it exists).
    OpenInstalledApp {
        exists: bool,
    },
}

/// What the banner asks the app to do.
#[derive(Debug, Default)]
pub(crate) struct BannerOut {
    /// Quit this instance (the installed app was opened instead).
    pub quit: bool,
}

pub(crate) struct VcamSetup {
    /// Why the null cameras are used (`None`: OS cameras, or off).
    pub fallback: Option<String>,
    pub outputs: [Output; 3],
    extension: Extension,
    ext_job: Option<Receiver<Extension>>,
    install: Option<Receiver<String>>,
    /// The last install / enable / retry progress line.
    pub install_text: Option<String>,
    offer: Offer,
    /// An install or enable was asked for: keep checking whether the
    /// cameras appeared, and switch to them when they do.
    watching: bool,
    watch_job: Option<Receiver<bool>>,
    last_watch: Option<Instant>,
    /// A retry in flight, with the sensors it retries (those on null
    /// cameras when it started).
    retry: Option<(Receiver<Sinks>, [bool; 3])>,
}

/// Folds a retry of the sensors in `which` into `current`: a sensor moves
/// to its new sink only when that reached the OS camera. Returns the
/// merged outputs and the sinks to hand over.
fn merge_retry(
    current: [Output; 3],
    which: [bool; 3],
    new: Sinks,
) -> ([Output; 3], Vec<SensorSink>) {
    let mut outputs = current;
    let mut sinks = Vec::new();
    for s in new.sinks {
        let i = s.sensor.index();
        if which[i] && new.outputs[i] == Output::Platform {
            outputs[i] = Output::Platform;
            sinks.push(s);
        }
        // Anything else (a sensor that stayed on a null camera) is
        // dropped here, before it ever reaches the supervisor.
    }
    (outputs, sinks)
}

impl VcamSetup {
    pub(crate) fn new(outputs: [Output; 3], fallback: Option<String>) -> Self {
        let mut s = Self {
            fallback,
            outputs,
            extension: Extension::Unknown,
            ext_job: None,
            install: None,
            install_text: None,
            offer: Offer::None,
            watching: false,
            watch_job: None,
            last_watch: None,
            retry: None,
        };
        if s.fallback.is_some() {
            s.probe_extension();
        }
        s
    }

    /// A new session's outputs (Device › Connect / Disconnect).
    pub(crate) fn set_outputs(&mut self, outputs: [Output; 3], fallback: Option<String>) {
        self.outputs = outputs;
        self.fallback = fallback;
        self.watching = false;
        self.offer = Offer::None;
        // A retry for the old session no longer applies.
        self.retry = None;
        if self.fallback.is_some() {
            self.probe_extension();
        }
    }

    /// An install request, probe or retry is running.
    pub(crate) fn busy(&self) -> bool {
        self.install.is_some() || self.retry.is_some()
    }

    fn probe_extension(&mut self) {
        if !cfg!(target_os = "macos") || self.ext_job.is_some() {
            return;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        if std::thread::Builder::new()
            .name("vcam-ext-state".into())
            .spawn(move || {
                let _ = tx.send(extension_state());
            })
            .is_ok()
        {
            self.ext_job = Some(rx);
        }
    }

    /// The banner button's title.
    pub(crate) fn install_label(&self) -> &'static str {
        if self.extension == Extension::AwaitingApproval {
            "Enable…"
        } else {
            "Install camera extension…"
        }
    }

    /// The banner button, the Virtual cameras menu item and the tab's
    /// Install / Update button: submit the activation request (or, while
    /// it waits for approval, open System Settings at the approval pane),
    /// then switch to the OS cameras once they appear.
    pub(crate) fn request_install(&mut self) {
        if !cfg!(target_os = "macos") || self.install.is_some() {
            return;
        }
        let awaiting = self.extension == Extension::AwaitingApproval;
        if awaiting {
            open_approval_settings();
            self.watching = self.fallback.is_some();
        }
        if !from_installed_app() {
            let exists = std::path::Path::new(APP_PATH).exists();
            self.offer = Offer::OpenInstalledApp { exists };
            self.install_text = Some(if awaiting {
                format!(
                    "Approve the oak-cam camera extension in System Settings; oak-cam \
                     switches to it as soon as it is enabled. (This oak-cam is not \
                     {APP_PATH}, so it cannot submit the extension itself.)"
                )
            } else if exists {
                format!(
                    "This oak-cam is not running from {APP_PATH}, and macOS only accepts \
                     the camera extension from the installed app. Open {APP_PATH} to \
                     install it."
                )
            } else {
                format!(
                    "This oak-cam is not running from {APP_PATH}, and macOS only accepts \
                     the camera extension from the installed app, which is missing: build \
                     and install it with scripts/vcam/build-macos.sh."
                )
            });
            return;
        }
        self.offer = Offer::None;
        let (tx, rx) = crossbeam_channel::bounded(1);
        if std::thread::Builder::new()
            .name("vcam-install".into())
            .spawn(move || {
                let _ = tx.send(install());
            })
            .is_ok()
        {
            self.install = Some(rx);
            self.install_text = Some("Requesting the camera extension…".into());
        }
    }

    /// Tries the OS cameras again (on a helper thread) for the sensors on
    /// null cameras only: a sensor already on its OS camera is left alone,
    /// so two publishers never attach to one camera. [`Self::poll`] hands
    /// the new sinks over when more sensors reach their OS camera.
    pub(crate) fn retry(&mut self, cfg: &Config) {
        if self.retry.is_some() {
            return;
        }
        let which = self.outputs.map(|o| o == Output::Null);
        if !which.contains(&true) {
            return;
        }
        let cfg = cfg.clone();
        let (tx, rx) = crossbeam_channel::bounded(1);
        if std::thread::Builder::new()
            .name("vcam-retry".into())
            .spawn(move || {
                let _ = tx.send(start_sinks_for(&cfg, false, which));
            })
            .is_ok()
        {
            self.retry = Some((rx, which));
        }
    }

    /// Collects finished helper jobs. Returns sinks to hand to the
    /// supervisor (`ReplaceSinks`) when a retry reached more OS cameras.
    pub(crate) fn poll(&mut self, cfg: &Config) -> Option<Vec<SensorSink>> {
        if let Some(rx) = &self.ext_job
            && let Ok(e) = rx.try_recv()
        {
            self.extension = e;
            self.ext_job = None;
        }
        if let Some(rx) = &self.install
            && let Ok(text) = rx.try_recv()
        {
            self.install = None;
            self.install_text = Some(text);
            // Whatever macOS answered, watch for the cameras: an approval
            // in System Settings completes without this process.
            self.watching = self.fallback.is_some();
            self.last_watch = None;
            self.probe_extension();
        }
        if self.watching && self.retry.is_none() {
            self.watch(cfg);
        }
        let (rx, which) = self.retry.as_ref()?;
        let which = *which;
        let new = rx.try_recv().ok()?;
        self.retry = None;
        let why = new.fallback.clone();
        let (outputs, sinks) = merge_retry(self.outputs, which, new);
        if sinks.is_empty() {
            if !self.watching {
                self.install_text = Some(format!(
                    "The OS virtual cameras are still unavailable: {}",
                    why.as_deref().unwrap_or("no change")
                ));
            }
            // The unused sinks (and their cameras) were dropped in
            // `merge_retry`.
            None
        } else {
            self.watching = false;
            let still_null = outputs.contains(&Output::Null);
            self.install_text = Some(if still_null {
                format!(
                    "Some virtual cameras are still unavailable: {}",
                    why.as_deref().unwrap_or("unknown")
                )
            } else {
                "Switched to the OS virtual cameras.".into()
            });
            self.fallback = still_null.then(|| why.unwrap_or_default());
            self.outputs = outputs;
            self.probe_extension();
            Some(sinks)
        }
    }

    /// While watching: every few seconds, check (on a helper thread)
    /// whether every enabled camera is listed, and retry when it is.
    fn watch(&mut self, cfg: &Config) {
        if let Some(rx) = &self.watch_job
            && let Ok(listed) = rx.try_recv()
        {
            self.watch_job = None;
            if listed {
                self.retry(cfg);
                return;
            }
        }
        let due = self
            .last_watch
            .is_none_or(|t| t.elapsed() >= WATCH_INTERVAL);
        if !due || self.watch_job.is_some() {
            return;
        }
        self.last_watch = Some(Instant::now());
        let wanted: Vec<CameraId> = SensorId::ALL
            .into_iter()
            .filter(|&s| cfg.vcam.get(s).enabled)
            .map(SensorId::camera_id)
            .collect();
        let (tx, rx) = crossbeam_channel::bounded(1);
        if std::thread::Builder::new()
            .name("vcam-watch".into())
            .spawn(move || {
                let _ = tx.send(cameras_listed(&wanted));
            })
            .is_ok()
        {
            self.watch_job = Some(rx);
        }
    }

    /// The "virtual cameras unavailable" banner, with Install / Enable and
    /// Retry. Draws nothing while the OS cameras are in use.
    pub(crate) fn banner(&mut self, ui: &mut egui::Ui, cfg: &Config) -> BannerOut {
        let mut out = BannerOut::default();
        let Some(why) = self.fallback.clone() else {
            return out;
        };
        theme::group_frame(ui).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.colored_label(
                theme::WARN,
                format!(
                    "Virtual cameras are unavailable, so frames go to null cameras (previews \
                     and stats still work). {why}"
                ),
            );
            ui.horizontal_wrapped(|ui| {
                let busy = self.busy();
                if cfg!(target_os = "macos")
                    && ui
                        .add_enabled(!busy, egui::Button::new(self.install_label()))
                        .on_hover_text(
                            "Submit oak-cam's camera extension to macOS (approve it in System \
                             Settings when asked); oak-cam switches to the virtual cameras \
                             without a restart",
                        )
                        .clicked()
                {
                    self.request_install();
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Retry"))
                    .on_hover_text("Try the OS virtual cameras again now")
                    .clicked()
                {
                    self.retry(cfg);
                }
                if let Offer::OpenInstalledApp { exists: true } = self.offer
                    && ui
                        .button(format!("Open {APP_PATH}"))
                        .on_hover_text("Opens the installed app and quits this one")
                        .clicked()
                {
                    match open_installed_app() {
                        Ok(()) => out.quit = true,
                        Err(e) => self.install_text = Some(format!("cannot open {APP_PATH}: {e}")),
                    }
                }
                if busy || self.watching {
                    ui.spinner();
                }
            });
            if let Some(t) = &self.install_text {
                ui.label(t);
            }
        });
        out
    }
}

/// The installed app bundle (macOS).
#[cfg(target_os = "macos")]
const APP_PATH: &str = vcam::macos::APP_INSTALL_PATH;
#[cfg(not(target_os = "macos"))]
const APP_PATH: &str = "";

/// Whether this process is /Applications/oak-cam.app's executable.
fn from_installed_app() -> bool {
    #[cfg(target_os = "macos")]
    {
        let installed = std::path::Path::new(APP_PATH).canonicalize().ok();
        vcam::macos::install::bundle_path()
            .is_some_and(|b| installed.is_some() && b.canonicalize().ok() == installed)
    }
    #[cfg(not(target_os = "macos"))]
    false
}

#[cfg(target_os = "macos")]
fn extension_state() -> Extension {
    use vcam::macos::install::{SystemState, query_system_state};
    match query_system_state() {
        Ok(SystemState::NotInstalled) => Extension::NotInstalled,
        Ok(SystemState::AwaitingApproval { .. }) => Extension::AwaitingApproval,
        Ok(SystemState::Enabled { .. }) => Extension::Enabled,
        Ok(SystemState::Other { state, .. }) => Extension::Other(state),
        Err(e) => Extension::Other(e.to_string()),
    }
}

#[cfg(not(target_os = "macos"))]
fn extension_state() -> Extension {
    Extension::Unknown
}

/// Whether every camera in `ids` is listed by the OS.
fn cameras_listed(ids: &[CameraId]) -> bool {
    #[cfg(target_os = "macos")]
    {
        ids.iter()
            .all(|&id| matches!(vcam::macos::status(id), vcam::macos::Status::Active { .. }))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ids;
        false
    }
}

/// The activation request (blocking; run on a helper thread).
#[cfg(target_os = "macos")]
pub(crate) fn install() -> String {
    use vcam::macos::install;
    install::request_activation(Duration::from_secs(60), false).describe()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn install() -> String {
    String::new()
}

/// System Settings at the extension approval pane.
fn open_approval_settings() {
    #[cfg(target_os = "macos")]
    {
        let url = "x-apple.systempreferences:com.apple.LoginItems-Settings.extension";
        if let Err(e) = std::process::Command::new("/usr/bin/open").arg(url).spawn() {
            tracing::warn!("cannot open System Settings: {e}");
        }
    }
}

fn open_installed_app() -> std::io::Result<()> {
    std::process::Command::new("/usr/bin/open")
        .arg(APP_PATH)
        .spawn()
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_null_publishes_to_null_cameras() {
        let cfg = Config::default();
        let s = start_sinks(&cfg, true);
        assert_eq!(s.fallback.as_deref(), Some(FORCED_REASON));
        assert_eq!(s.sinks.len(), 3);
        // Every OS, single-device backends included.
        assert_eq!(s.outputs, [Output::Null; 3]);
    }

    #[test]
    fn retry_touches_only_the_null_sensors() {
        let cfg = Config::default();
        // Only Left is selected: no sink is built for RGB or Right.
        let s = start_sinks_for(&cfg, true, [false, true, false]);
        assert_eq!(s.sinks.len(), 1);
        assert_eq!(s.sinks[0].sensor, SensorId::Left);
        assert_eq!(s.outputs, [Output::Off, Output::Null, Output::Off]);
        // A retry result that reached no OS camera hands nothing over.
        let current = [Output::Platform, Output::Null, Output::Null];
        let (out, sinks) = merge_retry(
            current,
            [false, true, true],
            start_sinks_for(&cfg, true, [false, true, true]),
        );
        assert_eq!(out, current);
        assert!(sinks.is_empty());
        // Sinks for sensors not being retried are never handed over, even
        // if a (buggy) result included them.
        let mut rgb = start_sinks_for(&cfg, true, [true, false, false]);
        rgb.outputs[0] = Output::Platform;
        let (out, sinks) = merge_retry(current, [false, true, true], rgb);
        assert_eq!(out, current);
        assert!(sinks.is_empty());
        // The retry is not even started when nothing is on a null camera.
        let mut setup = VcamSetup::new([Output::Platform, Output::Off, Output::Platform], None);
        setup.retry(&cfg);
        assert!(setup.retry.is_none());
    }

    #[test]
    fn install_label_follows_the_extension_state() {
        let mut s = VcamSetup::new([Output::Off; 3], None);
        assert_eq!(s.install_label(), "Install camera extension…");
        s.extension = Extension::AwaitingApproval;
        assert_eq!(s.install_label(), "Enable…");
        assert!(!s.busy());
    }

    #[test]
    fn banner_is_drawn_only_with_a_fallback() {
        let ctx = egui::Context::default();
        let cfg = Config::default();
        let mut ok = VcamSetup::new([Output::Platform; 3], None);
        let mut null = VcamSetup::new([Output::Null; 3], Some("x".into()));
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let before = ui.min_rect().height();
            let _ = ok.banner(ui, &cfg);
            assert!((ui.min_rect().height() - before).abs() < f32::EPSILON);
            let _ = null.banner(ui, &cfg);
            assert!(ui.min_rect().height() > before);
        });
        out.drop_without_applying_deltas();
    }
}
