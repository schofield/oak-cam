//! Top bar: device picker, Rescan, manual IP + Connect (recent IPs),
//! status chip, chip temperature, USB2 badge, the bandwidth estimate and
//! the link's stream profile (with Reset when it is the user's own).

use std::net::IpAddr;

use depthai::{DeviceInfo, Protocol, UsbSpeed};
use egui::Ui;
use oak_core::capture::SensorId;
use oak_core::config::{Config, DeviceSelection, Transport};

use super::bandwidth::{self, WARN_RATIO};
use super::status::{Link, Status, chip_text, shapes_text};
use super::theme;

/// Recent IPs remembered.
pub(crate) const MAX_RECENT_IPS: usize = 8;

/// What the top bar asks the app to do beyond editing the config.
#[derive(Debug, Default)]
pub(crate) struct TopBarOut {
    pub rescan: bool,
}

/// Transport text for a device row: `USB3` / `USB2` for the open device,
/// `USB` for others, `PoE` for TCP/IP.
pub(crate) fn transport(protocol: Protocol, usb: Option<UsbSpeed>) -> &'static str {
    match (protocol, usb) {
        (Protocol::TcpIp, _) => "PoE",
        (_, Some(UsbSpeed::Super | UsbSpeed::SuperPlus)) => "USB3",
        (_, Some(UsbSpeed::High)) => "USB2",
        (_, Some(UsbSpeed::Low | UsbSpeed::Full)) => "USB1",
        _ => "USB",
    }
}

/// `name · id · USB3/PoE · state`.
pub(crate) fn device_row(d: &DeviceInfo, open: Option<(&str, UsbSpeed)>) -> String {
    let usb = open.filter(|(id, _)| *id == d.id).map(|(_, s)| s);
    format!(
        "{} · {} · {} · {}",
        d.name,
        d.id,
        transport(d.protocol, usb),
        d.state
    )
}

/// Records `ip` as the newest recent IP (deduplicated, capped).
pub(crate) fn remember_ip(recent: &mut Vec<String>, ip: &str) {
    recent.retain(|x| x != ip);
    recent.insert(0, ip.to_owned());
    recent.truncate(MAX_RECENT_IPS);
}

/// Validates a typed address.
pub(crate) fn parse_ip(text: &str) -> Option<String> {
    let t = text.trim();
    t.parse::<IpAddr>().ok().map(|_| t.to_owned())
}

fn selection_text(cfg: &Config, status: &Status) -> String {
    match (&cfg.device, &status.info) {
        (DeviceSelection::Auto, Some(i)) => format!("Auto · {} · {}", i.name, i.id),
        (DeviceSelection::Auto, None) => "Auto (first found)".into(),
        (DeviceSelection::ById(id), _) => format!("Id {id}"),
        (DeviceSelection::ByIp(ip), _) => format!("IP {ip}"),
    }
}

/// Draws the bar.
pub(crate) fn show(ui: &mut Ui, cfg: &mut Config, status: &Status, ip: &mut String) -> TopBarOut {
    let mut out = TopBarOut::default();
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("oak-cam").strong());
        ui.separator();
        let open = status.info.as_ref().map(|i| (i.id.as_str(), status.usb));
        egui::ComboBox::from_id_salt("device")
            .width(300.0)
            .selected_text(selection_text(cfg, status))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut cfg.device, DeviceSelection::Auto, "Auto (first found)");
                for d in &status.devices {
                    let sel = if d.protocol == Protocol::TcpIp && !d.name.is_empty() {
                        DeviceSelection::ByIp(d.name.clone())
                    } else {
                        DeviceSelection::ById(d.id.clone())
                    };
                    ui.selectable_value(&mut cfg.device, sel, device_row(d, open));
                }
                if status.devices.is_empty() {
                    ui.weak(if status.scanned {
                        "No devices found (Rescan)"
                    } else {
                        "Scanning…"
                    });
                }
            });
        if ui.button("Rescan").clicked() {
            out.rescan = true;
        }
        ui.separator();
        let edit = egui::TextEdit::singleline(ip)
            .hint_text("PoE IP address")
            .desired_width(120.0);
        let resp = ui.add(edit);
        if !cfg.ui.recent_ips.is_empty() {
            egui::ComboBox::from_id_salt("recent-ips")
                .width(24.0)
                .selected_text("")
                .show_ui(ui, |ui| {
                    for r in &cfg.ui.recent_ips {
                        if ui.selectable_label(false, r).clicked() {
                            ip.clone_from(r);
                        }
                    }
                });
        }
        let valid = parse_ip(ip);
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let connect = ui
            .add_enabled(valid.is_some(), egui::Button::new("Connect"))
            .on_disabled_hover_text("Type an IPv4 or IPv6 address");
        if let Some(addr) = valid
            && (connect.clicked() || enter)
        {
            remember_ip(&mut cfg.ui.recent_ips, &addr);
            cfg.device = DeviceSelection::ByIp(addr);
        }
        ui.separator();

        let any_enabled = SensorId::ALL.into_iter().any(|s| cfg.stream(s).enabled);
        let color = match &status.link {
            Link::Connected if any_enabled => theme::OK,
            Link::Connected | Link::Stopped => theme::IDLE,
            Link::Booting | Link::Reconnecting(_) => theme::WARN,
            Link::Error(_) => theme::ERROR,
        };
        let chip = theme::chip(ui, &link_chip_text(cfg, status, any_enabled), color);
        if let Link::Error(why) = &status.link {
            chip.on_hover_text(why);
        } else if opening_poe(cfg, status) {
            chip.on_hover_text(
                "Opening a PoE camera can take up to 90 s: on a cable without DHCP it \
                 waits ~30 s for an address before falling back to 169.254.1.222",
            );
        }
        if let Some(t) = status.temp_c {
            ui.label(format!("{t:.0} °C"))
                .on_hover_text("Chip temperature");
        }
        if status.info.is_some() && status.usb == UsbSpeed::High {
            theme::chip(ui, "USB2", theme::ERROR)
                .on_hover_text("Connected at USB2, use a USB3 cable/port");
        }
        bandwidth_label(ui, cfg, status);
        if let Some(t) = status.transport {
            profile_chip(ui, cfg, t);
        }
    });
    out
}

