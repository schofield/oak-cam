//! The three cameras oak-cam publishes, their names, sensor kinds, advertised
//! sizes and (macOS) the pinned CoreMediaIO UUIDs.

use std::fmt;
use std::str::FromStr;

/// One published camera, one per OAK sensor socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CameraId {
    /// CAM_A, IMX378 colour sensor.
    Rgb = 0,
    /// CAM_B, OV9282 mono (left).
    Left = 1,
    /// CAM_C, OV9282 mono (right).
    Right = 2,
}

/// Sensor kind: picks the formats a camera advertises and publishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CameraKind {
    /// Colour; NV12 from the device.
    Rgb,
    /// Mono; GRAY8 from the device, published as NV12 with neutral chroma.
    Mono,
}

/// A frame size in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Resolution {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Resolution {
    /// `width` x `height`.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

const RGB_FORMATS: [Resolution; 3] = [
    Resolution::new(1920, 1080),
    Resolution::new(1280, 720),
    Resolution::new(3840, 2160),
];

const MONO_FORMATS: [Resolution; 3] = [
    Resolution::new(1280, 800),
    Resolution::new(1280, 720),
    Resolution::new(640, 400),
];

impl CameraKind {
    /// The NV12 sizes this kind advertises, default first. The macOS
    /// extension's `FormatTable.entries(for:)` lists the same NV12 sizes.
    #[must_use]
    pub const fn formats(self) -> &'static [Resolution] {
        match self {
            CameraKind::Rgb => &RGB_FORMATS,
            CameraKind::Mono => &MONO_FORMATS,
        }
    }

    /// The default size: 1920x1080 for RGB, 1280x800 for mono.
    #[must_use]
    pub const fn default_resolution(self) -> Resolution {
        self.formats()[0]
    }

    /// `rgb` / `mono`, the `Kind` string in the extension's Info.plist.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CameraKind::Rgb => "rgb",
            CameraKind::Mono => "mono",
        }
    }
}

impl CameraId {
    /// All cameras in socket order.
    pub const ALL: [CameraId; 3] = [CameraId::Rgb, CameraId::Left, CameraId::Right];

    /// The camera name other apps show.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            CameraId::Rgb => "OAK RGB",
            CameraId::Left => "OAK Left",
            CameraId::Right => "OAK Right",
        }
    }

    /// Short lowercase id used on the command line and in logs.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            CameraId::Rgb => "rgb",
            CameraId::Left => "left",
            CameraId::Right => "right",
        }
    }

    /// Colour or mono.
    #[must_use]
    pub const fn kind(self) -> CameraKind {
        match self {
            CameraId::Rgb => CameraKind::Rgb,
            CameraId::Left | CameraId::Right => CameraKind::Mono,
        }
    }

    /// Index 0..3 (the enum discriminant).
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The pinned CoreMediaIO UUIDs of this camera's device and streams.
    #[must_use]
    pub const fn uuids(self) -> &'static CameraUuids {
        &CAMERAS[self.index()]
    }
}

impl fmt::Display for CameraId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for CameraId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "rgb" => Ok(CameraId::Rgb),
            "left" => Ok(CameraId::Left),
            "right" => Ok(CameraId::Right),
            other => Err(format!("unknown camera '{other}' (rgb, left, right)")),
        }
    }
}

/// The CoreMediaIO identifiers of one camera in the macOS extension. The app
/// finds the device by [`device`](Self::device); the extension creates its
/// source and sink streams with the other two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CameraUuids {
    /// Which camera.
    pub id: CameraId,
    /// `DeviceUUID` (the CMIO device UID).
    pub device: &'static str,
    /// `SourceUUID`: the stream consumers read.
    pub source: &'static str,
    /// `SinkUUID`: the stream oak-cam writes.
    pub sink: &'static str,
}

