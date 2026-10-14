//! The configuration model, persisted as TOML at
//! `<config dir>/oak-cam/config.toml` (see [`Config::default_path`]).
//!
//! Every struct uses `#[serde(default)]`, so a file only needs the keys it
//! changes; unknown keys are an error (a typo should not be silently
//! ignored). Enums are externally tagged, e.g.
//! `exposure = { Manual = { us = 2000, iso = 400 } }`.
//!
//! [`Config::validate`] clamps every value into its device range and snaps
//! stream sizes to the supported table, returning one [`Warning`] per fix.

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::capture::SensorId;

/// The config file format this build reads and writes.
pub const CONFIG_VERSION: u32 = 1;

/// Errors loading or saving a [`Config`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Reading or writing the file failed.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
    /// The file is not valid TOML, or does not match the model.
    #[error("{path}: {message}")]
    Parse {
        /// The file.
        path: PathBuf,
        /// The parser's message (with line and column).
        message: String,
    },
    /// The file declares a format version this build does not know.
    #[error(
        "{path}: config version {found} is not supported by this oak-cam (it reads version \
         {CONFIG_VERSION}); upgrade oak-cam or move the file aside to start from defaults"
    )]
    UnsupportedVersion {
        /// The file.
        path: PathBuf,
        /// The `version` it declares.
        found: i64,
    },
    /// No per-user config directory on this system.
    #[error("cannot determine the per-user config directory")]
    NoConfigDir,
    /// Serialising failed (should not happen for a valid model).
    #[error("cannot serialise config: {0}")]
    Serialize(String),
}

/// Everything oak-cam remembers between runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// File format version ([`CONFIG_VERSION`]).
    pub version: u32,
    /// Which device to open.
    pub device: DeviceSelection,
    /// Device-level settings (IR, USB speed cap).
    pub device_settings: DeviceSettings,
    /// The colour camera (`CAM_A`).
    pub rgb: RgbSettings,
    /// The two mono cameras (`CAM_B`, `CAM_C`).
    pub mono: MonoGroup,
    /// The OS virtual cameras.
    pub vcam: VcamConfig,
    /// GUI state.
    pub ui: UiState,
    /// Per-transport stream sizes and rates the user chose (`None`: the
    /// built-in [`Transport::defaults`]). Written only when one is set.
    #[serde(skip_serializing_if = "Profiles::is_empty")]
    pub profiles: Profiles,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            device: DeviceSelection::default(),
            device_settings: DeviceSettings::default(),
            rgb: RgbSettings::default(),
            mono: MonoGroup::default(),
            vcam: VcamConfig::default(),
            ui: UiState::default(),
            profiles: Profiles::default(),
        }
    }
}

/// Which device to open.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceSelection {
    /// The first device found.
    #[default]
    Auto,
    /// By device id (MxId).
    ById(String),
    /// By IP address (PoE).
    ByIp(String),
}

impl DeviceSelection {
    /// The selector string `Device::open` takes (`""` for any).
    pub fn selector(&self) -> &str {
        match self {
            Self::Auto => "",
            Self::ById(s) | Self::ByIp(s) => s,
        }
    }
}

impl fmt::Display for DeviceSelection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("(first found)"),
            Self::ById(s) | Self::ByIp(s) => f.write_str(s),
        }
    }
}

/// USB link speed cap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UsbSpeedPref {
    /// USB 2 (480 Mbps).
    High,
    /// USB 3 (5 Gbps).
    Super,
    /// No cap.
    #[default]
    SuperPlus,
}

impl UsbSpeedPref {
    /// The depthai value.
    pub fn to_dai(self) -> depthai::UsbSpeed {
        match self {
            Self::High => depthai::UsbSpeed::High,
            Self::Super => depthai::UsbSpeed::Super,
            Self::SuperPlus => depthai::UsbSpeed::SuperPlus,
        }
    }
}

/// Device-level settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeviceSettings {
    /// IR dot projector intensity, 0.0..=1.0 (Pro models only).
    pub ir_dot: f32,
    /// IR flood LED intensity, 0.0..=1.0 (Pro models only).
    pub ir_flood: f32,
    /// USB link speed cap.
    pub max_usb_speed: UsbSpeedPref,
}

/// One sensor's output stream.
///
/// In a file, `width`, `height` and `fps` may be left out; they then take
/// the sensor's default (parsed as 0, filled in by [`Config::from_toml`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamSettings {
    /// Output width, pixels.
    #[serde(default)]
    pub width: u32,
    /// Output height, pixels.
    #[serde(default)]
    pub height: u32,
    /// Frame rate.
    #[serde(default)]
    pub fps: f32,
    /// Whether this sensor streams at all.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

impl StreamSettings {
    /// Replaces unset (zero) size and rate with `sensor`'s defaults.
    fn fill_defaults(&mut self, sensor: SensorId) {
        let d = Self::default_for(sensor);
        if self.width == 0 {
            self.width = d.width;
        }
        if self.height == 0 {
            self.height = d.height;
        }
        if self.fps == 0.0 {
            self.fps = d.fps;
        }
    }

    /// The USB3 default for `sensor`: RGB 1920x1080@30, mono 1280x800@30.
    pub fn default_for(sensor: SensorId) -> Self {
        let (width, height) = match sensor {
            SensorId::Rgb => (1920, 1080),
            SensorId::Left | SensorId::Right => (1280, 800),
        };
        Self {
            width,
            height,
            fps: 30.0,
            enabled: true,
        }
    }
}

/// How the device is attached, which sets the bandwidth the streams can
/// use and so the [`StreamProfile`] applied on connect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Transport {
    /// Gigabit Ethernet (PoE models); limited by a per-frame cost on the
    /// camera's network path (~8.5 ms per frame across all streams) plus
    /// bytes near the gigabit rate, not by the wire (measured).
    Poe,
    /// USB 3 (`Super` / `SuperPlus`), or a speed not reported.
    Usb3,
    /// USB 2 (High) or slower, ~320 Mbps usable.
    Usb2,
}

impl Transport {
    /// Every transport, in display order.
    pub const ALL: [Self; 3] = [Self::Poe, Self::Usb3, Self::Usb2];

    /// The transport of a device reporting `protocol` and `usb`: TCP/IP is
    /// PoE; USB `High` / `Full` / `Low` is USB2; anything else (`Super`,
    /// `SuperPlus`, `Unknown`) counts as USB3.
    pub fn of(protocol: depthai::Protocol, usb: depthai::UsbSpeed) -> Self {
        use depthai::UsbSpeed;
        match (protocol, usb) {
            (depthai::Protocol::TcpIp, _) => Self::Poe,
            (_, UsbSpeed::High | UsbSpeed::Full | UsbSpeed::Low) => Self::Usb2,
            _ => Self::Usb3,
        }
    }

    /// The `[profiles]` key: `poe` / `usb3` / `usb2`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Poe => "poe",
            Self::Usb3 => "usb3",
            Self::Usb2 => "usb2",
        }
    }

    /// `PoE` / `USB3` / `USB2`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Poe => "PoE",
            Self::Usb3 => "USB3",
            Self::Usb2 => "USB2",
        }
    }

    /// The built-in profile: USB3 RGB 1920x1080@30 + mono 1280x800@30
    /// (the [`StreamSettings::default_for`] values); PoE 1280x720@20 +
    /// 640x400@20 (60 frames/s + ~303 Mbps, ~76 % of the per-frame PoE
    /// model); USB2 1280x720@12 + 640x400@12
    /// (~182 Mbps of ~320).
    pub fn defaults(self) -> StreamProfile {
        let shape = |width, height, fps| StreamShape { width, height, fps };
        match self {
            Self::Usb3 => StreamProfile::new(
                StreamShape::of(&StreamSettings::default_for(SensorId::Rgb)),
                StreamShape::of(&StreamSettings::default_for(SensorId::Left)),
            ),
            Self::Poe => StreamProfile::new(shape(1280, 720, 20.0), shape(640, 400, 20.0)),
            Self::Usb2 => StreamProfile::new(shape(1280, 720, 12.0), shape(640, 400, 12.0)),
        }
    }
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A stream's size and rate (no `enabled`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamShape {
    /// Output width, pixels.
    pub width: u32,
    /// Output height, pixels.
    pub height: u32,
    /// Frame rate.
    pub fps: f32,
}

impl StreamShape {
    /// The shape of `s`.
    pub fn of(s: &StreamSettings) -> Self {
        Self {
            width: s.width,
            height: s.height,
            fps: s.fps,
        }
    }

    /// Sets `s`'s size and rate (not `enabled`); returns whether it changed.
    fn apply_to(self, s: &mut StreamSettings) -> bool {
        let before = *s;
        (s.width, s.height, s.fps) = (self.width, self.height, self.fps);
        *s != before
    }
}

impl fmt::Display for StreamShape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}@{}", self.width, self.height, self.fps)
    }
}

/// The RGB and mono stream shapes for one [`Transport`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamProfile {
    /// The colour camera.
    pub rgb: StreamShape,
    /// The mono pair (`mono.shared`).
    pub mono: StreamShape,
    /// The left camera's override, when it has its own size or rate
    /// (`None`: it follows `mono`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<StreamShape>,
    /// The right camera's override, as `left`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<StreamShape>,
}

impl StreamProfile {
    /// A profile without per-camera overrides.
    pub fn new(rgb: StreamShape, mono: StreamShape) -> Self {
        Self {
            rgb,
            mono,
            left: None,
            right: None,
        }
    }

