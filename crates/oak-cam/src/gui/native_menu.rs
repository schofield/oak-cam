//! Native macOS menu bar built with [`muda`] (as stream-concern's
//! `native_menu.rs`), from the shared description in [`super::menu`].
//!
//! The menu lives in the real `NSMenu`, so it renders in the system menu
//! bar rather than inside the window. Ownership rules: build it on the main
//! thread inside the app-creator closure (the `NSApp` exists by then), keep
//! the [`muda::Menu`] and every item later mutated alive in the struct
//! (they hold `Rc`, so `NativeMenu` is `!Send`, fine on the UI thread), and
//! call [`muda::Menu::init_for_nsapp`]. winit's default menu is turned off
//! in [`super::run`] so this is the only one.
//!
//! Custom items are used for Settings and Quit: `AppKit`'s predefined
//! Preferences selector is not routed through winit's responder chain, and
//! the predefined Quit sends `terminate:`, which would exit without the app
//! closing the device. No Edit menu is installed: its predefined items
//! would swallow ⌘C/⌘V before egui sees them, and without it the keys reach
//! the text fields directly.

use crossbeam_channel::{Receiver, Sender};
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{
    AboutMetadata, CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem,
    Submenu,
};

use super::menu::{self, Entry, MenuAction, MenuState};
use super::theme::ThemePreset;

/// The muda key code for a shortcut key used by [`menu::shortcut`].
fn key_code(key: egui::Key) -> Option<Code> {
    Some(match key {
        egui::Key::Comma => Code::Comma,
        egui::Key::Q => Code::KeyQ,
        egui::Key::R => Code::KeyR,
        egui::Key::Num1 => Code::Digit1,
        egui::Key::Num2 => Code::Digit2,
        egui::Key::Num3 => Code::Digit3,
        egui::Key::Num4 => Code::Digit4,
        _ => return None,
    })
}

fn accelerator(action: MenuAction) -> Option<Accelerator> {
    let key = key_code(menu::shortcut(action)?)?;
    Some(Accelerator::new(Some(Modifiers::SUPER), key))
}

/// The live native menu plus the items the app updates at runtime.
pub(crate) struct NativeMenu {
    /// Kept alive for the lifetime of the app; dropping it tears the menu
    /// down.
    _menu: Menu,
    /// Items whose title follows the state (Connect / Disconnect).
    titled: Vec<(MenuAction, MenuItem)>,
    /// Check items, by action (cameras, tabs, stats, themes).
    checks: Vec<(MenuAction, CheckMenuItem)>,
    events: Receiver<MenuEvent>,
    /// The state last pushed, so `sync` only crosses into `AppKit` on change.
    last: Option<MenuState>,
}

