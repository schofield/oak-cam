//! Discovery and device-level queries.

use std::ffi::{CString, c_char};
use std::ptr::NonNull;

use depthai_sys as sys;

use crate::{
    DeviceState, Error, Platform, Protocol, Result, SensorType, Socket, UsbSpeed, check, fixed_str,
};

/// One device found by discovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// USB path or IP address, as depthai reports it.
    pub name: String,
    /// Device id (MxId).
    pub id: String,
    /// Transport.
    pub protocol: Protocol,
    /// XLink state.
    pub state: DeviceState,
    /// Silicon platform.
    pub platform: Platform,
}

impl From<&sys::oak_device_info> for DeviceInfo {
    fn from(raw: &sys::oak_device_info) -> Self {
        Self {
            name: fixed_str(&raw.name),
            id: fixed_str(&raw.device_id),
            protocol: Protocol::from_raw(raw.protocol),
            state: DeviceState::from_raw(raw.state),
            platform: Platform::from_raw(raw.platform),
        }
    }
}

/// One resolution / fps range a sensor supports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SensorConfig {
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
    /// Minimum frame rate.
    pub min_fps: f32,
    /// Maximum frame rate.
    pub max_fps: f32,
    /// Sensor type in this mode.
    pub sensor_type: SensorType,
}

impl From<&sys::oak_sensor_config> for SensorConfig {
    fn from(raw: &sys::oak_sensor_config) -> Self {
        Self {
            width: raw.width,
            height: raw.height,
            min_fps: raw.min_fps,
            max_fps: raw.max_fps,
            sensor_type: SensorType::from_raw(raw.sensor_type),
        }
    }
}

/// One connected camera sensor.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraFeatures {
    /// Board socket.
    pub socket: Socket,
    /// Sensor type (first supported).
    pub sensor_type: SensorType,
    /// Sensor model, e.g. `"IMX378"`.
    pub sensor_name: String,
    /// Board alias, e.g. `"rgb"` (may be empty).
    pub name: String,
    /// Maximum sensor width.
    pub width: i32,
    /// Maximum sensor height.
    pub height: i32,
    /// Whether the sensor has autofocus.
    pub has_autofocus: bool,
    /// Supported sensor modes.
    pub configs: Vec<SensorConfig>,
}

/// An open OAK device. Closed on drop.
///
/// `Send` but not `Sync`: the shim allows a handle on any thread, but not
/// from two threads at once.
pub struct Device {
    raw: NonNull<sys::oak_device>,
}

// SAFETY: oak_device may be used from any thread, one at a time (oak_dai.h,
// "Threading"). `&mut`-free queries still go through `&self`, so we do not
// implement Sync.
unsafe impl Send for Device {}

impl Device {
    /// Lists every available device (USB and PoE).
    pub fn list() -> Result<Vec<DeviceInfo>> {
        let mut cap = 16usize;
        loop {
            let mut buf = vec![zeroed_info(); cap];
            let mut count = 0usize;
            // SAFETY: buf has `cap` writable entries; count is a valid out-param.
            check(unsafe { sys::oak_list_devices(buf.as_mut_ptr(), cap, &raw mut count) })?;
            if count <= cap {
                buf.truncate(count);
                return Ok(buf.iter().map(DeviceInfo::from).collect());
            }
            cap = count;
        }
    }

