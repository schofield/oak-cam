/*
 * oak_dai.h — a small C ABI over depthai-core v3 for oak-cam.
 *
 * Conventions
 *   - Every function returns int: 0 on success, -1 on error. Poll-style
 *     functions return 1 when they produced a value, 0 on timeout, -1 on error.
 *   - On -1 the reason is in oak_last_error() for the calling thread until the
 *     next call on that thread that fails (or oak_clear_error()).
 *   - No C++ exception ever leaves this library: every entry point catches.
 *   - Handles (oak_device, oak_pipeline, ...) are opaque and must be freed with
 *     the matching *_free / *_close. Freeing NULL is a no-op.
 *   - Out-parameters come last. Strings written into caller buffers are always
 *     NUL-terminated and truncated to fit.
 *   - Enum values below are the only ones crossing the boundary; the .cpp maps
 *     them to dai:: enums with switch statements (and static_asserts where a
 *     numeric identity is relied on), so a depthai change breaks the build,
 *     not the runtime.
 *
 * Threading: a handle may be used from any thread but not concurrently from
 * two threads, except oak_queue_* (one consumer per queue) and
 * oak_control_send (serialised by depthai). oak_frame_* are read-only and may
 * be used from any thread; oak_frame_release from any thread.
 */
#ifndef OAK_DAI_H
#define OAK_DAI_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(_WIN32)
#define OAK_API __declspec(dllexport)
#else
#define OAK_API __attribute__((visibility("default")))
#endif

/* ------------------------------------------------------------------------ */
/* Opaque handles                                                           */
/* ------------------------------------------------------------------------ */
typedef struct oak_device oak_device;
typedef struct oak_pipeline oak_pipeline;
typedef struct oak_camera oak_camera;         /* owned by the pipeline */
typedef struct oak_output oak_output;         /* owned by the pipeline */
typedef struct oak_queue oak_queue;           /* output queue */
typedef struct oak_control_queue oak_control_queue;
typedef struct oak_frame oak_frame;

/* ------------------------------------------------------------------------ */
/* Enums (plain ints across the boundary)                                    */
/* ------------------------------------------------------------------------ */
enum {
    OAK_PROTOCOL_UNKNOWN = 0,
    OAK_PROTOCOL_USB = 1,
    OAK_PROTOCOL_TCP_IP = 2,
};

enum {
    OAK_STATE_UNKNOWN = 0,
    OAK_STATE_UNBOOTED = 1,
    OAK_STATE_BOOTLOADER = 2,
    OAK_STATE_BOOTED = 3,
    OAK_STATE_FLASH_BOOTED = 4,
    OAK_STATE_GATE = 5,
};

enum {
    OAK_PLATFORM_UNKNOWN = 0,
    OAK_PLATFORM_RVC2 = 2,
    OAK_PLATFORM_RVC3 = 3,
    OAK_PLATFORM_RVC4 = 4,
};

/* dai::UsbSpeed */
enum {
    OAK_USB_UNKNOWN = 0,
    OAK_USB_LOW = 1,
    OAK_USB_FULL = 2,
    OAK_USB_HIGH = 3,       /* USB 2 */
    OAK_USB_SUPER = 4,      /* USB 3 */
    OAK_USB_SUPER_PLUS = 5,
};

/* dai::CameraBoardSocket */
enum {
    OAK_SOCKET_AUTO = -1,
    OAK_SOCKET_CAM_A = 0,
    OAK_SOCKET_CAM_B = 1,
    OAK_SOCKET_CAM_C = 2,
    OAK_SOCKET_CAM_D = 3,
};

/* dai::CameraSensorType */
enum {
    OAK_SENSOR_AUTO = -1,
    OAK_SENSOR_COLOR = 0,
    OAK_SENSOR_MONO = 1,
    OAK_SENSOR_TOF = 2,
    OAK_SENSOR_THERMAL = 3,
};

/* dai::ImgFrame::Type (only the ones oak-cam requests or expects back). */
enum {
    OAK_IMG_NONE = 0,
    OAK_IMG_NV12 = 1,
    OAK_IMG_GRAY8 = 2,
    OAK_IMG_YUV420P = 3,
    OAK_IMG_RAW8 = 4,
    OAK_IMG_BGR888I = 5,
    OAK_IMG_RGB888I = 6,
    OAK_IMG_OTHER = 100,
};

/* dai::ImgResizeMode */
enum {
    OAK_RESIZE_CROP = 0,
    OAK_RESIZE_STRETCH = 1,
    OAK_RESIZE_LETTERBOX = 2,
};

