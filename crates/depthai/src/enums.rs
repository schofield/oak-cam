//! Rust mirrors of the `OAK_*` integer enums in `oak_dai.h`.

use std::fmt;

use depthai_sys as sys;

/// Defines a fieldless enum with a fallback variant carrying unknown raw values,
/// plus `from_raw` / `to_raw` / `as_str` and `Display`.
macro_rules! oak_enum {
    (
        $(#[$meta:meta])*
        $name:ident, fallback = $fallback:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $raw:path => $text:literal, )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )*
            /// A value this build does not know (raw value kept).
            $fallback(i32),
        }

        impl $name {
            /// Every named variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            /// Converts the C constant; unknown values map to the fallback variant.
            pub fn from_raw(raw: i32) -> Self {
                match raw {
                    $( $raw => Self::$variant, )*
                    other => Self::$fallback(other),
                }
            }

            /// The C constant for this value.
            pub fn to_raw(self) -> i32 {
                match self {
                    $( Self::$variant => $raw, )*
                    Self::$fallback(raw) => raw,
                }
            }

            /// Short upper-case name as depthai spells it.
            pub fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $text, )*
                    Self::$fallback(_) => "UNKNOWN",
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    Self::$fallback(raw) => write!(f, "UNKNOWN({raw})"),
                    other => f.write_str(other.as_str()),
                }
            }
        }
    };
}

oak_enum! {
    /// How a device is attached.
    Protocol, fallback = Other {
        /// Transport not reported.
        Unknown = sys::OAK_PROTOCOL_UNKNOWN => "UNKNOWN",
        /// USB (VSC/CDC/EP).
        Usb = sys::OAK_PROTOCOL_USB => "USB",
        /// Ethernet (PoE).
        TcpIp = sys::OAK_PROTOCOL_TCP_IP => "TCP_IP",
    }
}

oak_enum! {
    /// XLink device state at discovery time.
    DeviceState, fallback = Other {
        /// State not reported.
        Unknown = sys::OAK_STATE_UNKNOWN => "UNKNOWN",
        /// Waiting for a USB boot.
        Unbooted = sys::OAK_STATE_UNBOOTED => "UNBOOTED",
        /// In the bootloader, waiting for a connection.
        Bootloader = sys::OAK_STATE_BOOTLOADER => "BOOTLOADER",
        /// Booted and in use by a host.
        Booted = sys::OAK_STATE_BOOTED => "BOOTED",
        /// Running flashed firmware (standalone / non-exclusive).
        FlashBooted = sys::OAK_STATE_FLASH_BOOTED => "FLASH_BOOTED",
        /// Running the Gate service (RVC4).
        Gate = sys::OAK_STATE_GATE => "GATE",
    }
}

oak_enum! {
    /// Device silicon platform.
    Platform, fallback = Other {
        /// Platform not reported.
        Unknown = sys::OAK_PLATFORM_UNKNOWN => "UNKNOWN",
        /// Myriad X (OAK-D, OAK-D Pro).
        Rvc2 = sys::OAK_PLATFORM_RVC2 => "RVC2",
        /// Keem Bay.
        Rvc3 = sys::OAK_PLATFORM_RVC3 => "RVC3",
        /// Qualcomm (OAK 4).
        Rvc4 = sys::OAK_PLATFORM_RVC4 => "RVC4",
    }
}

oak_enum! {
    /// USB link speed (`dai::UsbSpeed`).
    UsbSpeed, fallback = Other {
        /// Not USB, or not reported.
        Unknown = sys::OAK_USB_UNKNOWN => "UNKNOWN",
        /// USB 1 low speed.
        Low = sys::OAK_USB_LOW => "LOW",
        /// USB 1 full speed.
        Full = sys::OAK_USB_FULL => "FULL",
        /// USB 2 (480 Mbps).
        High = sys::OAK_USB_HIGH => "HIGH",
        /// USB 3 (5 Gbps).
        Super = sys::OAK_USB_SUPER => "SUPER",
        /// USB 3.1+ (10 Gbps); as a limit it means "no limit".
        SuperPlus = sys::OAK_USB_SUPER_PLUS => "SUPER_PLUS",
    }
}

