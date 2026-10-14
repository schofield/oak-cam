import CoreMedia
import CoreMediaIO
import CoreVideo
import Foundation

/// Which OAK sensor a camera device carries; selects its format table. Lives
/// here (not in ExtensionIdentity.swift) so the host test compiles it.
enum CameraKind: String {
    /// CAM_A (IMX378), NV12 from the device.
    case rgb
    /// CAM_B / CAM_C (OV9282), GRAY8 on the wire, published as NV12 with neutral chroma.
    case mono
}

/// One advertised camera format.
struct CameraFormat: Equatable {
    let pixelFormat: OSType
    let width: Int32
    let height: Int32
}

/// The formats each camera's source stream advertises, per sensor kind, in
/// preference order: NV12 (video range) at the kind's default size first, then
/// the other NV12 sizes, then packed 4:2:2 / BGRA. The sink stream accepts
/// whatever the app sends and the relayed frames carry their own format
/// description, so the list is what consumers negotiate against, not a hard
/// limit on what the app publishes. The Rust side's `CameraKind::formats`
/// (crates/vcam/src/camera_id.rs) lists the same NV12 sizes.
enum FormatTable {
    /// Frames per second the placeholder runs at and the nominal rate offered.
    static let nominalFrameRate: Int32 = 30
    /// Fastest rate offered; a 60 fps publisher relays at 60.
    static let maxFrameRate: Int32 = 60

    /// RGB (IMX378): 1080p default, 720p, 4K, plus YUY2/UYVY/BGRA at 1080p.
    static let rgbEntries: [CameraFormat] = [
        CameraFormat(pixelFormat: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, width: 1920, height: 1080),
        CameraFormat(pixelFormat: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, width: 1280, height: 720),
        CameraFormat(pixelFormat: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, width: 3840, height: 2160),
        CameraFormat(pixelFormat: kCVPixelFormatType_422YpCbCr8_yuvs, width: 1920, height: 1080),
        CameraFormat(pixelFormat: kCVPixelFormatType_422YpCbCr8, width: 1920, height: 1080),
        CameraFormat(pixelFormat: kCVPixelFormatType_32BGRA, width: 1920, height: 1080),
    ]

    /// Mono (OV9282): 1280x800 default, 720p, 640x400, plus UYVY at 1280x800.
    static let monoEntries: [CameraFormat] = [
        CameraFormat(pixelFormat: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, width: 1280, height: 800),
        CameraFormat(pixelFormat: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, width: 1280, height: 720),
        CameraFormat(pixelFormat: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, width: 640, height: 400),
        CameraFormat(pixelFormat: kCVPixelFormatType_422YpCbCr8, width: 1280, height: 800),
    ]

    static func entries(for kind: CameraKind) -> [CameraFormat] {
        switch kind {
        case .rgb: return rgbEntries
        case .mono: return monoEntries
        }
    }

    /// The default (placeholder) format of `kind`: its first entry (NV12).
    static func placeholder(for kind: CameraKind) -> CameraFormat { entries(for: kind)[0] }

    static func formatDescription(for format: CameraFormat) -> CMFormatDescription {
        var description: CMFormatDescription?
        let extensions: [CFString: Any] = [
            kCMFormatDescriptionExtension_ColorPrimaries: kCMFormatDescriptionColorPrimaries_ITU_R_709_2,
            kCMFormatDescriptionExtension_TransferFunction: kCMFormatDescriptionTransferFunction_ITU_R_709_2,
            kCMFormatDescriptionExtension_YCbCrMatrix: kCMFormatDescriptionYCbCrMatrix_ITU_R_709_2,
        ]
        let status = CMVideoFormatDescriptionCreate(
            allocator: kCFAllocatorDefault,
            codecType: format.pixelFormat,
            width: format.width,
            height: format.height,
            extensions: extensions as CFDictionary,
            formatDescriptionOut: &description
        )
        guard status == noErr, let description = description else {
            fatalError("camera extension: CMVideoFormatDescriptionCreate failed (\(status))")
        }
        return description
    }

    /// A format description that matches `buffer` right now: `cached` when it
    /// still matches, otherwise one derived afresh. A re-sent picture lives in
    /// one IOSurface that every consumer shares, so an attachment a consumer
    /// sets (a colour matrix, a colour space) lands on our buffer and the cached
    /// description stops matching; without this every later frame fails with
    /// -12743 until that consumer disconnects.
    static func matchingDescription(for buffer: CVPixelBuffer, cached: CMFormatDescription?)
        -> CMFormatDescription?
    {
        if let cached = cached, CMVideoFormatDescriptionMatchesImageBuffer(cached, imageBuffer: buffer) {
            return cached
        }
        var description: CMFormatDescription?
        let status = CMVideoFormatDescriptionCreateForImageBuffer(
            allocator: kCFAllocatorDefault, imageBuffer: buffer, formatDescriptionOut: &description)
        return status == noErr ? description : nil
    }

    static func streamFormats(for kind: CameraKind) -> [CMIOExtensionStreamFormat] {
        entries(for: kind).map { format in
            CMIOExtensionStreamFormat(
                formatDescription: formatDescription(for: format),
                maxFrameDuration: CMTime(value: 1, timescale: nominalFrameRate),
                minFrameDuration: CMTime(value: 1, timescale: maxFrameRate),
                validFrameDurations: nil
            )
        }
    }

    /// Index of the advertised format of `kind` matching a relayed frame, if
    /// any, so the source stream's active format can follow the publisher.
    static func index(for kind: CameraKind, pixelFormat: OSType, width: Int32, height: Int32) -> Int? {
        entries(for: kind).firstIndex(of: CameraFormat(pixelFormat: pixelFormat, width: width, height: height))
    }
}
