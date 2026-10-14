//! oak-cam: a Luxonis OAK-D Pro as three virtual webcams.
//!
//! M1: `--list [--json]` and `--probe [--device ID|IP]`. M2:
//! `--dump-frames N --sensor S --out DIR` and `--headless [--stats]`. M3:
//! `--headless` publishes to the virtual cameras; `vcam status|install|
//! test-pattern`. M5: `--headless` runs from the config file (`--config`,
//! hot-reloaded) through the oak-core supervisor; `config init|show`. M6:
//! with no arguments the GUI starts (`gui` module); the hidden
//! `--gui-script FILE` drives it for automated checks.
//!
//! depthai's host logger writes to fd 1, so after argument parsing (and
//! before any depthai call) fd 1 is pointed at stderr and all command output
//! goes through the saved original stdout.

mod cmd_config;
mod cmd_dump;
mod cmd_headless;
mod cmd_vcam;
mod gui;
mod signal;

use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use clap::{ArgGroup, CommandFactory as _, FromArgMatches as _, Parser};
use depthai::{Device, DeviceInfo, LogLevel, UsbSpeed};
use oak_core::capture::SensorId;
use serde::Serialize;

/// `--sensor` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum SensorArg {
    /// `CAM_A`, IMX378 colour.
    Rgb,
    /// `CAM_B`, OV9282 mono.
    Left,
    /// `CAM_C`, OV9282 mono.
    Right,
}

impl From<SensorArg> for SensorId {
    fn from(s: SensorArg) -> Self {
        match s {
            SensorArg::Rgb => Self::Rgb,
            SensorArg::Left => Self::Left,
            SensorArg::Right => Self::Right,
        }
    }
}

/// Turns a Luxonis OAK-D Pro (USB or PoE) into three virtual webcams.
#[derive(Debug, Parser)]
#[command(name = "oak-cam", args_conflicts_with_subcommands = true)]
#[allow(clippy::struct_excessive_bools)] // clap flags
#[command(group(ArgGroup::new("mode").args(["list", "probe", "dump_frames", "headless"])))]
#[command(group(
    ArgGroup::new("device_mode").args(["probe", "dump_frames", "headless"]).multiple(true)
))]
struct Cli {
    /// List available OAK devices (USB and PoE) and exit.
    #[arg(long, conflicts_with = "probe")]
    list: bool,

    /// With --list: print a JSON array instead of text.
    #[arg(long, requires = "list")]
    json: bool,

    /// Open a device and print its details and camera sensors.
    #[arg(long)]
    probe: bool,

    /// With --probe / --dump-frames / --headless: device id (MxId) or IP
    /// address. Default: the first found.
    #[arg(long, value_name = "ID|IP", requires = "device_mode")]
    device: Option<String>,

    /// Capture N frames from one sensor and write them as raw files.
    #[arg(long, value_name = "N", requires_all = ["sensor", "out"])]
    dump_frames: Option<u32>,

    /// With --dump-frames: which sensor.
    #[arg(long, value_enum, requires = "dump_frames")]
    sensor: Option<SensorArg>,

    /// With --dump-frames: output directory (created if missing).
    #[arg(long, value_name = "DIR", requires = "dump_frames")]
    out: Option<PathBuf>,

    /// With --dump-frames: output width (default 1920 rgb, 1280 mono).
    #[arg(long, requires = "dump_frames")]
    width: Option<u32>,

    /// With --dump-frames: output height (default 1080 rgb, 800 mono).
    #[arg(long, requires = "dump_frames")]
    height: Option<u32>,

    /// With --dump-frames: frame rate (default 30).
    #[arg(long, requires = "dump_frames")]
    fps: Option<f32>,

    /// Stream the sensors without a GUI until Ctrl-C, configured from the
    /// config file (reloaded whenever it changes).
    #[arg(long)]
    headless: bool,