/* dai::CameraControl::AutoFocusMode */
enum {
    OAK_AF_OFF = 0,
    OAK_AF_AUTO = 1,
    OAK_AF_MACRO = 2,
    OAK_AF_CONTINUOUS_VIDEO = 3,
    OAK_AF_CONTINUOUS_PICTURE = 4,
    OAK_AF_EDOF = 5,
};

/* dai::CameraControl::AutoWhiteBalanceMode */
enum {
    OAK_AWB_OFF = 0,
    OAK_AWB_AUTO = 1,
    OAK_AWB_INCANDESCENT = 2,
    OAK_AWB_FLUORESCENT = 3,
    OAK_AWB_WARM_FLUORESCENT = 4,
    OAK_AWB_DAYLIGHT = 5,
    OAK_AWB_CLOUDY_DAYLIGHT = 6,
    OAK_AWB_TWILIGHT = 7,
    OAK_AWB_SHADE = 8,
};

/* dai::CameraControl::AntiBandingMode */
enum {
    OAK_ANTIBANDING_OFF = 0,
    OAK_ANTIBANDING_50HZ = 1,
    OAK_ANTIBANDING_60HZ = 2,
    OAK_ANTIBANDING_AUTO = 3,
};

/* Log levels (dai::LogLevel) */
enum {
    OAK_LOG_TRACE = 0,
    OAK_LOG_DEBUG = 1,
    OAK_LOG_INFO = 2,
    OAK_LOG_WARN = 3,
    OAK_LOG_ERR = 4,
    OAK_LOG_CRITICAL = 5,
    OAK_LOG_OFF = 6,
};

/* ------------------------------------------------------------------------ */
/* POD structs                                                              */
/* ------------------------------------------------------------------------ */

/* One entry from device discovery. */
typedef struct oak_device_info {
    char name[64];       /* USB path or IP address as depthai reports it */
    char device_id[32];  /* MxId / device id */
    int32_t protocol;    /* OAK_PROTOCOL_* */
    int32_t state;       /* OAK_STATE_* */
    int32_t platform;    /* OAK_PLATFORM_* */
    int32_t _pad;
} oak_device_info;

/* One connected camera sensor. */
typedef struct oak_camera_features {
    int32_t socket;        /* OAK_SOCKET_* */
    int32_t sensor_type;   /* OAK_SENSOR_* (first supported type) */
    char sensor_name[32];  /* "IMX378", "OV9282" */
    char name[32];         /* alias, e.g. "rgb", "left", "right" (may be empty) */
    int32_t width;         /* max sensor resolution */
    int32_t height;
    int32_t has_autofocus;
    int32_t config_count;  /* number of sensor configs (see oak_device_camera_configs) */
} oak_camera_features;

/* One sensor config (resolution + fps range). */
typedef struct oak_sensor_config {
    int32_t width;
    int32_t height;
    float min_fps;
    float max_fps;
    int32_t sensor_type;   /* OAK_SENSOR_* */
    int32_t _pad;
} oak_sensor_config;

/* What to ask a Camera node for. */
typedef struct oak_output_spec {
    uint32_t width;
    uint32_t height;
    int32_t type;          /* OAK_IMG_* (OAK_IMG_NONE lets depthai choose) */
    int32_t resize_mode;   /* OAK_RESIZE_* */
    float fps;             /* <= 0: let depthai choose */
    uint32_t _pad;
} oak_output_spec;

/* A borrowed view of an ImgFrame. Valid until oak_frame_release. */
typedef struct oak_frame_view {
    const uint8_t* data;
    size_t len;
    uint32_t width;
    uint32_t height;
    uint32_t stride;          /* bytes per row of plane 0 */
    uint32_t plane2_offset;   /* byte offset of plane 1 (NV12 UV, YUV420P U); 0 if single-plane */
    uint32_t plane3_offset;   /* byte offset of plane 2 (YUV420P V); 0 otherwise */
    int32_t type;             /* OAK_IMG_* */
    int32_t instance_num;     /* camera instance the frame came from */
    int32_t _pad;
    int64_t ts_host_ns;       /* steady_clock ns, host-synchronised */
    int64_t ts_device_ns;     /* device monotonic ns */
    int64_t seq;              /* sequence number */
    uint32_t exposure_us;
    uint32_t iso;
    int32_t lens_pos;         /* lens position as reported (0 for fixed-focus cameras) */
    int32_t color_temp_k;
} oak_frame_view;