    /// The override shape for `cam`.
    pub fn cam(&self, cam: MonoCam) -> Option<StreamShape> {
        match cam {
            MonoCam::Left => self.left,
            MonoCam::Right => self.right,
        }
    }
}

impl fmt::Display for StreamProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rgb {}, mono {}", self.rgb, self.mono)?;
        if let Some(l) = self.left {
            write!(f, ", left {l}")?;
        }
        if let Some(r) = self.right {
            write!(f, ", right {r}")?;
        }
        Ok(())
    }
}

/// The user's stream profile per transport; `None` uses
/// [`Transport::defaults`]. In a file: `[profiles.poe]` with
/// `rgb = { width = 1280, height = 720, fps = 30.0 }` and `mono = { ... }`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profiles {
    /// PoE.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poe: Option<StreamProfile>,
    /// USB3.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usb3: Option<StreamProfile>,
    /// USB2.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usb2: Option<StreamProfile>,
}

impl Profiles {
    /// The custom profile for `t`, if any.
    pub fn get(&self, t: Transport) -> Option<&StreamProfile> {
        match t {
            Transport::Poe => self.poe.as_ref(),
            Transport::Usb3 => self.usb3.as_ref(),
            Transport::Usb2 => self.usb2.as_ref(),
        }
    }

    /// The slot for `t` (set it to `None` to go back to the defaults).
    pub fn get_mut(&mut self, t: Transport) -> &mut Option<StreamProfile> {
        match t {
            Transport::Poe => &mut self.poe,
            Transport::Usb3 => &mut self.usb3,
            Transport::Usb2 => &mut self.usb2,
        }
    }

    /// No custom profile at all.
    pub fn is_empty(&self) -> bool {
        Transport::ALL.iter().all(|&t| self.get(t).is_none())
    }
}

/// Exposure control.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Exposure {
    /// Auto exposure.
    Auto {
        /// Compensation, -9..=9.
        #[serde(default)]
        comp: i8,
        /// Longest exposure AE may pick, µs (`None`: about one frame time).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit_us: Option<u32>,
        /// Hold the current exposure.
        #[serde(default)]
        lock: bool,
    },
    /// Fixed exposure.
    Manual {
        /// Exposure time, µs (1..=33000).
        us: u32,
        /// Sensitivity, 100..=1600.
        iso: u32,
    },
}

impl Default for Exposure {
    fn default() -> Self {
        Self::Auto {
            comp: 0,
            limit_us: None,
            lock: false,
        }
    }
}

/// Mains-flicker compensation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Antibanding {
    /// None.
    Off,
    /// 50 Hz mains.
    Mains50Hz,
    /// 60 Hz mains.
    Mains60Hz,
    /// Detect automatically.
    #[default]
    Auto,
}

impl Antibanding {
    /// The depthai value.
    pub fn to_dai(self) -> depthai::Antibanding {
        match self {
            Self::Off => depthai::Antibanding::Off,
            Self::Mains50Hz => depthai::Antibanding::Mains50Hz,
            Self::Mains60Hz => depthai::Antibanding::Mains60Hz,
            Self::Auto => depthai::Antibanding::Auto,
        }
    }
}

/// Autofocus mode (RGB only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AfMode {
    /// Single-shot on trigger.
    Auto,
    /// Single-shot, close range.
    Macro,
    /// Continuous, tuned for video.
    #[default]
    ContinuousVideo,
    /// Continuous, tuned for stills.
    ContinuousPicture,
    /// Extended depth of field.
    Edof,
}

impl AfMode {
    /// The depthai value.
    pub fn to_dai(self) -> depthai::AfMode {
        match self {
            Self::Auto => depthai::AfMode::Auto,
            Self::Macro => depthai::AfMode::Macro,
            Self::ContinuousVideo => depthai::AfMode::ContinuousVideo,
            Self::ContinuousPicture => depthai::AfMode::ContinuousPicture,
            Self::Edof => depthai::AfMode::Edof,
        }
    }
}

/// Focus control (RGB only).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Focus {
    /// Autofocus in this mode.
    Auto(AfMode),
    /// Fixed lens position, 0 (far) ..=255 (near).
    Manual(u8),
}

impl Default for Focus {
    fn default() -> Self {
        Self::Auto(AfMode::default())
    }
}

/// Auto white balance mode (RGB only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AwbMode {
    /// Automatic.
    #[default]
    Auto,
    /// Incandescent preset.
    Incandescent,
    /// Fluorescent preset.
    Fluorescent,
    /// Warm fluorescent preset.
    WarmFluorescent,
    /// Daylight preset.
    Daylight,
    /// Cloudy daylight preset.
    CloudyDaylight,
    /// Twilight preset.
    Twilight,
    /// Shade preset.
    Shade,
}

impl AwbMode {
    /// The depthai value.
    pub fn to_dai(self) -> depthai::AwbMode {
        match self {
            Self::Auto => depthai::AwbMode::Auto,
            Self::Incandescent => depthai::AwbMode::Incandescent,
            Self::Fluorescent => depthai::AwbMode::Fluorescent,
            Self::WarmFluorescent => depthai::AwbMode::WarmFluorescent,
            Self::Daylight => depthai::AwbMode::Daylight,
            Self::CloudyDaylight => depthai::AwbMode::CloudyDaylight,
            Self::Twilight => depthai::AwbMode::Twilight,
            Self::Shade => depthai::AwbMode::Shade,
        }
    }
}

/// White balance (RGB only).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WhiteBalance {
    /// Automatic white balance.
    Auto {
        /// AWB mode / preset.
        #[serde(default)]
        mode: AwbMode,
        /// Hold the current white balance.
        #[serde(default)]
        lock: bool,
    },
    /// Fixed colour temperature, 1000..=12000 K.
    Kelvin(u32),
}

impl Default for WhiteBalance {
    fn default() -> Self {
        Self::Auto {
            mode: AwbMode::Auto,
            lock: false,
        }
    }
}

/// The colour camera's settings.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RgbSettings {
    /// Output stream.
    pub stream: StreamSettings,
    /// Exposure.
    pub exposure: Exposure,
    /// Anti-banding.
    pub antibanding: Antibanding,
    /// Focus.
    pub focus: Focus,
    /// White balance.
    pub wb: WhiteBalance,
    /// -10..=10.
    pub brightness: i8,
    /// -10..=10.
    pub contrast: i8,
    /// -10..=10.
    pub saturation: i8,
    /// 0..=4.
    pub sharpness: u8,
    /// 0..=4.
    pub luma_denoise: u8,
    /// 0..=4.
    pub chroma_denoise: u8,
}

impl Default for RgbSettings {
    fn default() -> Self {
        Self {
            stream: StreamSettings::default_for(SensorId::Rgb),
            exposure: Exposure::default(),
            antibanding: Antibanding::default(),
            focus: Focus::default(),
            wb: WhiteBalance::default(),
            brightness: 0,
            contrast: 0,
            saturation: 0,
            sharpness: 0,
            luma_denoise: 0,
            chroma_denoise: 0,
        }
    }
}

/// One mono camera's settings (fixed focus, no colour).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MonoSettings {
    /// Output stream.
    pub stream: StreamSettings,
    /// Exposure.
    pub exposure: Exposure,
    /// Anti-banding.
    pub antibanding: Antibanding,
    /// -10..=10.
    pub brightness: i8,
    /// -10..=10.
    pub contrast: i8,
    /// 0..=4.
    pub sharpness: u8,
    /// 0..=4.
    pub luma_denoise: u8,
}

impl Default for MonoSettings {
    fn default() -> Self {
        Self {
            stream: StreamSettings::default_for(SensorId::Left),
            exposure: Exposure::default(),
            antibanding: Antibanding::default(),
            brightness: 0,
            contrast: 0,
            sharpness: 0,
            luma_denoise: 0,
        }
    }
}

/// A mono camera of the pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MonoCam {
    /// `CAM_B`.
    Left,
    /// `CAM_C`.
    Right,
}

impl MonoCam {
    /// Both, left first.
    pub const ALL: [Self; 2] = [Self::Left, Self::Right];

    /// The sensor.
    pub fn sensor(self) -> SensorId {
        match self {
            Self::Left => SensorId::Left,
            Self::Right => SensorId::Right,
        }
    }
}

/// The mono pair: one shared settings block, plus an optional override per
/// camera.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MonoGroup {
    /// Settings both cameras use unless overridden.
    pub shared: MonoSettings,
    /// Left camera override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left: Option<MonoSettings>,
    /// Right camera override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right: Option<MonoSettings>,
}

impl MonoGroup {
    fn slot(&self, cam: MonoCam) -> Option<&MonoSettings> {
        match cam {
            MonoCam::Left => self.left.as_ref(),
            MonoCam::Right => self.right.as_ref(),
        }
    }

    fn slot_mut(&mut self, cam: MonoCam) -> &mut Option<MonoSettings> {
        match cam {
            MonoCam::Left => &mut self.left,
            MonoCam::Right => &mut self.right,
        }
    }

    /// The settings `cam` actually uses: its override, else `shared`.
    pub fn effective(&self, cam: MonoCam) -> &MonoSettings {
        self.slot(cam).unwrap_or(&self.shared)
    }

    /// Whether `cam` has an override.
    pub fn is_overridden(&self, cam: MonoCam) -> bool {
        self.slot(cam).is_some()
    }

