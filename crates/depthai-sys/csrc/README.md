# oak_dai shim

`oak_dai.h` is the C ABI oak-cam uses; `oak_dai.cpp` implements it against
depthai-core **v3.10.0**. The shim is the only code that touches C++. When
you upgrade depthai, diff the headers for the signatures below (paths under
`include/depthai/`), then rebuild: the `static_assert`s in `oak_dai.cpp`
catch enum renumbering.

## depthai-core v3.10.0 API relied on

Discovery and device (`device/DeviceBase.hpp`, `device/Device.hpp`, `xlink/XLinkConnection.hpp`)

- `static std::vector<DeviceInfo> DeviceBase::getAllAvailableDevices();`
- `struct DeviceInfo { std::string name, deviceId; XLinkDeviceState_t state; XLinkProtocol_t protocol; XLinkPlatform_t platform; XLinkError_t status; };`
- `explicit DeviceInfo::DeviceInfo(std::string deviceIdOrName);` (a `.` in the string means IP or USB path, otherwise device id)
- `std::string DeviceInfo::getDeviceId() const;`
- `DeviceBase::DeviceBase(UsbSpeed maxUsbSpeed);` and `DeviceBase(const DeviceInfo&, UsbSpeed maxUsbSpeed);` (inherited by `dai::Device`)
- `std::string DeviceBase::getDeviceId();`, `getProductName();`, `getDeviceName();`
- `DeviceInfo DeviceBase::getDeviceInfo() const;`
- `Platform DeviceBase::getPlatform() const;`, `enum class Platform { RVC2, RVC3, RVC4 };` (`device/Platform.hpp`)
- `UsbSpeed DeviceBase::getUsbSpeed();`
- `bool DeviceBase::isClosed() const;`
- `std::vector<CameraFeatures> DeviceBase::getConnectedCameraFeatures();`
- `std::vector<std::tuple<std::string, int, int>> DeviceBase::getIrDrivers();`
- `bool DeviceBase::setIrLaserDotProjectorIntensity(float intensity, int mask = -1);`
- `bool DeviceBase::setIrFloodLightIntensity(float intensity, int mask = -1);`
- `ChipTemperature DeviceBase::getChipTemperature();`, `struct ChipTemperature { float css, mss, upa, dss, average; };`
- `void DeviceBase::setLogLevel(LogLevel);`, `void DeviceBase::setLogOutputLevel(LogLevel);`
- `int DeviceBase::addLogCallback(const std::function<void(LogMessage)>&);`
- `struct LogMessage { std::string nodeIdName; LogLevel level; Timestamp time; size_t colorRangeStart, colorRangeEnd; std::string payload; };`

Enums (`common/*.hpp`, `log/LogLevel.hpp`, XLink `XLinkPublicDefines.h`)

- `enum class UsbSpeed : int32_t { UNKNOWN, LOW, FULL, HIGH, SUPER, SUPER_PLUS };`
- `enum class CameraBoardSocket : int32_t { AUTO = -1, CAM_A, CAM_B, CAM_C, CAM_D, ... };`
- `enum class CameraSensorType : int32_t { AUTO = -1, COLOR = 0, MONO = 1, TOF = 2, THERMAL = 3 };`
- `enum class LogLevel : int32_t { TRACE = 0, DEBUG, INFO, WARN, ERR, CRITICAL, OFF };`
- `XLinkProtocol_t { X_LINK_USB_VSC = 0, X_LINK_USB_CDC, X_LINK_PCIE, X_LINK_IPC, X_LINK_TCP_IP, X_LINK_LOCAL_SHDMEM, X_LINK_TCP_IP_OR_LOCAL_SHDMEM, X_LINK_USB_EP, ... }` (mapped by switch)
- `XLinkDeviceState_t { X_LINK_ANY_STATE = 0, X_LINK_BOOTED, X_LINK_UNBOOTED, X_LINK_BOOTLOADER, X_LINK_FLASH_BOOTED, X_LINK_GATE, X_LINK_GATE_BOOTED, X_LINK_GATE_SETUP }` (mapped by switch)
- `XLinkPlatform_t { X_LINK_ANY_PLATFORM = 0, X_LINK_MYRIAD_2 = 2450, X_LINK_MYRIAD_X = 2480, X_LINK_RVC3 = 3000, X_LINK_RVC4 = 4000 }` (mapped by switch)

Camera features (`common/CameraFeatures.hpp`)

- `struct CameraFeatures { CameraBoardSocket socket; std::string sensorName; int32_t width, height; ...; std::vector<CameraSensorType> supportedTypes; bool hasAutofocusIC, hasAutofocus; std::string name; ...; std::vector<CameraSensorConfig> configs; };`
- `struct CameraSensorConfig { int32_t width, height; float minFps, maxFps; Rect fov; CameraSensorType type; bool hdr, hfr; };`

Pipeline and Camera node (`pipeline/Pipeline.hpp`, `pipeline/node/Camera.hpp`, `pipeline/Node.hpp`)

