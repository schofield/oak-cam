//! The eframe app: owns the supervisor and the edited config, drains
//! events every frame, runs menu actions, and lays out the window.
//!
//! Widgets edit `cfg` directly. After each frame, if `cfg` differs from
//! what was last sent, it is validated and sent as `ApplyConfig` (the
//! supervisor applies controls live, coalesced to one send per sensor per
//! 33 ms, and restarts the pipeline for stream changes; `[ui]`-only changes
//! are not sent), and a save is scheduled 500 ms later (atomic write; a
//! burst of slider moves saves once). The `[ui]` settings apply live:
//! theme, thumbnail rate and automatic reconnect the frame they change;
//! the side panel tab and window size are remembered too. On close the
//! supervisor is shut down and joined, then any pending change is saved.
//!
//! Menu actions come from the native macOS menu bar, the egui menu strip
//! elsewhere, keyboard shortcuts and `--gui-script` `menu` steps, and all
//! run through [`OakApp::apply_action`].
//!
//! Stream profiles: when the supervisor reports the link's profile
//! (`Event::Profile`), the same `Config::apply_transport` is applied to
//! `cfg`, so the widgets show what streams; the resulting change goes out
//! through the path above (the supervisor already has those streams, so
//! it does not restart). While connected, every commit first stores the
//! current sizes/rates as that link's profile (`Config::remember_profile`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use crossbeam_channel::Receiver;
use oak_core::capture::SensorId;
use oak_core::config::{Config, DeviceSelection, MonoCam, ThemePreset};
use oak_core::supervisor::{Command, Event, Options, Supervisor};
use oak_core::thumb::FrameTaps;

use super::camera_card::{self, Output};
use super::menu::{self, MenuAction, MenuState};
use super::panels::{self, Tab, vcam::VcamPanel};
use super::script::{Script, Step};
use super::settings::SettingsWindow;
use super::status::{Link, Status};
use super::thumbs::Thumbs;
use super::vcam_setup::{self, VcamSetup};
use super::{png, theme, top_bar};

/// Quiet time after the last change before the config is saved.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
/// Repaint period (stats and thumbnails refresh at up to 10 Hz).
const REPAINT: Duration = Duration::from_millis(100);
/// How often the supervisor reports stats.
const STATS_INTERVAL: Duration = Duration::from_millis(500);
/// How long a message stays in the message bar.
const MESSAGE_TTL: Duration = Duration::from_secs(12);
/// Longest a Disconnect waits for a device open still in flight (depthai's
/// device search gives up after about 10 s).
const RELEASE_TIMEOUT: Duration = Duration::from_secs(15);

/// Supervisor options for `cfg`.
fn options(cfg: &Config, taps: &Arc<FrameTaps>) -> Options {
    Options {
        stats_interval: Some(STATS_INTERVAL),
        taps: Some(Arc::clone(taps)),
        auto_reconnect: cfg.ui.auto_reconnect,
    }
}

/// Everything prepared before the window opens.
pub(crate) struct Startup {
    pub(crate) cfg: Config,
    path: PathBuf,
    save_enabled: bool,
    banners: Vec<String>,
    messages: Vec<String>,
    sup: Option<Supervisor>,
    taps: Arc<FrameTaps>,
    outputs: [Output; 3],
    fallback: Option<String>,
    script: Option<Script>,
}