    /// Opens a device by id (MxId), IP address, or `""` for the first one
    /// available. `max_usb` caps the USB link speed (`SuperPlus` = no cap).
    ///
    /// Blocks while the device boots. A network (PoE) device is booted with
    /// a 60 s initial watchdog, because on a link without DHCP its firmware
    /// spends ~30 s soliciting an address before falling back to
    /// 169.254.1.222: such an open takes about 30 s, and needs depthai's
    /// boot wait (`DEPTHAI_BOOTUP_TIMEOUT`, ms) raised above that, which the
    /// caller does through the environment (the oak-cam binary sets 90 s at
    /// start). For an id or `""`, telling USB from PoE costs one discovery
    /// scan first.
    pub fn open(selector: &str, max_usb: UsbSpeed) -> Result<Self> {
        let sel = CString::new(selector).map_err(|_| {
            Error::InvalidArgument(format!("device selector {selector:?} contains NUL"))
        })?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: sel is NUL-terminated; raw is a valid out-param.
        check(unsafe { sys::oak_device_open(sel.as_ptr(), max_usb.to_raw(), &raw mut raw) })?;
        let raw = NonNull::new(raw)
            .ok_or_else(|| Error::Depthai("oak_device_open returned NULL".into()))?;
        Ok(Self { raw })
    }

    fn ptr(&self) -> *mut sys::oak_device {
        self.raw.as_ptr()
    }

    fn string(
        &self,
        f: unsafe extern "C" fn(*mut sys::oak_device, *mut c_char, usize) -> i32,
    ) -> Result<String> {
        let mut buf = [0 as c_char; 256];
        // SAFETY: handle is live; buf has 256 writable bytes.
        check(unsafe { f(self.ptr(), buf.as_mut_ptr(), buf.len()) })?;
        Ok(fixed_str(&buf))
    }

    fn int(&self, f: unsafe extern "C" fn(*mut sys::oak_device, *mut i32) -> i32) -> Result<i32> {
        let mut v = 0i32;
        // SAFETY: handle is live; v is a valid out-param.
        check(unsafe { f(self.ptr(), &raw mut v) })?;
        Ok(v)
    }

    /// Device id (MxId).
    pub fn id(&self) -> Result<String> {
        self.string(sys::oak_device_id)
    }

    /// Product name, e.g. `"OAK-D-PRO-AF"`.
    pub fn name(&self) -> Result<String> {
        self.string(sys::oak_device_name)
    }

    /// Negotiated USB speed (`Unknown` for PoE).
    pub fn usb_speed(&self) -> Result<UsbSpeed> {
        self.int(sys::oak_device_usb_speed).map(UsbSpeed::from_raw)
    }

    /// Transport the device is connected over.
    pub fn protocol(&self) -> Result<Protocol> {
        self.int(sys::oak_device_protocol).map(Protocol::from_raw)
    }

    /// Silicon platform.
    pub fn platform(&self) -> Result<Platform> {
        self.int(sys::oak_device_platform).map(Platform::from_raw)
    }

    /// Whether the connection has closed (e.g. cable pulled).
    pub fn is_closed(&self) -> Result<bool> {
        self.int(sys::oak_device_is_closed).map(|v| v != 0)
    }

    /// Connected camera sensors, each with its supported configs.
    pub fn camera_features(&self) -> Result<Vec<CameraFeatures>> {
        let mut cap = 8usize;
        let raw = loop {
            let mut buf = vec![zeroed_features(); cap];
            let mut count = 0usize;
            // SAFETY: handle is live; buf has `cap` writable entries.
            check(unsafe {
                sys::oak_device_camera_features(self.ptr(), buf.as_mut_ptr(), cap, &raw mut count)
            })?;
            if count <= cap {
                buf.truncate(count);
                break buf;
            }
            cap = count;
        };
        raw.iter()
            .map(|f| {
                Ok(CameraFeatures {
                    socket: Socket::from_raw(f.socket),
                    sensor_type: SensorType::from_raw(f.sensor_type),
                    sensor_name: fixed_str(&f.sensor_name),
                    name: fixed_str(&f.name),
                    width: f.width,
                    height: f.height,
                    has_autofocus: f.has_autofocus != 0,
                    configs: self
                        .camera_configs(f.socket, usize::try_from(f.config_count).unwrap_or(0))?,
                })
            })
            .collect()
    }