- `explicit Pipeline::Pipeline(std::shared_ptr<Device> device);`
- `template <class N, typename... Args> std::shared_ptr<N> Pipeline::create(Args&&...);`
- `void Pipeline::start();`, `void Pipeline::stop();`, `bool Pipeline::isRunning() const;`
- `std::shared_ptr<Camera> Camera::build(CameraBoardSocket = AUTO, const std::optional<std::pair<uint32_t, uint32_t>>& sensorResolution = std::nullopt, std::optional<float> sensorFps = std::nullopt);`
- `Node::Output* Camera::requestOutput(const std::pair<uint32_t, uint32_t>& size, std::optional<ImgFrame::Type> type = std::nullopt, ImgResizeMode resizeMode = ImgResizeMode::CROP, std::optional<float> fps = std::nullopt, std::optional<bool> enableUndistortion = std::nullopt, std::optional<float> alphaScaling = std::nullopt);`
- `uint32_t Camera::getMaxWidth() const;`, `uint32_t Camera::getMaxHeight() const;`
- `CameraControl Camera::initialControl;`, `Node::Input Camera::inputControl;`
- `enum struct ImgResizeMode { CROP, STRETCH, LETTERBOX };` (`capabilities/ImgFrameCapability.hpp`)
- `std::shared_ptr<MessageQueue> Node::Output::createOutputQueue(unsigned int maxSize = 16, bool blocking = false);`
- `std::shared_ptr<InputQueue> Node::Input::createInputQueue(unsigned int maxSize = 16, bool blocking = false);`

Queues (`pipeline/MessageQueue.hpp`, `pipeline/InputQueue.hpp`)

- `std::shared_ptr<ADatatype> MessageQueue::get();` (blocking; throws `QueueException` when closed)
- `template <typename Rep, typename Period> std::shared_ptr<ADatatype> MessageQueue::get(std::chrono::duration<Rep, Period> timeout, bool& hasTimedout);`
- `std::shared_ptr<ADatatype> MessageQueue::tryGet();`
- `bool MessageQueue::isClosed() const;`
- `void InputQueue::send(const std::shared_ptr<ADatatype>& msg);`

Frames (`pipeline/datatype/ImgFrame.hpp`, `pipeline/datatype/Buffer.hpp`)

- `span<const uint8_t> Buffer::getData() const;`
- `steady_clock::time_point Buffer::getTimestamp() const;`, `getTimestampDevice() const;`
- `int64_t Buffer::getSequenceNum() const;`
- `unsigned int ImgFrame::getWidth() const;`, `getHeight() const;`, `getInstanceNum() const;`
- `unsigned int ImgFrame::getStride() const;` (falls back to `width * bytesPerPixel` when `fb.stride == 0`)
- `unsigned int ImgFrame::getPlaneStride(int planeIndex = 0) const;` (`p2Offset - p1Offset`, or `stride * height`)
- `ImgFrame::Specs ImgFrame::fb;` (public; `p1Offset` used as the plane-0 start)
- `ImgFrame::Type ImgFrame::getType() const;` (`YUV420p, RGB888i, BGR888i, RAW8, NV12, GRAY8, NONE, ...`; mapped by switch, the header's numbers differ)
- `std::chrono::microseconds ImgFrame::getExposureTime() const;`
- `int ImgFrame::getSensitivity() const;`, `int getColorTemperature() const;`, `int getLensPosition() const;`

Controls (`pipeline/datatype/CameraControl.hpp`)

- `enum class AutoFocusMode : uint8_t { OFF = 0, AUTO, MACRO, CONTINUOUS_VIDEO, CONTINUOUS_PICTURE, EDOF };`
- `enum class AutoWhiteBalanceMode : uint8_t { OFF = 0, AUTO, INCANDESCENT, FLUORESCENT, WARM_FLUORESCENT, DAYLIGHT, CLOUDY_DAYLIGHT, TWILIGHT, SHADE };`
- `enum class AntiBandingMode : uint8_t { OFF = 0, MAINS_50_HZ, MAINS_60_HZ, AUTO };`
- `setAutoExposureEnable()`, `setManualExposure(uint32_t exposureTimeUs, uint32_t sensitivityIso)`, `setAutoExposureCompensation(int)`, `setAutoExposureLock(bool)`, `setAutoExposureLimit(uint32_t maxExposureTimeUs)`, `setAutoExposureMaxISO(uint32_t)`, `setAntiBandingMode(AntiBandingMode)`
- `setAutoFocusMode(AutoFocusMode)`, `setManualFocus(uint8_t)`, `setAutoFocusLensRange(int infinityPosition, int macroPosition)`, `setAutoFocusTrigger()`
- `setAutoWhiteBalanceMode(AutoWhiteBalanceMode)`, `setManualWhiteBalance(int colorTemperatureK)`, `setAutoWhiteBalanceLock(bool)`
- `setBrightness(int)`, `setContrast(int)`, `setSaturation(int)`, `setSharpness(int)`, `setLumaDenoise(int)`, `setChromaDenoise(int)` (all return `CameraControl&`)

Version (`build/version.hpp`, generated at install)

- `constexpr static const char* dai::build::VERSION;`

## Deliberately not used

- `dai::Logging` (the host spdlog logger) is private to libdepthai-core in
  v3.10 (`src/utility/Logging.hpp` is not installed). `oak_set_log_level`
  therefore sets `DEPTHAI_LEVEL` (read once when depthai initialises) unless
  the user already set it, and applies the level to devices opened later via
  `DeviceBase::setLogLevel`. Host-side depthai messages still go to stdout
  through spdlog; only device logs reach `oak_set_log_callback`. The
  `depthai` crate's `redirect_host_logs_to_stderr()` moves fd 1 onto fd 2.
- `ImgFrame::getPlaneStride(1)` (`p3Offset - p2Offset`) gives the YUV420P
  U-plane size; `plane3_offset = plane2_offset + getPlaneStride(1)`.