/* Bits of oak_control_params.set: only fields whose bit is set are applied. */
enum {
    OAK_CTL_AE_AUTO          = 1ull << 0,   /* setAutoExposureEnable() */
    OAK_CTL_MANUAL_EXPOSURE  = 1ull << 1,   /* setManualExposure(exposure_us, iso) */
    OAK_CTL_AE_COMPENSATION  = 1ull << 2,
    OAK_CTL_AE_LOCK          = 1ull << 3,
    OAK_CTL_AE_LIMIT         = 1ull << 4,   /* setAutoExposureLimit(ae_limit_us) */
    OAK_CTL_AE_MAX_ISO       = 1ull << 5,
    OAK_CTL_ANTIBANDING      = 1ull << 6,
    OAK_CTL_AF_MODE          = 1ull << 7,
    OAK_CTL_MANUAL_FOCUS     = 1ull << 8,   /* setManualFocus(lens_pos) */
    OAK_CTL_AF_TRIGGER       = 1ull << 9,
    OAK_CTL_AWB_MODE         = 1ull << 10,
    OAK_CTL_MANUAL_WB        = 1ull << 11,  /* setManualWhiteBalance(wb_kelvin) */
    OAK_CTL_AWB_LOCK         = 1ull << 12,
    OAK_CTL_BRIGHTNESS       = 1ull << 13,
    OAK_CTL_CONTRAST         = 1ull << 14,
    OAK_CTL_SATURATION       = 1ull << 15,
    OAK_CTL_SHARPNESS        = 1ull << 16,
    OAK_CTL_LUMA_DENOISE     = 1ull << 17,
    OAK_CTL_CHROMA_DENOISE   = 1ull << 18,
    OAK_CTL_AF_LENS_RANGE    = 1ull << 19,  /* setAutoFocusLensRange(af_infinity, af_macro) */
};

/* A CameraControl, built on the C++ side from the fields whose bit is set. */
typedef struct oak_control_params {
    uint64_t set;
    uint32_t exposure_us;
    uint32_t iso;
    int32_t ae_compensation;   /* -9..9 */
    int32_t ae_lock;           /* 0/1 */
    uint32_t ae_limit_us;
    uint32_t ae_max_iso;
    int32_t antibanding;       /* OAK_ANTIBANDING_* */
    int32_t af_mode;           /* OAK_AF_* */
    int32_t lens_pos;          /* 0..255 */
    int32_t af_infinity;       /* 0..255 */
    int32_t af_macro;          /* 0..255 */
    int32_t awb_mode;          /* OAK_AWB_* */
    int32_t wb_kelvin;         /* 1000..12000 */
    int32_t awb_lock;          /* 0/1 */
    int32_t brightness;        /* -10..10 */
    int32_t contrast;          /* -10..10 */
    int32_t saturation;        /* -10..10 */
    int32_t sharpness;         /* 0..4 */
    int32_t luma_denoise;      /* 0..4 */
    int32_t chroma_denoise;    /* 0..4 */
    int32_t _pad;
} oak_control_params;

/* Log callback: called from depthai threads. Must not throw or block. */
typedef void (*oak_log_fn)(void* user, int32_t level, const char* node, const char* message);

/* ------------------------------------------------------------------------ */
/* Global                                                                   */
/* ------------------------------------------------------------------------ */

/* depthai-core version string, e.g. "3.10.0". Static storage. */
OAK_API const char* oak_version(void);

/* Last error message for this thread ("" if none). Static per-thread storage. */
OAK_API const char* oak_last_error(void);
OAK_API void oak_clear_error(void);

/* std::chrono::steady_clock::now() in ns: the clock ImgFrame host timestamps use. */
OAK_API int oak_steady_now_ns(int64_t* out_ns);

/* Library-wide log level (OAK_LOG_*). */
OAK_API int oak_set_log_level(int32_t level);

/* Sets environment variable `key` to `value` unless it is already set, through
 * the C runtime (on Windows the CRT's copy, which is what depthai reads). Call
 * before other threads use the environment. Returns 0, or -1 on error. */
OAK_API int oak_env_default(const char* key, const char* value);

/* Install a log callback applied to every device opened afterwards. NULL clears.
 * Only device (firmware) logs are delivered; depthai's host logger writes to
 * stdout (fd 1) and cannot be hooked. `user` must stay valid for as long as
 * any device opened while this callback was set is alive: fn/user are copied
 * under a lock and then called outside it, so clearing or replacing the
 * callback does not wait for in-flight calls. */
OAK_API void oak_set_log_callback(oak_log_fn fn, void* user);

/* ------------------------------------------------------------------------ */
/* Discovery                                                                */
/* ------------------------------------------------------------------------ */

/* Lists every available device (USB and PoE). Writes up to `cap` entries and
 * the total count found to *out_count (may exceed cap). */
OAK_API int oak_list_devices(oak_device_info* out, size_t cap, size_t* out_count);