    fn camera_configs(&self, socket: i32, hint: usize) -> Result<Vec<SensorConfig>> {
        let mut cap = hint.max(1);
        loop {
            let mut buf = vec![sys::oak_sensor_config::default(); cap];
            let mut count = 0usize;
            // SAFETY: handle is live; buf has `cap` writable entries.
            check(unsafe {
                sys::oak_device_camera_configs(
                    self.ptr(),
                    socket,
                    buf.as_mut_ptr(),
                    cap,
                    &raw mut count,
                )
            })?;
            if count <= cap {
                buf.truncate(count);
                return Ok(buf.iter().map(SensorConfig::from).collect());
            }
            cap = count;
        }
    }

    /// Number of IR drivers (2 on an OAK-D Pro: dot projector + flood).
    pub fn ir_drivers(&self) -> Result<u32> {
        self.int(sys::oak_device_ir_drivers)
            .map(|n| u32::try_from(n).unwrap_or(0))
    }

    /// IR dot projector intensity, `0.0..=1.0`.
    pub fn set_ir_dot(&self, intensity: f32) -> Result<()> {
        // SAFETY: handle is live.
        check(unsafe { sys::oak_device_set_ir_dot(self.ptr(), intensity) })
    }

    /// IR flood light intensity, `0.0..=1.0`.
    pub fn set_ir_flood(&self, intensity: f32) -> Result<()> {
        // SAFETY: handle is live.
        check(unsafe { sys::oak_device_set_ir_flood(self.ptr(), intensity) })
    }

    /// Average chip temperature in °C.
    pub fn chip_temp(&self) -> Result<f32> {
        let mut t = 0f32;
        // SAFETY: handle is live; t is a valid out-param.
        check(unsafe { sys::oak_device_chip_temp(self.ptr(), &raw mut t) })?;
        Ok(t)
    }

    /// The raw handle, for the pipeline wrapper.
    pub(crate) fn as_raw(&self) -> *mut sys::oak_device {
        self.ptr()
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: we own the handle; closed exactly once.
        unsafe { sys::oak_device_close(self.raw.as_ptr()) };
    }
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device").field("raw", &self.raw).finish()
    }
}

fn zeroed_info() -> sys::oak_device_info {
    sys::oak_device_info {
        name: [0; 64],
        device_id: [0; 32],
        protocol: 0,
        state: 0,
        platform: 0,
        _pad: 0,
    }
}

fn zeroed_features() -> sys::oak_camera_features {
    sys::oak_camera_features {
        socket: 0,
        sensor_type: 0,
        sensor_name: [0; 32],
        name: [0; 32],
        width: 0,
        height: 0,
        has_autofocus: 0,
        config_count: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn assert_send<T: Send>() {}
    const _: () = assert_send::<Device>();

    #[test]
    fn device_info_from_raw() {
        let mut raw = zeroed_info();
        raw.name[..5].copy_from_slice(&crate::c_chars(b"1.2.3"));
        raw.device_id[..8].copy_from_slice(&crate::c_chars(b"14442C10"));
        raw.protocol = sys::OAK_PROTOCOL_USB;
        raw.state = sys::OAK_STATE_UNBOOTED;
        raw.platform = sys::OAK_PLATFORM_RVC2;
        let info = DeviceInfo::from(&raw);
        assert_eq!(info.name, "1.2.3");
        assert_eq!(info.id, "14442C10");
        assert_eq!(info.protocol, Protocol::Usb);
        assert_eq!(info.state, DeviceState::Unbooted);
        assert_eq!(info.platform, Platform::Rvc2);
    }

    /// Needs an OAK attached: `cargo test -p depthai -- --ignored`.
    #[test]
    #[ignore = "needs an OAK device"]
    fn open_first_device() {
        // As oak-cam's main: depthai 3.10.0 crashes saving a crash dump on Windows.
        #[cfg(windows)]
        crate::env_default("DEPTHAI_CRASHDUMP", "0").unwrap();
        let dev = Device::open("", UsbSpeed::SuperPlus).expect("open");
        assert!(!dev.id().unwrap().is_empty());
        assert!(!dev.camera_features().unwrap().is_empty());
    }
}
