// oak_dai.cpp — implementation of oak_dai.h over depthai-core v3.10.0.
//
// Every entry point is wrapped in OAK_TRY / OAK_CATCH: no C++ exception
// crosses the C boundary, the message lands in a thread_local string read by
// oak_last_error(). The dai:: signatures relied on are listed in README.md.

#include "oak_dai.h"

#include <atomic>
#include <chrono>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <memory>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

#include "depthai/build/version.hpp"
#include "depthai/common/CameraBoardSocket.hpp"
#include "depthai/common/CameraFeatures.hpp"
#include "depthai/common/CameraSensorType.hpp"
#include "depthai/common/UsbSpeed.hpp"
#include "depthai/capabilities/ImgFrameCapability.hpp"
#include "depthai/device/Device.hpp"
#include "depthai/device/Platform.hpp"
#include "depthai/log/LogLevel.hpp"
#include "depthai/log/LogMessage.hpp"
#include "depthai/pipeline/InputQueue.hpp"
#include "depthai/pipeline/MessageQueue.hpp"
#include "depthai/pipeline/Pipeline.hpp"
#include "depthai/pipeline/datatype/CameraControl.hpp"
#include "depthai/pipeline/datatype/ImgFrame.hpp"
#include "depthai/pipeline/node/Camera.hpp"

// inet_pton, for telling an IP selector from a device id. Included after the
// depthai headers; on Windows without the min/max macros, and linked against
// Ws2_32 from here since the shim is a static library.
#if defined(_WIN32)
    #ifndef NOMINMAX
        #define NOMINMAX
    #endif
    #ifndef WIN32_LEAN_AND_MEAN
        #define WIN32_LEAN_AND_MEAN
    #endif
    #include <winsock2.h>
    #include <ws2tcpip.h>
    #pragma comment(lib, "Ws2_32.lib")
#else
    #include <arpa/inet.h>
#endif

// ===========================================================================
// Layout and enum checks (a depthai change breaks the build, not the runtime)
// ===========================================================================

static_assert(sizeof(oak_device_info) == 112, "oak_device_info layout");
static_assert(sizeof(oak_camera_features) == 88, "oak_camera_features layout");
static_assert(sizeof(oak_sensor_config) == 24, "oak_sensor_config layout");
static_assert(sizeof(oak_output_spec) == 24, "oak_output_spec layout");
static_assert(sizeof(oak_frame_view) == 88, "oak_frame_view layout");
static_assert(sizeof(oak_control_params) == 96, "oak_control_params layout");

#define OAK_SAME(c, e) static_assert(static_cast<int>(c) == static_cast<int>(e), #c " != " #e)

// Numeric identity is relied on (checked cast after a range check).
OAK_SAME(OAK_USB_UNKNOWN, dai::UsbSpeed::UNKNOWN);
OAK_SAME(OAK_USB_LOW, dai::UsbSpeed::LOW);
OAK_SAME(OAK_USB_FULL, dai::UsbSpeed::FULL);
OAK_SAME(OAK_USB_HIGH, dai::UsbSpeed::HIGH);
OAK_SAME(OAK_USB_SUPER, dai::UsbSpeed::SUPER);
OAK_SAME(OAK_USB_SUPER_PLUS, dai::UsbSpeed::SUPER_PLUS);

OAK_SAME(OAK_SOCKET_AUTO, dai::CameraBoardSocket::AUTO);
OAK_SAME(OAK_SOCKET_CAM_A, dai::CameraBoardSocket::CAM_A);
OAK_SAME(OAK_SOCKET_CAM_B, dai::CameraBoardSocket::CAM_B);
OAK_SAME(OAK_SOCKET_CAM_C, dai::CameraBoardSocket::CAM_C);
OAK_SAME(OAK_SOCKET_CAM_D, dai::CameraBoardSocket::CAM_D);

OAK_SAME(OAK_SENSOR_AUTO, dai::CameraSensorType::AUTO);
OAK_SAME(OAK_SENSOR_COLOR, dai::CameraSensorType::COLOR);
OAK_SAME(OAK_SENSOR_MONO, dai::CameraSensorType::MONO);
OAK_SAME(OAK_SENSOR_TOF, dai::CameraSensorType::TOF);
OAK_SAME(OAK_SENSOR_THERMAL, dai::CameraSensorType::THERMAL);

OAK_SAME(OAK_RESIZE_CROP, dai::ImgResizeMode::CROP);
OAK_SAME(OAK_RESIZE_STRETCH, dai::ImgResizeMode::STRETCH);
OAK_SAME(OAK_RESIZE_LETTERBOX, dai::ImgResizeMode::LETTERBOX);

OAK_SAME(OAK_AF_OFF, dai::CameraControl::AutoFocusMode::OFF);
OAK_SAME(OAK_AF_AUTO, dai::CameraControl::AutoFocusMode::AUTO);
OAK_SAME(OAK_AF_MACRO, dai::CameraControl::AutoFocusMode::MACRO);
OAK_SAME(OAK_AF_CONTINUOUS_VIDEO, dai::CameraControl::AutoFocusMode::CONTINUOUS_VIDEO);
OAK_SAME(OAK_AF_CONTINUOUS_PICTURE, dai::CameraControl::AutoFocusMode::CONTINUOUS_PICTURE);
OAK_SAME(OAK_AF_EDOF, dai::CameraControl::AutoFocusMode::EDOF);

