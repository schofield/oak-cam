import Foundation

/// The identifiers the app and the extension must agree on, one per camera
/// device. They are read from the `OakCameras` array in Info.plist (written by
/// scripts/vcam/build-macos.sh from Info.plist.in) so a single template is the
/// source of truth; the Rust side pins the same UUIDs in
/// `crates/vcam/src/camera_id.rs` and a test checks the two files agree.
struct ExtensionIdentity {
    let deviceUUID: UUID
    let sourceStreamUUID: UUID
    let sinkStreamUUID: UUID
    let deviceName: String
    let kind: CameraKind

    /// Every camera the bundle declares, in Info.plist order (RGB, Left, Right).
    static func fromMainBundle() -> [ExtensionIdentity] {
        guard let cameras = Bundle.main.object(forInfoDictionaryKey: "OakCameras") as? [[String: Any]],
            !cameras.isEmpty
        else {
            fatalError("Info.plist key OakCameras is missing or empty")
        }
        return cameras.enumerated().map { index, entry in
            parse(entry, index: index)
        }
    }

    /// One `OakCameras` entry; a malformed entry is a build error, so it is fatal.
    static func parse(_ entry: [String: Any], index: Int) -> ExtensionIdentity {
        func uuid(_ key: String) -> UUID {
            guard let raw = entry[key] as? String, let value = UUID(uuidString: raw) else {
                fatalError("OakCameras[\(index)].\(key) is missing or not a UUID")
            }
            return value
        }
        guard let name = entry["Name"] as? String, !name.isEmpty else {
            fatalError("OakCameras[\(index)].Name is missing")
        }
        guard let rawKind = entry["Kind"] as? String, let kind = CameraKind(rawValue: rawKind) else {
            fatalError("OakCameras[\(index)].Kind must be rgb or mono")
        }
        return ExtensionIdentity(
            deviceUUID: uuid("DeviceUUID"),
            sourceStreamUUID: uuid("SourceUUID"),
            sinkStreamUUID: uuid("SinkUUID"),
            deviceName: name,
            kind: kind
        )
    }
}