/* ------------------------------------------------------------------------ */
/* Device                                                                   */
/* ------------------------------------------------------------------------ */

/* Opens a device by id (MxId), by IP address, or "" for any. `max_usb_speed`
 * is OAK_USB_* (OAK_USB_SUPER_PLUS for no limit). */
OAK_API int oak_device_open(const char* id_or_ip, int32_t max_usb_speed, oak_device** out);

/* Frees this device handle. A pipeline built on the device holds its own
 * shared_ptr<dai::Device>, so the connection stays open until the last such
 * pipeline is freed too; closing the handle before the pipeline (and its
 * queues) is allowed. Once the pipeline stops, its output queues report
 * closed (oak_queue_get fails, oak_queue_is_closed gives 1). */
OAK_API void oak_device_close(oak_device* dev);

OAK_API int oak_device_is_closed(oak_device* dev, int32_t* out);
OAK_API int oak_device_id(oak_device* dev, char* buf, size_t cap);
OAK_API int oak_device_name(oak_device* dev, char* buf, size_t cap);       /* product name */
OAK_API int oak_device_usb_speed(oak_device* dev, int32_t* out);          /* OAK_USB_* */
OAK_API int oak_device_protocol(oak_device* dev, int32_t* out);           /* OAK_PROTOCOL_* */
OAK_API int oak_device_platform(oak_device* dev, int32_t* out);           /* OAK_PLATFORM_* */
OAK_API int oak_device_camera_features(oak_device* dev, oak_camera_features* out, size_t cap, size_t* out_count);
/* Sensor configs of the camera at `socket`. */
OAK_API int oak_device_camera_configs(oak_device* dev, int32_t socket, oak_sensor_config* out, size_t cap, size_t* out_count);
OAK_API int oak_device_ir_drivers(oak_device* dev, int32_t* out_count);
OAK_API int oak_device_set_ir_dot(oak_device* dev, float intensity);      /* 0..1 */
OAK_API int oak_device_set_ir_flood(oak_device* dev, float intensity);    /* 0..1 */
OAK_API int oak_device_chip_temp(oak_device* dev, float* out_celsius);

/* ------------------------------------------------------------------------ */
/* Pipeline (build everything, then start)                                  */
/* ------------------------------------------------------------------------ */

OAK_API int oak_pipeline_new(oak_device* dev, oak_pipeline** out);
/* Stops if running, then frees. Output/control queues must be freed first. */
OAK_API void oak_pipeline_free(oak_pipeline* p);

/* Adds a Camera node. sensor_w/h/fps <= 0 let depthai pick from the outputs. */
OAK_API int oak_pipeline_add_camera(oak_pipeline* p, int32_t socket, uint32_t sensor_w, uint32_t sensor_h, float sensor_fps, oak_camera** out);
OAK_API int oak_camera_request_output(oak_camera* cam, const oak_output_spec* spec, oak_output** out);
OAK_API int oak_camera_set_initial_control(oak_camera* cam, const oak_control_params* params);
OAK_API int oak_camera_control_queue(oak_camera* cam, oak_control_queue** out);
OAK_API int oak_camera_max_size(oak_camera* cam, uint32_t* out_w, uint32_t* out_h);

/* Creates an output queue on `out` (must be called before start). */
OAK_API int oak_output_create_queue(oak_output* out, uint32_t max_size, int32_t blocking, oak_queue** out_queue);

OAK_API int oak_pipeline_start(oak_pipeline* p);
OAK_API int oak_pipeline_is_running(oak_pipeline* p, int32_t* out);
OAK_API int oak_pipeline_stop(oak_pipeline* p);

/* ------------------------------------------------------------------------ */
/* Queues and frames                                                        */
/* ------------------------------------------------------------------------ */

/* Waits up to timeout_ns for an ImgFrame. 1 = got one (*out set), 0 = timeout. */
OAK_API int oak_queue_get(oak_queue* q, int64_t timeout_ns, oak_frame** out);
OAK_API int oak_queue_try_get(oak_queue* q, oak_frame** out);
OAK_API int oak_queue_is_closed(oak_queue* q, int32_t* out);
OAK_API void oak_queue_free(oak_queue* q);

OAK_API int oak_frame_get_view(const oak_frame* f, oak_frame_view* out);
OAK_API void oak_frame_release(oak_frame* f);

/* ------------------------------------------------------------------------ */
/* Controls                                                                 */
/* ------------------------------------------------------------------------ */

OAK_API int oak_control_send(oak_control_queue* q, const oak_control_params* params);
OAK_API void oak_control_queue_free(oak_control_queue* q);

#ifdef __cplusplus
}
#endif

#endif /* OAK_DAI_H */