impl NativeMenu {
    /// Builds and installs the menu. `None` (logged) if that fails: the app
    /// then runs without a native menu rather than refusing to start.
    pub(crate) fn build(ctx: &egui::Context) -> Option<Self> {
        match Self::try_build(ctx) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::error!("failed to build the native menu: {e}");
                None
            }
        }
    }

    #[allow(clippy::too_many_lines)] // one straight-line builder
    fn try_build(ctx: &egui::Context) -> muda::Result<Self> {
        let state = MenuState::default();
        let root = Menu::new();
        let mut titled = Vec::new();
        let mut checks = Vec::new();
        for (i, m) in menu::menus().into_iter().enumerate() {
            let sub = Submenu::new(m.title, true);
            for e in &m.entries {
                match *e {
                    Entry::Separator => sub.append(&PredefinedMenuItem::separator())?,
                    Entry::Item(MenuAction::About) => {
                        sub.append(&PredefinedMenuItem::about(
                            Some("About oak-cam"),
                            Some(AboutMetadata {
                                name: Some("oak-cam".into()),
                                version: Some(env!("CARGO_PKG_VERSION").into()),
                                comments: Some(
                                    "A Luxonis OAK-D Pro as three virtual webcams".into(),
                                ),
                                website: Some(menu::README_URL.into()),
                                ..Default::default()
                            }),
                        ))?;
                    }
                    Entry::Item(a) => {
                        let item = MenuItem::with_id(
                            menu::id(a),
                            menu::label(a, state),
                            true,
                            accelerator(a),
                        );
                        sub.append(&item)?;
                        if a == MenuAction::ToggleConnection {
                            titled.push((a, item));
                        }
                    }
                    Entry::Check(a) => {
                        let item = CheckMenuItem::with_id(
                            menu::id(a),
                            menu::label(a, state),
                            true,
                            state.checked(a),
                            accelerator(a),
                        );
                        sub.append(&item)?;
                        checks.push((a, item));
                    }
                    Entry::Themes => {
                        let themes = Submenu::new("Theme", true);
                        for t in ThemePreset::ALL {
                            let a = MenuAction::SetTheme(t);
                            let item = CheckMenuItem::with_id(
                                menu::id(a),
                                t.label(),
                                true,
                                state.checked(a),
                                None,
                            );
                            themes.append(&item)?;
                            checks.push((a, item));
                        }
                        sub.append(&themes)?;
                    }
                }
            }
            if i == 0 {
                // The application menu's usual system items, before Quit.
                let quit_at = sub.items().len().saturating_sub(1);
                let extra: [&dyn IsMenuItem; 6] = [
                    &PredefinedMenuItem::services(None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::hide(None),
                    &PredefinedMenuItem::hide_others(None),
                    &PredefinedMenuItem::show_all(None),
                    &PredefinedMenuItem::separator(),
                ];
                sub.insert_items(&extra, quit_at)?;
            }
            root.append(&sub)?;
            if m.title == "View" {
                root.append(&Submenu::with_items(
                    "Window",
                    true,
                    &[
                        &PredefinedMenuItem::minimize(None),
                        &PredefinedMenuItem::maximize(None),
                        &PredefinedMenuItem::separator(),
                        &PredefinedMenuItem::bring_all_to_front(None),
                    ],
                )?)?;
            }
        }
        root.init_for_nsapp();

        // Route events into our own channel and wake egui. Setting a handler
        // replaces muda's built-in channel, so only `rx` is drained.
        let (tx, rx): (Sender<MenuEvent>, Receiver<MenuEvent>) = crossbeam_channel::unbounded();
        let repaint = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = tx.send(event);
            repaint.request_repaint();
        }));

        Ok(Self {
            _menu: root,
            titled,
            checks,
            events: rx,
            last: None,
        })
    }

    /// Every pending menu event as an action. Never blocks; `AppKit`'s own
    /// items are ignored.
    pub(crate) fn drain_actions(&self) -> Vec<MenuAction> {
        let mut out = Vec::new();
        while let Ok(ev) = self.events.try_recv() {
            if let Some(a) = menu::action_for(ev.id().as_ref()) {
                out.push(a);
            }
        }
        out
    }

    /// Forget the state last pushed: AppKit/muda flip a check item's mark
    /// on their own when it is chosen, so after any action the next
    /// [`Self::sync`] rewrites every mark from the app's state.
    pub(crate) fn resync(&mut self) {
        self.last = None;
    }

    /// Brings titles and check marks in line with `state`; touches `AppKit`
    /// only when something changed.
    pub(crate) fn sync(&mut self, state: MenuState) {
        if self.last == Some(state) {
            return;
        }
        for (a, item) in &self.titled {
            item.set_text(menu::label(*a, state));
        }
        for (a, item) in &self.checks {
            item.set_checked(state.checked(*a));
        }
        self.last = Some(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shortcut_has_a_native_key() {
        for a in menu::all_actions() {
            if let Some(k) = menu::shortcut(a) {
                assert!(key_code(k).is_some(), "{a:?} {k:?}");
                assert!(accelerator(a).is_some());
            }
        }
    }
}
