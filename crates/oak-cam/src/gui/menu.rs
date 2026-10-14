//! The menu bar, described once and drawn twice: as the native macOS menu
//! bar ([`super::native_menu`], muda, as stream-concern) and as an egui
//! menu strip in the window on other platforms ([`menu_bar`]). Both, the
//! keyboard shortcuts and the `--gui-script` `{"menu": "Device/Rescan"}`
//! step resolve to the same [`MenuAction`]s, which the app applies in one
//! place.

use egui::{Key, KeyboardShortcut, Modifiers};
use oak_core::capture::SensorId;

use super::panels::Tab;
use super::theme::{self, ThemePreset};

/// README on GitHub (Help menu).
pub(crate) const README_URL: &str = "https://github.com/schofield/oak-cam";

/// A menu action, independent of the toolkit drawing the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuAction {
    /// About oak-cam.
    About,
    /// Settings… (⌘,).
    OpenSettings,
    /// Quit (⌘Q), through the window's close request so the app shuts the
    /// device down cleanly.
    Quit,
    /// List devices again (⌘R).
    Rescan,
    /// Ask for a PoE address and open it.
    ConnectToIp,
    /// Close the device and stop streaming, or start again.
    ToggleConnection,
    /// One-shot RGB autofocus.
    TriggerAf,
    /// The side panel's Reset to defaults (camera controls).
    ResetControls,
    /// Stream a sensor or not.
    ToggleCamera(SensorId),
    /// Submit the camera extension activation request.
    InstallExtension,
    /// Show the Virtual cameras tab.
    ShowVcamStatus,
    /// Show a side panel tab (⌘1-4).
    ShowTab(Tab),
    /// Stats line on the camera cards.
    ToggleStats,
    /// Switch the UI theme (View › Theme).
    SetTheme(ThemePreset),
    /// Open the README on GitHub.
    OpenReadme,
    /// Show the config file in the file manager.
    RevealConfig,
    /// Show the log file in the file manager.
    RevealLog,
}

/// One entry of a menu.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Entry {
    /// A command.
    Item(MenuAction),
    /// A command with a check mark (see [`MenuState::checked`]).
    Check(MenuAction),
    /// A separator line.
    Separator,
    /// View › Theme: one check item per preset.
    Themes,
}

/// A top-level menu.
pub(crate) struct Menu {
    pub title: &'static str,
    pub entries: Vec<Entry>,
}

/// App state the menus reflect (titles and check marks).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MenuState {
    /// A supervisor is running (the device is open or being opened).
    pub connected: bool,
    /// Each sensor's `enabled`.
    pub cams: [bool; 3],
    /// Stats on the cards.
    pub stats: bool,
    pub theme: ThemePreset,
    pub tab: Tab,
}

impl Default for MenuState {
    fn default() -> Self {
        Self {
            connected: true,
            cams: [true; 3],
            stats: true,
            theme: ThemePreset::default(),
            tab: Tab::Rgb,
        }
    }
}

impl MenuState {
    /// The check mark of `action`, for check entries.
    pub(crate) fn checked(self, action: MenuAction) -> bool {
        match action {
            MenuAction::ToggleCamera(s) => self.cams[s.index()],
            MenuAction::ShowTab(t) => self.tab == t,
            MenuAction::ToggleStats => self.stats,
            MenuAction::SetTheme(t) => self.theme == t,
            _ => false,
        }
    }
}

const TABS: [(Tab, Key); 4] = [
    (Tab::Rgb, Key::Num1),
    (Tab::Mono, Key::Num2),
    (Tab::Device, Key::Num3),
    (Tab::Vcam, Key::Num4),
];