    /// With --headless or the GUI: config file (default: the per-user
    /// config, see `oak-cam config show`). A missing file means defaults.
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with_all = ["list", "probe", "dump_frames"]
    )]
    config: Option<PathBuf>,

    /// With --headless: do not publish to virtual cameras.
    #[arg(long, requires = "headless")]
    no_vcam: bool,

    /// With --headless: publish into null virtual cameras (no OS device), to
    /// exercise the publish path without the OS side installed.
    #[arg(long, requires = "headless", conflicts_with = "no_vcam", hide = true)]
    null_vcam: bool,

    /// With --headless: print fps / latency / drops once per second.
    #[arg(long, requires = "headless")]
    stats: bool,

    /// With --headless: stop cleanly after SECS seconds instead of waiting
    /// for Ctrl-C.
    #[arg(long, value_name = "SECS", requires = "headless")]
    duration: Option<u64>,

    /// With --headless: test hook; after SECS seconds, stop the pipeline as
    /// if the device went away, to exercise lost-stream reconnect.
    #[arg(long, value_name = "SECS", requires = "headless", hide = true)]
    test_stream_loss_after: Option<f32>,

    /// GUI test hook: run the JSON step list in FILE (config edits, waits,
    /// screenshots, quit) once the window is up.
    #[arg(
        long,
        value_name = "FILE",
        hide = true,
        conflicts_with_all = ["list", "probe", "dump_frames", "headless"]
    )]
    gui_script: Option<PathBuf>,

    /// GUI test hook: publish into null virtual cameras at startup even when
    /// the OS ones are available, to exercise the "unavailable" banner and
    /// the switch to the OS cameras (the banner's Retry).
    #[arg(
        long,
        hide = true,
        conflicts_with_all = ["list", "probe", "dump_frames", "headless"]
    )]
    force_null_vcam: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Manage and test the OS virtual cameras (status, install, test-pattern).
    Vcam(cmd_vcam::VcamArgs),
    /// Create or show the config file (init, show).
    Config(cmd_config::ConfigArgs),
}

/// `--version` text. The depthai version comes from depthai-sys build
/// metadata so nothing calls into depthai before the stdout redirect.
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (depthai-core ",
    env!("OAK_DEPTHAI_VERSION"),
    ")"
);

/// depthai's wait for a booted device to reappear
/// (`DEPTHAI_BOOTUP_TIMEOUT`, ms), raised from its default for PoE: after
/// booting over Ethernet the camera's firmware solicits DHCP for ~25-30 s
/// before falling back to 169.254.1.222, so on a direct cable the default
/// wait gives up first (the shim also lengthens the PoE initial watchdog,
/// see `oak_device_open`). It applies to every open; USB boots take 1-3 s,
/// so it only matters when a device vanishes after boot.
const BOOTUP_TIMEOUT_MS: &str = "90000";

/// depthai's automatic crash-dump collection (`DEPTHAI_CRASHDUMP`), turned
/// off on Windows: when the camera holds a crash dump (left by a process
/// that exited without closing the device), depthai-core 3.10.0 retrieves
/// it at device close and faults in depthai-core.dll while writing the dump
/// archive. The dump is then never cleared, so every later close crashes
/// until the camera is power-cycled. A value the user sets (a dump
/// directory, or anything else) wins: `env_default` only sets it when unset.
#[cfg(windows)]
const CRASHDUMP: &str = "0";

