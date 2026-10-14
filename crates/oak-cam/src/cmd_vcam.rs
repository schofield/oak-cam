//! `oak-cam vcam status|install|test-pattern`: manage the OS virtual cameras
//! and verify them without an OAK attached.

use std::io::Write;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use vcam::test_pattern::{GrayPattern, Nv12Pattern};
use vcam::{CameraId, CameraKind, VcamError, VirtualCamera};

/// `oak-cam vcam …`
#[derive(Debug, Args)]
pub struct VcamArgs {
    #[command(subcommand)]
    action: VcamAction,
}

#[derive(Debug, Subcommand)]
enum VcamAction {
    /// Show whether the virtual cameras are installed and active.
    Status,
    /// Install the virtual cameras (macOS: request activation of the camera
    /// extension; approve it in System Settings afterwards).
    Install {
        /// Seconds to wait for macOS to answer the request.
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        /// Keep waiting (up to --timeout) until the user approves in System
        /// Settings, instead of returning once approval is requested.
        #[arg(long)]
        wait: bool,
    },
    /// Publish a moving test pattern until Ctrl-C (colour bars on RGB, gray
    /// steps on the mono cameras), printing published/dropped counts each
    /// second.
    TestPattern {
        /// Which camera(s) to drive.
        #[arg(long, value_enum, default_value_t = CameraSel::Rgb)]
        camera: CameraSel,
        /// Frame width (default: the camera's default, 1920 RGB / 1280 mono).
        #[arg(long, requires = "height")]
        width: Option<u32>,
        /// Frame height (default: the camera's default, 1080 RGB / 800 mono).
        #[arg(long, requires = "width")]
        height: Option<u32>,
        /// Frames per second.
        #[arg(long, default_value_t = 30.0)]
        fps: f32,
        /// Stop after this many seconds instead of waiting for Ctrl-C.
        #[arg(long)]
        seconds: Option<u64>,
        /// Publish into a null camera (no OS device): checks the pattern and
        /// pacing loop on a machine without the virtual camera installed.
        #[arg(long, hide = true)]
        null: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CameraSel {
    Rgb,
    Left,
    Right,
    All,
}

/// Whether this platform's backend publishes each `CameraId` to its own OS
/// device. Only macOS does today; Linux (one loopback for all) and Windows
/// (one instance lock per user) are single-device until M4b / M4c.
pub(crate) const MULTI_DEVICE: bool = cfg!(target_os = "macos");

impl CameraSel {
    fn cameras(self) -> Vec<CameraId> {
        match self {
            CameraSel::Rgb => vec![CameraId::Rgb],
            CameraSel::Left => vec![CameraId::Left],
            CameraSel::Right => vec![CameraId::Right],
            CameraSel::All => CameraId::ALL.to_vec(),
        }
    }

    /// The cameras to drive, refusing anything but RGB on a single-device
    /// backend (`multi_device` false).
    fn checked_cameras(self, multi_device: bool) -> Result<Vec<CameraId>> {
        if !multi_device && self != CameraSel::Rgb {
            let milestone = if cfg!(target_os = "windows") {
                "M4c (Windows multi-index media source)"
            } else {
                "M4b (Linux multi-loopback)"
            };
            bail!(
                "--camera {} is not supported on this platform yet: the virtual camera backend \
                 publishes a single device until {milestone}; use --camera rgb",
                self.to_possible_value()
                    .map_or_else(|| format!("{self:?}"), |v| v.get_name().to_string())
            );
        }
        Ok(self.cameras())
    }
}

/// Run one `vcam` action, writing its report to `out`.
pub fn run(out: &mut impl Write, args: &VcamArgs) -> Result<()> {
    match args.action {
        VcamAction::Status => status(out),
        VcamAction::Install { timeout, wait } => install(out, Duration::from_secs(timeout), wait),
        VcamAction::TestPattern {
            camera,
            width,
            height,
            fps,
            seconds,
            null,
        } => {
            if !(fps > 0.0 && fps <= 240.0) {
                bail!("--fps must be in (0, 240]");
            }
            let size = width.zip(height);
            // --null touches no OS device, so every camera is fine there.
            let cameras = camera.checked_cameras(MULTI_DEVICE || null)?;
            test_pattern(
                out,
                &cameras,
                size,
                fps,
                seconds.map(Duration::from_secs),
                null,
            )
        }
    }
}

#[cfg(target_os = "macos")]
fn status(out: &mut impl Write) -> Result<()> {
    use std::path::Path;
    use vcam::macos::install::{self, SystemState};
    use vcam::macos::{self as mac, APP_INSTALL_PATH, EXTENSION_BUNDLE_ID};

    let app = Path::new(APP_INSTALL_PATH);
    let embedded = install::embedded_extension(app);
    writeln!(out, "extension:     {EXTENSION_BUNDLE_ID}")?;
    writeln!(
        out,
        "app bundle:    {APP_INSTALL_PATH} ({})",
        if embedded.exists() {
            "present, extension embedded"
        } else if app.exists() {
            "present, but no embedded extension"
        } else {
            "missing: build and install it with scripts/vcam/build-macos.sh"
        }
    )?;
    match install::bundle_path() {
        Some(b) => writeln!(out, "running from:  {}", b.display())?,
        None => writeln!(
            out,
            "running from:  not an app bundle (vcam install must run from {APP_INSTALL_PATH}/Contents/MacOS/oak-cam)"
        )?,
    }
    let system = install::query_system_state();
    let system_text = match &system {
        Ok(SystemState::NotInstalled) => "not listed by systemextensionsctl".to_string(),
        Ok(SystemState::AwaitingApproval { version }) => {
            format!("activated, waiting for user approval ({version})")
        }
        Ok(SystemState::Enabled { version }) => format!("activated and enabled ({version})"),
        Ok(SystemState::Other { version, state }) => format!("{state} ({version})"),
        Err(e) => format!("systemextensionsctl failed: {e}"),
    };
    writeln!(out, "system:        {system_text}")?;
    let mut listed = 0;
    for id in CameraId::ALL {
        let line = match mac::status(id) {
            mac::Status::Active { device } => {
                listed += 1;
                format!("listed (CoreMediaIO device {device})")
            }
            mac::Status::NotInstalled => "not listed".to_string(),
        };
        writeln!(out, "  {:<10} {line}   [{}]", id.name(), id.uuids().device)?;
    }
    let state = if listed == CameraId::ALL.len() {
        "active"
    } else if matches!(system, Ok(SystemState::AwaitingApproval { .. })) {
        "awaiting approval"
    } else if matches!(system, Ok(SystemState::Enabled { .. })) {
        "enabled, but not all cameras are listed (an older extension build? bump CFBundleVersion, reinstall, maybe reboot)"
    } else {
        "not installed"
    };
    writeln!(out, "state:         {state}")?;
    if listed < CameraId::ALL.len() {
        write_macos_steps(out)?;
    }
    Ok(())
}

/// The exact steps from nothing to three active cameras on macOS.
#[cfg(target_os = "macos")]
pub(crate) fn write_macos_steps(out: &mut impl Write) -> Result<()> {
    use vcam::macos::{APP_INSTALL_PATH, SETTINGS_PATH};
    writeln!(out, "\nTo install the oak-cam cameras:")?;
    writeln!(
        out,
        "  1. scripts/vcam/build-macos.sh --provisionprofile <OakCam Developer ID profile> \\\n     \
         --notarize <notarytool keychain profile>\n     \
         (builds oak-cam.app with the extension, signs, notarises, installs {APP_INSTALL_PATH};\n     \
         see extensions/oak-camera-extension/README.md for the one-time App ID/profile setup)"
    )?;
    writeln!(
        out,
        "  2. {APP_INSTALL_PATH}/Contents/MacOS/oak-cam vcam install"
    )?;
    writeln!(
        out,
        "  3. approve \"oak-cam Cameras\" under {SETTINGS_PATH}\n     \
         (macOS 14: System Settings › Privacy & Security)"
    )?;
    writeln!(out, "  4. oak-cam vcam status   (expect: state: active)")?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn status(out: &mut impl Write) -> Result<()> {
    match vcam::linux::find_loopback_device() {
        Some(path) => writeln!(
            out,
            "v4l2loopback: {path} (all cameras publish here until M4b)"
        )?,
        None => writeln!(
            out,
            "v4l2loopback: no output device found\nload it with:\n  {}",
            vcam::linux::SETUP_HINT
        )?,
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn status(out: &mut impl Write) -> Result<()> {
    writeln!(out, "media source: {:?}", vcam::windows::status())?;
    writeln!(out, "{}", vcam::windows::missing_message())?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn status(out: &mut impl Write) -> Result<()> {
    writeln!(out, "no virtual camera backend on this platform")?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn install(out: &mut impl Write, timeout: Duration, wait: bool) -> Result<()> {
    use vcam::macos::APP_INSTALL_PATH;
    use vcam::macos::install::{self, InstallState};

    match install::bundle_path() {
        Some(app) => {
            let ext = install::embedded_extension(&app);
            writeln!(out, "requesting activation of {}", ext.display())?;
            if !ext.exists() {
                bail!(
                    "{} is missing; rebuild with scripts/vcam/build-macos.sh",
                    ext.display()
                );
            }
        }
        None => writeln!(
            out,
            "warning: not running from an app bundle; macOS will not find the extension.\n\
             Run {APP_INSTALL_PATH}/Contents/MacOS/oak-cam vcam install instead."
        )?,
    }
    out.flush()?;
    let state = install::request_activation(timeout, wait);
    if !matches!(state, InstallState::Failed(_)) {
        writeln!(out, "{}", state.describe())?;
    }
    match state {
        InstallState::Activated | InstallState::NeedsApproval | InstallState::AfterReboot => Ok(()),
        InstallState::Failed(reason) => bail!("activation failed: {reason}"),
        InstallState::Idle | InstallState::Requested => bail!("no answer from macOS"),
    }
}

#[cfg(target_os = "linux")]
fn install(out: &mut impl Write, _timeout: Duration, _wait: bool) -> Result<()> {
    writeln!(
        out,
        "Load v4l2loopback (needs sudo):\n  {}",
        vcam::linux::SETUP_HINT
    )?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn install(out: &mut impl Write, _timeout: Duration, _wait: bool) -> Result<()> {
    writeln!(
        out,
        "From an administrator PowerShell in the oak-cam folder run:\n  {}",
        vcam::windows::INSTALL_COMMAND
    )?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn install(out: &mut impl Write, _timeout: Duration, _wait: bool) -> Result<()> {
    writeln!(out, "no virtual camera backend on this platform")?;
    Ok(())
}

/// `rgb published N dropped M | left …` for every driven camera.
fn summary(driven: &[Driven]) -> String {
    driven
        .iter()
        .map(|d| {
            let s = d.cam.stats();
            format!(
                "{} published {} dropped {}",
                d.id.slug(),
                s.published,
                s.dropped
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// One camera being driven with a pattern.
struct Driven {
    id: CameraId,
    cam: Box<dyn VirtualCamera>,
    pattern: Pattern,
}

enum Pattern {
    Nv12(Nv12Pattern),
    Gray(GrayPattern),
}

fn test_pattern(
    out: &mut impl Write,
    cameras: &[CameraId],
    size: Option<(u32, u32)>,
    fps: f32,
    duration: Option<Duration>,
    null: bool,
) -> Result<()> {
    let mut driven = Vec::new();
    for &id in cameras {
        let res = id.kind().default_resolution();
        let (w, h) = size.unwrap_or((res.width, res.height));
        if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 {
            bail!("--width/--height must be even and non-zero (got {w}x{h})");
        }
        let mut cam: Box<dyn VirtualCamera> = if null {
            Box::new(vcam::NullCamera::new(id))
        } else {
            vcam::platform_camera(id)
        };
        match cam.start(w, h, fps) {
            Ok(()) => {}
            Err(VcamError::NotAvailable(msg)) => {
                writeln!(out, "{}: {msg}", id.name())?;
                #[cfg(target_os = "macos")]
                write_macos_steps(out)?;
                bail!("{} is not available", id.name());
            }
            Err(e) => bail!("{}: {e}", id.name()),
        }
        writeln!(
            out,
            "{}: publishing {} {w}x{h} @ {fps} fps",
            id.name(),
            match id.kind() {
                CameraKind::Rgb => "colour bars (NV12)",
                CameraKind::Mono => "gray steps (GRAY8 -> NV12)",
            }
        )?;
        let pattern = match id.kind() {
            CameraKind::Rgb => Pattern::Nv12(Nv12Pattern::new(w, h)),
            CameraKind::Mono => Pattern::Gray(GrayPattern::new(w, h)),
        };
        driven.push(Driven { id, cam, pattern });
    }
    match duration {
        Some(d) => writeln!(out, "running for {} s", d.as_secs())?,
        None => writeln!(out, "Ctrl-C to stop")?,
    }
    out.flush()?;

    let stop = crate::signal::install();

    let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
    let started = Instant::now();
    let mut next = started;
    let mut next_report = started + Duration::from_secs(1);
    while !stop.load(Ordering::Acquire) && duration.is_none_or(|d| started.elapsed() < d) {
        let ts = i64::try_from(started.elapsed().as_nanos()).unwrap_or(i64::MAX);
        for d in &mut driven {
            let frame = match &mut d.pattern {
                Pattern::Nv12(p) => p.next_frame(ts),
                Pattern::Gray(p) => p.next_frame(ts),
            };
            let _ = d.cam.publish(&frame);
        }
        let now = Instant::now();
        if now >= next_report {
            // Elapsed wall-clock seconds; after a stall the next report is
            // realigned rather than bursting to catch up.
            let seconds = now.duration_since(started).as_secs();
            next_report = started + Duration::from_secs(seconds + 1);
            writeln!(out, "[{seconds:>3}s] {}", summary(&driven))?;
            out.flush()?;
        }
        next += interval;
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        } else if now.duration_since(next) > interval * 5 {
            // Fell far behind (debugger, sleep): resync instead of bursting.
            next = Instant::now();
        }
    }
    for d in &mut driven {
        d.cam.stop();
    }
    writeln!(out, "stopped: {}", summary(&driven))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Harness {
        #[command(subcommand)]
        action: VcamAction,
    }

    #[test]
    fn parses_actions() {
        let h = Harness::try_parse_from(["vcam", "test-pattern", "--camera", "all"]).unwrap();
        let VcamAction::TestPattern {
            camera, width, fps, ..
        } = h.action
        else {
            panic!("expected test-pattern");
        };
        assert_eq!(camera, CameraSel::All);
        assert_eq!(camera.cameras(), CameraId::ALL);
        assert_eq!(width, None);
        assert!((fps - 30.0).abs() < f32::EPSILON);
        assert!(
            Harness::try_parse_from(["vcam", "test-pattern", "--width", "640"]).is_err(),
            "--width needs --height"
        );
        assert!(Harness::try_parse_from(["vcam", "test-pattern", "--camera", "depth"]).is_err());
        assert!(matches!(
            Harness::try_parse_from(["vcam", "status"]).unwrap().action,
            VcamAction::Status
        ));
        assert!(matches!(
            Harness::try_parse_from(["vcam", "install", "--wait"])
                .unwrap()
                .action,
            VcamAction::Install { wait: true, .. }
        ));
    }

    /// Single-device backends accept only RGB and name the milestone; a
    /// multi-device backend accepts `all` as the three cameras.
    #[test]
    fn single_device_backends_accept_only_rgb() {
        assert_eq!(
            CameraSel::Rgb.checked_cameras(false).unwrap(),
            [CameraId::Rgb]
        );
        for sel in [CameraSel::Left, CameraSel::Right, CameraSel::All] {
            let err = sel.checked_cameras(false).unwrap_err().to_string();
            assert!(err.contains("M4"), "{err}");
            assert!(err.contains("--camera rgb"), "{err}");
        }
        assert_eq!(CameraSel::All.checked_cameras(true).unwrap(), CameraId::ALL);
        assert_eq!(MULTI_DEVICE, cfg!(target_os = "macos"));
    }

    /// Driving `NullCamera`s for a short run prints elapsed-second reports and
    /// no Ctrl-C hint when a duration is given.
    #[test]
    fn timed_null_run_reports_elapsed_seconds() {
        let mut out = Vec::new();
        test_pattern(
            &mut out,
            &[CameraId::Left],
            Some((64, 40)),
            60.0,
            Some(Duration::from_millis(1100)),
            true,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("Ctrl-C"), "{text}");
        assert!(text.contains("[  1s] left published"), "{text}");
        assert!(text.contains("stopped: left published"), "{text}");
    }

    /// The pacing/report loop runs against real backends only on demand; here
    /// it is exercised with the size validation path.
    #[test]
    fn rejects_odd_sizes() {
        let mut out = Vec::new();
        let err = test_pattern(
            &mut out,
            &[CameraId::Rgb],
            Some((641, 480)),
            30.0,
            Some(Duration::ZERO),
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("even"), "{err}");
    }
}