/// Whether the selected device is a network (PoE) camera: an IP; an id
/// that the last scan listed over TCP/IP, or that is the last device open
/// and was PoE; for `Auto`, the first device of the last scan (as the shim
/// picks it), or the last device open when there is no scan.
pub(crate) fn poe_selected(cfg: &Config, status: &Status) -> bool {
    let last_poe = |id: Option<&str>| {
        status
            .info
            .as_ref()
            .is_some_and(|i| i.protocol == Protocol::TcpIp && id.is_none_or(|id| i.id == id))
    };
    match &cfg.device {
        DeviceSelection::ByIp(_) => true,
        DeviceSelection::ById(id) => {
            status
                .devices
                .iter()
                .any(|d| d.id == *id && d.protocol == Protocol::TcpIp)
                || last_poe(Some(id))
        }
        DeviceSelection::Auto => match status.devices.first() {
            Some(d) => d.protocol == Protocol::TcpIp,
            None => last_poe(None),
        },
    }
}

/// A PoE camera is being opened (booting, or waiting to reconnect), which
/// can take ~30 s on a link without DHCP.
pub(crate) fn opening_poe(cfg: &Config, status: &Status) -> bool {
    matches!(status.link, Link::Booting | Link::Reconnecting(_)) && poe_selected(cfg, status)
}

/// The status chip's text: [`chip_text`], or `Booting (PoE, up to 90 s)`
/// while a PoE camera boots. Reconnecting keeps its text; the chip's hover
/// carries the PoE note.
pub(crate) fn link_chip_text(cfg: &Config, status: &Status, any_enabled: bool) -> String {
    if status.link == Link::Booting && poe_selected(cfg, status) {
        "Booting (PoE, up to 90 s)".into()
    } else {
        chip_text(&status.link, any_enabled)
    }
}

/// The link load estimate: bytes against the budget on USB, the per-frame
/// model on PoE (see [`bandwidth::link_load`]).
fn bandwidth_label(ui: &mut Ui, cfg: &Config, status: &Status) {
    let protocol = status.info.as_ref().map(|i| i.protocol);
    let l = bandwidth::link_load(cfg, protocol, status.usb);
    let color = if l.load > WARN_RATIO {
        theme::WARN
    } else {
        ui.visuals().text_color()
    };
    ui.colored_label(color, l.label).on_hover_text(l.hover);
}

/// `PoE defaults` / `PoE custom`.
pub(crate) fn profile_text(cfg: &Config, t: Transport) -> String {
    let kind = if cfg.profiles.get(t).is_some() {
        "custom"
    } else {
        "defaults"
    };
    format!("{} {kind}", t.label())
}