/// The menus, in menu bar order. The first is the application menu.
pub(crate) fn menus() -> Vec<Menu> {
    use Entry::{Check, Item, Separator, Themes};
    use MenuAction as A;
    let mut view: Vec<Entry> = TABS.iter().map(|&(t, _)| Check(A::ShowTab(t))).collect();
    view.extend([Separator, Check(A::ToggleStats), Separator, Themes]);
    vec![
        Menu {
            title: "oak-cam",
            entries: vec![
                Item(A::About),
                Separator,
                Item(A::OpenSettings),
                Separator,
                Item(A::Quit),
            ],
        },
        Menu {
            title: "Device",
            entries: vec![
                Item(A::Rescan),
                Item(A::ConnectToIp),
                Separator,
                Item(A::ToggleConnection),
            ],
        },
        Menu {
            title: "Camera",
            entries: vec![
                Item(A::TriggerAf),
                Item(A::ResetControls),
                Separator,
                Check(A::ToggleCamera(SensorId::Rgb)),
                Check(A::ToggleCamera(SensorId::Left)),
                Check(A::ToggleCamera(SensorId::Right)),
            ],
        },
        Menu {
            title: "Virtual Cameras",
            entries: vec![Item(A::InstallExtension), Item(A::ShowVcamStatus)],
        },
        Menu {
            title: "View",
            entries: view,
        },
        Menu {
            title: "Help",
            entries: vec![
                Item(A::OpenReadme),
                Separator,
                Item(A::RevealConfig),
                Item(A::RevealLog),
            ],
        },
    ]
}

const IN_FILE_MANAGER: &str = if cfg!(target_os = "macos") {
    " in Finder"
} else {
    ""
};

/// The item title for `action` in `state`.
pub(crate) fn label(action: MenuAction, state: MenuState) -> String {
    use MenuAction as A;
    match action {
        A::About => "About oak-cam".into(),
        A::OpenSettings => "Settings…".into(),
        A::Quit => "Quit oak-cam".into(),
        A::Rescan => "Rescan".into(),
        A::ConnectToIp => "Connect to IP…".into(),
        A::ToggleConnection => if state.connected {
            "Disconnect"
        } else {
            "Connect"
        }
        .into(),
        A::TriggerAf => "Trigger Autofocus".into(),
        A::ResetControls => "Reset Controls to Defaults".into(),
        A::ToggleCamera(s) => format!("Enable {}", sensor_title(s)),
        A::InstallExtension => "Install / Update Extension…".into(),
        A::ShowVcamStatus => "Show Status".into(),
        A::ShowTab(t) => t.label().into(),
        A::ToggleStats => "Show Stats".into(),
        A::SetTheme(t) => t.label().into(),
        A::OpenReadme => "oak-cam README on GitHub".into(),
        A::RevealConfig => format!("Reveal Config File{IN_FILE_MANAGER}"),
        A::RevealLog => format!("Reveal Log{IN_FILE_MANAGER}"),
    }
}

fn sensor_title(s: SensorId) -> &'static str {
    match s {
        SensorId::Rgb => "RGB",
        SensorId::Left => "Left",
        SensorId::Right => "Right",
    }
}

/// The ⌘ (Ctrl elsewhere) shortcut of `action`.
pub(crate) fn shortcut(action: MenuAction) -> Option<Key> {
    match action {
        MenuAction::OpenSettings => Some(Key::Comma),
        MenuAction::Quit => Some(Key::Q),
        MenuAction::Rescan => Some(Key::R),
        MenuAction::ShowTab(t) => TABS.iter().find(|(x, _)| *x == t).map(|&(_, k)| k),
        _ => None,
    }
}

