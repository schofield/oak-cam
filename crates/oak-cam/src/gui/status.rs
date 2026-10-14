//! Supervisor events folded into what the window shows: the link state
//! behind the status chip, the open device, the latest stats per sensor,
//! the discovered devices, and a short message log.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use depthai::{DeviceInfo, UsbSpeed};
use oak_core::capture::SensorId;
use oak_core::config::{StreamProfile, Transport};
use oak_core::supervisor::{ConnectedInfo, Event, SensorReport};

/// Messages kept for the message bar's hover list.
const MAX_MESSAGES: usize = 30;

/// Where the device link stands.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Link {
    /// Opening the device / starting a pipeline.
    Booting,
    /// Open and streaming.
    Connected,
    /// Waiting before reconnect attempt `n`.
    Reconnecting(u32),
    /// Lost, or the pipeline cannot run with this config.
    Error(String),
    /// Not streaming: disconnected by the user, or not started at launch.
    Stopped,
}

/// The chip text for `link`; `Idle` when connected with every stream off.
pub(crate) fn chip_text(link: &Link, any_enabled: bool) -> String {
    match link {
        Link::Booting => "Booting".into(),
        Link::Connected if !any_enabled => "Idle".into(),
        Link::Connected => "Connected".into(),
        Link::Reconnecting(n) => format!("Reconnecting ({n})"),
        Link::Error(_) => "Error".into(),
        Link::Stopped => "Disconnected".into(),
    }
}

/// `RGB 1280x720@30<sep>mono 640x400@30`, plus `left` / `right` lines for
/// mono overrides with their own shape.
pub(crate) fn shapes_text(p: &StreamProfile, sep: &str) -> String {
    let mut parts = vec![format!("RGB {}", p.rgb), format!("mono {}", p.mono)];
    if let Some(l) = p.left {
        parts.push(format!("left {l}"));
    }
    if let Some(r) = p.right {
        parts.push(format!("right {r}"));
    }
    parts.join(sep)
}

/// Everything the supervisor told us.
#[derive(Debug)]
pub(crate) struct Status {
    pub link: Link,
    pub info: Option<ConnectedInfo>,
    pub stats: [Option<SensorReport>; 3],
    pub temp_c: Option<f32>,
    pub usb: UsbSpeed,
    pub lost: [bool; 3],
    pub devices: Vec<DeviceInfo>,
    pub scanned: bool,
    pub messages: VecDeque<(Instant, String)>,
    /// Restarts seen (for the status bar and scripted checks).
    pub restarts: u32,
    /// The open device's link type, once its profile is applied.
    pub transport: Option<Transport>,
    /// The applied profile is the user's rather than the built-in one.
    pub profile_custom: bool,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            link: Link::Booting,
            info: None,
            stats: [None, None, None],
            temp_c: None,
            usb: UsbSpeed::Unknown,
            lost: [false; 3],
            devices: Vec::new(),
            scanned: false,
            messages: VecDeque::new(),
            restarts: 0,
            transport: None,
            profile_custom: false,
        }
    }
}

impl Status {
    /// Adds a line to the message log (and stderr).
    pub(crate) fn message(&mut self, text: impl Into<String>) {
        let text = text.into();
        eprintln!("oak-cam gui: {text}");
        if self.messages.len() == MAX_MESSAGES {
            self.messages.pop_front();
        }
        self.messages.push_back((Instant::now(), text));
    }

    /// The newest message if it is younger than `max_age`.
    pub(crate) fn recent(&self, max_age: Duration) -> Option<&str> {
        self.messages
            .back()
            .filter(|(t, _)| t.elapsed() <= max_age)
            .map(|(_, m)| m.as_str())
    }

    /// Folds one event in.
    pub(crate) fn apply(&mut self, ev: Event) {
        match ev {
            Event::Connected(info) => {
                tracing::info!(
                    "supervisor: connected: {} ({}, {} {})",
                    info.id,
                    info.name,
                    info.protocol,
                    info.usb_speed
                );
                self.message(format!(
                    "connected to {} ({}, {} {})",
                    info.id, info.name, info.protocol, info.usb_speed
                ));
                self.usb = info.usb_speed;
                self.link = Link::Connected;
                self.lost = [false; 3];
                self.info = Some(info);
                self.transport = None;
            }
            Event::Profile {
                transport,
                profile,
                changed,
            } => {
                let custom = profile != transport.defaults();
                let shapes = shapes_text(&profile, ", ");
                tracing::info!(
                    "supervisor: profile: {transport} {shapes} (custom {custom}, changed {changed})"
                );
                if changed {
                    let which = if custom { "custom " } else { "" };
                    self.message(format!("{transport} link: streams set to {which}{shapes}"));
                } else if custom {
                    self.message(format!(
                        "{transport} link: keeping custom streams ({shapes})"
                    ));
                }
                self.transport = Some(transport);
                self.profile_custom = custom;
            }
            Event::Disconnected(reason) => {
                tracing::info!("supervisor: disconnected: {reason}");
                self.message(format!("disconnected: {reason}"));
                self.link = Link::Error(reason);
                self.stats = [None, None, None];
                self.transport = None;
            }
            Event::Reconnecting { attempt, delay } => {
                tracing::info!(
                    "supervisor: reconnecting: attempt {attempt} in {} s",
                    delay.as_secs()
                );
                self.message(format!(
                    "reconnecting in {} s (attempt {attempt})",
                    delay.as_secs()
                ));
                self.link = Link::Reconnecting(attempt);
                self.transport = None;
            }
            Event::Restarted(reason) => {
                tracing::info!("supervisor: restarted: {reason}");
                self.restarts += 1;
                self.message(format!("pipeline restarted: {reason}"));
                self.link = Link::Booting;
                self.stats = [None, None, None];
            }
            Event::Stats(report) => {
                self.stats = [None, None, None];
                self.temp_c = report.temp_c;
                self.usb = report.usb;
                for s in report.sensors {
                    let i = s.sensor.index();
                    self.stats[i] = Some(s);
                }
            }
            Event::StreamLost(sensor) => {
                tracing::info!("supervisor: stream lost: {sensor}");
                self.message(format!("{sensor}: stream lost"));
                self.lost[sensor.index()] = true;
            }
            Event::Warning(w) => {
                tracing::warn!("supervisor: warning: {w}");
                if w.contains("waiting for a config change") {
                    self.link = Link::Error(w.clone());
                }
                self.message(w);
            }
            Event::Devices(list) => {
                tracing::debug!("supervisor: devices: {} found", list.len());
                self.scanned = true;
                self.devices = list;
            }
        }
    }