/// The active link's profile chip, and Reset when it is custom.
fn profile_chip(ui: &mut Ui, cfg: &mut Config, t: Transport) {
    let custom = cfg.profiles.get(t).is_some();
    let p = cfg.profile(t);
    let color = if custom { theme::WARN } else { theme::IDLE };
    theme::chip(ui, &profile_text(cfg, t), color).on_hover_text(format!(
        "Stream profile for {} links: {}.\nSize and fps edits made while \
         connected over {} are remembered for that link type.",
        t.label(),
        shapes_text(&p, ", "),
        t.label()
    ));
    if custom
        && ui
            .small_button("Reset")
            .on_hover_text(format!(
                "Back to the {} defaults: {}",
                t.label(),
                t.defaults()
            ))
            .clicked()
    {
        cfg.reset_profile(t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use depthai::{DeviceState, Platform};

    #[test]
    fn device_rows() {
        let d = DeviceInfo {
            name: "1.1".into(),
            id: "1844301041C7".into(),
            protocol: Protocol::Usb,
            state: DeviceState::Booted,
            platform: Platform::Rvc2,
        };
        assert_eq!(device_row(&d, None), "1.1 · 1844301041C7 · USB · BOOTED");
        assert_eq!(
            device_row(&d, Some(("1844301041C7", UsbSpeed::SuperPlus))),
            "1.1 · 1844301041C7 · USB3 · BOOTED"
        );
        assert_eq!(
            device_row(&d, Some(("other", UsbSpeed::High))),
            "1.1 · 1844301041C7 · USB · BOOTED"
        );
        let poe = DeviceInfo {
            protocol: Protocol::TcpIp,
            name: "192.0.2.44".into(),
            state: DeviceState::Bootloader,
            ..d
        };
        assert_eq!(
            device_row(&poe, None),
            "192.0.2.44 · 1844301041C7 · PoE · BOOTLOADER"
        );
    }

    #[test]
    fn poe_boot_chip_text() {
        let mut cfg = Config::default();
        let mut status = Status::default();
        assert_eq!(link_chip_text(&cfg, &status, true), "Booting");
        cfg.device = DeviceSelection::ByIp("169.254.1.222".into());
        assert_eq!(
            link_chip_text(&cfg, &status, true),
            "Booting (PoE, up to 90 s)"
        );
        status.link = Link::Connected;
        assert_eq!(link_chip_text(&cfg, &status, true), "Connected");
        // Auto selection, restarting a PoE device.
        cfg.device = DeviceSelection::Auto;
        status.info = Some(oak_core::supervisor::ConnectedInfo {
            id: "X".into(),
            name: "OAK-D-PRO-POE".into(),
            protocol: Protocol::TcpIp,
            usb_speed: UsbSpeed::Unknown,
            ir_drivers: 1,
        });
        status.link = Link::Booting;
        assert_eq!(
            link_chip_text(&cfg, &status, true),
            "Booting (PoE, up to 90 s)"
        );
        // Reconnecting to it: same text, PoE note in the hover.
        status.link = Link::Reconnecting(2);
        assert_eq!(link_chip_text(&cfg, &status, true), "Reconnecting (2)");
        assert!(opening_poe(&cfg, &status));
        // Auto with a scan: the first listed device decides, here USB.
        let usb = DeviceInfo {
            name: "1.1".into(),
            id: "1844301041C7".into(),
            protocol: Protocol::Usb,
            state: DeviceState::Unbooted,
            platform: Platform::Rvc2,
        };
        status.devices = vec![usb];
        assert!(!poe_selected(&cfg, &status));
        assert!(!opening_poe(&cfg, &status));
        status.devices[0].protocol = Protocol::TcpIp;
        assert!(poe_selected(&cfg, &status));
        // By id: PoE when the scan listed it over TCP/IP.
        let mut status = Status::default();
        cfg.device = DeviceSelection::ById("14442C10".into());
        assert!(!poe_selected(&cfg, &status));
        let d = DeviceInfo {
            name: "192.0.2.44".into(),
            id: "14442C10".into(),
            protocol: Protocol::TcpIp,
            state: DeviceState::Bootloader,
            platform: Platform::Rvc2,
        };
        status.devices = vec![DeviceInfo {
            protocol: Protocol::Usb,
            ..d.clone()
        }];
        assert!(!poe_selected(&cfg, &status));
        status.devices = vec![d];
        assert_eq!(
            link_chip_text(&cfg, &status, true),
            "Booting (PoE, up to 90 s)"
        );
        status.link = Link::Connected;
        assert!(!opening_poe(&cfg, &status));
    }

    #[test]
    fn profile_chip_text() {
        let mut cfg = Config::default();
        assert_eq!(profile_text(&cfg, Transport::Poe), "PoE defaults");
        cfg.apply_transport(Transport::Poe);
        cfg.rgb.stream.fps = 15.0;
        cfg.remember_profile(Transport::Poe);
        assert_eq!(profile_text(&cfg, Transport::Poe), "PoE custom");
        assert_eq!(profile_text(&cfg, Transport::Usb2), "USB2 defaults");
    }

    #[test]
    fn recent_ips_are_deduplicated_newest_first() {
        let mut r = vec!["a".to_owned(), "b".to_owned()];
        remember_ip(&mut r, "b");
        assert_eq!(r, ["b", "a"]);
        for i in 0..20 {
            remember_ip(&mut r, &i.to_string());
        }
        assert_eq!(r.len(), MAX_RECENT_IPS);
        assert_eq!(r[0], "19");
        assert_eq!(parse_ip(" 192.0.2.44 ").as_deref(), Some("192.0.2.44"));
        assert_eq!(parse_ip("fe80::1").as_deref(), Some("fe80::1"));
        assert_eq!(parse_ip("oak.local"), None);
    }
}
