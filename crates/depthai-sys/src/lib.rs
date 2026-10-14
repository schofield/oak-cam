//! Raw `extern "C"` declarations for the oak-cam shim over depthai-core v3
//! (`csrc/oak_dai.h`). Hand-written, no bindgen: the header is ours and small.
//!
//! Every `#[repr(C)]` struct's size is checked against the C++ side's
//! `static_assert`s (see `oak_dai.cpp`), so a drift fails the build.
//!
//! See the header for the calling conventions (0/-1 return codes, per-thread
//! last error, opaque handles). The safe wrapper lives in the `depthai` crate.

// Names mirror oak_dai.h exactly (including the public `_pad` fields that
// keep the C layouts explicit), and the header documents every item.
#![allow(
    non_camel_case_types,
    missing_docs,
    clippy::missing_safety_doc,
    clippy::pub_underscore_fields
)]

use std::ffi::{c_char, c_void};

// ---------------------------------------------------------------------------
// Opaque handles
// ---------------------------------------------------------------------------

macro_rules! opaque {
    ($($name:ident),* $(,)?) => {$(
        #[repr(C)]
        pub struct $name {
            _private: [u8; 0],
            _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
        }
    )*};
}
opaque!(
    oak_device,
    oak_pipeline,
    oak_camera,
    oak_output,
    oak_queue,
    oak_control_queue,
    oak_frame,
);

// ---------------------------------------------------------------------------
// Enum constants
// ---------------------------------------------------------------------------

pub const OAK_PROTOCOL_UNKNOWN: i32 = 0;
pub const OAK_PROTOCOL_USB: i32 = 1;
pub const OAK_PROTOCOL_TCP_IP: i32 = 2;

pub const OAK_STATE_UNKNOWN: i32 = 0;
pub const OAK_STATE_UNBOOTED: i32 = 1;
pub const OAK_STATE_BOOTLOADER: i32 = 2;
pub const OAK_STATE_BOOTED: i32 = 3;
pub const OAK_STATE_FLASH_BOOTED: i32 = 4;
pub const OAK_STATE_GATE: i32 = 5;

pub const OAK_PLATFORM_UNKNOWN: i32 = 0;
pub const OAK_PLATFORM_RVC2: i32 = 2;
pub const OAK_PLATFORM_RVC3: i32 = 3;
pub const OAK_PLATFORM_RVC4: i32 = 4;

pub const OAK_USB_UNKNOWN: i32 = 0;
pub const OAK_USB_LOW: i32 = 1;
pub const OAK_USB_FULL: i32 = 2;
pub const OAK_USB_HIGH: i32 = 3;
pub const OAK_USB_SUPER: i32 = 4;
pub const OAK_USB_SUPER_PLUS: i32 = 5;

pub const OAK_SOCKET_AUTO: i32 = -1;
pub const OAK_SOCKET_CAM_A: i32 = 0;
pub const OAK_SOCKET_CAM_B: i32 = 1;
pub const OAK_SOCKET_CAM_C: i32 = 2;
pub const OAK_SOCKET_CAM_D: i32 = 3;

pub const OAK_SENSOR_AUTO: i32 = -1;
pub const OAK_SENSOR_COLOR: i32 = 0;
pub const OAK_SENSOR_MONO: i32 = 1;
pub const OAK_SENSOR_TOF: i32 = 2;
pub const OAK_SENSOR_THERMAL: i32 = 3;

pub const OAK_IMG_NONE: i32 = 0;
pub const OAK_IMG_NV12: i32 = 1;
pub const OAK_IMG_GRAY8: i32 = 2;
pub const OAK_IMG_YUV420P: i32 = 3;
pub const OAK_IMG_RAW8: i32 = 4;
pub const OAK_IMG_BGR888I: i32 = 5;
pub const OAK_IMG_RGB888I: i32 = 6;
pub const OAK_IMG_OTHER: i32 = 100;

pub const OAK_RESIZE_CROP: i32 = 0;
pub const OAK_RESIZE_STRETCH: i32 = 1;
pub const OAK_RESIZE_LETTERBOX: i32 = 2;

pub const OAK_AF_OFF: i32 = 0;
pub const OAK_AF_AUTO: i32 = 1;
pub const OAK_AF_MACRO: i32 = 2;
pub const OAK_AF_CONTINUOUS_VIDEO: i32 = 3;
pub const OAK_AF_CONTINUOUS_PICTURE: i32 = 4;
pub const OAK_AF_EDOF: i32 = 5;

pub const OAK_AWB_OFF: i32 = 0;
pub const OAK_AWB_AUTO: i32 = 1;
pub const OAK_AWB_INCANDESCENT: i32 = 2;
pub const OAK_AWB_FLUORESCENT: i32 = 3;
pub const OAK_AWB_WARM_FLUORESCENT: i32 = 4;
pub const OAK_AWB_DAYLIGHT: i32 = 5;
pub const OAK_AWB_CLOUDY_DAYLIGHT: i32 = 6;
pub const OAK_AWB_TWILIGHT: i32 = 7;
pub const OAK_AWB_SHADE: i32 = 8;