    /// Turns `cam`'s override on (starting from a copy of `shared`, so the
    /// camera's effective settings do not change) or off (back to
    /// `shared`). Turning on an existing override keeps it.
    pub fn set_override(&mut self, cam: MonoCam, on: bool) {
        let shared = self.shared;
        let slot = self.slot_mut(cam);
        if on {
            slot.get_or_insert(shared);
        } else {
            *slot = None;
        }
    }

    /// The settings to edit for `cam`: its override if it has one, else
    /// `shared` (which edits both cameras that are not overridden).
    pub fn editable(&mut self, cam: MonoCam) -> &mut MonoSettings {
        match cam {
            MonoCam::Left => self.left.as_mut().unwrap_or(&mut self.shared),
            MonoCam::Right => self.right.as_mut().unwrap_or(&mut self.shared),
        }
    }
}

/// One OS virtual camera.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VcamOutput {
    /// Publish this sensor to its virtual camera.
    pub enabled: bool,
    /// Linux: the v4l2loopback device to use (e.g. `/dev/video10`);
    /// `None` matches by card label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linux_device: Option<String>,
}

impl Default for VcamOutput {
    fn default() -> Self {
        Self {
            enabled: true,
            linux_device: None,
        }
    }
}

/// The three OS virtual cameras.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VcamConfig {
    /// "OAK RGB".
    pub rgb: VcamOutput,
    /// "OAK Left".
    pub left: VcamOutput,
    /// "OAK Right".
    pub right: VcamOutput,
}

impl VcamConfig {
    /// The output for `sensor`.
    pub fn get(&self, sensor: SensorId) -> &VcamOutput {
        match sensor {
            SensorId::Rgb => &self.rgb,
            SensorId::Left => &self.left,
            SensorId::Right => &self.right,
        }
    }
}

/// GUI state and the GUI's Settings window. Every key is optional (a file
/// written before a key existed reads it as its default).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiState {
    /// Recently used PoE addresses, newest first.
    pub recent_ips: Vec<String>,
    /// The UI theme preset.
    pub theme: ThemePreset,
    /// Open the device and stream as soon as the GUI starts.
    pub start_streaming: bool,
    /// Reopen a lost device automatically (with backoff).
    pub auto_reconnect: bool,
    /// Camera card thumbnail rate, per second ([`THUMBNAIL_FPS`]).
    pub thumbnail_fps: u32,
    /// Show the live stats line on each camera card.
    pub stats_overlay: bool,
    /// The side panel tab last shown (`rgb`, `mono`, `device`, `vcam`).
    pub tab: String,
    /// The window's inner size last time, points (`None`: the default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_size: Option<[u32; 2]>,
}

/// The thumbnail rates the GUI offers; the last is the default.
pub const THUMBNAIL_FPS: [u32; 2] = [5, 10];

impl Default for UiState {
    fn default() -> Self {
        Self {
            recent_ips: Vec::new(),
            theme: ThemePreset::default(),
            start_streaming: true,
            auto_reconnect: true,
            thumbnail_fps: THUMBNAIL_FPS[1],
            stats_overlay: true,
            tab: "rgb".into(),
            window_size: None,
        }
    }
}

impl UiState {
    /// Restores the Settings-window preferences (theme, start streaming,
    /// reconnect, thumbnail rate, stats) to their defaults; the remembered
    /// IPs, tab and window size are kept.
    pub fn reset_preferences(&mut self) {
        *self = Self {
            recent_ips: std::mem::take(&mut self.recent_ips),
            tab: std::mem::take(&mut self.tab),
            window_size: self.window_size,
            ..Self::default()
        };
    }
}

/// A named UI theme preset (the presets and names of stream-concern),
/// stored kebab-case in TOML (`theme = "mac-native"`). The GUI maps each to
/// an egui style; this crate only stores the choice.
///
/// Deserialisation is lenient: an unknown name (from a newer build) reads
/// as the default ([`ThemePreset::MacNative`], the look oak-cam has had
/// since M6) with a warning instead of failing the file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemePreset {
    /// egui's stock look, unchanged, following the system light/dark
    /// setting.
    Classic,
    /// Rounded controls, filled groups and the system accent, following the
    /// system light/dark setting (the default: oak-cam's look since M6).
    #[default]
    MacNative,
    /// A dark, square, high-contrast "studio console" look.
    FlatDark,
    /// A light, rounded, airy look.
    SoftLight,
}

impl<'de> Deserialize<'de> for ThemePreset {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(ThemePreset::from_name(&name).unwrap_or_else(|| {
            tracing::warn!(theme = %name, "unknown UI theme in config; using mac-native");
            ThemePreset::default()
        }))
    }
}

impl ThemePreset {
    /// Every preset, in menu order.
    pub const ALL: [ThemePreset; 4] = [
        ThemePreset::Classic,
        ThemePreset::MacNative,
        ThemePreset::FlatDark,
        ThemePreset::SoftLight,
    ];

    /// The preset stored under `name` (its kebab-case TOML name).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "classic" => ThemePreset::Classic,
            "mac-native" => ThemePreset::MacNative,
            "flat-dark" => ThemePreset::FlatDark,
            "soft-light" => ThemePreset::SoftLight,
            _ => return None,
        })
    }

    /// The kebab-case TOML name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            ThemePreset::Classic => "classic",
            ThemePreset::MacNative => "mac-native",
            ThemePreset::FlatDark => "flat-dark",
            ThemePreset::SoftLight => "soft-light",
        }
    }

    /// The preset with this TOML name or menu label (any case).
    #[must_use]
    pub fn lookup(text: &str) -> Option<Self> {
        let t = text.trim();
        Self::ALL
            .into_iter()
            .find(|p| p.name().eq_ignore_ascii_case(t) || p.label().eq_ignore_ascii_case(t))
    }

    /// The name shown in menus and Settings.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ThemePreset::Classic => "Classic",
            ThemePreset::MacNative => "Mac Native",
            ThemePreset::FlatDark => "Flat Dark",
            ThemePreset::SoftLight => "Soft Light",
        }
    }
}

/// One value [`Config::validate`] changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// Dotted key path, e.g. `rgb.exposure.us`.
    pub field: String,
    /// What was wrong and what it became.
    pub message: String,
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

/// Supported RGB output sizes; the first is the default.
pub const RGB_SIZES: [(u32, u32); 3] = [(1920, 1080), (1280, 720), (3840, 2160)];
/// Supported mono output sizes; the first is the default.
pub const MONO_SIZES: [(u32, u32); 3] = [(1280, 800), (1280, 720), (640, 400)];

/// Exposure time range, µs.
pub const EXPOSURE_US: (u32, u32) = (1, 33_000);
/// ISO range.
pub const ISO: (u32, u32) = (100, 1600);
/// Manual white balance range, K.
pub const KELVIN: (u32, u32) = (1000, 12_000);
/// Brightness / contrast / saturation range.
pub const TUNE: (i8, i8) = (-10, 10);
/// Sharpness / denoise range.
pub const DETAIL: (u8, u8) = (0, 4);
/// AE compensation range.
pub const AE_COMP: (i8, i8) = (-9, 9);
/// Lowest frame rate.
pub const MIN_FPS: f32 = 1.0;

/// Highest RGB (IMX378) frame rate at an output size: 3840x2160 is 30 (the
/// sensor mode reaches 42 but the 4K NV12 ISP path is 30), the rest 60.
pub fn rgb_max_fps(size: (u32, u32)) -> f32 {
    if size.0 * size.1 > 1920 * 1080 {
        30.0
    } else {
        60.0
    }
}

/// Highest mono (OV9282) frame rate at an output size (120 for every
/// supported size).
pub fn mono_max_fps(_size: (u32, u32)) -> f32 {
    120.0
}

/// Collects warnings while clamping.
struct Checker(Vec<Warning>);

impl Checker {
    fn warn(&mut self, field: &str, message: String) {
        self.0.push(Warning {
            field: field.to_owned(),
            message,
        });
    }

    fn clamp<T: PartialOrd + Copy + fmt::Display>(&mut self, field: &str, v: &mut T, r: (T, T)) {
        let c = if *v < r.0 {
            r.0
        } else if *v > r.1 {
            r.1
        } else {
            return;
        };
        self.warn(
            field,
            format!("{v} is outside {}..={}, using {c}", r.0, r.1),
        );
        *v = c;
    }

    fn ratio(&mut self, field: &str, v: &mut f32) {
        if v.is_nan() {
            self.warn(field, "NaN, using 0".into());
            *v = 0.0;
        } else {
            self.clamp(field, v, (0.0, 1.0));
        }
    }

    fn stream(
        &mut self,
        field: &str,
        s: &mut StreamSettings,
        sizes: &[(u32, u32)],
        max_fps: fn((u32, u32)) -> f32,
    ) {
        let size = (s.width, s.height);
        if !sizes.contains(&size) {
            let snapped = snap_size(size, sizes);
            let list: Vec<String> = sizes.iter().map(|(w, h)| format!("{w}x{h}")).collect();
            self.warn(
                &format!("{field}.width/height"),
                format!(
                    "{}x{} is not supported ({}), using {}x{}",
                    s.width,
                    s.height,
                    list.join(", "),
                    snapped.0,
                    snapped.1
                ),
            );
            (s.width, s.height) = snapped;
        }
        if s.fps.is_nan() {
            self.warn(&format!("{field}.fps"), "NaN, using 30".into());
            s.fps = 30.0;
        }
        let max = max_fps((s.width, s.height));
        self.clamp(&format!("{field}.fps"), &mut s.fps, (MIN_FPS, max));
    }