    /// No supervisor any more (Device › Disconnect): forget the device.
    pub(crate) fn stopped(&mut self) {
        self.link = Link::Stopped;
        self.info = None;
        self.stats = [None, None, None];
        self.temp_c = None;
        self.usb = UsbSpeed::Unknown;
        self.lost = [false; 3];
        self.transport = None;
        self.profile_custom = false;
    }

    /// The latest stats for `sensor`.
    pub(crate) fn sensor(&self, sensor: SensorId) -> Option<&SensorReport> {
        self.stats[sensor.index()].as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use depthai::Protocol;
    use oak_core::capture::StatsSnapshot;
    use oak_core::supervisor::StatsReport;

    #[test]
    fn events_drive_the_chip() {
        let mut s = Status::default();
        assert_eq!(chip_text(&s.link, true), "Booting");
        s.apply(Event::Connected(ConnectedInfo {
            id: "X".into(),
            name: "OAK-D-PRO-AF".into(),
            protocol: Protocol::Usb,
            usb_speed: UsbSpeed::SuperPlus,
            ir_drivers: 1,
        }));
        assert_eq!(chip_text(&s.link, true), "Connected");
        assert_eq!(chip_text(&s.link, false), "Idle");
        assert_eq!(s.transport, None);
        s.apply(Event::Profile {
            transport: Transport::Poe,
            profile: Transport::Poe.defaults(),
            changed: true,
        });
        assert_eq!(s.transport, Some(Transport::Poe));
        assert!(!s.profile_custom);
        assert_eq!(
            s.recent(Duration::from_secs(5)),
            Some("PoE link: streams set to RGB 1280x720@20, mono 640x400@20")
        );
        let mut custom = Transport::Poe.defaults();
        custom.rgb.fps = 15.0;
        s.apply(Event::Profile {
            transport: Transport::Poe,
            profile: custom,
            changed: false,
        });
        assert!(s.profile_custom);
        custom.left = Some(custom.mono);
        assert_eq!(
            shapes_text(&custom, " | "),
            "RGB 1280x720@15 | mono 640x400@20 | left 640x400@20"
        );
        assert_eq!(
            s.recent(Duration::from_secs(5)),
            Some("PoE link: keeping custom streams (RGB 1280x720@15, mono 640x400@20)")
        );
        s.apply(Event::Stats(StatsReport {
            sensors: vec![SensorReport {
                sensor: SensorId::Left,
                stats: StatsSnapshot {
                    fps: 30.0,
                    ..StatsSnapshot::default()
                },
                vcam: Some((10, 0)),
            }],
            usb: UsbSpeed::SuperPlus,
            temp_c: Some(48.0),
        }));
        assert!(s.sensor(SensorId::Left).is_some());
        assert!(s.sensor(SensorId::Rgb).is_none());
        assert_eq!(s.temp_c, Some(48.0));
        s.apply(Event::Restarted("left 1280x800@30 -> 640x400@30".into()));
        assert_eq!(chip_text(&s.link, true), "Booting");
        assert!(s.sensor(SensorId::Left).is_none());
        assert_eq!(s.restarts, 1);
        s.apply(Event::Disconnected("gone".into()));
        assert_eq!(chip_text(&s.link, true), "Error");
        assert_eq!(s.transport, None);
        s.apply(Event::Reconnecting {
            attempt: 2,
            delay: Duration::from_secs(2),
        });
        assert_eq!(chip_text(&s.link, true), "Reconnecting (2)");
        s.apply(Event::Warning(
            "cannot start; device closed, waiting for a config change".into(),
        ));
        assert!(matches!(s.link, Link::Error(_)));
        assert!(s.recent(Duration::from_secs(5)).is_some());
        s.stopped();
        assert_eq!(chip_text(&s.link, true), "Disconnected");
        assert!(s.info.is_none() && s.sensor(SensorId::Left).is_none());
    }
}