pub const OAK_ANTIBANDING_OFF: i32 = 0;
pub const OAK_ANTIBANDING_50HZ: i32 = 1;
pub const OAK_ANTIBANDING_60HZ: i32 = 2;
pub const OAK_ANTIBANDING_AUTO: i32 = 3;

pub const OAK_LOG_TRACE: i32 = 0;
pub const OAK_LOG_DEBUG: i32 = 1;
pub const OAK_LOG_INFO: i32 = 2;
pub const OAK_LOG_WARN: i32 = 3;
pub const OAK_LOG_ERR: i32 = 4;
pub const OAK_LOG_CRITICAL: i32 = 5;
pub const OAK_LOG_OFF: i32 = 6;

pub const OAK_CTL_AE_AUTO: u64 = 1 << 0;
pub const OAK_CTL_MANUAL_EXPOSURE: u64 = 1 << 1;
pub const OAK_CTL_AE_COMPENSATION: u64 = 1 << 2;
pub const OAK_CTL_AE_LOCK: u64 = 1 << 3;
pub const OAK_CTL_AE_LIMIT: u64 = 1 << 4;
pub const OAK_CTL_AE_MAX_ISO: u64 = 1 << 5;
pub const OAK_CTL_ANTIBANDING: u64 = 1 << 6;
pub const OAK_CTL_AF_MODE: u64 = 1 << 7;
pub const OAK_CTL_MANUAL_FOCUS: u64 = 1 << 8;
pub const OAK_CTL_AF_TRIGGER: u64 = 1 << 9;
pub const OAK_CTL_AWB_MODE: u64 = 1 << 10;
pub const OAK_CTL_MANUAL_WB: u64 = 1 << 11;
pub const OAK_CTL_AWB_LOCK: u64 = 1 << 12;
pub const OAK_CTL_BRIGHTNESS: u64 = 1 << 13;
pub const OAK_CTL_CONTRAST: u64 = 1 << 14;
pub const OAK_CTL_SATURATION: u64 = 1 << 15;
pub const OAK_CTL_SHARPNESS: u64 = 1 << 16;
pub const OAK_CTL_LUMA_DENOISE: u64 = 1 << 17;
pub const OAK_CTL_CHROMA_DENOISE: u64 = 1 << 18;
pub const OAK_CTL_AF_LENS_RANGE: u64 = 1 << 19;

// ---------------------------------------------------------------------------
// POD structs (layouts mirror oak_dai.h exactly)
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct oak_device_info {
    pub name: [c_char; 64],
    pub device_id: [c_char; 32],
    pub protocol: i32,
    pub state: i32,
    pub platform: i32,
    pub _pad: i32,
}
const _: () = assert!(size_of::<oak_device_info>() == 112);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct oak_camera_features {
    pub socket: i32,
    pub sensor_type: i32,
    pub sensor_name: [c_char; 32],
    pub name: [c_char; 32],
    pub width: i32,
    pub height: i32,
    pub has_autofocus: i32,
    pub config_count: i32,
}
const _: () = assert!(size_of::<oak_camera_features>() == 88);

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct oak_sensor_config {
    pub width: i32,
    pub height: i32,
    pub min_fps: f32,
    pub max_fps: f32,
    pub sensor_type: i32,
    pub _pad: i32,
}
const _: () = assert!(size_of::<oak_sensor_config>() == 24);

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct oak_output_spec {
    pub width: u32,
    pub height: u32,
    pub type_: i32,
    pub resize_mode: i32,
    pub fps: f32,
    pub _pad: u32,
}
const _: () = assert!(size_of::<oak_output_spec>() == 24);

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct oak_frame_view {
    pub data: *const u8,
    pub len: usize,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub plane2_offset: u32,
    pub plane3_offset: u32,
    pub type_: i32,
    pub instance_num: i32,
    pub _pad: i32,
    pub ts_host_ns: i64,
    pub ts_device_ns: i64,
    pub seq: i64,
    pub exposure_us: u32,
    pub iso: u32,
    /// Lens position as reported (0 for fixed-focus cameras).
    pub lens_pos: i32,
    pub color_temp_k: i32,
}
const _: () = assert!(size_of::<oak_frame_view>() == 88);