impl Startup {
    /// Loads the config and, unless `[ui] start_streaming` is off, starts
    /// the virtual cameras (null ones with `force_null_vcam`) and the
    /// supervisor.
    pub(crate) fn new(path: &Path, script: Option<Script>, force_null_vcam: bool) -> Result<Self> {
        let mut banners = Vec::new();
        let mut messages = Vec::new();
        let (mut cfg, save_enabled) = match Config::load_if_exists(path) {
            Ok(Some(c)) => {
                messages.push(format!("config: {}", path.display()));
                (c, true)
            }
            Ok(None) => {
                messages.push(format!(
                    "config: {} not found; starting from defaults",
                    path.display()
                ));
                (Config::default(), true)
            }
            Err(e) => {
                banners.push(format!(
                    "The config file could not be read, so defaults are used and changes \
                     will not be saved: {e}"
                ));
                (Config::default(), false)
            }
        };
        for w in cfg.validate() {
            messages.push(format!("config: {w}"));
        }
        let taps = Arc::new(FrameTaps::new());
        taps.set_rate(cfg.ui.thumbnail_fps);
        let (sup, outputs, fallback) = if cfg.ui.start_streaming {
            let s = vcam_setup::start_sinks(&cfg, force_null_vcam);
            let sup = Supervisor::spawn(cfg.clone(), s.sinks, options(&cfg, &taps))
                .context("cannot start the supervisor")?;
            (Some(sup), s.outputs, s.fallback)
        } else {
            messages.push(
                "not streaming: \"Start streaming on launch\" is off (Device › Connect starts)"
                    .into(),
            );
            (None, [Output::Off; 3], None)
        };
        Ok(Self {
            cfg,
            path: path.to_owned(),
            save_enabled,
            banners,
            messages,
            sup,
            taps,
            outputs,
            fallback,
            script,
        })
    }
}

/// A supervisor started from the Device menu: it, its outputs, why the OS
/// cameras are not used (if not), and the config it started from.
type Started = std::io::Result<(Supervisor, [Output; 3], Option<String>, Config)>;

#[allow(clippy::struct_excessive_bools)] // independent flags
pub(crate) struct OakApp {
    cfg: Config,
    sent: Config,
    path: PathBuf,
    path_text: String,
    save_enabled: bool,
    save_due: Option<Instant>,
    sup: Option<Supervisor>,
    /// A supervisor being started on a helper thread (Device › Connect),
    /// with that thread.
    starting: Option<(Receiver<Started>, JoinHandle<()>)>,
    /// The thread shutting the last supervisor down (Disconnect), or an
    /// abandoned start (which shuts down what it started); the next start
    /// waits for it, so two sessions never hold the device at once.
    stopping: Option<JoinHandle<()>>,
    taps: Arc<FrameTaps>,
    thumbs: Thumbs,
    status: Status,
    tab: Tab,
    vcam_panel: VcamPanel,
    setup: VcamSetup,
    settings: SettingsWindow,
    /// The Connect to IP window's text, while it is open.
    connect_ip: Option<String>,
    about_open: bool,
    banners: Vec<String>,
    ip: String,
    script: Option<Script>,
    /// Ask for the device list once the first open attempt has finished
    /// (discovery during a device boot is avoided).
    rescan_pending: bool,
    applied_theme: Option<ThemePreset>,
    applied_rate: u32,
    applied_reconnect: bool,
    #[cfg(target_os = "macos")]
    native_menu: Option<super::native_menu::NativeMenu>,
}

impl OakApp {
    pub(crate) fn new(cc: &eframe::CreationContext<'_>, s: Startup) -> Self {
        let mut applied_theme = None;
        theme::sync(&cc.egui_ctx, &mut applied_theme, s.cfg.ui.theme);
        let mut status = Status::default();
        for m in s.messages {
            status.message(m);
        }
        if s.sup.is_none() {
            status.stopped();
        }
        let tab = Tab::parse(&s.cfg.ui.tab).unwrap_or(Tab::Rgb);
        Self {
            sent: s.cfg.clone(),
            applied_rate: s.cfg.ui.thumbnail_fps,
            applied_reconnect: s.cfg.ui.auto_reconnect,
            path_text: super::display_path(&s.path),
            path: s.path,
            save_enabled: s.save_enabled,
            save_due: None,
            rescan_pending: s.sup.is_some(),
            sup: s.sup,
            starting: None,
            stopping: None,
            thumbs: Thumbs::new(Arc::clone(&s.taps)),
            taps: s.taps,
            status,
            tab,
            vcam_panel: VcamPanel::new(),
            setup: VcamSetup::new(s.outputs, s.fallback),
            settings: SettingsWindow::default(),
            connect_ip: None,
            about_open: false,
            banners: s.banners,
            ip: String::new(),
            script: s.script,
            applied_theme,
            cfg: s.cfg,
            #[cfg(target_os = "macos")]
            native_menu: super::native_menu::NativeMenu::build(&cc.egui_ctx),
        }
    }