OAK_SAME(OAK_AWB_OFF, dai::CameraControl::AutoWhiteBalanceMode::OFF);
OAK_SAME(OAK_AWB_AUTO, dai::CameraControl::AutoWhiteBalanceMode::AUTO);
OAK_SAME(OAK_AWB_INCANDESCENT, dai::CameraControl::AutoWhiteBalanceMode::INCANDESCENT);
OAK_SAME(OAK_AWB_FLUORESCENT, dai::CameraControl::AutoWhiteBalanceMode::FLUORESCENT);
OAK_SAME(OAK_AWB_WARM_FLUORESCENT, dai::CameraControl::AutoWhiteBalanceMode::WARM_FLUORESCENT);
OAK_SAME(OAK_AWB_DAYLIGHT, dai::CameraControl::AutoWhiteBalanceMode::DAYLIGHT);
OAK_SAME(OAK_AWB_CLOUDY_DAYLIGHT, dai::CameraControl::AutoWhiteBalanceMode::CLOUDY_DAYLIGHT);
OAK_SAME(OAK_AWB_TWILIGHT, dai::CameraControl::AutoWhiteBalanceMode::TWILIGHT);
OAK_SAME(OAK_AWB_SHADE, dai::CameraControl::AutoWhiteBalanceMode::SHADE);

OAK_SAME(OAK_ANTIBANDING_OFF, dai::CameraControl::AntiBandingMode::OFF);
OAK_SAME(OAK_ANTIBANDING_50HZ, dai::CameraControl::AntiBandingMode::MAINS_50_HZ);
OAK_SAME(OAK_ANTIBANDING_60HZ, dai::CameraControl::AntiBandingMode::MAINS_60_HZ);
OAK_SAME(OAK_ANTIBANDING_AUTO, dai::CameraControl::AntiBandingMode::AUTO);

OAK_SAME(OAK_LOG_TRACE, dai::LogLevel::TRACE);
OAK_SAME(OAK_LOG_DEBUG, dai::LogLevel::DEBUG);
OAK_SAME(OAK_LOG_INFO, dai::LogLevel::INFO);
OAK_SAME(OAK_LOG_WARN, dai::LogLevel::WARN);
OAK_SAME(OAK_LOG_ERR, dai::LogLevel::ERR);
OAK_SAME(OAK_LOG_CRITICAL, dai::LogLevel::CRITICAL);
OAK_SAME(OAK_LOG_OFF, dai::LogLevel::OFF);

// ===========================================================================
// Error plumbing
// ===========================================================================

namespace {

thread_local std::string t_last_error;

void set_error(const char* msg) {
    try {
        t_last_error = msg ? msg : "unknown error";
    } catch(...) {
        // Allocation failed; keep whatever was there.
    }
}

// Thrown by the shim itself for argument errors.
struct OakError : std::runtime_error {
    using std::runtime_error::runtime_error;
};

void require(bool cond, const char* msg) {
    if(!cond) throw OakError(msg);
}

void copy_str(char* buf, size_t cap, const std::string& s) {
    if(!buf || cap == 0) return;
    size_t n = s.size() < cap - 1 ? s.size() : cap - 1;
    std::memcpy(buf, s.data(), n);
    buf[n] = '\0';
}

}  // namespace

#define OAK_TRY try {
#define OAK_CATCH(ret)                                \
    }                                                 \
    catch(const std::exception& e) {                  \
        set_error(e.what());                          \
        return ret;                                   \
    }                                                 \
    catch(...) {                                      \
        set_error("unknown C++ exception in oak_dai"); \
        return ret;                                   \
    }
#define OAK_CATCH_VOID                                \
    }                                                 \
    catch(const std::exception& e) {                  \
        set_error(e.what());                          \
    }                                                 \
    catch(...) {                                      \
        set_error("unknown C++ exception in oak_dai"); \
    }

// ===========================================================================
// Handle types
// ===========================================================================

struct oak_pipeline;

struct oak_device {
    std::shared_ptr<dai::Device> dev;
};

struct oak_camera {
    oak_pipeline* owner = nullptr;  // outputs are stored in the owning pipeline
    std::shared_ptr<dai::node::Camera> node;
};

struct oak_output {
    std::shared_ptr<dai::node::Camera> node;  // keeps the Output's owner alive
    dai::Node::Output* out = nullptr;
};

struct oak_pipeline {
    std::shared_ptr<dai::Device> dev;
    std::unique_ptr<dai::Pipeline> pipeline;
    std::vector<std::unique_ptr<oak_camera>> cameras;
    std::vector<std::unique_ptr<oak_output>> outputs;
};

struct oak_queue {
    std::shared_ptr<dai::MessageQueue> q;
};

struct oak_control_queue {
    std::shared_ptr<dai::InputQueue> q;
};

struct oak_frame {
    std::shared_ptr<dai::ImgFrame> img;
};

// ===========================================================================
// Enum mapping
// ===========================================================================