fn main() -> ExitCode {
    // First, before any thread of ours exists. depthai's static initialisers
    // have already started one idle async-log worker (the spdlog thread_pool
    // in ThreadedNodeImpl.hpp), which does not touch the environment until a
    // pipeline exists. Through the shim so that, on Windows, depthai's
    // getenv (the CRT copy) sees it. Shim-only: it does not initialise
    // depthai, so it may precede the log redirect below.
    if let Err(e) = depthai::env_default("DEPTHAI_BOOTUP_TIMEOUT", BOOTUP_TIMEOUT_MS) {
        eprintln!("oak-cam: cannot set DEPTHAI_BOOTUP_TIMEOUT: {e}");
    }
    #[cfg(windows)]
    {
        if let Err(e) = depthai::env_default("DEPTHAI_CRASHDUMP", CRASHDUMP) {
            eprintln!("oak-cam: cannot set DEPTHAI_CRASHDUMP: {e}");
        }
    }
    // clap's --help / --version / usage errors print and exit here, before
    // the redirect, on the real stdout / stderr.
    let cmd = Cli::command().version(VERSION);
    let cli = match Cli::from_arg_matches(&cmd.clone().get_matches()) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };

    // Must precede every depthai call.
    let saved = match depthai::redirect_host_logs_to_stderr() {
        Ok(saved) => saved,
        Err(e) => {
            eprintln!("oak-cam: cannot redirect stdout: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut out = BufWriter::new(saved);
    // The GUI is whatever no other mode claims (the final `else` below).
    let gui_mode = cli.command.is_none()
        && !cli.list
        && !cli.probe
        && cli.dump_frames.is_none()
        && !cli.headless;
    // Held to the end of `main`: dropping it flushes the log file. No log
    // file for `--gui-script` runs, which must not touch the user's log.
    let _log_guard = init_tracing(gui_mode, cli.gui_script.is_none());

    let result = if let Some(Command::Vcam(args)) = &cli.command {
        cmd_vcam::run(&mut out, args)
    } else if let Some(Command::Config(args)) = &cli.command {
        cmd_config::run(&mut out, args)
    } else if cli.list {
        list(&mut out, cli.json)
    } else if cli.probe {
        probe(&mut out, cli.device.as_deref())
    } else if let (Some(count), Some(sensor), Some(dir)) = (cli.dump_frames, cli.sensor, &cli.out) {
        let args = cmd_dump::DumpArgs {
            count,
            sensor: sensor.into(),
            out_dir: dir,
            width: cli.width,
            height: cli.height,
            fps: cli.fps,
            device: cli.device.as_deref(),
        };
        cmd_dump::run(&mut out, &args)
    } else if cli.headless {
        let args = cmd_headless::HeadlessArgs {
            device: cli.device.as_deref(),
            vcam: if cli.no_vcam {
                cmd_headless::VcamMode::Off
            } else if cli.null_vcam {
                cmd_headless::VcamMode::Null
            } else {
                cmd_headless::VcamMode::Platform
            },
            stats: cli.stats,
            duration: cli.duration.map(std::time::Duration::from_secs),
            config: cli.config.as_deref(),
            test_stream_loss_after: cli.test_stream_loss_after,
        };
        cmd_headless::run(&mut out, &args)
    } else {
        // No mode selected: the GUI.
        out.flush().map_err(anyhow::Error::from).and_then(|()| {
            gui::run(&gui::GuiArgs {
                script: cli.gui_script.clone(),
                force_null_vcam: cli.force_null_vcam,
                config: cli.config.clone(),
            })
        })
    };
    let result = result.and_then(|()| out.flush().map_err(anyhow::Error::from));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        // The reader went away (`oak-cam ... | head`): end quietly.
        Err(e) if is_broken_pipe(&e) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = out.flush();
            eprintln!("oak-cam: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Whether `e` is (caused by) EPIPE on our stdout.
fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
    })
}

/// `tracing` to stderr, filtered by `RUST_LOG` (default `warn`); depthai
/// device logs are forwarded under target `depthai`.
///
/// With `gui` and `gui_log_file` set, a second layer also writes to
/// [`gui::log_file_path`] (a Finder- or Explorer-launched app has no
/// terminal; Help › Reveal Log) at `info` for oak-cam's own targets and
/// `warn` elsewhere, independent of `RUST_LOG`. The file is appended to and
/// rotated to `oak-cam.log.1` past [`gui::LOG_MAX_BYTES`]. `gui_log_file` is
/// false for `--gui-script` runs. The returned guard keeps the writer
/// thread alive.
fn init_tracing(
    gui: bool,
    gui_log_file: bool,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::filter::{LevelFilter, Targets};
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    use tracing_subscriber::{EnvFilter, Layer as _, fmt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let stderr = fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(filter);

    let mut log_error = None;
    let file = if gui && gui_log_file {
        match gui::log_file_path()
            .ok_or_else(|| std::io::Error::other("no log directory"))
            .and_then(|path| Ok((path.clone(), gui::LogFile::open(&path, gui::LOG_MAX_BYTES)?)))
        {
            Ok(f) => Some(f),
            Err(e) => {
                log_error = Some(e);
                None
            }
        }
    } else {
        None
    };
    let (file_layer, guard) = match file {
        Some((path, file)) => {
            let (writer, guard) = tracing_appender::non_blocking(file);
            let targets = Targets::new()
                .with_target("oak_cam", LevelFilter::INFO)
                .with_target("oak_core", LevelFilter::INFO)
                .with_target("vcam", LevelFilter::INFO)
                .with_target("depthai", LevelFilter::INFO)
                .with_default(LevelFilter::WARN);
            let layer = fmt::layer()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(targets);
            gui::set_log_path(path);
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };
    tracing_subscriber::registry()
        .with(stderr)
        .with(file_layer)
        .init();
    if let Some(e) = log_error {
        tracing::warn!("no log file: {e}");
    }
    if let Some(path) = gui::log_path() {
        tracing::info!("oak-cam {VERSION} started, log {}", path.display());
    }

    depthai::set_log_handler(|level, node, message| match level {
        LogLevel::Trace => tracing::trace!(target: "depthai", node, "{message}"),
        LogLevel::Debug => tracing::debug!(target: "depthai", node, "{message}"),
        LogLevel::Info => tracing::info!(target: "depthai", node, "{message}"),
        LogLevel::Warn => tracing::warn!(target: "depthai", node, "{message}"),
        LogLevel::Err | LogLevel::Critical => tracing::error!(target: "depthai", node, "{message}"),
        LogLevel::Off | LogLevel::Other(_) => {}
    });
    guard
}

#[derive(Serialize)]
struct DeviceJson<'a> {
    id: &'a str,
    name: &'a str,
    protocol: &'a str,
    state: &'a str,
    platform: &'a str,
}

fn list(out: &mut impl Write, json: bool) -> Result<()> {
    let devices = Device::list().context("device discovery failed")?;
    if json {
        let rows: Vec<DeviceJson<'_>> = devices
            .iter()
            .map(|d| DeviceJson {
                id: &d.id,
                name: &d.name,
                protocol: d.protocol.as_str(),
                state: d.state.as_str(),
                platform: d.platform.as_str(),
            })
            .collect();
        serde_json::to_writer_pretty(&mut *out, &rows)?;
        writeln!(out)?;
        return Ok(());
    }
    if devices.is_empty() {
        writeln!(out, "no devices found")?;
        return Ok(());
    }
    for DeviceInfo {
        name,
        id,
        protocol,
        state,
        ..
    } in &devices
    {
        writeln!(out, "{id}  {protocol}  {state}  {name}")?;
    }
    Ok(())
}

fn probe(out: &mut impl Write, selector: Option<&str>) -> Result<()> {
    let selector = selector.unwrap_or("");
    if selector.is_empty()
        && Device::list()
            .context("device discovery failed")?
            .is_empty()
    {
        bail!("no devices found");
    }
    let dev = Device::open(selector, UsbSpeed::SuperPlus).with_context(|| {
        format!(
            "cannot open device {}",
            if selector.is_empty() {
                "(any)"
            } else {
                selector
            }
        )
    })?;

    writeln!(out, "name:       {}", dev.name()?)?;
    writeln!(out, "id:         {}", dev.id()?)?;
    writeln!(out, "protocol:   {}", dev.protocol()?)?;
    writeln!(out, "usb speed:  {}", dev.usb_speed()?)?;
    writeln!(out, "platform:   {}", dev.platform()?)?;
    match dev.chip_temp() {
        Ok(t) => writeln!(out, "chip temp:  {t:.1} C")?,
        Err(e) => writeln!(out, "chip temp:  n/a ({e})")?,
    }
    match dev.ir_drivers() {
        Ok(n) => writeln!(out, "ir drivers: {n}")?,
        Err(e) => writeln!(out, "ir drivers: n/a ({e})")?,
    }
    let cams = dev.camera_features()?;
    writeln!(out, "cameras:    {}", cams.len())?;
    for c in &cams {
        let alias = if c.name.is_empty() {
            String::new()
        } else {
            format!(" ({})", c.name)
        };
        writeln!(
            out,
            "  {}{alias}  {}  {}  max {}x{}  autofocus: {}",
            c.socket,
            c.sensor_name,
            c.sensor_type,
            c.width,
            c.height,
            if c.has_autofocus { "yes" } else { "no" }
        )?;
        for cfg in &c.configs {
            writeln!(
                out,
                "      {}x{} @ {}–{} fps  {}",
                cfg.width,
                cfg.height,
                fmt_fps(cfg.min_fps),
                fmt_fps(cfg.max_fps),
                cfg.sensor_type
            )?;
        }
    }
    Ok(())
}

/// `30` for whole numbers, `2.5` otherwise.
fn fmt_fps(fps: f32) -> String {
    if (fps - fps.round()).abs() < 0.01 {
        format!("{fps:.0}")
    } else {
        format!("{fps:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses() {
        Cli::command().debug_assert();
        let cli = Cli::try_parse_from(["oak-cam", "--list", "--json"]).unwrap();
        assert!(cli.list && cli.json);
        let cli = Cli::try_parse_from(["oak-cam", "--probe", "--device", "192.0.2.44"]).unwrap();
        assert!(cli.probe);
        assert_eq!(cli.device.as_deref(), Some("192.0.2.44"));
        assert!(Cli::try_parse_from(["oak-cam", "--json"]).is_err());
        assert!(Cli::try_parse_from(["oak-cam", "--device", "foo"]).is_err());
    }

    #[test]
    fn m2_cli_parses() {
        let cli = Cli::try_parse_from([
            "oak-cam",
            "--dump-frames",
            "10",
            "--sensor",
            "left",
            "--out",
            "/tmp/d",
            "--width",
            "640",
        ])
        .unwrap();
        assert_eq!(cli.dump_frames, Some(10));
        assert_eq!(cli.sensor, Some(SensorArg::Left));
        assert_eq!(SensorId::from(SensorArg::Left), SensorId::Left);
        assert_eq!(cli.width, Some(640));
        // --sensor and --out are required with --dump-frames.
        assert!(Cli::try_parse_from(["oak-cam", "--dump-frames", "1", "--out", "x"]).is_err());
        assert!(Cli::try_parse_from(["oak-cam", "--sensor", "rgb"]).is_err());

        let cli = Cli::try_parse_from([
            "oak-cam",
            "--headless",
            "--device",
            "14442C10",
            "--no-vcam",
            "--stats",
        ])
        .unwrap();
        assert!(cli.headless && cli.no_vcam && cli.stats);
        assert!(Cli::try_parse_from(["oak-cam", "--stats"]).is_err());
        let cli = Cli::try_parse_from(["oak-cam", "--headless", "--null-vcam"]).unwrap();
        assert!(cli.null_vcam && !cli.no_vcam);
        assert!(
            Cli::try_parse_from(["oak-cam", "--headless", "--null-vcam", "--no-vcam"]).is_err()
        );
        assert!(Cli::try_parse_from(["oak-cam", "--null-vcam"]).is_err());
        assert!(Cli::try_parse_from(["oak-cam", "--headless", "--probe"]).is_err());
        let cli = Cli::try_parse_from(["oak-cam", "--headless", "--duration", "5"]).unwrap();
        assert_eq!(cli.duration, Some(5));
        assert!(Cli::try_parse_from(["oak-cam", "--duration", "5"]).is_err());
        assert!(Cli::try_parse_from(["oak-cam", "--list", "--device", "x"]).is_err());
    }

    #[test]
    fn m5_cli_parses() {
        let cli = Cli::try_parse_from([
            "oak-cam",
            "--headless",
            "--null-vcam",
            "--config",
            "/tmp/oak-m5.toml",
        ])
        .unwrap();
        assert_eq!(
            cli.config.as_deref(),
            Some(std::path::Path::new("/tmp/oak-m5.toml"))
        );
        assert!(Cli::try_parse_from(["oak-cam", "--config", "x.toml", "--list"]).is_err());
        let cli = Cli::try_parse_from(["oak-cam", "--config", "x.toml"]).unwrap();
        assert_eq!(cli.config.as_deref(), Some(std::path::Path::new("x.toml")));
        let cli =
            Cli::try_parse_from(["oak-cam", "config", "init", "--path", "/tmp/c.toml"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Config(_))));
        let cli = Cli::try_parse_from(["oak-cam", "config", "show"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Config(_))));
        assert!(Cli::try_parse_from(["oak-cam", "config", "show", "--headless"]).is_err());
    }

    #[test]
    fn gui_cli_parses() {
        let cli = Cli::try_parse_from(["oak-cam"]).unwrap();
        assert!(!cli.list && !cli.headless && cli.command.is_none() && cli.gui_script.is_none());
        let cli = Cli::try_parse_from(["oak-cam", "--gui-script", "/tmp/s.json"]).unwrap();
        assert_eq!(
            cli.gui_script.as_deref(),
            Some(std::path::Path::new("/tmp/s.json"))
        );
        assert!(Cli::try_parse_from(["oak-cam", "--gui-script", "x", "--headless"]).is_err());
        let cli = Cli::try_parse_from(["oak-cam", "--force-null-vcam"]).unwrap();
        assert!(cli.force_null_vcam);
        assert!(Cli::try_parse_from(["oak-cam", "--force-null-vcam", "--list"]).is_err());
    }

    #[test]
    fn fps_formatting() {
        assert_eq!(fmt_fps(30.0), "30");
        assert_eq!(fmt_fps(1.03), "1.0");
        assert_eq!(fmt_fps(2.5), "2.5");
    }
}