    fn send(&self, cmd: Command) {
        if let Some(sup) = &self.sup {
            sup.send(cmd);
        }
    }

    fn connected(&self) -> bool {
        self.sup.is_some() || self.starting.is_some()
    }

    /// Device › Connect: starts the virtual cameras and a supervisor on a
    /// helper thread (after any previous one has shut down).
    fn start(&mut self) {
        if self.connected() {
            return;
        }
        let prev = self.stopping.take();
        let cfg = self.cfg.clone();
        let taps = Arc::clone(&self.taps);
        let (tx, rx) = crossbeam_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("gui-start".into())
            .spawn(move || {
                if let Some(h) = prev {
                    let _ = h.join();
                }
                let s = vcam_setup::start_sinks(&cfg, false);
                let (outputs, fallback) = (s.outputs, s.fallback);
                let started = Supervisor::spawn(cfg.clone(), s.sinks, options(&cfg, &taps))
                    .map(|sup| (sup, outputs, fallback, cfg));
                // Disconnected meanwhile: shut down what was started, device
                // open included, before a later start (which joins this
                // thread) runs.
                if let Err(unwanted) = tx.send(started)
                    && let Ok((sup, ..)) = unwanted.into_inner()
                {
                    sup.shutdown_and_release(RELEASE_TIMEOUT);
                }
            });
        match spawned {
            Ok(h) => {
                self.starting = Some((rx, h));
                self.status.link = Link::Booting;
                self.status.message("connecting…");
            }
            Err(e) => self.status.message(format!("cannot start: {e}")),
        }
    }

    /// Collects a finished [`Self::start`].
    fn poll_start(&mut self) {
        let Some((rx, _)) = &self.starting else {
            return;
        };
        let Ok(result) = rx.try_recv() else { return };
        self.starting = None;
        match result {
            Ok((sup, outputs, fallback, started_from)) => {
                self.sup = Some(sup);
                self.setup.set_outputs(outputs, fallback);
                self.rescan_pending = true;
                self.applied_reconnect = started_from.ui.auto_reconnect;
                if started_from != self.cfg {
                    self.send(Command::ApplyConfig(Box::new(self.cfg.clone())));
                }
            }
            Err(e) => {
                self.status.stopped();
                self.status
                    .message(format!("cannot start the supervisor: {e}"));
            }
        }
    }

    /// Device › Disconnect: shuts the supervisor down on a helper thread
    /// (closing the device and the virtual cameras), waiting for a device
    /// open still in flight. A start still running is abandoned (it shuts
    /// down what it started) and becomes the thread the next start waits
    /// for.
    fn stop(&mut self) {
        if let Some((_, h)) = self.starting.take() {
            self.stopping = Some(h);
        }
        if let Some(sup) = self.sup.take() {
            let prev = self.stopping.take();
            match std::thread::Builder::new()
                .name("gui-stop".into())
                .spawn(move || {
                    if let Some(h) = prev {
                        let _ = h.join();
                    }
                    sup.shutdown_and_release(RELEASE_TIMEOUT);
                }) {
                Ok(h) => self.stopping = Some(h),
                Err(e) => tracing::error!("cannot spawn the shutdown thread: {e}"),
            }
        }
        self.status.stopped();
        self.status.message("disconnected");
        self.setup.set_outputs([Output::Off; 3], None);
    }