namespace {

int32_t map_protocol(XLinkProtocol_t p) {
    switch(p) {
        case X_LINK_USB_VSC:
        case X_LINK_USB_CDC:
        case X_LINK_USB_EP:
            return OAK_PROTOCOL_USB;
        case X_LINK_TCP_IP:
        case X_LINK_TCP_IP_OR_LOCAL_SHDMEM:
            return OAK_PROTOCOL_TCP_IP;
        default:
            return OAK_PROTOCOL_UNKNOWN;
    }
}

int32_t map_state(XLinkDeviceState_t s) {
    switch(s) {
        case X_LINK_UNBOOTED:
            return OAK_STATE_UNBOOTED;
        case X_LINK_BOOTLOADER:
            return OAK_STATE_BOOTLOADER;
        case X_LINK_BOOTED:
            return OAK_STATE_BOOTED;
        case X_LINK_FLASH_BOOTED:  // == X_LINK_BOOTED_NON_EXCLUSIVE
            return OAK_STATE_FLASH_BOOTED;
        case X_LINK_GATE:
        case X_LINK_GATE_BOOTED:
        case X_LINK_GATE_SETUP:
            return OAK_STATE_GATE;
        default:
            return OAK_STATE_UNKNOWN;
    }
}

int32_t map_xlink_platform(XLinkPlatform_t p) {
    switch(p) {
        case X_LINK_MYRIAD_X:
            return OAK_PLATFORM_RVC2;
        case X_LINK_RVC3:
            return OAK_PLATFORM_RVC3;
        case X_LINK_RVC4:
            return OAK_PLATFORM_RVC4;
        default:
            return OAK_PLATFORM_UNKNOWN;
    }
}

int32_t map_platform(dai::Platform p) {
    switch(p) {
        case dai::Platform::RVC2:
            return OAK_PLATFORM_RVC2;
        case dai::Platform::RVC3:
            return OAK_PLATFORM_RVC3;
        case dai::Platform::RVC4:
            return OAK_PLATFORM_RVC4;
    }
    return OAK_PLATFORM_UNKNOWN;
}

dai::UsbSpeed to_usb_speed(int32_t v) {
    require(v >= OAK_USB_UNKNOWN && v <= OAK_USB_SUPER_PLUS, "invalid OAK_USB_* value");
    return static_cast<dai::UsbSpeed>(v);
}

int32_t from_usb_speed(dai::UsbSpeed s) {
    int32_t v = static_cast<int32_t>(s);
    return (v >= OAK_USB_UNKNOWN && v <= OAK_USB_SUPER_PLUS) ? v : OAK_USB_UNKNOWN;
}

dai::CameraBoardSocket to_socket(int32_t v) {
    require(v >= OAK_SOCKET_AUTO && v <= OAK_SOCKET_CAM_D, "invalid OAK_SOCKET_* value");
    return static_cast<dai::CameraBoardSocket>(v);
}

// Sockets beyond CAM_D are passed through numerically (dai's enum continues
// CAM_E, CAM_F, ...); oak-cam only uses A..C.
int32_t from_socket(dai::CameraBoardSocket s) {
    return static_cast<int32_t>(s);
}

int32_t from_sensor_type(dai::CameraSensorType t) {
    int32_t v = static_cast<int32_t>(t);
    return (v >= OAK_SENSOR_AUTO && v <= OAK_SENSOR_THERMAL) ? v : OAK_SENSOR_AUTO;
}

std::optional<dai::ImgFrame::Type> to_img_type(int32_t v) {
    using T = dai::ImgFrame::Type;
    switch(v) {
        case OAK_IMG_NONE:
            return std::nullopt;
        case OAK_IMG_NV12:
            return T::NV12;
        case OAK_IMG_GRAY8:
            return T::GRAY8;
        case OAK_IMG_YUV420P:
            return T::YUV420p;
        case OAK_IMG_RAW8:
            return T::RAW8;
        case OAK_IMG_BGR888I:
            return T::BGR888i;
        case OAK_IMG_RGB888I:
            return T::RGB888i;
        default:
            throw OakError("invalid OAK_IMG_* value for an output request");
    }
}

int32_t from_img_type(dai::ImgFrame::Type t) {
    using T = dai::ImgFrame::Type;
    switch(t) {
        case T::NV12:
            return OAK_IMG_NV12;
        case T::GRAY8:
            return OAK_IMG_GRAY8;
        case T::YUV420p:
            return OAK_IMG_YUV420P;
        case T::RAW8:
            return OAK_IMG_RAW8;
        case T::BGR888i:
            return OAK_IMG_BGR888I;
        case T::RGB888i:
            return OAK_IMG_RGB888I;
        case T::NONE:
            return OAK_IMG_NONE;
        default:
            return OAK_IMG_OTHER;
    }
}

dai::ImgResizeMode to_resize_mode(int32_t v) {
    require(v >= OAK_RESIZE_CROP && v <= OAK_RESIZE_LETTERBOX, "invalid OAK_RESIZE_* value");
    return static_cast<dai::ImgResizeMode>(v);
}

dai::LogLevel to_log_level(int32_t v) {
    require(v >= OAK_LOG_TRACE && v <= OAK_LOG_OFF, "invalid OAK_LOG_* value");
    return static_cast<dai::LogLevel>(v);
}

int64_t steady_ns(std::chrono::steady_clock::time_point tp) {
    return std::chrono::duration_cast<std::chrono::nanoseconds>(tp.time_since_epoch()).count();
}

// ===========================================================================
// CameraControl from oak_control_params
// ===========================================================================

constexpr uint64_t kAllCtlBits = (1ull << 20) - 1;

void apply_control(dai::CameraControl& c, const oak_control_params& p) {
    using CC = dai::CameraControl;
    require((p.set & ~kAllCtlBits) == 0, "oak_control_params.set has unknown bits");
    const uint64_t s = p.set;

    if(s & OAK_CTL_AE_AUTO) c.setAutoExposureEnable();
    if(s & OAK_CTL_MANUAL_EXPOSURE) c.setManualExposure(p.exposure_us, p.iso);
    if(s & OAK_CTL_AE_COMPENSATION) c.setAutoExposureCompensation(p.ae_compensation);
    if(s & OAK_CTL_AE_LOCK) c.setAutoExposureLock(p.ae_lock != 0);
    if(s & OAK_CTL_AE_LIMIT) c.setAutoExposureLimit(p.ae_limit_us);
    if(s & OAK_CTL_AE_MAX_ISO) c.setAutoExposureMaxISO(p.ae_max_iso);
    if(s & OAK_CTL_ANTIBANDING) {
        require(p.antibanding >= OAK_ANTIBANDING_OFF && p.antibanding <= OAK_ANTIBANDING_AUTO, "invalid OAK_ANTIBANDING_* value");
        c.setAntiBandingMode(static_cast<CC::AntiBandingMode>(p.antibanding));
    }
    if(s & OAK_CTL_AF_MODE) {
        require(p.af_mode >= OAK_AF_OFF && p.af_mode <= OAK_AF_EDOF, "invalid OAK_AF_* value");
        c.setAutoFocusMode(static_cast<CC::AutoFocusMode>(p.af_mode));
    }
    if(s & OAK_CTL_MANUAL_FOCUS) {
        require(p.lens_pos >= 0 && p.lens_pos <= 255, "lens_pos must be 0..255");
        c.setManualFocus(static_cast<uint8_t>(p.lens_pos));
    }
    if(s & OAK_CTL_AF_LENS_RANGE) c.setAutoFocusLensRange(p.af_infinity, p.af_macro);
    if(s & OAK_CTL_AF_TRIGGER) c.setAutoFocusTrigger();
    if(s & OAK_CTL_AWB_MODE) {
        require(p.awb_mode >= OAK_AWB_OFF && p.awb_mode <= OAK_AWB_SHADE, "invalid OAK_AWB_* value");
        c.setAutoWhiteBalanceMode(static_cast<CC::AutoWhiteBalanceMode>(p.awb_mode));
    }
    if(s & OAK_CTL_MANUAL_WB) c.setManualWhiteBalance(p.wb_kelvin);
    if(s & OAK_CTL_AWB_LOCK) c.setAutoWhiteBalanceLock(p.awb_lock != 0);
    if(s & OAK_CTL_BRIGHTNESS) c.setBrightness(p.brightness);
    if(s & OAK_CTL_CONTRAST) c.setContrast(p.contrast);
    if(s & OAK_CTL_SATURATION) c.setSaturation(p.saturation);
    if(s & OAK_CTL_SHARPNESS) c.setSharpness(p.sharpness);
    if(s & OAK_CTL_LUMA_DENOISE) c.setLumaDenoise(p.luma_denoise);
    if(s & OAK_CTL_CHROMA_DENOISE) c.setChromaDenoise(p.chroma_denoise);
}

// ===========================================================================
// Logging state
// ===========================================================================

struct LogSink {
    std::mutex mu;
    oak_log_fn fn = nullptr;
    void* user = nullptr;
};

LogSink& log_sink() {
    static LogSink sink;
    return sink;
}

// -1 = never set; otherwise an OAK_LOG_* applied to devices opened afterwards.
std::atomic<int32_t> g_device_log_level{-1};

void forward_log(const dai::LogMessage& msg) {
    oak_log_fn fn;
    void* user;
    {
        auto& sink = log_sink();
        std::lock_guard<std::mutex> lock(sink.mu);
        fn = sink.fn;
        user = sink.user;
    }
    if(fn) fn(user, static_cast<int32_t>(msg.level), msg.nodeIdName.c_str(), msg.payload.c_str());
}

bool has_log_callback() {
    auto& sink = log_sink();
    std::lock_guard<std::mutex> lock(sink.mu);
    return sink.fn != nullptr;
}

void set_env(const char* key, const char* value) {
#if defined(_WIN32)
    _putenv_s(key, value);
#else
    setenv(key, value, 1);
#endif
}

const char* level_env_name(int32_t level) {
    switch(level) {
        case OAK_LOG_TRACE:
            return "trace";
        case OAK_LOG_DEBUG:
            return "debug";
        case OAK_LOG_INFO:
            return "info";
        case OAK_LOG_WARN:
            return "warn";
        case OAK_LOG_ERR:
            return "error";
        case OAK_LOG_CRITICAL:
            return "critical";
        default:
            return "off";
    }
}

}  // namespace