/// The `OakCameras` table of `extensions/oak-camera-extension/Info.plist.in`,
/// in the same order. A unit test checks the two agree.
pub const CAMERAS: [CameraUuids; 3] = [
    CameraUuids {
        id: CameraId::Rgb,
        device: "3B47A5CF-F57E-4C20-8A99-97F002D9A947",
        source: "59C671E1-F908-408C-A68F-A6B4D4514D02",
        sink: "AB3FCD34-3403-48E5-8E68-63A195CD50BD",
    },
    CameraUuids {
        id: CameraId::Left,
        device: "9D2BC40B-FC42-4646-9935-C5B4D9776122",
        source: "874750B0-9C5F-494B-B007-B9D69812443B",
        sink: "1C916B8D-1F4C-4545-A9F9-BD4ECCD3FF5D",
    },
    CameraUuids {
        id: CameraId::Right,
        device: "490657B4-CA80-4657-B900-5E57EEEB06E9",
        source: "805C8E3F-7673-47C7-81E9-E544BA8C9B07",
        sink: "BBD8C4BA-A1BB-4141-BD96-1347F80421C7",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_kinds_and_parse() {
        assert_eq!(CameraId::Rgb.name(), "OAK RGB");
        assert_eq!(CameraId::Left.name(), "OAK Left");
        assert_eq!(CameraId::Right.name(), "OAK Right");
        assert_eq!(CameraId::Rgb.kind(), CameraKind::Rgb);
        assert_eq!(CameraId::Left.kind(), CameraKind::Mono);
        assert_eq!(CameraId::Right.kind(), CameraKind::Mono);
        for id in CameraId::ALL {
            assert_eq!(id.slug().parse::<CameraId>().unwrap(), id);
            assert_eq!(id.uuids().id, id);
        }
        assert_eq!("RGB".parse::<CameraId>().unwrap(), CameraId::Rgb);
        assert!("depth".parse::<CameraId>().is_err());
        assert_eq!(CameraId::Right as u8, 2);
    }

    #[test]
    fn format_tables_per_kind() {
        assert_eq!(
            CameraKind::Rgb.default_resolution(),
            Resolution::new(1920, 1080)
        );
        assert_eq!(
            CameraKind::Rgb.formats(),
            [
                Resolution::new(1920, 1080),
                Resolution::new(1280, 720),
                Resolution::new(3840, 2160)
            ]
        );
        assert_eq!(
            CameraKind::Mono.default_resolution(),
            Resolution::new(1280, 800)
        );
        assert_eq!(
            CameraKind::Mono.formats(),
            [
                Resolution::new(1280, 800),
                Resolution::new(1280, 720),
                Resolution::new(640, 400)
            ]
        );
    }

    #[test]
    fn uuids_are_distinct_and_well_formed() {
        let all: Vec<&str> = CAMERAS
            .iter()
            .flat_map(|c| [c.device, c.source, c.sink])
            .collect();
        let mut dedup = all.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(dedup.len(), 9, "nine distinct UUIDs");
        for u in all {
            let groups: Vec<usize> = u.split('-').map(str::len).collect();
            assert_eq!(groups, [8, 4, 4, 4, 12], "{u}");
            assert!(u.chars().all(|c| c == '-' || c.is_ascii_hexdigit()));
            assert_eq!(u, u.to_ascii_uppercase());
        }
    }

    /// The value of `<key>{key}</key>` (the next `<string>`) in `chunk`.
    fn plist_string<'a>(chunk: &'a str, key: &str) -> &'a str {
        let after = chunk
            .split_once(&format!("<key>{key}</key>"))
            .unwrap_or_else(|| panic!("key {key} missing in {chunk}"))
            .1;
        let start = after.find("<string>").expect("<string>") + "<string>".len();
        let end = after.find("</string>").expect("</string>");
        after[start..end].trim()
    }

    /// The Rust table and the extension's Info.plist template agree on every
    /// camera's name, kind and all three UUIDs, in order.
    #[test]
    fn camera_table_matches_extension_plist() {
        let plist = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../extensions/oak-camera-extension/Info.plist.in"
        ));
        let array = plist
            .split_once("<key>OakCameras</key>")
            .expect("Info.plist.in declares OakCameras")
            .1;
        let array = array.split_once("</array>").expect("OakCameras array").0;
        let entries: Vec<&str> = array.split("<dict>").skip(1).collect();
        assert_eq!(entries.len(), CAMERAS.len(), "one plist entry per camera");
        for (entry, cam) in entries.iter().zip(CAMERAS) {
            assert_eq!(plist_string(entry, "Name"), cam.id.name());
            assert_eq!(plist_string(entry, "Kind"), cam.id.kind().as_str());
            assert_eq!(plist_string(entry, "DeviceUUID"), cam.device, "{}", cam.id);
            assert_eq!(plist_string(entry, "SourceUUID"), cam.source, "{}", cam.id);
            assert_eq!(plist_string(entry, "SinkUUID"), cam.sink, "{}", cam.id);
        }
        assert!(plist.contains("<string>@BUNDLE_ID@</string>"));
    }
}