/// A stable id for `action` (the native menu item id).
#[cfg(target_os = "macos")]
pub(crate) fn id(action: MenuAction) -> String {
    use MenuAction as A;
    match action {
        A::About => "app.about".into(),
        A::OpenSettings => "app.settings".into(),
        A::Quit => "app.quit".into(),
        A::Rescan => "device.rescan".into(),
        A::ConnectToIp => "device.connect_ip".into(),
        A::ToggleConnection => "device.toggle_connection".into(),
        A::TriggerAf => "camera.trigger_af".into(),
        A::ResetControls => "camera.reset".into(),
        A::ToggleCamera(s) => format!("camera.enable.{}", s.name()),
        A::InstallExtension => "vcam.install".into(),
        A::ShowVcamStatus => "vcam.status".into(),
        A::ShowTab(t) => format!("view.tab.{}", t.key()),
        A::ToggleStats => "view.stats".into(),
        A::SetTheme(t) => format!("view.theme.{}", t.name()),
        A::OpenReadme => "help.readme".into(),
        A::RevealConfig => "help.reveal_config".into(),
        A::RevealLog => "help.reveal_log".into(),
    }
}

/// Every action some menu offers, in menu order.
pub(crate) fn all_actions() -> Vec<MenuAction> {
    let mut out = Vec::new();
    for m in menus() {
        for e in m.entries {
            match e {
                Entry::Item(a) | Entry::Check(a) => out.push(a),
                Entry::Themes => out.extend(ThemePreset::ALL.map(MenuAction::SetTheme)),
                Entry::Separator => {}
            }
        }
    }
    out
}

/// The action with native item id `id` (`None` for `AppKit`'s own items).
#[cfg(target_os = "macos")]
pub(crate) fn action_for(id: &str) -> Option<MenuAction> {
    all_actions().into_iter().find(|&a| self::id(a) == id)
}

fn norm(s: &str) -> String {
    s.trim()
        .trim_end_matches('…')
        .trim_end_matches("...")
        .trim()
        .to_ascii_lowercase()
}

/// The action at a `Menu/Item` (or `View/Theme/Flat Dark`) path, matched
/// case-insensitively without a trailing ellipsis; for `--gui-script`.
/// Toggle items answer to either title (`Device/Connect`,
/// `Device/Disconnect`).
pub(crate) fn find(path: &str) -> Option<MenuAction> {
    let parts: Vec<String> = path.split('/').map(norm).collect();
    let menu = menus().into_iter().find(|m| norm(m.title) == parts[0])?;
    let titles = |a: MenuAction| {
        let on = MenuState::default();
        let off = MenuState {
            connected: false,
            ..on
        };
        [norm(&label(a, on)), norm(&label(a, off))]
    };
    match parts.as_slice() {
        [_, item] => menu.entries.iter().find_map(|e| match *e {
            Entry::Item(a) | Entry::Check(a) if titles(a).contains(item) => Some(a),
            _ => None,
        }),
        [_, sub, item]
            if sub == "theme" && menu.entries.iter().any(|e| matches!(e, Entry::Themes)) =>
        {
            ThemePreset::lookup(item).map(MenuAction::SetTheme)
        }
        _ => None,
    }
}

/// Keyboard shortcuts when no native menu delivers them (not macOS).
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn shortcuts(ctx: &egui::Context) -> Vec<MenuAction> {
    let mut out = Vec::new();
    for a in all_actions() {
        if let Some(key) = shortcut(a) {
            let sc = KeyboardShortcut::new(Modifiers::COMMAND, key);
            if ctx.input_mut(|i| i.consume_shortcut(&sc)) {
                out.push(a);
            }
        }
    }
    out
}