impl Default for oak_frame_view {
    fn default() -> Self {
        Self {
            data: std::ptr::null(),
            len: 0,
            width: 0,
            height: 0,
            stride: 0,
            plane2_offset: 0,
            plane3_offset: 0,
            type_: OAK_IMG_NONE,
            instance_num: 0,
            _pad: 0,
            ts_host_ns: 0,
            ts_device_ns: 0,
            seq: 0,
            exposure_us: 0,
            iso: 0,
            lens_pos: 0,
            color_temp_k: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct oak_control_params {
    pub set: u64,
    pub exposure_us: u32,
    pub iso: u32,
    pub ae_compensation: i32,
    pub ae_lock: i32,
    pub ae_limit_us: u32,
    pub ae_max_iso: u32,
    pub antibanding: i32,
    pub af_mode: i32,
    pub lens_pos: i32,
    pub af_infinity: i32,
    pub af_macro: i32,
    pub awb_mode: i32,
    pub wb_kelvin: i32,
    pub awb_lock: i32,
    pub brightness: i32,
    pub contrast: i32,
    pub saturation: i32,
    pub sharpness: i32,
    pub luma_denoise: i32,
    pub chroma_denoise: i32,
    pub _pad: i32,
}
const _: () = assert!(size_of::<oak_control_params>() == 96);

pub type oak_log_fn = Option<
    unsafe extern "C" fn(
        user: *mut c_void,
        level: i32,
        node: *const c_char,
        message: *const c_char,
    ),
>;

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

unsafe extern "C" {
    pub fn oak_version() -> *const c_char;
    pub fn oak_last_error() -> *const c_char;
    pub fn oak_clear_error();
    pub fn oak_steady_now_ns(out_ns: *mut i64) -> i32;
    pub fn oak_set_log_level(level: i32) -> i32;
    pub fn oak_env_default(key: *const c_char, value: *const c_char) -> i32;
    pub fn oak_set_log_callback(f: oak_log_fn, user: *mut c_void);

    pub fn oak_list_devices(out: *mut oak_device_info, cap: usize, out_count: *mut usize) -> i32;

    pub fn oak_device_open(
        id_or_ip: *const c_char,
        max_usb_speed: i32,
        out: *mut *mut oak_device,
    ) -> i32;
    pub fn oak_device_close(dev: *mut oak_device);
    pub fn oak_device_is_closed(dev: *mut oak_device, out: *mut i32) -> i32;
    pub fn oak_device_id(dev: *mut oak_device, buf: *mut c_char, cap: usize) -> i32;
    pub fn oak_device_name(dev: *mut oak_device, buf: *mut c_char, cap: usize) -> i32;
    pub fn oak_device_usb_speed(dev: *mut oak_device, out: *mut i32) -> i32;
    pub fn oak_device_protocol(dev: *mut oak_device, out: *mut i32) -> i32;
    pub fn oak_device_platform(dev: *mut oak_device, out: *mut i32) -> i32;
    pub fn oak_device_camera_features(
        dev: *mut oak_device,
        out: *mut oak_camera_features,
        cap: usize,
        out_count: *mut usize,
    ) -> i32;
    pub fn oak_device_camera_configs(
        dev: *mut oak_device,
        socket: i32,
        out: *mut oak_sensor_config,
        cap: usize,
        out_count: *mut usize,
    ) -> i32;
    pub fn oak_device_ir_drivers(dev: *mut oak_device, out_count: *mut i32) -> i32;
    pub fn oak_device_set_ir_dot(dev: *mut oak_device, intensity: f32) -> i32;
    pub fn oak_device_set_ir_flood(dev: *mut oak_device, intensity: f32) -> i32;
    pub fn oak_device_chip_temp(dev: *mut oak_device, out_celsius: *mut f32) -> i32;

    pub fn oak_pipeline_new(dev: *mut oak_device, out: *mut *mut oak_pipeline) -> i32;
    pub fn oak_pipeline_free(p: *mut oak_pipeline);
    pub fn oak_pipeline_add_camera(
        p: *mut oak_pipeline,
        socket: i32,
        sensor_w: u32,
        sensor_h: u32,
        sensor_fps: f32,
        out: *mut *mut oak_camera,
    ) -> i32;
    pub fn oak_camera_request_output(
        cam: *mut oak_camera,
        spec: *const oak_output_spec,
        out: *mut *mut oak_output,
    ) -> i32;
    pub fn oak_camera_set_initial_control(
        cam: *mut oak_camera,
        params: *const oak_control_params,
    ) -> i32;
    pub fn oak_camera_control_queue(cam: *mut oak_camera, out: *mut *mut oak_control_queue) -> i32;
    pub fn oak_camera_max_size(cam: *mut oak_camera, out_w: *mut u32, out_h: *mut u32) -> i32;
    pub fn oak_output_create_queue(
        out: *mut oak_output,
        max_size: u32,
        blocking: i32,
        out_queue: *mut *mut oak_queue,
    ) -> i32;
    pub fn oak_pipeline_start(p: *mut oak_pipeline) -> i32;
    pub fn oak_pipeline_is_running(p: *mut oak_pipeline, out: *mut i32) -> i32;
    pub fn oak_pipeline_stop(p: *mut oak_pipeline) -> i32;

    pub fn oak_queue_get(q: *mut oak_queue, timeout_ns: i64, out: *mut *mut oak_frame) -> i32;
    pub fn oak_queue_try_get(q: *mut oak_queue, out: *mut *mut oak_frame) -> i32;
    pub fn oak_queue_is_closed(q: *mut oak_queue, out: *mut i32) -> i32;
    pub fn oak_queue_free(q: *mut oak_queue);

    /// `lens_pos` in the view is the lens position as reported (0 for
    /// fixed-focus cameras).
    pub fn oak_frame_get_view(f: *const oak_frame, out: *mut oak_frame_view) -> i32;
    pub fn oak_frame_release(f: *mut oak_frame);

    pub fn oak_control_send(q: *mut oak_control_queue, params: *const oak_control_params) -> i32;
    pub fn oak_control_queue_free(q: *mut oak_control_queue);
}