    /// [`Self::stream`] for a bare shape.
    fn shape(
        &mut self,
        field: &str,
        s: &mut StreamShape,
        sizes: &[(u32, u32)],
        max_fps: fn((u32, u32)) -> f32,
    ) {
        let mut full = StreamSettings {
            width: s.width,
            height: s.height,
            fps: s.fps,
            enabled: true,
        };
        self.stream(field, &mut full, sizes, max_fps);
        *s = StreamShape::of(&full);
    }

    fn exposure(&mut self, field: &str, e: &mut Exposure) {
        match e {
            Exposure::Auto { comp, limit_us, .. } => {
                self.clamp(&format!("{field}.comp"), comp, AE_COMP);
                if let Some(l) = limit_us {
                    self.clamp(&format!("{field}.limit_us"), l, EXPOSURE_US);
                }
            }
            Exposure::Manual { us, iso } => {
                self.clamp(&format!("{field}.us"), us, EXPOSURE_US);
                self.clamp(&format!("{field}.iso"), iso, ISO);
            }
        }
    }

    fn mono(&mut self, field: &str, m: &mut MonoSettings) {
        self.stream(
            &format!("{field}.stream"),
            &mut m.stream,
            &MONO_SIZES,
            mono_max_fps,
        );
        self.exposure(&format!("{field}.exposure"), &mut m.exposure);
        self.clamp(&format!("{field}.brightness"), &mut m.brightness, TUNE);
        self.clamp(&format!("{field}.contrast"), &mut m.contrast, TUNE);
        self.clamp(&format!("{field}.sharpness"), &mut m.sharpness, DETAIL);
        self.clamp(
            &format!("{field}.luma_denoise"),
            &mut m.luma_denoise,
            DETAIL,
        );
    }
}

/// The entry of `sizes` nearest to `size` (by |dw| + |dh|; ties go to the
/// earlier entry).
pub fn snap_size(size: (u32, u32), sizes: &[(u32, u32)]) -> (u32, u32) {
    *sizes
        .iter()
        .min_by_key(|(w, h)| size.0.abs_diff(*w) + size.1.abs_diff(*h))
        .expect("non-empty size table")
}

impl Config {
    /// The per-user config file: `<config dir>/oak-cam/config.toml`
    /// (macOS `~/Library/Application Support`, Linux `$XDG_CONFIG_HOME` or
    /// `~/.config`, Windows `%APPDATA%`).
    pub fn default_path() -> Result<PathBuf, ConfigError> {
        dirs::config_dir()
            .map(|d| d.join("oak-cam").join("config.toml"))
            .ok_or(ConfigError::NoConfigDir)
    }

    /// Parses TOML. `path` is only used in error messages.
    pub fn from_toml(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let parse_err = |e: toml::de::Error| ConfigError::Parse {
            path: path.to_owned(),
            message: e.to_string().trim_end().to_owned(),
        };
        // Check the version before the full model, so a newer file gets
        // the version message rather than an unknown-key error.
        let table: toml::Table = toml::from_str(text).map_err(parse_err)?;
        match table.get("version") {
            None => {}
            Some(toml::Value::Integer(v)) if *v == i64::from(CONFIG_VERSION) => {}
            Some(toml::Value::Integer(v)) => {
                return Err(ConfigError::UnsupportedVersion {
                    path: path.to_owned(),
                    found: *v,
                });
            }
            Some(other) => {
                return Err(ConfigError::Parse {
                    path: path.to_owned(),
                    message: format!("`version` must be an integer, found {}", other.type_str()),
                });
            }
        }
        let mut cfg: Self = toml::from_str(text).map_err(parse_err)?;
        cfg.rgb.stream.fill_defaults(SensorId::Rgb);
        cfg.mono.shared.stream.fill_defaults(SensorId::Left);
        for m in [&mut cfg.mono.left, &mut cfg.mono.right]
            .into_iter()
            .flatten()
        {
            m.stream.fill_defaults(SensorId::Left);
        }
        Ok(cfg)
    }

