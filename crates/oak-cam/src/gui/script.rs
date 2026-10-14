//! `--gui-script FILE` (hidden): a JSON list of steps the GUI runs in
//! order, for automated checks without clicking. Config edits go through
//! the same path as the widgets (validated, sent as `ApplyConfig`, saved
//! after 500 ms). Example:
//!
//! ```json
//! [
//!   {"wait": 8000},
//!   {"screenshot": "/tmp/gui.png"},
//!   {"tab": "rgb"},
//!   {"set": "rgb.exposure = { Manual = { us = 2000, iso = 400 } }"},
//!   {"override": {"cam": "left", "on": true}},
//!   "trigger_af",
//!   {"log": "marker"},
//!   {"window_size": [1000, 700]},
//!   {"settings": true},
//!   {"theme": "flat-dark"},
//!   {"menu": "Device/Rescan"},
//!   "install_vcam",
//!   "retry_vcam",
//!   "quit"
//! ]
//! ```

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use serde::Deserialize;

/// One scripted step.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Step {
    /// Do nothing for this many milliseconds.
    Wait(u64),
    /// Lay a TOML fragment over the config (see `Config::merged_toml`).
    Set(String),
    /// Turn a mono override on or off (`cam`: `left` or `right`).
    Override {
        /// `left` or `right`.
        cam: String,
        /// On or off.
        on: bool,
    },
    /// Show a side panel tab: `rgb`, `mono`, `device` or `vcam`.
    Tab(String),
    /// One-shot RGB autofocus.
    TriggerAf,
    /// Save the window's contents as a PNG.
    Screenshot(PathBuf),
    /// Resize the window's inner area to `[width, height]` points.
    WindowSize([f32; 2]),
    /// Write a line to stderr (and the message bar).
    Log(String),
    /// Open (`true`) or close the Settings window.
    Settings(bool),
    /// Switch the theme (TOML name or label, e.g. `flat-dark`).
    Theme(String),
    /// Run a menu item by path (`Device/Rescan`, `View/Theme/Soft Light`),
    /// through the same handler as the menu bar.
    Menu(String),
    /// The virtual-cameras banner's Install / Enable button.
    InstallVcam,
    /// The virtual-cameras banner's Retry button.
    RetryVcam,
    /// Close the window (as Cmd-Q would).
    Quit,
}

/// A loaded script and where it stands.
#[derive(Debug)]
pub(crate) struct Script {
    steps: VecDeque<Step>,
    resume_at: Option<Instant>,
    /// A screenshot was requested and has not arrived yet (with the time
    /// it was asked for); later steps wait so the image shows the state
    /// before them.
    shot_pending: Option<Instant>,
}

/// Longest a script waits for a requested screenshot before going on.
const SHOT_TIMEOUT: Duration = Duration::from_secs(3);

impl Script {
    /// Parses a script.
    pub(crate) fn parse(text: &str) -> Result<Self> {
        let steps: Vec<Step> = serde_json::from_str(text)?;
        Ok(Self {
            steps: steps.into(),
            resume_at: None,
            shot_pending: None,
        })
    }

    /// Loads and parses `path`.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read GUI script {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid GUI script {}", path.display()))
    }

    /// The steps to run now: everything up to the next unexpired wait. A
    /// screenshot ends the batch, and nothing more runs until
    /// [`Self::shot_taken`] (or [`SHOT_TIMEOUT`]).
    pub(crate) fn due(&mut self, now: Instant) -> Vec<Step> {
        let mut out = Vec::new();
        if let Some(t) = self.shot_pending {
            if now.duration_since(t) < SHOT_TIMEOUT {
                return out;
            }
            self.shot_pending = None;
        }
        loop {
            if let Some(t) = self.resume_at {
                if now < t {
                    return out;
                }
                self.resume_at = None;
            }
            match self.steps.pop_front() {
                None => return out,
                Some(Step::Wait(ms)) => {
                    self.resume_at = Some(now + Duration::from_millis(ms));
                }
                Some(step @ Step::Screenshot(_)) => {
                    out.push(step);
                    self.shot_pending = Some(now);
                    return out;
                }
                Some(step) => out.push(step),
            }
        }
    }

    /// The requested screenshot arrived: later steps may run.
    pub(crate) fn shot_taken(&mut self) {
        self.shot_pending = None;
    }

    /// Time until the next step is due (`None` when finished).
    pub(crate) fn next_in(&self, now: Instant) -> Option<Duration> {
        if self.steps.is_empty() && self.resume_at.is_none() {
            return None;
        }
        if self.shot_pending.is_some() {
            return Some(Duration::ZERO);
        }
        Some(
            self.resume_at
                .map_or(Duration::ZERO, |t| t.saturating_duration_since(now)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_step_kind() {
        let s = Script::parse(
            r#"[{"wait": 10}, {"set": "rgb.brightness = 2"},
                {"override": {"cam": "left", "on": true}}, {"tab": "mono"},
                "trigger_af", {"screenshot": "/tmp/x.png"}, {"log": "hi"},
                {"window_size": [1000, 700]}, "quit", {"settings": true},
                {"theme": "flat-dark"}, {"menu": "Device/Rescan"}, "install_vcam",
                "retry_vcam"]"#,
        )
        .unwrap();
        assert_eq!(s.steps.len(), 14);
        assert_eq!(s.steps[9], Step::Settings(true));
        assert_eq!(s.steps[10], Step::Theme("flat-dark".into()));
        assert_eq!(s.steps[11], Step::Menu("Device/Rescan".into()));
        assert_eq!(s.steps[13], Step::RetryVcam);
        assert_eq!(s.steps[7], Step::WindowSize([1000.0, 700.0]));
        assert_eq!(
            s.steps[2],
            Step::Override {
                cam: "left".into(),
                on: true
            }
        );
        assert!(Script::parse(r#"[{"sleep": 1}]"#).is_err());
    }

    #[test]
    fn a_screenshot_ends_the_batch_until_it_arrives() {
        let mut s = Script::parse(
            r#"[{"log": "a"}, {"screenshot": "/tmp/x.png"}, {"set": "rgb.brightness = 1"}]"#,
        )
        .unwrap();
        let t0 = Instant::now();
        let first = s.due(t0);
        assert_eq!(first.len(), 2);
        assert!(matches!(first[1], Step::Screenshot(_)));
        assert!(s.due(t0).is_empty(), "blocked until the image arrives");
        s.shot_taken();
        assert_eq!(s.due(t0), [Step::Set("rgb.brightness = 1".into())]);
        // A lost screenshot does not block forever.
        let mut s = Script::parse(r#"[{"screenshot": "/tmp/x.png"}, {"log": "b"}]"#).unwrap();
        assert_eq!(s.due(t0).len(), 1);
        assert!(s.due(t0 + Duration::from_secs(1)).is_empty());
        assert_eq!(s.due(t0 + SHOT_TIMEOUT), [Step::Log("b".into())]);
    }

    #[test]
    fn waits_split_the_steps() {
        let mut s =
            Script::parse(r#"[{"log": "a"}, {"wait": 100}, {"log": "b"}, "quit"]"#).unwrap();
        let t0 = Instant::now();
        assert_eq!(s.due(t0), [Step::Log("a".into())]);
        assert_eq!(s.next_in(t0), Some(Duration::from_millis(100)));
        assert!(s.due(t0 + Duration::from_millis(50)).is_empty());
        assert_eq!(
            s.due(t0 + Duration::from_millis(100)),
            [Step::Log("b".into()), Step::Quit]
        );
        assert_eq!(s.next_in(t0), None);
    }
}