oak_enum! {
    /// Camera board socket (`dai::CameraBoardSocket`).
    Socket, fallback = Other {
        /// Let depthai choose.
        Auto = sys::OAK_SOCKET_AUTO => "AUTO",
        /// Socket A (RGB on OAK-D).
        CamA = sys::OAK_SOCKET_CAM_A => "CAM_A",
        /// Socket B (left mono on OAK-D).
        CamB = sys::OAK_SOCKET_CAM_B => "CAM_B",
        /// Socket C (right mono on OAK-D).
        CamC = sys::OAK_SOCKET_CAM_C => "CAM_C",
        /// Socket D.
        CamD = sys::OAK_SOCKET_CAM_D => "CAM_D",
    }
}

oak_enum! {
    /// Sensor kind (`dai::CameraSensorType`).
    SensorType, fallback = Other {
        /// Unspecified.
        Auto = sys::OAK_SENSOR_AUTO => "AUTO",
        /// Colour sensor.
        Color = sys::OAK_SENSOR_COLOR => "COLOR",
        /// Monochrome sensor.
        Mono = sys::OAK_SENSOR_MONO => "MONO",
        /// Time-of-flight sensor.
        Tof = sys::OAK_SENSOR_TOF => "TOF",
        /// Thermal sensor.
        Thermal = sys::OAK_SENSOR_THERMAL => "THERMAL",
    }
}

oak_enum! {
    /// Image pixel format (subset of `dai::ImgFrame::Type`).
    ImgType, fallback = Unmapped {
        /// Let depthai choose / not set.
        None = sys::OAK_IMG_NONE => "NONE",
        /// 4:2:0 semi-planar (Y plane, interleaved UV plane).
        Nv12 = sys::OAK_IMG_NV12 => "NV12",
        /// 8-bit grayscale.
        Gray8 = sys::OAK_IMG_GRAY8 => "GRAY8",
        /// 4:2:0 planar.
        Yuv420p = sys::OAK_IMG_YUV420P => "YUV420p",
        /// 8-bit raw Bayer.
        Raw8 = sys::OAK_IMG_RAW8 => "RAW8",
        /// Interleaved 8-bit BGR.
        Bgr888i = sys::OAK_IMG_BGR888I => "BGR888i",
        /// Interleaved 8-bit RGB.
        Rgb888i = sys::OAK_IMG_RGB888I => "RGB888i",
        /// Any other depthai type.
        Other = sys::OAK_IMG_OTHER => "OTHER",
    }
}

oak_enum! {
    /// depthai log severity (`dai::LogLevel`, spdlog levels).
    LogLevel, fallback = Other {
        /// Trace.
        Trace = sys::OAK_LOG_TRACE => "TRACE",
        /// Debug.
        Debug = sys::OAK_LOG_DEBUG => "DEBUG",
        /// Info.
        Info = sys::OAK_LOG_INFO => "INFO",
        /// Warning.
        Warn = sys::OAK_LOG_WARN => "WARN",
        /// Error.
        Err = sys::OAK_LOG_ERR => "ERR",
        /// Critical.
        Critical = sys::OAK_LOG_CRITICAL => "CRITICAL",
        /// Logging disabled.
        Off = sys::OAK_LOG_OFF => "OFF",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! round_trip {
        ($($ty:ident),*) => {$(
            for &v in $ty::ALL {
                assert_eq!($ty::from_raw(v.to_raw()), v, "{} {v}", stringify!($ty));
            }
            assert_eq!($ty::from_raw(4242).to_raw(), 4242);
            assert_eq!($ty::from_raw(4242).as_str(), "UNKNOWN");
        )*};
    }

    #[test]
    fn enums_round_trip() {
        round_trip!(
            Protocol,
            DeviceState,
            Platform,
            UsbSpeed,
            Socket,
            SensorType,
            ImgType,
            LogLevel
        );
    }

    #[test]
    fn raw_values_match_header() {
        assert_eq!(Protocol::TcpIp.to_raw(), 2);
        assert_eq!(Platform::Rvc2.to_raw(), 2);
        assert_eq!(UsbSpeed::Super.to_raw(), 4);
        assert_eq!(Socket::Auto.to_raw(), -1);
        assert_eq!(Socket::CamC.to_raw(), 2);
        assert_eq!(ImgType::Other.to_raw(), 100);
        assert_eq!(LogLevel::Off.to_raw(), 6);
    }

    #[test]
    fn display() {
        assert_eq!(Protocol::Usb.to_string(), "USB");
        assert_eq!(Socket::CamA.to_string(), "CAM_A");
        assert_eq!(Socket::from_raw(7).to_string(), "UNKNOWN(7)");
        assert_eq!(ImgType::Nv12.to_string(), "NV12");
    }
}