    /// Serialises to TOML.
    ///
    /// The enum settings (`exposure`, `focus`, `wb`) are written as inline
    /// tables, `exposure = { Manual = { us = 2000, iso = 400 } }`, so
    /// switching mode is a one-line edit.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        let text =
            toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize(e.to_string()))?;
        let mut doc: toml_edit::DocumentMut = text
            .parse()
            .map_err(|e: toml_edit::TomlError| ConfigError::Serialize(e.to_string()))?;
        let mut inline = |path: &[&str]| {
            let (key, parents) = path.split_last().expect("non-empty path");
            let mut table = doc.as_table_mut();
            for p in parents {
                match table.get_mut(p).and_then(toml_edit::Item::as_table_mut) {
                    Some(t) => table = t,
                    None => return,
                }
            }
            if let Some(item) = table.get_mut(key)
                && let Some(t) = item.as_table()
            {
                let mut v = t.clone().into_inline_table();
                v.decor_mut().clear();
                *item = toml_edit::Item::Value(toml_edit::Value::InlineTable(v));
                if let Some(mut k) = table.key_mut(key) {
                    k.leaf_decor_mut().clear();
                }
            }
        };
        inline(&["device"]);
        inline(&["rgb", "exposure"]);
        inline(&["rgb", "focus"]);
        inline(&["rgb", "wb"]);
        for cam in ["shared", "left", "right"] {
            inline(&["mono", cam, "exposure"]);
        }
        for t in Transport::ALL {
            for k in ["rgb", "mono", "left", "right"] {
                inline(&["profiles", t.key(), k]);
            }
        }
        Ok(doc.to_string())
    }

    /// This config with a TOML fragment laid over it, e.g.
    /// `rgb.exposure = { Manual = { us = 2000, iso = 400 } }` or
    /// `mono.left.stream = { width = 640, height = 400 }`. Tables merge key
    /// by key, except that a one-key table meeting a different one-key
    /// table (an enum switching variant, `Auto` to `Manual`) is replaced.
    /// A fragment that creates a mono override (`mono.left = { ... }` while
    /// there is none) starts it from a copy of `mono.shared`, as
    /// [`MonoGroup::set_override`] does, with the given fields on top.
    /// Used by scripted GUI runs. Not validated.
    pub fn merged_toml(&self, fragment: &str) -> Result<Self, ConfigError> {
        let path = Path::new("<fragment>");
        let parse_err = |message: String| ConfigError::Parse {
            path: path.to_owned(),
            message,
        };
        let mut base: toml::Table =
            toml::from_str(&self.to_toml()?).map_err(|e| parse_err(e.to_string()))?;
        let over: toml::Table = toml::from_str(fragment).map_err(|e| parse_err(e.to_string()))?;
        if let (Some(toml::Value::Table(over_mono)), Some(toml::Value::Table(base_mono))) =
            (over.get("mono"), base.get_mut("mono"))
        {
            let shared = base_mono.get("shared").cloned();
            for cam in ["left", "right"] {
                if over_mono.contains_key(cam)
                    && !base_mono.contains_key(cam)
                    && let Some(s) = &shared
                {
                    base_mono.insert(cam.to_owned(), s.clone());
                }
            }
        }
        merge_table(&mut base, over);
        let text = toml::to_string(&base).map_err(|e| ConfigError::Serialize(e.to_string()))?;
        Self::from_toml(&text, path)
    }

    /// Loads `path`.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::from_toml(&text, path)
    }

    /// Loads `path`, or returns `None` if it does not exist.
    pub fn load_if_exists(path: &Path) -> Result<Option<Self>, ConfigError> {
        match Self::load(path) {
            Ok(c) => Ok(Some(c)),
            Err(ConfigError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Saves to `path` atomically (temp file in the same directory, then
    /// rename), creating the directory if needed.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        write_atomic(path, self.to_toml()?.as_bytes())
    }

    /// Clamps every value into range and snaps stream sizes to the
    /// supported tables. Returns one warning per change; an empty list
    /// means the config was already valid.
    pub fn validate(&mut self) -> Vec<Warning> {
        let mut c = Checker(Vec::new());
        let d = &mut self.device_settings;
        c.ratio("device_settings.ir_dot", &mut d.ir_dot);
        c.ratio("device_settings.ir_flood", &mut d.ir_flood);

        let r = &mut self.rgb;
        c.stream("rgb.stream", &mut r.stream, &RGB_SIZES, rgb_max_fps);
        c.exposure("rgb.exposure", &mut r.exposure);
        if let WhiteBalance::Kelvin(k) = &mut r.wb {
            c.clamp("rgb.wb.Kelvin", k, KELVIN);
        }
        c.clamp("rgb.brightness", &mut r.brightness, TUNE);
        c.clamp("rgb.contrast", &mut r.contrast, TUNE);
        c.clamp("rgb.saturation", &mut r.saturation, TUNE);
        c.clamp("rgb.sharpness", &mut r.sharpness, DETAIL);
        c.clamp("rgb.luma_denoise", &mut r.luma_denoise, DETAIL);
        c.clamp("rgb.chroma_denoise", &mut r.chroma_denoise, DETAIL);
        // Lens position is a u8, so 0..=255 holds by construction.

        c.mono("mono.shared", &mut self.mono.shared);
        if let Some(m) = &mut self.mono.left {
            c.mono("mono.left", m);
        }
        if let Some(m) = &mut self.mono.right {
            c.mono("mono.right", m);
        }

        let t = &mut self.ui.thumbnail_fps;
        if !THUMBNAIL_FPS.contains(t) {
            let snapped = if *t < THUMBNAIL_FPS[1] {
                THUMBNAIL_FPS[0]
            } else {
                THUMBNAIL_FPS[1]
            };
            c.warn(
                "ui.thumbnail_fps",
                format!("{t} is not offered; using {snapped}"),
            );
            *t = snapped;
        }
        for t in Transport::ALL {
            if let Some(p) = self.profiles.get_mut(t) {
                let key = format!("profiles.{}", t.key());
                c.shape(&format!("{key}.rgb"), &mut p.rgb, &RGB_SIZES, rgb_max_fps);
                c.shape(
                    &format!("{key}.mono"),
                    &mut p.mono,
                    &MONO_SIZES,
                    mono_max_fps,
                );
                for (name, cam) in [("left", &mut p.left), ("right", &mut p.right)] {
                    if let Some(shape) = cam {
                        c.shape(&format!("{key}.{name}"), shape, &MONO_SIZES, mono_max_fps);
                    }
                }
            }
        }
        c.0
    }

    /// The stream profile for `t`: the user's, else the built-in one.
    pub fn profile(&self, t: Transport) -> StreamProfile {
        self.profiles
            .get(t)
            .copied()
            .unwrap_or_else(|| t.defaults())
    }

    /// The shapes in use now: `rgb.stream`, `mono.shared.stream`, and each
    /// mono override whose size or rate differs from `mono.shared` (an
    /// override kept only for exposure etc. is `None`, so it does not make
    /// the profile custom).
    pub fn active_profile(&self) -> StreamProfile {
        let mono = StreamShape::of(&self.mono.shared.stream);
        let cam = |cam| {
            self.mono
                .slot(cam)
                .map(|m| StreamShape::of(&m.stream))
                .filter(|s| *s != mono)
        };
        StreamProfile {
            rgb: StreamShape::of(&self.rgb.stream),
            mono,
            left: cam(MonoCam::Left),
            right: cam(MonoCam::Right),
        }
    }

    /// Sets the size and rate (not `enabled`) of `rgb.stream`,
    /// `mono.shared.stream` and the mono overrides from [`Self::profile`].
    /// An override takes the profile's shape for its camera when there is
    /// one; otherwise an override that followed `mono.shared` (same shape)
    /// follows the new mono shape, and one with its own size or rate is
    /// left alone. Returns whether anything changed.
    pub fn apply_transport(&mut self, t: Transport) -> bool {
        let p = self.profile(t);
        let old_mono = StreamShape::of(&self.mono.shared.stream);
        let mut changed = p.rgb.apply_to(&mut self.rgb.stream);
        changed |= p.mono.apply_to(&mut self.mono.shared.stream);
        for cam in MonoCam::ALL {
            if let Some(m) = self.mono.slot_mut(cam) {
                let target = match p.cam(cam) {
                    Some(s) => s,
                    None if StreamShape::of(&m.stream) == old_mono => p.mono,
                    None => continue,
                };
                changed |= target.apply_to(&mut m.stream);
            }
        }
        changed
    }

    /// Stores [`Self::active_profile`] as `t`'s profile, or clears it when
    /// it equals [`Transport::defaults`]. Idempotent.
    pub fn remember_profile(&mut self, t: Transport) {
        let active = self.active_profile();
        *self.profiles.get_mut(t) = (active != t.defaults()).then_some(active);
    }

    /// Drops `t`'s custom profile and applies the defaults, to mono
    /// overrides with their own size or rate too. Returns whether the
    /// streams changed.
    pub fn reset_profile(&mut self, t: Transport) -> bool {
        *self.profiles.get_mut(t) = None;
        let mut changed = self.apply_transport(t);
        let mono = t.defaults().mono;
        for m in [&mut self.mono.left, &mut self.mono.right]
            .into_iter()
            .flatten()
        {
            changed |= mono.apply_to(&mut m.stream);
        }
        changed
    }

    /// The stream settings `sensor` uses.
    pub fn stream(&self, sensor: SensorId) -> &StreamSettings {
        match sensor {
            SensorId::Rgb => &self.rgb.stream,
            SensorId::Left => &self.mono.effective(MonoCam::Left).stream,
            SensorId::Right => &self.mono.effective(MonoCam::Right).stream,
        }
    }
}

/// Lays `over` onto `base` (see [`Config::merged_toml`]).
fn merge_table(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => {
                let variant_switch =
                    b.len() == 1 && o.len() == 1 && b.keys().next() != o.keys().next();
                if variant_switch {
                    *b = o;
                } else {
                    merge_table(b, o);
                }
            }
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

/// Writes `bytes` to `path` via a temp file and a rename, so readers (and
/// the hot-reload watcher) never see a half-written file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let io = |source| ConfigError::Io {
        path: path.to_owned(),
        source,
    };
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(io)?;
    let name = path
        .file_name()
        .map_or_else(|| "config".into(), |n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(io)
}

/// The default config as TOML, with a comment header explaining the
/// syntax, for `oak-cam config init`.
pub fn commented_default() -> Result<String, ConfigError> {
    let body = Config::default().to_toml()?;
    Ok(format!("{COMMENT_HEADER}\n{body}"))
}

/// Writes [`commented_default`] to `path` unless the file exists. Returns
/// `false` (and leaves the file alone) if it already exists.
pub fn init_file(path: &Path) -> Result<bool, ConfigError> {
    if path.exists() {
        return Ok(false);
    }
    write_atomic(path, commented_default()?.as_bytes())?;
    Ok(true)
}

const COMMENT_HEADER: &str = r#"# oak-cam configuration (format version 1).
#
# `oak-cam --headless` reloads this file whenever it changes. Every key is
# optional; missing keys take the defaults shown below. Ranges are clamped:
#   exposure us 1..33000, iso 100..1600, AE comp -9..9, Kelvin 1000..12000,
#   brightness / contrast / saturation -10..10, sharpness / denoise 0..4,
#   IR dot / flood 0.0..1.0, lens position 0..255.
# Stream sizes: rgb 1920x1080, 1280x720, 3840x2160; mono 1280x800, 1280x720,
#   640x400. Frame rate: rgb up to 30 at 3840x2160, else 60; mono up to 120.
#   Size, fps, `enabled`, device and USB speed changes restart the
#   pipeline; everything else applies live.
#
# Choices:
#   device        = "Auto" | { ById = "14442C10..." } | { ByIp = "192.0.2.44" }
#   exposure      = { Auto = { comp = 0, lock = false, limit_us = 20000 } }
#                 | { Manual = { us = 2000, iso = 400 } }
#   antibanding   = "Off" | "Mains50Hz" | "Mains60Hz" | "Auto"
#   focus (rgb)   = { Auto = "ContinuousVideo" } (or "Auto", "Macro",
#                   "ContinuousPicture", "Edof") | { Manual = 130 }
#   wb (rgb)      = { Auto = { mode = "Auto", lock = false } } | { Kelvin = 5600 }
#   max_usb_speed = "High" | "Super" | "SuperPlus"
# Per-camera mono overrides: add a [mono.left] or [mono.right] table with the
# same keys as [mono.shared]; remove it to follow the shared settings again.
# Stream profiles: when a device connects, the stream sizes and rates above
# are replaced by the profile for its link (PoE, USB3 or USB2). Built-in:
#   usb3 rgb 1920x1080@30, mono 1280x800@30; poe 1280x720@20, 640x400@20;
#   usb2 1280x720@12, 640x400@12. To change one, add for example
#   [profiles.poe]
#   rgb = { width = 1920, height = 1080, fps = 15.0 }
#   mono = { width = 1280, height = 800, fps = 15.0 }
"#;

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    fn p() -> &'static Path {
        Path::new("test.toml")
    }

    #[test]
    fn defaults_are_the_usb3_profile() {
        let c = Config::default();
        assert_eq!(c.version, 1);
        assert_eq!(c.device, DeviceSelection::Auto);
        assert_eq!(
            (
                c.rgb.stream.width,
                c.rgb.stream.height,
                c.rgb.stream.fps,
                c.rgb.stream.enabled
            ),
            (1920, 1080, 30.0, true)
        );
        for cam in MonoCam::ALL {
            let s = c.mono.effective(cam).stream;
            assert_eq!(
                (s.width, s.height, s.fps, s.enabled),
                (1280, 800, 30.0, true)
            );
        }
        assert_eq!(c.device_settings.ir_dot, 0.0);
        assert_eq!(c.device_settings.ir_flood, 0.0);
        assert_eq!(c.device_settings.max_usb_speed, UsbSpeedPref::SuperPlus);
        assert!(matches!(
            c.rgb.exposure,
            Exposure::Auto {
                comp: 0,
                limit_us: None,
                lock: false
            }
        ));
        assert_eq!(c.rgb.focus, Focus::Auto(AfMode::ContinuousVideo));
        assert!(matches!(
            c.rgb.wb,
            WhiteBalance::Auto {
                mode: AwbMode::Auto,
                lock: false
            }
        ));
        assert!(c.vcam.rgb.enabled && c.vcam.left.enabled && c.vcam.right.enabled);
        assert!(Config::default().validate().is_empty());
    }

    #[test]
    fn round_trips_through_toml() {
        let mut c = Config::default();
        c.device = DeviceSelection::ById("14442C10514ACDD600".into());
        c.device_settings.ir_dot = 0.8;
        c.rgb.exposure = Exposure::Manual { us: 2000, iso: 400 };
        c.rgb.focus = Focus::Manual(130);
        c.rgb.wb = WhiteBalance::Kelvin(5600);
        c.mono.set_override(MonoCam::Right, true);
        c.mono.right.as_mut().unwrap().brightness = 3;
        c.ui.recent_ips.push("192.0.2.44".into());
        let text = c.to_toml().unwrap();
        assert_eq!(Config::from_toml(&text, p()).unwrap(), c, "{text}");
        // Enum settings are one-line inline tables, as the docs show them.
        for line in [
            "exposure = { Manual = { us = 2000, iso = 400 } }",
            "focus = { Manual = 130 }",
            "wb = { Kelvin = 5600 }",
            "exposure = { Auto = { comp = 0, lock = false } }",
            "device = { ById = \"14442C10514ACDD600\" }",
        ] {
            assert!(
                text.lines().any(|l| l == line),
                "missing {line:?} in\n{text}"
            );
        }
    }

    #[test]
    fn partial_file_takes_defaults_and_plan_syntax_parses() {
        let text = r"
            [rgb]
            exposure = { Manual = { us = 2000, iso = 400 } }
            focus = { Manual = 40 }
            [mono.shared.stream]
            width = 640
            height = 400
        ";
        let c = Config::from_toml(text, p()).unwrap();
        assert_eq!(c.rgb.exposure, Exposure::Manual { us: 2000, iso: 400 });
        assert_eq!(c.rgb.focus, Focus::Manual(40));
        assert_eq!(c.rgb.stream, StreamSettings::default_for(SensorId::Rgb));
        assert_eq!(
            (c.mono.shared.stream.width, c.mono.shared.stream.height),
            (640, 400)
        );
        assert_eq!(c.mono.shared.stream.fps, 30.0);
        assert!(c.mono.shared.stream.enabled);
        let c = Config::from_toml(
            "[rgb.stream]\nfps = 15.0\n[mono.left.stream]\nenabled = false",
            p(),
        )
        .unwrap();
        assert_eq!(
            c.rgb.stream,
            StreamSettings {
                fps: 15.0,
                ..StreamSettings::default_for(SensorId::Rgb)
            }
        );
        let left = c.mono.left.unwrap().stream;
        assert_eq!(
            (left.width, left.height, left.fps, left.enabled),
            (1280, 800, 30.0, false)
        );
        let c = Config::from_toml("device = { ByIp = \"198.51.100.2\" }", p()).unwrap();
        assert_eq!(c.device.selector(), "198.51.100.2");
        let c = Config::from_toml("[rgb]\nexposure = { Auto = { comp = -2 } }", p()).unwrap();
        assert_eq!(
            c.rgb.exposure,
            Exposure::Auto {
                comp: -2,
                limit_us: None,
                lock: false
            }
        );
    }

    #[test]
    fn unknown_version_is_a_clear_error() {
        let err = Config::from_toml("version = 2\n[rgb]\nnew_thing = 1", p()).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::UnsupportedVersion { found: 2, .. }
        ));
        let msg = err.to_string();
        assert!(msg.contains("config version 2 is not supported"), "{msg}");
        assert!(msg.contains("reads version 1"), "{msg}");
        assert!(matches!(
            Config::from_toml("version = \"1\"", p()),
            Err(ConfigError::Parse { .. })
        ));
        assert!(Config::from_toml("version = 1", p()).is_ok());
    }

    #[test]
    fn unknown_keys_and_bad_toml_are_errors() {
        let err = Config::from_toml("[rgb]\nbrightnes = 2", p()).unwrap_err();
        assert!(err.to_string().contains("brightnes"), "{err}");
        assert!(Config::from_toml("[rgb", p()).is_err());
        assert!(
            Config::from_toml("[rgb]\nsharpness = 300", p()).is_err(),
            "u8 overflow"
        );
    }

    #[test]
    fn validate_clamps_every_range() {
        let mut c = Config::default();
        c.device_settings.ir_dot = 1.5;
        c.device_settings.ir_flood = -0.1;
        c.rgb.exposure = Exposure::Manual {
            us: 50_000,
            iso: 50,
        };
        c.rgb.wb = WhiteBalance::Kelvin(20_000);
        c.rgb.brightness = 11;
        c.rgb.contrast = -11;
        c.rgb.saturation = 100;
        c.rgb.sharpness = 5;
        c.rgb.luma_denoise = 9;
        c.rgb.chroma_denoise = 200;
        c.mono.shared.exposure = Exposure::Auto {
            comp: -12,
            limit_us: Some(0),
            lock: false,
        };
        c.mono.set_override(MonoCam::Left, true);
        c.mono.left.as_mut().unwrap().contrast = 20;
        let w = c.validate();
        let fields: Vec<&str> = w.iter().map(|w| w.field.as_str()).collect();
        assert_eq!(
            fields,
            [
                "device_settings.ir_dot",
                "device_settings.ir_flood",
                "rgb.exposure.us",
                "rgb.exposure.iso",
                "rgb.wb.Kelvin",
                "rgb.brightness",
                "rgb.contrast",
                "rgb.saturation",
                "rgb.sharpness",
                "rgb.luma_denoise",
                "rgb.chroma_denoise",
                "mono.shared.exposure.comp",
                "mono.shared.exposure.limit_us",
                "mono.left.exposure.comp",
                "mono.left.exposure.limit_us",
                "mono.left.contrast",
            ]
        );
        assert_eq!(c.device_settings.ir_dot, 1.0);
        assert_eq!(c.device_settings.ir_flood, 0.0);
        assert_eq!(
            c.rgb.exposure,
            Exposure::Manual {
                us: 33_000,
                iso: 100
            }
        );
        assert_eq!(c.rgb.wb, WhiteBalance::Kelvin(12_000));
        assert_eq!(
            (c.rgb.brightness, c.rgb.contrast, c.rgb.saturation),
            (10, -10, 10)
        );
        assert_eq!(
            (c.rgb.sharpness, c.rgb.luma_denoise, c.rgb.chroma_denoise),
            (4, 4, 4)
        );
        assert_eq!(
            c.mono.shared.exposure,
            Exposure::Auto {
                comp: -9,
                limit_us: Some(1),
                lock: false
            }
        );
        assert_eq!(c.mono.left.unwrap().contrast, 10);
        assert_eq!(
            w[0].to_string(),
            "device_settings.ir_dot: 1.5 is outside 0..=1, using 1"
        );
        // Validation is idempotent.
        assert!(c.validate().is_empty());
    }

    #[test]
    fn validate_snaps_sizes_and_clamps_fps() {
        let mut c = Config::default();
        c.rgb.stream.width = 1280;
        c.rgb.stream.height = 700;
        c.rgb.stream.fps = 90.0;
        c.mono.shared.stream.width = 600;
        c.mono.shared.stream.height = 480;
        c.mono.shared.stream.fps = f32::NAN;
        let w = c.validate();
        assert_eq!(w.len(), 4, "{w:?}");
        assert_eq!((c.rgb.stream.width, c.rgb.stream.height), (1280, 720));
        assert_eq!(c.rgb.stream.fps, 60.0);
        assert_eq!(
            (c.mono.shared.stream.width, c.mono.shared.stream.height),
            (640, 400)
        );
        assert_eq!(c.mono.shared.stream.fps, 30.0);
        assert!(
            w[0].message.contains("1280x700 is not supported"),
            "{}",
            w[0]
        );
        // 4K request snaps to 4K; mono asked for 1080p snaps to 1280x800.
        assert_eq!(snap_size((4000, 2000), &RGB_SIZES), (3840, 2160));
        assert_eq!(snap_size((1920, 1080), &MONO_SIZES), (1280, 800));
    }

    #[test]
    fn fps_limit_depends_on_size() {
        let cases = [
            ((3840, 2160), 60.0, 30.0),
            ((3840, 2160), 30.0, 30.0),
            ((1920, 1080), 90.0, 60.0),
            ((1920, 1080), 60.0, 60.0),
            ((1280, 720), 75.0, 60.0),
        ];
        for ((w, h), asked, want) in cases {
            let mut c = Config::default();
            c.rgb.stream.width = w;
            c.rgb.stream.height = h;
            c.rgb.stream.fps = asked;
            let warned = !c.validate().is_empty();
            assert_eq!(c.rgb.stream.fps, want, "rgb {w}x{h}@{asked}");
            assert_eq!(warned, asked > want);
        }
        for (w, h) in MONO_SIZES {
            let mut c = Config::default();
            c.mono.shared.stream.width = w;
            c.mono.shared.stream.height = h;
            c.mono.shared.stream.fps = 200.0;
            c.validate();
            assert_eq!(c.mono.shared.stream.fps, 120.0, "mono {w}x{h}");
            c.mono.shared.stream.fps = 120.0;
            assert!(c.validate().is_empty());
        }
        // Size snapping happens first, so the limit is the snapped size's.
        let mut c = Config::default();
        c.rgb.stream.width = 4000;
        c.rgb.stream.height = 2000;
        c.rgb.stream.fps = 60.0;
        assert_eq!(c.validate().len(), 2);
        assert_eq!((c.rgb.stream.width, c.rgb.stream.fps), (3840, 30.0));
    }

    #[test]
    fn mono_override_helpers() {
        let mut g = MonoGroup::default();
        g.shared.brightness = 2;
        assert!(!g.is_overridden(MonoCam::Left));
        assert_eq!(g.effective(MonoCam::Left).brightness, 2);
        // On: copies shared, effective unchanged.
        g.set_override(MonoCam::Left, true);
        assert_eq!(g.left, Some(g.shared));
        g.editable(MonoCam::Left).brightness = -4;
        assert_eq!(g.effective(MonoCam::Left).brightness, -4);
        assert_eq!(g.effective(MonoCam::Right).brightness, 2);
        // On again keeps the existing override.
        g.set_override(MonoCam::Left, true);
        assert_eq!(g.effective(MonoCam::Left).brightness, -4);
        // Editing a non-overridden camera edits shared.
        g.editable(MonoCam::Right).brightness = 5;
        assert_eq!(g.shared.brightness, 5);
        assert_eq!(g.effective(MonoCam::Left).brightness, -4);
        // Off: back to shared.
        g.set_override(MonoCam::Left, false);
        assert_eq!(g.left, None);
        assert_eq!(g.effective(MonoCam::Left).brightness, 5);
    }

    #[test]
    fn save_is_atomic_and_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        let mut c = Config::default();
        c.rgb.brightness = 4;
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), c);
        // No temp file left behind.
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["config.toml"]);
        assert!(
            Config::load_if_exists(&dir.path().join("missing.toml"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn init_writes_commented_default_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert!(init_file(&path).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# oak-cam configuration"));
        assert_eq!(Config::from_toml(&text, &path).unwrap(), Config::default());
        std::fs::write(&path, "version = 1\n").unwrap();
        assert!(!init_file(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "version = 1\n");
    }

    #[test]
    fn merged_toml_overlays_fields_and_switches_enums() {
        let base = Config::default();
        let c = base
            .merged_toml("rgb.exposure = { Manual = { us = 2000, iso = 400 } }")
            .unwrap();
        assert_eq!(c.rgb.exposure, Exposure::Manual { us: 2000, iso: 400 });
        assert_eq!(c.rgb.stream, base.rgb.stream);
        let c = c
            .merged_toml("device_settings.ir_dot = 0.5\nmono.left = { brightness = 3 }")
            .unwrap();
        assert!((c.device_settings.ir_dot - 0.5).abs() < f32::EPSILON);
        assert_eq!(c.mono.left.map(|m| m.brightness), Some(3));
        let c = c
            .merged_toml("mono.left.stream = { width = 640, height = 400 }")
            .unwrap();
        let left = c.mono.left.unwrap();
        assert_eq!((left.stream.width, left.stream.height), (640, 400));
        assert!((left.stream.fps - 30.0).abs() < f32::EPSILON);
        assert_eq!(left.brightness, 3);
        assert!(c.merged_toml("rgb.nope = 1").is_err());
        assert!(c.merged_toml("rgb = ").is_err());
    }

    /// Creating an override through a fragment copies `shared` first, like
    /// `set_override`, then applies the fragment's fields.
    #[test]
    fn merged_toml_new_override_starts_from_shared() {
        let mut base = Config::default();
        base.mono.shared.contrast = 4;
        base.mono.shared.exposure = Exposure::Manual { us: 3000, iso: 200 };
        base.mono.shared.stream.width = 640;
        base.mono.shared.stream.height = 400;
        let c = base.merged_toml("mono.left = { brightness = 3 }").unwrap();
        let mut want = base.mono.shared;
        want.brightness = 3;
        assert_eq!(c.mono.left, Some(want));
        assert_eq!(c.mono.right, None);
        assert_eq!(c.mono.shared, base.mono.shared);
        // Same through a dotted key into the new table.
        let c = base.merged_toml("mono.right.stream.fps = 60.0").unwrap();
        let right = c.mono.right.unwrap();
        assert_eq!((right.stream.width, right.stream.height), (640, 400));
        assert!((right.stream.fps - 60.0).abs() < f32::EPSILON);
        assert_eq!(right.contrast, 4);
        // An existing override is merged into, not reset from shared.
        let mut with = base.clone();
        with.mono.set_override(MonoCam::Left, true);
        with.mono.left.as_mut().unwrap().contrast = -2;
        let c = with.merged_toml("mono.left = { brightness = 1 }").unwrap();
        let left = c.mono.left.unwrap();
        assert_eq!((left.contrast, left.brightness), (-2, 1));
    }

    #[test]
    fn ui_state_defaults_and_old_files() {
        let d = UiState::default();
        assert_eq!(d.theme, ThemePreset::MacNative);
        assert!(d.start_streaming && d.auto_reconnect && d.stats_overlay);
        assert_eq!(d.thumbnail_fps, 10);
        assert!(d.recent_ips.is_empty());
        // A file from before the Settings window (only recent_ips) loads
        // with every new key at its default.
        let c = Config::from_toml("[ui]\nrecent_ips = [\"198.51.100.2\"]\n", p()).unwrap();
        assert_eq!(
            c.ui,
            UiState {
                recent_ips: vec!["198.51.100.2".into()],
                ..UiState::default()
            }
        );
        // No [ui] table at all.
        assert_eq!(Config::from_toml("", p()).unwrap().ui, UiState::default());
    }

    #[test]
    fn ui_state_round_trips_and_unknown_theme_is_lenient() {
        let mut c = Config::default();
        c.ui = UiState {
            recent_ips: vec!["192.0.2.44".into()],
            theme: ThemePreset::FlatDark,
            start_streaming: false,
            auto_reconnect: false,
            thumbnail_fps: 5,
            stats_overlay: false,
            tab: "mono".into(),
            window_size: Some([1200, 800]),
        };
        let text = c.to_toml().unwrap();
        assert!(text.contains("theme = \"flat-dark\""), "{text}");
        assert_eq!(Config::from_toml(&text, p()).unwrap(), c);
        let c = Config::from_toml("[ui]\ntheme = \"neon\"\n", p()).unwrap();
        assert_eq!(c.ui.theme, ThemePreset::MacNative);
        let mut c = Config::from_toml("[ui]\nthumbnail_fps = 7\n", p()).unwrap();
        let w = c.validate();
        assert_eq!(c.ui.thumbnail_fps, 5);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].field, "ui.thumbnail_fps");
    }

    #[test]
    fn theme_lookup_by_name_and_label() {
        for t in ThemePreset::ALL {
            assert_eq!(ThemePreset::from_name(t.name()), Some(t));
            assert_eq!(ThemePreset::lookup(t.name()), Some(t));
            assert_eq!(ThemePreset::lookup(t.label()), Some(t));
            assert_eq!(ThemePreset::lookup(&t.label().to_uppercase()), Some(t));
        }
        assert_eq!(
            ThemePreset::lookup("Mac Native"),
            Some(ThemePreset::MacNative)
        );
        assert_eq!(
            ThemePreset::lookup("soft-light"),
            Some(ThemePreset::SoftLight)
        );
        assert_eq!(ThemePreset::lookup("system"), None);
        assert_eq!(
            ThemePreset::from_name("Classic"),
            None,
            "TOML names are exact"
        );
    }

    #[test]
    fn reset_preferences_keeps_recent_ips() {
        let mut u = UiState {
            recent_ips: vec!["a".into()],
            theme: ThemePreset::SoftLight,
            start_streaming: false,
            auto_reconnect: false,
            thumbnail_fps: 5,
            stats_overlay: false,
            tab: "vcam".into(),
            window_size: Some([1000, 700]),
        };
        u.reset_preferences();
        assert_eq!(
            u,
            UiState {
                recent_ips: vec!["a".into()],
                tab: "vcam".into(),
                window_size: Some([1000, 700]),
                ..UiState::default()
            }
        );
    }

    #[test]
    fn transport_of_protocol_and_speed() {
        use depthai::{Protocol, UsbSpeed};
        assert_eq!(
            Transport::of(Protocol::TcpIp, UsbSpeed::Unknown),
            Transport::Poe
        );
        assert_eq!(
            Transport::of(Protocol::TcpIp, UsbSpeed::High),
            Transport::Poe
        );
        for usb in [UsbSpeed::High, UsbSpeed::Full, UsbSpeed::Low] {
            assert_eq!(Transport::of(Protocol::Usb, usb), Transport::Usb2, "{usb}");
        }
        for usb in [UsbSpeed::Super, UsbSpeed::SuperPlus, UsbSpeed::Unknown] {
            assert_eq!(Transport::of(Protocol::Usb, usb), Transport::Usb3, "{usb}");
        }
        assert_eq!(
            Transport::of(Protocol::Unknown, UsbSpeed::Unknown),
            Transport::Usb3
        );
        let labels: Vec<&str> = Transport::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(labels, ["PoE", "USB3", "USB2"]);
    }

    #[test]
    fn transport_defaults_are_valid_and_usb3_is_the_config_default() {
        let c = Config::default();
        assert_eq!(c.active_profile(), Transport::Usb3.defaults());
        assert_eq!(
            Transport::Poe.defaults().to_string(),
            "rgb 1280x720@20, mono 640x400@20"
        );
        assert_eq!(
            Transport::Usb2.defaults().to_string(),
            "rgb 1280x720@12, mono 640x400@12"
        );
        for t in Transport::ALL {
            let mut c = Config::default();
            c.apply_transport(t);
            assert!(c.validate().is_empty(), "{t}");
            assert_eq!(c.active_profile(), t.defaults());
        }
    }

    #[test]
    fn profiles_round_trip_and_are_omitted_when_unset() {
        let c = Config::default();
        let text = c.to_toml().unwrap();
        assert!(!text.contains("profiles"), "{text}");
        let mut c = Config::default();
        for t in Transport::ALL {
            *c.profiles.get_mut(t) = Some(t.defaults());
        }
        c.profiles.poe.as_mut().unwrap().rgb = StreamShape {
            width: 1920,
            height: 1080,
            fps: 15.0,
        };
        let text = c.to_toml().unwrap();
        assert_eq!(Config::from_toml(&text, p()).unwrap(), c, "{text}");
        for line in [
            "[profiles.poe]",
            "rgb = { width = 1920, height = 1080, fps = 15.0 }",
            "mono = { width = 640, height = 400, fps = 12.0 }",
        ] {
            assert!(
                text.lines().any(|l| l == line),
                "missing {line:?} in\n{text}"
            );
        }
    }

    #[test]
    fn file_without_profiles_parses_to_none_and_profiles_parse() {
        let c = Config::from_toml("version = 1\n[rgb]\nbrightness = 2", p()).unwrap();
        assert_eq!(c.profiles, Profiles::default());
        assert!(c.profiles.is_empty());
        let c = Config::from_toml(
            "[profiles.poe]\nrgb = { width = 1920, height = 1080, fps = 30 }\n\
             mono = { width = 1280, height = 800, fps = 15.0 }",
            p(),
        )
        .unwrap();
        let poe = c.profiles.poe.unwrap();
        assert_eq!(poe.rgb.to_string(), "1920x1080@30");
        assert_eq!(poe.mono.to_string(), "1280x800@15");
        assert_eq!((c.profiles.usb3, c.profiles.usb2), (None, None));
        assert!(Config::from_toml("[profiles.poe]\nrgb = { width = 1 }", p()).is_err());
        assert!(Config::from_toml("[profiles.wifi]", p()).is_err());
    }

    #[test]
    fn validate_fixes_custom_profiles() {
        let mut c = Config::default();
        c.profiles.usb2 = Some(StreamProfile {
            rgb: StreamShape {
                width: 3840,
                height: 2160,
                fps: 60.0,
            },
            mono: StreamShape {
                width: 600,
                height: 400,
                fps: 200.0,
            },
            left: None,
            right: Some(StreamShape {
                width: 1280,
                height: 800,
                fps: 0.0,
            }),
        });
        let w = c.validate();
        let fields: Vec<&str> = w.iter().map(|w| w.field.as_str()).collect();
        assert_eq!(
            fields,
            [
                "profiles.usb2.rgb.fps",
                "profiles.usb2.mono.width/height",
                "profiles.usb2.mono.fps",
                "profiles.usb2.right.fps"
            ]
        );
        assert_eq!(
            c.profiles.usb2.unwrap().to_string(),
            "rgb 3840x2160@30, mono 640x400@120, right 1280x800@1"
        );
        assert!(c.validate().is_empty());
    }

    #[test]
    fn apply_remember_and_reset_profiles() {
        let mut c = Config::default();
        c.rgb.stream.enabled = false;
        c.mono.set_override(MonoCam::Right, true);
        c.mono.right.as_mut().unwrap().stream.enabled = false;
        // USB3 defaults are already in place.
        assert!(!c.apply_transport(Transport::Usb3));
        // PoE: every stream follows, `enabled` survives.
        assert!(c.apply_transport(Transport::Poe));
        assert!(!c.apply_transport(Transport::Poe), "idempotent");
        assert_eq!(c.active_profile(), Transport::Poe.defaults());
        assert!(!c.rgb.stream.enabled);
        let right = c.mono.right.unwrap().stream;
        assert_eq!(
            (right.width, right.height, right.fps, right.enabled),
            (640, 400, 20.0, false)
        );
        assert!(c.mono.shared.stream.enabled);
        // Remembering the defaults stores nothing.
        c.remember_profile(Transport::Poe);
        assert_eq!(c.profiles.poe, None);
        // An edit is remembered, and applied on the next PoE connect.
        c.rgb.stream.fps = 15.0;
        c.rgb.stream.width = 1920;
        c.rgb.stream.height = 1080;
        c.remember_profile(Transport::Poe);
        c.remember_profile(Transport::Poe);
        let custom = c.profiles.poe.unwrap();
        assert_eq!(custom.rgb.to_string(), "1920x1080@15");
        assert_eq!(c.profile(Transport::Poe), custom);
        assert!(c.apply_transport(Transport::Usb3));
        assert_eq!(c.active_profile(), Transport::Usb3.defaults());
        assert!(c.apply_transport(Transport::Poe));
        assert_eq!(c.active_profile(), custom);
        // Editing back to the defaults counts as the defaults.
        c.rgb.stream.fps = 20.0;
        c.rgb.stream.width = 1280;
        c.rgb.stream.height = 720;
        c.remember_profile(Transport::Poe);
        assert_eq!(c.profiles.poe, None);
        // Reset drops the custom profile and applies the defaults.
        c.rgb.stream.fps = 25.0;
        c.remember_profile(Transport::Poe);
        assert!(c.profiles.poe.is_some());
        assert!(c.reset_profile(Transport::Poe));
        assert_eq!(c.profiles.poe, None);
        assert_eq!(c.active_profile(), Transport::Poe.defaults());
        assert!(!c.reset_profile(Transport::Poe));
        assert!(!c.rgb.stream.enabled);
    }

    /// Sets `cam`'s override (creating it) to `w`x`h`@`fps`.
    fn set_override_shape(c: &mut Config, cam: MonoCam, w: u32, h: u32, fps: f32) {
        c.mono.set_override(cam, true);
        let s = &mut c.mono.editable(cam).stream;
        (s.width, s.height, s.fps) = (w, h, fps);
    }

    #[test]
    fn override_with_its_own_shape_survives_apply_transport() {
        let mut c = Config::default();
        c.apply_transport(Transport::Poe);
        set_override_shape(&mut c, MonoCam::Left, 1280, 800, 15.0);
        // An exposure-only override follows the mono shape.
        c.mono.set_override(MonoCam::Right, true);
        c.mono.editable(MonoCam::Right).brightness = 2;
        // Not remembered yet: the PoE profile has no `left`, so the
        // override keeps its own shape (a restart re-applies the profile).
        assert!(!c.apply_transport(Transport::Poe));
        assert_eq!(c.mono.left.unwrap().stream.fps, 15.0);
        assert_eq!(
            (
                c.mono.left.unwrap().stream.width,
                c.mono.right.unwrap().stream.width
            ),
            (1280, 640)
        );
        // Going to USB3 moves shared and the following right override only.
        assert!(c.apply_transport(Transport::Usb3));
        let (l, r) = (c.mono.left.unwrap().stream, c.mono.right.unwrap().stream);
        assert_eq!((l.width, l.fps), (1280, 15.0));
        assert_eq!((r.width, r.height, r.fps), (1280, 800, 30.0));
        assert_eq!(c.mono.right.unwrap().brightness, 2);
    }

    #[test]
    fn override_shape_round_trips_through_remember_and_apply() {
        let mut c = Config::default();
        c.apply_transport(Transport::Poe);
        // An exposure-only override does not make the profile custom.
        c.mono.set_override(MonoCam::Right, true);
        c.remember_profile(Transport::Poe);
        assert_eq!(c.profiles.poe, None);
        set_override_shape(&mut c, MonoCam::Left, 1280, 800, 15.0);
        c.remember_profile(Transport::Poe);
        let p = c.profiles.poe.unwrap();
        assert_eq!(
            p.left.map(|s| s.to_string()).as_deref(),
            Some("1280x800@15")
        );
        assert_eq!(p.right, None);
        assert_eq!(
            p.to_string(),
            "rgb 1280x720@20, mono 640x400@20, left 1280x800@15"
        );
        // Elsewhere and back: the override's shape comes back from the
        // profile.
        c.apply_transport(Transport::Usb3);
        let s = &mut c.mono.editable(MonoCam::Left).stream;
        (s.width, s.height, s.fps) = (640, 400, 60.0);
        assert!(c.apply_transport(Transport::Poe));
        assert_eq!(c.active_profile(), p);
        // Reset collapses it to the default mono shape.
        assert!(c.reset_profile(Transport::Poe));
        assert_eq!(c.active_profile(), Transport::Poe.defaults());
        assert_eq!(c.mono.left.unwrap().stream.fps, 20.0);
    }

    #[test]
    fn override_in_a_profile_round_trips_through_toml() {
        let mut c = Config::default();
        c.apply_transport(Transport::Usb2);
        set_override_shape(&mut c, MonoCam::Right, 1280, 720, 10.0);
        c.remember_profile(Transport::Usb2);
        let text = c.to_toml().unwrap();
        assert!(
            text.lines()
                .any(|l| l == "right = { width = 1280, height = 720, fps = 10.0 }"),
            "{text}"
        );
        assert!(!text.contains("left = {"), "{text}");
        let back = Config::from_toml(&text, p()).unwrap();
        assert_eq!(back, c);
        assert_eq!(
            back.profile(Transport::Usb2).right,
            c.active_profile().right
        );
    }

    #[test]
    fn default_path_ends_in_oak_cam_config() {
        if let Ok(p) = Config::default_path() {
            assert!(p.ends_with("oak-cam/config.toml"), "{}", p.display());
        }
    }
}