    fn drain_events(&mut self) {
        let Some(sup) = &self.sup else { return };
        let events: Vec<Event> = sup.events().try_iter().collect();
        let finished = sup.is_finished();
        for ev in events {
            let settled = matches!(
                ev,
                Event::Connected(_) | Event::Disconnected(_) | Event::Reconnecting { .. }
            );
            if let Event::Profile { transport, .. } = &ev {
                self.cfg.apply_transport(*transport);
            }
            self.status.apply(ev);
            if settled && self.rescan_pending {
                self.rescan_pending = false;
                self.send(Command::Rescan);
            }
        }
        if finished && !matches!(self.status.link, Link::Error(_)) {
            self.status.link = Link::Error("the supervisor thread ended".into());
        }
    }

    /// Applies the `[ui]` settings that changed: theme, thumbnail rate,
    /// automatic reconnect; remembers the tab and window size.
    fn apply_ui_settings(&mut self, ctx: &egui::Context) {
        theme::sync(ctx, &mut self.applied_theme, self.cfg.ui.theme);
        if self.applied_rate != self.cfg.ui.thumbnail_fps {
            self.applied_rate = self.cfg.ui.thumbnail_fps;
            self.taps.set_rate(self.applied_rate);
        }
        if self.applied_reconnect != self.cfg.ui.auto_reconnect && self.sup.is_some() {
            self.applied_reconnect = self.cfg.ui.auto_reconnect;
            self.send(Command::SetAutoReconnect(self.applied_reconnect));
        }
        if self.cfg.ui.tab != self.tab.key() {
            self.tab.key().clone_into(&mut self.cfg.ui.tab);
        }
        let (rect, full) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.inner_rect,
                v.fullscreen.unwrap_or(false) || v.minimized.unwrap_or(false),
            )
        });
        if let Some(r) = rect
            && !full
            && r.width() >= 1.0
        {
            let size = [r.width().round() as u32, r.height().round() as u32];
            if self.cfg.ui.window_size != Some(size) {
                self.cfg.ui.window_size = Some(size);
            }
        }
    }

    /// Sends and schedules saving of any config change made this frame.
    fn commit(&mut self) {
        if self.cfg == self.sent {
            return;
        }
        for w in self.cfg.validate() {
            self.status.message(format!("config: {w}"));
        }
        if let Some(t) = self.status.transport {
            self.cfg.remember_profile(t);
        }
        if self.cfg == self.sent {
            return;
        }
        // The supervisor has no use for `[ui]`: a window resize or a theme
        // change is only saved.
        let mut without_ui = self.cfg.clone();
        without_ui.ui.clone_from(&self.sent.ui);
        if without_ui != self.sent {
            self.send(Command::ApplyConfig(Box::new(self.cfg.clone())));
        }
        self.sent = self.cfg.clone();
        self.save_due = Some(Instant::now() + SAVE_DEBOUNCE);
    }

    fn save(&mut self) {
        self.save_due = None;
        if !self.save_enabled {
            return;
        }
        if let Err(e) = self.cfg.save(&self.path) {
            tracing::warn!("gui: cannot save the config: {e}");
            self.status.message(format!("cannot save the config: {e}"));
        } else {
            tracing::info!("gui: config saved to {}", self.path.display());
        }
    }

    /// What the menus show.
    fn menu_state(&self) -> MenuState {
        MenuState {
            connected: self.connected(),
            cams: SensorId::ALL.map(|s| self.cfg.stream(s).enabled),
            stats: self.cfg.ui.stats_overlay,
            theme: self.cfg.ui.theme,
            tab: self.tab,
        }
    }

    /// Runs one menu action (native menu, egui menu, shortcut or script).
    pub(crate) fn apply_action(&mut self, ctx: &egui::Context, action: MenuAction) {
        tracing::debug!("menu action {action:?}");
        match action {
            MenuAction::About => self.about_open = true,
            MenuAction::OpenSettings => self.settings.open = true,
            MenuAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            MenuAction::Rescan => {
                if self.sup.is_some() {
                    self.send(Command::Rescan);
                } else {
                    self.status.message("not connected: Device › Connect first");
                }
            }
            MenuAction::ConnectToIp => {
                self.connect_ip = Some(self.cfg.ui.recent_ips.first().cloned().unwrap_or_default());
            }
            MenuAction::ToggleConnection => {
                if self.connected() {
                    self.stop();
                } else {
                    self.start();
                }
            }
            MenuAction::TriggerAf => self.send(Command::TriggerAf),
            MenuAction::ResetControls => self.settings.ask_reset_controls(),
            MenuAction::ToggleCamera(s) => {
                let st = camera_card::stream_mut(&mut self.cfg, s);
                st.enabled = !st.enabled;
            }
            MenuAction::InstallExtension => {
                self.tab = Tab::Vcam;
                self.setup.request_install();
            }
            MenuAction::ShowVcamStatus => {
                self.tab = Tab::Vcam;
                self.vcam_panel.refresh();
            }
            MenuAction::ShowTab(t) => self.tab = t,
            MenuAction::ToggleStats => self.cfg.ui.stats_overlay = !self.cfg.ui.stats_overlay,
            MenuAction::SetTheme(t) => self.cfg.ui.theme = t,
            MenuAction::OpenReadme => {
                if let Err(e) = super::open_url(menu::README_URL) {
                    self.status.message(format!("cannot open the README: {e}"));
                }
            }
            MenuAction::RevealConfig => self.reveal(&self.path.clone()),
            MenuAction::RevealLog => match super::log_path() {
                Some(p) => self.reveal(p),
                None => self.status.message("no log file in this session"),
            },
        }
        #[cfg(target_os = "macos")]
        if let Some(m) = &mut self.native_menu {
            m.resync();
        }
    }

    fn reveal(&mut self, path: &Path) {
        if let Err(e) = super::reveal(path) {
            self.status
                .message(format!("cannot show {}: {e}", path.display()));
        }
    }

    fn run_script(&mut self, ctx: &egui::Context) {
        let Some(script) = &mut self.script else {
            return;
        };
        let steps = script.due(Instant::now());
        for step in steps {
            self.status.message(format!("script: {step:?}"));
            match step {
                Step::Wait(_) => {}
                Step::Set(fragment) => match self.cfg.merged_toml(&fragment) {
                    Ok(c) => self.cfg = c,
                    Err(e) => self.status.message(format!("script: set failed: {e}")),
                },
                Step::Override { cam, on } => match cam.to_ascii_lowercase().as_str() {
                    "left" => self.cfg.mono.set_override(MonoCam::Left, on),
                    "right" => self.cfg.mono.set_override(MonoCam::Right, on),
                    other => self
                        .status
                        .message(format!("script: unknown camera {other}")),
                },
                Step::Tab(name) => match Tab::parse(&name) {
                    Some(t) => self.tab = t,
                    None => self.status.message(format!("script: unknown tab {name}")),
                },
                Step::TriggerAf => self.send(Command::TriggerAf),
                Step::Screenshot(path) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                        path,
                    )));
                }
                Step::Log(text) => self.status.message(format!("script: {text}")),
                Step::WindowSize([w, h]) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
                }
                Step::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Step::Settings(open) => {
                    self.settings.open = open;
                    if !open {
                        self.settings.confirm_reset = false;
                    }
                }
                Step::Theme(name) => match ThemePreset::lookup(&name) {
                    Some(t) => self.cfg.ui.theme = t,
                    None => self.status.message(format!("script: unknown theme {name}")),
                },
                Step::Menu(path) => match menu::find(&path) {
                    Some(a) => self.apply_action(ctx, a),
                    None => self.status.message(format!("script: no menu item {path}")),
                },
                Step::InstallVcam => self.setup.request_install(),
                Step::RetryVcam => self.setup.retry(&self.cfg),
            }
        }
    }

    fn save_screenshots(&mut self, ctx: &egui::Context) {
        let shots: Vec<(PathBuf, Arc<egui::ColorImage>)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot {
                        user_data, image, ..
                    } => user_data
                        .data
                        .as_ref()
                        .and_then(|d| d.downcast_ref::<PathBuf>())
                        .map(|p| (p.clone(), Arc::clone(image))),
                    _ => None,
                })
                .collect()
        });
        if !shots.is_empty()
            && let Some(script) = &mut self.script
        {
            script.shot_taken();
        }
        for (path, image) in shots {
            let [w, h] = image.size;
            let rgba: Vec<u8> = image
                .pixels
                .iter()
                .flat_map(egui::Color32::to_array)
                .collect();
            let bytes = png::encode_rgba(w as u32, h as u32, &rgba);
            match std::fs::write(&path, bytes) {
                Ok(()) => self
                    .status
                    .message(format!("screenshot {w}x{h} saved to {}", path.display())),
                Err(e) => self
                    .status
                    .message(format!("cannot write {}: {e}", path.display())),
            }
        }
    }

    fn message_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let text = self.status.recent(MESSAGE_TTL).unwrap_or("");
            let resp = ui.label(egui::RichText::new(text).small());
            if !self.status.messages.is_empty() {
                resp.on_hover_ui(|ui| {
                    for (t, m) in self.status.messages.iter().rev().take(15) {
                        ui.label(format!("{:>4} s ago  {m}", t.elapsed().as_secs()));
                    }
                });
            }
        });
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        for b in &self.banners {
            theme::group_frame(ui).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.colored_label(theme::WARN, b);
            });
        }
        if self.setup.banner(ui, &self.cfg).quit {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.status.link == Link::Stopped && self.starting.is_none() {
            theme::group_frame(ui).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label("Not streaming.");
                    if ui.button("Connect").clicked() {
                        self.start();
                    }
                });
            });
        }
        if self.status.info.is_some() && self.status.usb == depthai::UsbSpeed::High {
            theme::group_frame(ui).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.colored_label(theme::ERROR, "Connected at USB2, use a USB3 cable/port");
            });
        }
        let stats = self.cfg.ui.stats_overlay;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let spacing = ui.spacing().item_spacing.x;
                let (cols, w) = camera_card::card_layout(ui.available_width(), spacing);
                for row in SensorId::ALL.chunks(cols) {
                    ui.horizontal_top(|ui| {
                        for &sensor in row {
                            let view = self.thumbs.view(sensor);
                            camera_card::show(
                                ui,
                                sensor,
                                &mut self.cfg,
                                &self.status,
                                &view,
                                self.setup.outputs[sensor.index()],
                                w,
                                stats,
                            );
                        }
                    });
                }
            });
    }

    /// Device › Connect to IP…
    fn connect_ip_window(&mut self, ctx: &egui::Context) {
        let Some(text) = &mut self.connect_ip else {
            return;
        };
        let mut open = true;
        let mut chosen = None;
        let mut cancel = false;
        egui::Window::new("Connect to IP")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label("Address of an OAK PoE device:");
                let resp = ui.add(
                    egui::TextEdit::singleline(text)
                        .hint_text("192.0.2.44")
                        .desired_width(220.0),
                );
                resp.request_focus();
                let valid = top_bar::parse_ip(text);
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    let connect = ui
                        .add_enabled(valid.is_some(), egui::Button::new("Connect"))
                        .on_disabled_hover_text("Type an IPv4 or IPv6 address");
                    if connect.clicked() || (enter && valid.is_some()) {
                        chosen.clone_from(&valid);
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if let Some(addr) = chosen {
            top_bar::remember_ip(&mut self.cfg.ui.recent_ips, &addr);
            self.cfg.device = DeviceSelection::ByIp(addr);
            self.connect_ip = None;
            if !self.connected() {
                self.start();
            }
        } else if !open || cancel || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.connect_ip = None;
        }
    }

    /// oak-cam › About (the native About panel on macOS; this window
    /// elsewhere and from scripts).
    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        egui::Window::new("About oak-cam")
            .open(&mut self.about_open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.heading("oak-cam");
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.label("A Luxonis OAK-D Pro as three virtual webcams.");
                ui.hyperlink(menu::README_URL);
            });
    }
}

