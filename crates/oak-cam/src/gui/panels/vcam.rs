//! Virtual cameras tab: where the OS side stands (macOS extension state
//! and Install/Update; Windows registration; Linux loopback devices), and
//! which sensors publish. Status probes run on a helper thread so the
//! window never waits on them; the install request is [`VcamSetup`]'s.

use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use egui::Ui;
use oak_core::capture::SensorId;
use oak_core::config::{Config, VcamConfig, VcamOutput};

use crate::gui::theme;
use crate::gui::vcam_setup::VcamSetup;

/// How often the status is re-read while the tab is visible.
const REFRESH: Duration = Duration::from_secs(5);

/// A platform status snapshot, as label/value rows.
#[derive(Clone, Debug, Default)]
pub(crate) struct Report {
    pub rows: Vec<(String, String)>,
    /// Every camera is usable.
    pub ok: bool,
    /// What to do next, when not `ok`.
    pub steps: Option<String>,
}

pub(crate) struct VcamPanel {
    report: Option<Report>,
    job: Option<Receiver<Report>>,
    last_refresh: Option<Instant>,
    /// The install state seen last frame (a change re-reads the status).
    was_busy: bool,
}

fn output_mut(v: &mut VcamConfig, sensor: SensorId) -> &mut VcamOutput {
    match sensor {
        SensorId::Rgb => &mut v.rgb,
        SensorId::Left => &mut v.left,
        SensorId::Right => &mut v.right,
    }
}

impl VcamPanel {
    pub(crate) fn new() -> Self {
        Self {
            report: None,
            job: None,
            last_refresh: None,
            was_busy: false,
        }
    }

    /// Re-read the status on the next frame.
    pub(crate) fn refresh(&mut self) {
        self.last_refresh = None;
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.job
            && let Ok(r) = rx.try_recv()
        {
            self.report = Some(r);
            self.job = None;
        }
        let due = self.last_refresh.is_none_or(|t| t.elapsed() >= REFRESH);
        if due && self.job.is_none() {
            let (tx, rx) = crossbeam_channel::bounded(1);
            let spawned = std::thread::Builder::new()
                .name("vcam-status".into())
                .spawn(move || {
                    let _ = tx.send(probe());
                });
            if spawned.is_ok() {
                self.job = Some(rx);
            }
            self.last_refresh = Some(Instant::now());
        }
    }

    /// Draws the tab.
    pub(crate) fn show(&mut self, ui: &mut Ui, cfg: &mut Config, setup: &mut VcamSetup) {
        let busy = setup.busy();
        if busy != self.was_busy {
            self.was_busy = busy;
            self.refresh();
        }
        self.poll();
        if let Some(why) = &setup.fallback {
            theme::group_frame(ui).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.colored_label(theme::WARN, "Publishing to null cameras this session");
                ui.label(why);
            });
        }
        theme::section(ui, "Status", |ui| match &self.report {
            None => {
                ui.spinner();
            }
            Some(r) => {
                egui::Grid::new("vcam-status")
                    .num_columns(2)
                    .show(ui, |ui| {
                        for (k, v) in &r.rows {
                            ui.label(k);
                            ui.label(v);
                            ui.end_row();
                        }
                    });
                if r.ok {
                    ui.colored_label(theme::OK, "All three cameras are available.");
                }
                if let Some(steps) = &r.steps {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(steps).monospace().small());
                }
            }
        });
        if cfg!(target_os = "macos") {
            theme::section(ui, "Camera extension", |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("Install / Update"))
                        .on_hover_text(
                            "Submit the camera extension embedded in this app to macOS \
                             (works when running from /Applications/oak-cam.app)",
                        )
                        .clicked()
                    {
                        setup.request_install();
                    }
                    if busy {
                        ui.spinner();
                    }
                });
                if let Some(t) = &setup.install_text {
                    ui.label(t);
                }
            });
        }
        theme::section(ui, "Outputs", |ui| {
            for sensor in SensorId::ALL {
                let out = output_mut(&mut cfg.vcam, sensor);
                ui.checkbox(
                    &mut out.enabled,
                    format!("Publish {}", sensor.camera_id().name()),
                );
            }
            ui.weak("Output changes take effect when oak-cam restarts.");
        });
    }
}

#[cfg(target_os = "macos")]
fn probe() -> Report {
    use std::path::Path;
    use vcam::CameraId;
    use vcam::macos::install::{self, SystemState};
    use vcam::macos::{self as mac, APP_INSTALL_PATH};

    let mut rows = Vec::new();
    let app = Path::new(APP_INSTALL_PATH);
    rows.push((
        "App bundle".into(),
        if install::embedded_extension(app).exists() {
            format!("{APP_INSTALL_PATH} (extension embedded)")
        } else if app.exists() {
            format!("{APP_INSTALL_PATH} (no embedded extension)")
        } else {
            "not installed".into()
        },
    ));
    rows.push((
        "Running from".into(),
        install::bundle_path().map_or_else(
            || "not an app bundle".to_owned(),
            |b| b.display().to_string(),
        ),
    ));
    let system = install::query_system_state();
    rows.push((
        "Extension".into(),
        match &system {
            Ok(SystemState::NotInstalled) => "not installed".into(),
            Ok(SystemState::AwaitingApproval { version }) => {
                format!("waiting for approval ({version})")
            }
            Ok(SystemState::Enabled { version }) => format!("activated and enabled ({version})"),
            Ok(SystemState::Other { version, state }) => format!("{state} ({version})"),
            Err(e) => format!("systemextensionsctl failed: {e}"),
        },
    ));
    let mut listed = 0;
    for id in CameraId::ALL {
        let text = match mac::status(id) {
            mac::Status::Active { device } => {
                listed += 1;
                format!("listed (device {device})")
            }
            mac::Status::NotInstalled => "not listed".into(),
        };
        rows.push((id.name().to_owned(), text));
    }
    let ok = listed == CameraId::ALL.len();
    let steps = (!ok).then(|| {
        let mut buf = Vec::new();
        let _ = crate::cmd_vcam::write_macos_steps(&mut buf);
        String::from_utf8_lossy(&buf).trim().to_owned()
    });
    Report { rows, ok, steps }
}

#[cfg(target_os = "windows")]
fn probe() -> Report {
    let status = vcam::windows::status();
    let ok = vcam::windows::available();
    Report {
        rows: vec![("Media source".into(), format!("{status:?}"))],
        ok,
        steps: (!ok).then(|| {
            format!(
                "{}\n\nAdministrator PowerShell, in the oak-cam folder:\n  {}",
                vcam::windows::missing_message(),
                vcam::windows::INSTALL_COMMAND
            )
        }),
    }
}

#[cfg(target_os = "linux")]
fn probe() -> Report {
    match vcam::linux::find_loopback_device() {
        Some(path) => Report {
            rows: vec![("v4l2loopback".into(), path)],
            ok: true,
            steps: None,
        },
        None => Report {
            rows: vec![("v4l2loopback".into(), "no output device found".into())],
            ok: false,
            steps: Some(format!("Load it with:\n  {}", vcam::linux::SETUP_HINT)),
        },
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn probe() -> Report {
    Report {
        rows: vec![("Backend".into(), "none on this platform".into())],
        ok: false,
        steps: None,
    }
}
