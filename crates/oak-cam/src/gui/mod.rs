//! The GUI (`oak-cam` with no arguments): an eframe/egui window over the
//! oak-core [`Supervisor`](oak_core::supervisor::Supervisor).
//!
//! - [`app`]: the eframe app; owns the supervisor, the edited config
//!   (sent as `ApplyConfig` on every change, saved 500 ms after the last),
//!   the event drain, menu actions and the layout.
//! - [`menu`]: the menus, described once; drawn natively on macOS by
//!   [`native_menu`] (muda) and as an egui strip elsewhere.
//! - [`settings`]: the Settings window (`[ui]` defaults, resets).
//! - [`theme`]: stream-concern's theme presets.
//! - [`top_bar`]: device picker, manual IP, status chip, temperature, USB2
//!   badge, bandwidth estimate ([`bandwidth`]).
//! - [`camera_card`]: one card per sensor with its thumbnail ([`thumbs`]),
//!   stream settings and live stats.
//! - [`panels`]: the side panel tabs (RGB, Mono, Device, Virtual cameras).
//! - [`vcam_setup`]: the frame sinks, the "virtual cameras unavailable"
//!   banner with Install / Enable, and the switch to the OS cameras.
//! - [`status`]: supervisor events folded into what the window shows.
//! - [`script`]: the hidden `--gui-script` driver for automated runs, with
//!   [`png`] for its window screenshots.

mod app;
mod bandwidth;
mod camera_card;
mod menu;
#[cfg(target_os = "macos")]
mod native_menu;
mod panels;
mod png;
mod script;
mod settings;
mod status;
mod theme;
mod thumbs;
mod top_bar;
mod vcam_setup;
mod widgets;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context as _, Result, anyhow};

/// Options for [`run`].
#[derive(Debug, Default)]
pub struct GuiArgs {
    /// A `--gui-script` file to run once the window is up.
    pub script: Option<PathBuf>,
    /// `--force-null-vcam`: start on null cameras (test hook).
    pub force_null_vcam: bool,
    /// `--config`: the config file (default: the per-user one).
    pub config: Option<PathBuf>,
}

/// Default window size, points.
const DEFAULT_SIZE: [f32; 2] = [1400.0, 900.0];
/// Smallest window, points.
const MIN_SIZE: [f32; 2] = [1000.0, 700.0];

/// Opens the window and runs until it is closed.
pub fn run(args: &GuiArgs) -> Result<()> {
    let path = match &args.config {
        Some(p) => p.clone(),
        None => oak_core::config::Config::default_path().context("no config path")?,
    };
    let script = args
        .script
        .as_deref()
        .map(script::Script::load)
        .transpose()?;
    let state = app::Startup::new(&path, script, args.force_null_vcam)?;
    let size = state.cfg.ui.window_size.map_or(DEFAULT_SIZE, |[w, h]| {
        [(w as f32).max(MIN_SIZE[0]), (h as f32).max(MIN_SIZE[1])]
    });
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("oak-cam")
        .with_app_id("oak-cam")
        .with_inner_size(size)
        .with_min_inner_size(MIN_SIZE)
        // Windows: winit's drag-and-drop support calls OleInitialize, which
        // needs single-threaded COM on the main thread; the Media Foundation
        // virtual camera has already initialised COM multithreaded there, so
        // the window creation panicked with RPC_E_CHANGED_MODE (seen
        // 2026-09-29). The app accepts no dropped files, so switch it off.
        .with_drag_and_drop(false);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    // Suppress winit's default application menu so muda's is the only one
    // (as stream-concern does).
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::EventLoopBuilderExtMacOS as _;
        options.event_loop_builder = Some(Box::new(|builder| {
            builder.with_default_menu(false);
        }));
    }
    eframe::run_native(
        "oak-cam",
        options,
        Box::new(move |cc| Ok(Box::new(app::OakApp::new(cc, state)))),
    )
    .map_err(|e| anyhow!("cannot open the window: {e}"))
}

/// The window/taskbar icon. Inside `oak-cam.app` the Dock shows the bundle's
/// `oak-cam.icns`, so the default (empty) icon is returned there: eframe
/// skips its runtime `setApplicationIconImage` for that, while `None` would
/// make it install egui's own logo. Elsewhere (Windows, Linux, `cargo run`)
/// the 256 px PNG is used; one that fails to decode only costs the icon.
fn window_icon() -> Option<std::sync::Arc<egui::IconData>> {
    const PNG: &[u8] = include_bytes!("../../../../assets/icon/oak-cam-256.png");
    if cfg!(target_os = "macos") && in_app_bundle() {
        return Some(std::sync::Arc::new(egui::IconData::default()));
    }
    match eframe::icon_data::from_png_bytes(PNG) {
        Ok(icon) => Some(std::sync::Arc::new(icon)),
        Err(e) => {
            tracing::warn!("cannot decode the window icon: {e}");
            None
        }
    }
}