impl eframe::App for OakApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        #[cfg(target_os = "macos")]
        let actions = self
            .native_menu
            .as_ref()
            .map(super::native_menu::NativeMenu::drain_actions)
            .unwrap_or_default();
        #[cfg(not(target_os = "macos"))]
        let actions = menu::shortcuts(&ctx);
        for a in actions {
            self.apply_action(&ctx, a);
        }
        self.poll_start();
        self.drain_events();
        self.run_script(&ctx);
        self.save_screenshots(&ctx);
        if let Some(sinks) = self.setup.poll(&self.cfg) {
            if self.sup.is_some() {
                self.status
                    .message("switching to the OS virtual cameras (the pipeline restarts)");
                self.send(Command::ReplaceSinks(sinks));
            } else {
                self.setup.set_outputs([Output::Off; 3], None);
            }
        }
        self.apply_ui_settings(&ctx);
        self.thumbs.refresh(&ctx);

        #[cfg(not(target_os = "macos"))]
        {
            let state = self.menu_state();
            let mut chosen = None;
            egui::Panel::top("menu_bar").show(ui, |ui| chosen = menu::menu_bar(ui, state));
            if let Some(a) = chosen {
                self.apply_action(&ctx, a);
            }
        }
        egui::Panel::top("top_bar").show(ui, |ui| {
            let before = self.cfg.device.clone();
            let out = top_bar::show(ui, &mut self.cfg, &self.status, &mut self.ip);
            if self.cfg.device != before {
                tracing::info!("gui: device selected: {}", self.cfg.device);
            }
            if out.rescan {
                self.apply_action(&ctx, MenuAction::Rescan);
            }
        });
        egui::Panel::bottom("message_bar").show(ui, |ui| self.message_bar(ui));
        egui::Panel::right("side")
            .resizable(true)
            .default_size(380.0)
            .size_range(320.0..=560.0)
            .show(ui, |ui| {
                let out = panels::show(
                    ui,
                    &mut self.tab,
                    &mut self.cfg,
                    &self.status,
                    &mut self.vcam_panel,
                    &mut self.setup,
                    &self.path_text,
                );
                if out.trigger_af {
                    self.send(Command::TriggerAf);
                }
            });
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));
        let out = self
            .settings
            .show(&ctx, &mut self.cfg, &self.status, &self.path_text);
        if out.reveal_config {
            self.apply_action(&ctx, MenuAction::RevealConfig);
        }
        self.connect_ip_window(&ctx);
        self.about_window(&ctx);

        #[cfg(target_os = "macos")]
        {
            let state = self.menu_state();
            if let Some(m) = &mut self.native_menu {
                m.sync(state);
            }
        }
        self.commit();
        if self.save_due.is_some_and(|t| Instant::now() >= t) {
            self.save();
        }
        let mut next = REPAINT;
        if let Some(d) = self.script.as_ref().and_then(|s| s.next_in(Instant::now())) {
            next = next.min(d.max(Duration::from_millis(5)));
        }
        ctx.request_repaint_after(next);
    }

    fn on_exit(&mut self) {
        if let Some(sup) = self.sup.take() {
            eprintln!("oak-cam gui: shutting down");
            sup.shutdown();
        }
        if let Some((rx, h)) = self.starting.take() {
            if let Ok(Ok((sup, ..))) = rx.recv_timeout(RELEASE_TIMEOUT) {
                sup.shutdown();
            }
            drop(rx);
            let _ = h.join();
        }
        if let Some(h) = self.stopping.take() {
            let _ = h.join();
        }
        if self.cfg != self.sent || self.save_due.is_some() {
            self.save();
        }
        eprintln!("oak-cam gui: exited cleanly");
    }
}