/// The in-window menu strip (other platforms). Returns the chosen action.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn menu_bar(ui: &mut egui::Ui, state: MenuState) -> Option<MenuAction> {
    let mut chosen = None;
    egui::MenuBar::new().ui(ui, |ui| {
        for m in menus() {
            ui.menu_button(m.title, |ui| {
                for e in &m.entries {
                    match *e {
                        Entry::Separator => {
                            ui.separator();
                        }
                        Entry::Item(a) | Entry::Check(a) => {
                            let mut text = label(a, state);
                            if matches!(e, Entry::Check(_)) && state.checked(a) {
                                text = format!("✔ {text}");
                            }
                            let mut button = egui::Button::new(text);
                            if let Some(k) = shortcut(a) {
                                button = button.shortcut_text(ui.ctx().format_shortcut(
                                    &KeyboardShortcut::new(Modifiers::COMMAND, k),
                                ));
                            }
                            if ui.add(button).clicked() {
                                chosen = Some(a);
                                ui.close();
                            }
                        }
                        Entry::Themes => {
                            ui.menu_button("Theme", |ui| {
                                if let Some(t) = theme::theme_choices(ui, state.theme) {
                                    chosen = Some(MenuAction::SetTheme(t));
                                    ui.close();
                                }
                            });
                        }
                    }
                }
            });
        }
    });
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn every_action_round_trips_through_its_id() {
        let all = all_actions();
        assert!(all.len() >= 20, "{all:?}");
        for a in &all {
            assert_eq!(action_for(&id(*a)), Some(*a), "{a:?}");
        }
        let mut ids: Vec<String> = all.iter().map(|&a| id(a)).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "ids are unique");
        assert_eq!(action_for("1000"), None);
    }

    #[test]
    fn menus_have_the_planned_items_and_shortcuts() {
        let titles: Vec<&str> = menus().iter().map(|m| m.title).collect();
        assert_eq!(
            titles,
            [
                "oak-cam",
                "Device",
                "Camera",
                "Virtual Cameras",
                "View",
                "Help"
            ]
        );
        assert_eq!(shortcut(MenuAction::OpenSettings), Some(Key::Comma));
        assert_eq!(shortcut(MenuAction::Quit), Some(Key::Q));
        assert_eq!(shortcut(MenuAction::Rescan), Some(Key::R));
        assert_eq!(shortcut(MenuAction::ShowTab(Tab::Vcam)), Some(Key::Num4));
        for t in ThemePreset::ALL {
            assert!(all_actions().contains(&MenuAction::SetTheme(t)));
        }
    }

    #[test]
    fn script_paths_find_actions() {
        assert_eq!(find("Device/Rescan"), Some(MenuAction::Rescan));
        assert_eq!(find("device/connect to ip"), Some(MenuAction::ConnectToIp));
        assert_eq!(
            find("Device/Disconnect"),
            Some(MenuAction::ToggleConnection)
        );
        assert_eq!(find("Device/Connect"), Some(MenuAction::ToggleConnection));
        assert_eq!(find("oak-cam/Settings…"), Some(MenuAction::OpenSettings));
        assert_eq!(
            find("Camera/Enable Left"),
            Some(MenuAction::ToggleCamera(SensorId::Left))
        );
        assert_eq!(
            find("View/Theme/Flat Dark"),
            Some(MenuAction::SetTheme(ThemePreset::FlatDark))
        );
        assert_eq!(
            find("View/Virtual cameras"),
            Some(MenuAction::ShowTab(Tab::Vcam))
        );
        assert_eq!(find("Help/Nope"), None);
        assert_eq!(find("Nope/Rescan"), None);
    }

    #[test]
    fn state_drives_titles_and_checks() {
        let mut s = MenuState::default();
        assert_eq!(label(MenuAction::ToggleConnection, s), "Disconnect");
        s.connected = false;
        assert_eq!(label(MenuAction::ToggleConnection, s), "Connect");
        s.cams[SensorId::Left.index()] = false;
        assert!(!s.checked(MenuAction::ToggleCamera(SensorId::Left)));
        assert!(s.checked(MenuAction::ToggleCamera(SensorId::Rgb)));
        assert!(s.checked(MenuAction::ShowTab(Tab::Rgb)));
        assert!(s.checked(MenuAction::SetTheme(ThemePreset::MacNative)));
        assert!(!s.checked(MenuAction::SetTheme(ThemePreset::Classic)));
        assert!(!s.checked(MenuAction::SetTheme(ThemePreset::FlatDark)));
    }

    #[test]
    fn egui_menu_bar_draws() {
        let ctx = egui::Context::default();
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert_eq!(menu_bar(ui, MenuState::default()), None);
        });
        out.drop_without_applying_deltas();
    }
}