/// Whether the executable runs from `<something>.app/Contents/MacOS/`.
fn in_app_bundle() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.ends_with("Contents/MacOS")))
        .unwrap_or(false)
}

/// The config file the GUI edits (shown in the Device tab and Settings).
pub(crate) fn display_path(path: &Path) -> String {
    match std::env::var_os("HOME") {
        Some(home) if path.starts_with(&home) => {
            format!("~/{}", path.strip_prefix(&home).unwrap_or(path).display())
        }
        _ => path.display().to_string(),
    }
}

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// The GUI log is rotated to `oak-cam.log.1` past this size.
pub(crate) const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;

/// The GUI's log file: appended to, and rotated (renamed to `<name>.1`,
/// replacing an older one) once it grows past its limit, at open and
/// while writing.
#[derive(Debug)]
pub(crate) struct LogFile {
    path: PathBuf,
    file: std::fs::File,
    len: u64,
    max: u64,
}

impl LogFile {
    /// Opens `path` for appending (creating it and its folder).
    pub(crate) fn open(path: &Path, max: u64) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let len = file.metadata()?.len();
        let mut f = Self {
            path: path.to_owned(),
            file,
            len,
            max,
        };
        if f.len > f.max {
            f.rotate()?;
        }
        Ok(f)
    }

    fn rotated_path(&self) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_owned();
        name.push(".1");
        self.path.with_file_name(name)
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        std::fs::rename(&self.path, self.rotated_path())?;
        self.file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.len = 0;
        Ok(())
    }
}

impl std::io::Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.len > self.max {
            // A failed rotation keeps appending rather than losing lines.
            let _ = self.rotate();
        }
        let n = self.file.write(buf)?;
        self.len += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Where the GUI writes its log: `~/Library/Logs/oak-cam` (macOS),
/// `%LOCALAPPDATA%\oak-cam\logs` (Windows), `$XDG_STATE_HOME/oak-cam`
/// (Linux, default `~/.local/state/oak-cam`), file `oak-cam.log`.
pub(crate) fn log_file_path() -> Option<PathBuf> {
    let dir = if cfg!(target_os = "macos") {
        dirs::state_dir()
            .map(|d| d.join("oak-cam"))
            .or_else(|| dirs::home_dir().map(|h| h.join("Library/Logs/oak-cam")))
    } else if cfg!(windows) {
        dirs::data_local_dir().map(|d| d.join("oak-cam").join("logs"))
    } else {
        dirs::state_dir().map(|d| d.join("oak-cam"))
    }?;
    Some(dir.join("oak-cam.log"))
}

/// Records the log file this session writes (set once, at startup).
pub(crate) fn set_log_path(path: PathBuf) {
    let _ = LOG_PATH.set(path);
}

/// The log file this session writes, if any (Device tab, Help › Reveal Log).
pub(crate) fn log_path() -> Option<&'static Path> {
    LOG_PATH.get().map(PathBuf::as_path)
}

/// Opens `url` in the default browser.
pub(crate) fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("/usr/bin/open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("explorer");
        c.arg(url);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");
    #[cfg(not(target_os = "windows"))]
    cmd.arg(url);
    cmd.spawn().map(drop)
}

/// Shows `path` in the file manager (Finder, selecting it, on macOS; its
/// folder elsewhere, or when the file does not exist yet).
pub(crate) fn reveal(path: &Path) -> std::io::Result<()> {
    let exists = path.exists();
    let dir = if exists {
        path.parent().unwrap_or(path)
    } else {
        path.parent()
            .filter(|d| d.exists())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "not found"))?
    };
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("/usr/bin/open");
        if exists {
            cmd.arg("-R").arg(path);
        } else {
            cmd.arg(dir);
        }
        cmd.spawn().map(drop)
    }
    #[cfg(target_os = "windows")]
    {
        let mut cmd = std::process::Command::new("explorer");
        if exists {
            cmd.arg(format!("/select,{}", path.display()));
        } else {
            cmd.arg(dir);
        }
        cmd.spawn().map(drop)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::process::Command::new("xdg-open")
            .arg(dir)
            .spawn()
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_file_appends_and_rotates() {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join(format!("oak-cam-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("oak-cam.log");
        let mut f = LogFile::open(&path, 10).unwrap();
        f.write_all(b"first\n").unwrap();
        drop(f);
        let mut f = LogFile::open(&path, 10).unwrap();
        f.write_all(b"second\n").unwrap();
        drop(f);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "first\nsecond\n",
            "appended, not truncated"
        );
        // Now past 10 bytes: the next write rotates first.
        let mut f = LogFile::open(&path, 10).unwrap();
        f.write_all(b"third\n").unwrap();
        drop(f);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "third\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("oak-cam.log.1")).unwrap(),
            "first\nsecond\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_file_is_per_user() {
        if let Some(p) = log_file_path() {
            assert!(p.ends_with("oak-cam.log"), "{}", p.display());
            assert!(
                p.components().any(|c| c.as_os_str() == "oak-cam"),
                "{}",
                p.display()
            );
        }
    }
}