// ===========================================================================
// Global
// ===========================================================================

extern "C" {

const char* oak_version(void) {
    return dai::build::VERSION;
}

const char* oak_last_error(void) {
    return t_last_error.c_str();
}

void oak_clear_error(void) {
    t_last_error.clear();
}

int oak_steady_now_ns(int64_t* out_ns) {
    OAK_TRY
    require(out_ns != nullptr, "out_ns is NULL");
    *out_ns = steady_ns(std::chrono::steady_clock::now());
    return 0;
    OAK_CATCH(-1)
}

// depthai's host logger (dai::Logging) is private to libdepthai-core in v3.10
// (src/utility/Logging.hpp is not installed), so the host level can only be set
// through DEPTHAI_LEVEL, which is read once when the library first initialises
// (dai::initialize(), triggered by discovery or the first Device). We set it
// here unless the user already exported it; call this before any other oak_*
// function. The level is also applied to every device opened afterwards via
// DeviceBase::setLogLevel (firmware-side log level).
int oak_set_log_level(int32_t level) {
    OAK_TRY
    to_log_level(level);  // validates
    if(std::getenv("DEPTHAI_LEVEL") == nullptr) set_env("DEPTHAI_LEVEL", level_env_name(level));
    g_device_log_level.store(level);
    return 0;
    OAK_CATCH(-1)
}

int oak_env_default(const char* key, const char* value) {
    OAK_TRY
    require(key != nullptr && key[0] != '\0', "key is NULL or empty");
    require(std::strchr(key, '=') == nullptr, "key contains '='");
    require(value != nullptr, "value is NULL");
    // Same CRT path as DEPTHAI_LEVEL in oak_set_log_level: on Windows,
    // SetEnvironmentVariableW (Rust's std::env::set_var) does not update the
    // CRT copy that depthai reads with getenv, but _putenv_s does.
    if(std::getenv(key) == nullptr) set_env(key, value);
    return 0;
    OAK_CATCH(-1)
}

void oak_set_log_callback(oak_log_fn fn, void* user) {
    OAK_TRY
    auto& sink = log_sink();
    std::lock_guard<std::mutex> lock(sink.mu);
    sink.fn = fn;
    sink.user = user;
    OAK_CATCH_VOID
}

// ===========================================================================
// Discovery
// ===========================================================================

int oak_list_devices(oak_device_info* out, size_t cap, size_t* out_count) {
    OAK_TRY
    require(out_count != nullptr, "out_count is NULL");
    require(out != nullptr || cap == 0, "out is NULL with cap > 0");
    auto devices = dai::Device::getAllAvailableDevices();
    *out_count = devices.size();
    for(size_t i = 0; i < devices.size() && i < cap; ++i) {
        const auto& d = devices[i];
        oak_device_info info{};
        copy_str(info.name, sizeof info.name, d.name);
        copy_str(info.device_id, sizeof info.device_id, d.getDeviceId());
        info.protocol = map_protocol(d.protocol);
        info.state = map_state(d.state);
        info.platform = map_xlink_platform(d.platform);
        out[i] = info;
    }
    return 0;
    OAK_CATCH(-1)
}

// ===========================================================================
// Device
// ===========================================================================

namespace {

// PoE boot timing. After booting over Ethernet the RVC2 firmware solicits DHCP
// for ~25-30 s before falling back to its static 169.254.1.222 address, but its
// initial watchdog fires after 15 s if no host has connected (crash dump:
// "initial keepalive, countdown" with "WATCHDOG_INITIAL_DELAY=15000ms"). On a
// direct cable with no DHCP server every open therefore failed with
// "Failed to find device after booting: X_LINK_DEVICE_NOT_FOUND" and the
// device rebooted to its bootloader. Network opens get a longer initial
// watchdog (verified on an OAK-D Pro PoE: opens in ~29 s). USB opens keep
// depthai's default, so a failed USB open still recovers in 15 s. depthai's
// wait for the booted device to reappear must also outlast the DHCP fallback:
// the oak-cam binary raises DEPTHAI_BOOTUP_TIMEOUT to 90 s at process start
// (crates/oak-cam/src/main.rs), not here, since setenv would race getenv on
// other threads and would leak into every later open.
constexpr uint32_t kPoeWatchdogInitialDelayMs = 60000;

bool is_ip_literal(const char* s) {
    unsigned char buf[16];
    return inet_pton(AF_INET, s, buf) == 1 || inet_pton(AF_INET6, s, buf) == 1;
}

bool is_tcp_ip(XLinkProtocol_t p) {
    return p == X_LINK_TCP_IP || p == X_LINK_TCP_IP_OR_LOCAL_SHDMEM;
}

// Whether `selector` targets a network device: an IP literal, or an id that
// discovery lists over TCP/IP. For the empty selector ("first found") this
// looks at the first device getAllAvailableDevices() returns, an
// approximation of what dai::Device(speed) picks (it searches with its own
// timeout and takes the first device found).
bool targets_network(const std::string& selector) {
    if(!selector.empty() && is_ip_literal(selector.c_str())) return true;
    const auto devices = dai::Device::getAllAvailableDevices();
    if(selector.empty()) return !devices.empty() && is_tcp_ip(devices.front().protocol);
    for(const auto& d : devices) {
        if(d.getDeviceId() == selector) return is_tcp_ip(d.protocol);
    }
    return false;
}

}  // namespace

int oak_device_open(const char* id_or_ip, int32_t max_usb_speed, oak_device** out) {
    OAK_TRY
    require(out != nullptr, "out is NULL");
    *out = nullptr;
    const dai::UsbSpeed speed = to_usb_speed(max_usb_speed);
    const std::string selector = id_or_ip == nullptr ? std::string() : std::string(id_or_ip);
    std::shared_ptr<dai::Device> dev;
    if(targets_network(selector)) {
        dai::Device::Config cfg;
        cfg.board.watchdogInitialDelayMs = kPoeWatchdogInitialDelayMs;
        if(selector.empty()) {
            dev = std::make_shared<dai::Device>(cfg, speed);
        } else {
            dev = std::make_shared<dai::Device>(cfg, dai::DeviceInfo(selector), speed);
        }
    } else if(selector.empty()) {
        dev = std::make_shared<dai::Device>(speed);
    } else {
        dev = std::make_shared<dai::Device>(dai::DeviceInfo(selector), speed);
    }
    const int32_t level = g_device_log_level.load();
    if(level >= 0) dev->setLogLevel(to_log_level(level));
    if(has_log_callback()) {
        // Device logs go to the callback only, not also to stdout.
        dev->setLogOutputLevel(dai::LogLevel::OFF);
        dev->addLogCallback([](dai::LogMessage msg) {
            try {
                forward_log(msg);
            } catch(...) {
            }
        });
    }
    auto handle = std::make_unique<oak_device>();
    handle->dev = std::move(dev);
    *out = handle.release();
    return 0;
    OAK_CATCH(-1)
}

void oak_device_close(oak_device* dev) {
    OAK_TRY
    delete dev;
    OAK_CATCH_VOID
}

#define OAK_DEV(d) require((d) != nullptr && (d)->dev != nullptr, "device handle is NULL")

int oak_device_is_closed(oak_device* dev, int32_t* out) {
    OAK_TRY
    OAK_DEV(dev);
    require(out != nullptr, "out is NULL");
    *out = dev->dev->isClosed() ? 1 : 0;
    return 0;
    OAK_CATCH(-1)
}

int oak_device_id(oak_device* dev, char* buf, size_t cap) {
    OAK_TRY
    OAK_DEV(dev);
    copy_str(buf, cap, dev->dev->getDeviceId());
    return 0;
    OAK_CATCH(-1)
}

int oak_device_name(oak_device* dev, char* buf, size_t cap) {
    OAK_TRY
    OAK_DEV(dev);
    std::string name = dev->dev->getProductName();
    if(name.empty()) name = dev->dev->getDeviceName();
    copy_str(buf, cap, name);
    return 0;
    OAK_CATCH(-1)
}

int oak_device_usb_speed(oak_device* dev, int32_t* out) {
    OAK_TRY
    OAK_DEV(dev);
    require(out != nullptr, "out is NULL");
    *out = from_usb_speed(dev->dev->getUsbSpeed());
    return 0;
    OAK_CATCH(-1)
}

int oak_device_protocol(oak_device* dev, int32_t* out) {
    OAK_TRY
    OAK_DEV(dev);
    require(out != nullptr, "out is NULL");
    *out = map_protocol(dev->dev->getDeviceInfo().protocol);
    return 0;
    OAK_CATCH(-1)
}

int oak_device_platform(oak_device* dev, int32_t* out) {
    OAK_TRY
    OAK_DEV(dev);
    require(out != nullptr, "out is NULL");
    *out = map_platform(dev->dev->getPlatform());
    return 0;
    OAK_CATCH(-1)
}

int oak_device_camera_features(oak_device* dev, oak_camera_features* out, size_t cap, size_t* out_count) {
    OAK_TRY
    OAK_DEV(dev);
    require(out_count != nullptr, "out_count is NULL");
    require(out != nullptr || cap == 0, "out is NULL with cap > 0");
    auto feats = dev->dev->getConnectedCameraFeatures();
    *out_count = feats.size();
    for(size_t i = 0; i < feats.size() && i < cap; ++i) {
        const auto& f = feats[i];
        oak_camera_features cf{};
        cf.socket = from_socket(f.socket);
        cf.sensor_type = f.supportedTypes.empty() ? OAK_SENSOR_AUTO : from_sensor_type(f.supportedTypes.front());
        copy_str(cf.sensor_name, sizeof cf.sensor_name, f.sensorName);
        copy_str(cf.name, sizeof cf.name, f.name);
        cf.width = f.width;
        cf.height = f.height;
        cf.has_autofocus = f.hasAutofocus ? 1 : 0;
        cf.config_count = static_cast<int32_t>(f.configs.size());
        out[i] = cf;
    }
    return 0;
    OAK_CATCH(-1)
}

int oak_device_camera_configs(oak_device* dev, int32_t socket, oak_sensor_config* out, size_t cap, size_t* out_count) {
    OAK_TRY
    OAK_DEV(dev);
    require(out_count != nullptr, "out_count is NULL");
    require(out != nullptr || cap == 0, "out is NULL with cap > 0");
    auto feats = dev->dev->getConnectedCameraFeatures();
    for(const auto& f : feats) {
        if(from_socket(f.socket) != socket) continue;
        *out_count = f.configs.size();
        for(size_t i = 0; i < f.configs.size() && i < cap; ++i) {
            const auto& c = f.configs[i];
            oak_sensor_config sc{};
            sc.width = c.width;
            sc.height = c.height;
            sc.min_fps = c.minFps;
            sc.max_fps = c.maxFps;
            sc.sensor_type = from_sensor_type(c.type);
            out[i] = sc;
        }
        return 0;
    }
    throw OakError("no camera connected at that socket");
    OAK_CATCH(-1)
}

int oak_device_ir_drivers(oak_device* dev, int32_t* out_count) {
    OAK_TRY
    OAK_DEV(dev);
    require(out_count != nullptr, "out_count is NULL");
    *out_count = static_cast<int32_t>(dev->dev->getIrDrivers().size());
    return 0;
    OAK_CATCH(-1)
}

int oak_device_set_ir_dot(oak_device* dev, float intensity) {
    OAK_TRY
    OAK_DEV(dev);
    require(intensity >= 0.0f && intensity <= 1.0f, "IR intensity must be 0..1");
    require(dev->dev->setIrLaserDotProjectorIntensity(intensity), "setIrLaserDotProjectorIntensity failed (no IR driver?)");
    return 0;
    OAK_CATCH(-1)
}

int oak_device_set_ir_flood(oak_device* dev, float intensity) {
    OAK_TRY
    OAK_DEV(dev);
    require(intensity >= 0.0f && intensity <= 1.0f, "IR intensity must be 0..1");
    require(dev->dev->setIrFloodLightIntensity(intensity), "setIrFloodLightIntensity failed (no IR driver?)");
    return 0;
    OAK_CATCH(-1)
}

int oak_device_chip_temp(oak_device* dev, float* out_celsius) {
    OAK_TRY
    OAK_DEV(dev);
    require(out_celsius != nullptr, "out_celsius is NULL");
    *out_celsius = dev->dev->getChipTemperature().average;
    return 0;
    OAK_CATCH(-1)
}

// ===========================================================================
// Pipeline
// ===========================================================================

int oak_pipeline_new(oak_device* dev, oak_pipeline** out) {
    OAK_TRY
    OAK_DEV(dev);
    require(out != nullptr, "out is NULL");
    *out = nullptr;
    auto p = std::make_unique<oak_pipeline>();
    p->dev = dev->dev;
    p->pipeline = std::make_unique<dai::Pipeline>(dev->dev);
    *out = p.release();
    return 0;
    OAK_CATCH(-1)
}

void oak_pipeline_free(oak_pipeline* p) {
    if(!p) return;
    OAK_TRY
    if(p->pipeline && p->pipeline->isRunning()) p->pipeline->stop();
    OAK_CATCH_VOID
    OAK_TRY
    p->outputs.clear();
    p->cameras.clear();
    p->pipeline.reset();
    delete p;
    OAK_CATCH_VOID
}

int oak_pipeline_add_camera(oak_pipeline* p, int32_t socket, uint32_t sensor_w, uint32_t sensor_h, float sensor_fps, oak_camera** out) {
    OAK_TRY
    require(p != nullptr && p->pipeline != nullptr, "pipeline handle is NULL");
    require(out != nullptr, "out is NULL");
    *out = nullptr;
    std::optional<std::pair<uint32_t, uint32_t>> res;
    if(sensor_w > 0 && sensor_h > 0) res = std::make_pair(sensor_w, sensor_h);
    std::optional<float> fps;
    if(sensor_fps > 0.0f) fps = sensor_fps;
    auto node = p->pipeline->create<dai::node::Camera>()->build(to_socket(socket), res, fps);
    auto cam = std::make_unique<oak_camera>();
    cam->owner = p;
    cam->node = std::move(node);
    *out = cam.get();
    p->cameras.push_back(std::move(cam));
    return 0;
    OAK_CATCH(-1)
}

int oak_camera_request_output(oak_camera* cam, const oak_output_spec* spec, oak_output** out) {
    OAK_TRY
    require(cam != nullptr && cam->node != nullptr && cam->owner != nullptr, "camera handle is NULL");
    require(spec != nullptr, "spec is NULL");
    require(out != nullptr, "out is NULL");
    require(spec->width > 0 && spec->height > 0, "output width/height must be > 0");
    *out = nullptr;
    std::optional<float> fps;
    if(spec->fps > 0.0f) fps = spec->fps;
    dai::Node::Output* o = cam->node->requestOutput(
        std::make_pair(spec->width, spec->height), to_img_type(spec->type), to_resize_mode(spec->resize_mode), fps);
    require(o != nullptr, "requestOutput returned no output");
    auto h = std::make_unique<oak_output>();
    h->node = cam->node;
    h->out = o;
    *out = h.get();
    cam->owner->outputs.push_back(std::move(h));
    return 0;
    OAK_CATCH(-1)
}

int oak_camera_set_initial_control(oak_camera* cam, const oak_control_params* params) {
    OAK_TRY
    require(cam != nullptr && cam->node != nullptr, "camera handle is NULL");
    require(params != nullptr, "params is NULL");
    // Apply to a scratch copy first so a bad field leaves initialControl untouched.
    dai::CameraControl scratch = cam->node->initialControl;
    apply_control(scratch, *params);
    cam->node->initialControl = scratch;
    return 0;
    OAK_CATCH(-1)
}

int oak_camera_control_queue(oak_camera* cam, oak_control_queue** out) {
    OAK_TRY
    require(cam != nullptr && cam->node != nullptr, "camera handle is NULL");
    require(out != nullptr, "out is NULL");
    *out = nullptr;
    auto q = std::make_unique<oak_control_queue>();
    q->q = cam->node->inputControl.createInputQueue();
    require(q->q != nullptr, "createInputQueue returned NULL");
    *out = q.release();
    return 0;
    OAK_CATCH(-1)
}

int oak_camera_max_size(oak_camera* cam, uint32_t* out_w, uint32_t* out_h) {
    OAK_TRY
    require(cam != nullptr && cam->node != nullptr, "camera handle is NULL");
    require(out_w != nullptr && out_h != nullptr, "out_w/out_h is NULL");
    *out_w = cam->node->getMaxWidth();
    *out_h = cam->node->getMaxHeight();
    return 0;
    OAK_CATCH(-1)
}

int oak_output_create_queue(oak_output* out, uint32_t max_size, int32_t blocking, oak_queue** out_queue) {
    OAK_TRY
    require(out != nullptr && out->out != nullptr, "output handle is NULL");
    require(out_queue != nullptr, "out_queue is NULL");
    require(max_size > 0, "max_size must be > 0");
    *out_queue = nullptr;
    auto q = std::make_unique<oak_queue>();
    q->q = out->out->createOutputQueue(max_size, blocking != 0);
    require(q->q != nullptr, "createOutputQueue returned NULL");
    *out_queue = q.release();
    return 0;
    OAK_CATCH(-1)
}

int oak_pipeline_start(oak_pipeline* p) {
    OAK_TRY
    require(p != nullptr && p->pipeline != nullptr, "pipeline handle is NULL");
    p->pipeline->start();
    return 0;
    OAK_CATCH(-1)
}

int oak_pipeline_is_running(oak_pipeline* p, int32_t* out) {
    OAK_TRY
    require(p != nullptr && p->pipeline != nullptr, "pipeline handle is NULL");
    require(out != nullptr, "out is NULL");
    *out = p->pipeline->isRunning() ? 1 : 0;
    return 0;
    OAK_CATCH(-1)
}

int oak_pipeline_stop(oak_pipeline* p) {
    OAK_TRY
    require(p != nullptr && p->pipeline != nullptr, "pipeline handle is NULL");
    p->pipeline->stop();
    return 0;
    OAK_CATCH(-1)
}

// ===========================================================================
// Queues and frames
// ===========================================================================

namespace {
oak_frame* wrap_frame(std::shared_ptr<dai::ImgFrame> img) {
    auto f = std::make_unique<oak_frame>();
    f->img = std::move(img);
    return f.release();
}
}  // namespace

// timeout_ns < 0 (or > 10 years) waits indefinitely. Messages that are not ImgFrames are
// skipped (they count against the same deadline). A closed queue is an error
// (-1, "MessageQueue was closed"); oak_queue_is_closed tells it apart.
int oak_queue_get(oak_queue* q, int64_t timeout_ns, oak_frame** out) {
    OAK_TRY
    require(q != nullptr && q->q != nullptr, "queue handle is NULL");
    require(out != nullptr, "out is NULL");
    *out = nullptr;
    using clock = std::chrono::steady_clock;
    // Anything beyond ~10 years is "forever"; this also keeps now() + timeout
    // far from overflowing steady_clock's int64 nanosecond representation.
    constexpr int64_t kForeverNs = int64_t{10} * 365 * 24 * 3600 * 1000000000;
    const bool forever = timeout_ns < 0 || timeout_ns > kForeverNs;
    const auto deadline = clock::now() + std::chrono::nanoseconds(forever ? 0 : timeout_ns);
    for(;;) {
        std::shared_ptr<dai::ADatatype> msg;
        if(forever) {
            msg = q->q->get();
        } else {
            auto remaining = std::chrono::duration_cast<std::chrono::nanoseconds>(deadline - clock::now());
            if(remaining.count() < 0) remaining = std::chrono::nanoseconds(0);
            bool timed_out = false;
            msg = q->q->get(remaining, timed_out);
            if(timed_out) return 0;
        }
        if(auto img = std::dynamic_pointer_cast<dai::ImgFrame>(msg)) {
            *out = wrap_frame(std::move(img));
            return 1;
        }
        if(!forever && clock::now() >= deadline) return 0;
    }
    OAK_CATCH(-1)
}

int oak_queue_try_get(oak_queue* q, oak_frame** out) {
    OAK_TRY
    require(q != nullptr && q->q != nullptr, "queue handle is NULL");
    require(out != nullptr, "out is NULL");
    *out = nullptr;
    for(;;) {
        auto msg = q->q->tryGet();
        if(!msg) return 0;
        if(auto img = std::dynamic_pointer_cast<dai::ImgFrame>(msg)) {
            *out = wrap_frame(std::move(img));
            return 1;
        }
    }
    OAK_CATCH(-1)
}

int oak_queue_is_closed(oak_queue* q, int32_t* out) {
    OAK_TRY
    require(q != nullptr && q->q != nullptr, "queue handle is NULL");
    require(out != nullptr, "out is NULL");
    *out = q->q->isClosed() ? 1 : 0;
    return 0;
    OAK_CATCH(-1)
}

void oak_queue_free(oak_queue* q) {
    OAK_TRY
    delete q;
    OAK_CATCH_VOID
}

int oak_frame_get_view(const oak_frame* f, oak_frame_view* out) {
    OAK_TRY
    require(f != nullptr && f->img != nullptr, "frame handle is NULL");
    require(out != nullptr, "out is NULL");
    const dai::ImgFrame& img = *f->img;
    auto data = img.getData();
    const size_t p1 = img.fb.p1Offset;
    require(p1 <= data.size(), "ImgFrame plane 0 offset beyond data");

    oak_frame_view v{};
    v.data = data.data() + p1;
    v.len = data.size() - p1;
    v.width = img.getWidth();
    v.height = img.getHeight();
    v.type = from_img_type(img.getType());
    // getStride() already falls back to width * bytesPerPixel when fb.stride == 0.
    v.stride = img.getStride();
    if(v.stride == 0 && (v.type == OAK_IMG_NV12 || v.type == OAK_IMG_GRAY8)) v.stride = v.width;
    // getPlaneStride(0) = p2Offset - p1Offset, or stride * height when unset.
    v.plane2_offset = (v.type == OAK_IMG_NV12 || v.type == OAK_IMG_YUV420P) ? img.getPlaneStride(0) : 0;
    // getPlaneStride(1) = p3Offset - p2Offset (U plane size) or stride * height.
    v.plane3_offset = v.type == OAK_IMG_YUV420P ? v.plane2_offset + img.getPlaneStride(1) : 0;
    v.instance_num = static_cast<int32_t>(img.getInstanceNum());
    v.ts_host_ns = steady_ns(img.getTimestamp());
    v.ts_device_ns = steady_ns(img.getTimestampDevice());
    v.seq = img.getSequenceNum();
    v.exposure_us = static_cast<uint32_t>(img.getExposureTime().count());
    v.iso = static_cast<uint32_t>(img.getSensitivity());
    v.lens_pos = img.getLensPosition();
    v.color_temp_k = img.getColorTemperature();
    *out = v;
    return 0;
    OAK_CATCH(-1)
}

void oak_frame_release(oak_frame* f) {
    OAK_TRY
    delete f;
    OAK_CATCH_VOID
}

// ===========================================================================
// Controls
// ===========================================================================

int oak_control_send(oak_control_queue* q, const oak_control_params* params) {
    OAK_TRY
    require(q != nullptr && q->q != nullptr, "control queue handle is NULL");
    require(params != nullptr, "params is NULL");
    auto ctrl = std::make_shared<dai::CameraControl>();
    apply_control(*ctrl, *params);
    q->q->send(ctrl);
    return 0;
    OAK_CATCH(-1)
}

void oak_control_queue_free(oak_control_queue* q) {
    OAK_TRY
    delete q;
    OAK_CATCH_VOID
}

}  // extern "C"
