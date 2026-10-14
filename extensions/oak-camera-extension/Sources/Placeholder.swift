import CoreMedia
import CoreVideo
import Foundation
import os.log

private let log = OSLog(subsystem: "com.surlysoft.oak-cam.camera-extension", category: "placeholder")

/// The frame shown while nothing has ever been published (or a reconnect hold
/// timed out): an NV12 (video range) IOSurface-backed pixel buffer filled once
/// per size, re-sent each tick with a fresh timestamp. Dark grey with a band of
/// luma steps across the middle third and a "no signal" caption, so a consumer
/// can tell "camera alive, no picture yet" from black or a frozen picture.
struct Placeholder {
    /// The caption drawn on a camera's idle placeholder (NV12 supports the
    /// badge): "OAK RGB — waiting for oak-cam".
    static func caption(for deviceName: String) -> String {
        "\(deviceName) — waiting for oak-cam"
    }

    /// The caption this placeholder draws.
    let caption: String
    private(set) var width: Int32
    private(set) var height: Int32
    private var pixelBuffer: CVPixelBuffer?
    private var formatDescription: CMFormatDescription?

    init(width: Int32, height: Int32, caption: String = Placeholder.caption(for: "OAK")) {
        self.width = width
        self.height = height
        self.caption = caption
    }

    mutating func resize(width: Int32, height: Int32) {
        guard width > 0, height > 0, width != self.width || height != self.height else {
            return
        }
        self.width = width
        self.height = height
        pixelBuffer = nil
        formatDescription = nil
    }

    mutating func sampleBuffer(hostTimeNs: UInt64) -> CMSampleBuffer? {
        if pixelBuffer == nil {
            build()
        }
        guard let pixelBuffer = pixelBuffer,
            let formatDescription = FormatTable.matchingDescription(for: pixelBuffer, cached: formatDescription)
        else {
            return nil
        }
        self.formatDescription = formatDescription
        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: FormatTable.nominalFrameRate),
            presentationTimeStamp: CMTime(value: CMTimeValue(hostTimeNs), timescale: 1_000_000_000),
            decodeTimeStamp: .invalid
        )
        var sampleBuffer: CMSampleBuffer?
        let status = CMSampleBufferCreateForImageBuffer(
            allocator: kCFAllocatorDefault,
            imageBuffer: pixelBuffer,
            dataReady: true,
            makeDataReadyCallback: nil,
            refcon: nil,
            formatDescription: formatDescription,
            sampleTiming: &timing,
            sampleBufferOut: &sampleBuffer
        )
        if status != noErr {
            os_log(.error, log: log, "CMSampleBufferCreateForImageBuffer failed: %d", status)
            return nil
        }
        return sampleBuffer
    }

    private mutating func build() {
        let format = CameraFormat(
            pixelFormat: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, width: width, height: height)
        let attributes: [CFString: Any] = [
            kCVPixelBufferIOSurfacePropertiesKey: [CFString: Any](),
        ]
        var buffer: CVPixelBuffer?
        let status = CVPixelBufferCreate(
            kCFAllocatorDefault, Int(width), Int(height), format.pixelFormat,
            attributes as CFDictionary, &buffer)
        guard status == kCVReturnSuccess, let buffer = buffer else {
            os_log(.error, log: log, "CVPixelBufferCreate failed: %d", status)
            return
        }
        Placeholder.paint(buffer)
        // A small caption so the idle placeholder reads as "no signal", not just
        // grey bars. NV12 is a supported badge format; the return is ignored (a
        // caption-less placeholder is still fine).
        Badge.composite(text: caption, into: buffer)
        // Tag the buffer BT.709 (video-range NV12, what oak-cam publishes),
        // then derive the format description FROM the buffer so the two match by
        // construction. Building the description by hand (as the advertised
        // stream formats do) leaves CMVideoFormatDescriptionMatchesImageBuffer
        // false for an untagged buffer, and CMSampleBufferCreateForImageBuffer
        // then drops every frame with -12743 (kCMSampleBufferError_InvalidMediaFormat).
        Placeholder.tagBT709(buffer)
        var description: CMFormatDescription?
        let descStatus = CMVideoFormatDescriptionCreateForImageBuffer(
            allocator: kCFAllocatorDefault,
            imageBuffer: buffer,
            formatDescriptionOut: &description
        )
        guard descStatus == noErr, let description = description else {
            os_log(.error, log: log, "CMVideoFormatDescriptionCreateForImageBuffer failed: %d", descStatus)
            return
        }
        pixelBuffer = buffer
        formatDescription = description
    }

    /// Set the three BT.709 colour attachments a video format description carries
    /// so a description derived from the buffer matches it; `.shouldPropagate`
    /// keeps them on the buffer as it travels to the consumer.
    private static func tagBT709(_ buffer: CVPixelBuffer) {
        CVBufferSetAttachment(
            buffer, kCVImageBufferColorPrimariesKey,
            kCVImageBufferColorPrimaries_ITU_R_709_2, .shouldPropagate)
        CVBufferSetAttachment(
            buffer, kCVImageBufferTransferFunctionKey,
            kCVImageBufferTransferFunction_ITU_R_709_2, .shouldPropagate)
        CVBufferSetAttachment(
            buffer, kCVImageBufferYCbCrMatrixKey,
            kCVImageBufferYCbCrMatrix_ITU_R_709_2, .shouldPropagate)
    }

    private static func paint(_ buffer: CVPixelBuffer) {
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        let width = CVPixelBufferGetWidth(buffer)
        let height = CVPixelBufferGetHeight(buffer)

        if let yBase = CVPixelBufferGetBaseAddressOfPlane(buffer, 0) {
            let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
            let bandTop = height / 3
            let bandBottom = height * 2 / 3
            let bars = 8
            let y = yBase.assumingMemoryBound(to: UInt8.self)
            for row in 0..<height {
                let line = y.advanced(by: row * stride)
                if row < bandTop || row >= bandBottom {
                    line.initialize(repeating: 32, count: width)
                    continue
                }
                for col in 0..<width {
                    let bar = col * bars / max(width, 1)
                    line[col] = UInt8(16 + bar * (235 - 16) / (bars - 1))
                }
            }
        }
        if let uvBase = CVPixelBufferGetBaseAddressOfPlane(buffer, 1) {
            let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)
            let rows = CVPixelBufferGetHeightOfPlane(buffer, 1)
            uvBase.assumingMemoryBound(to: UInt8.self).initialize(repeating: 128, count: stride * rows)
        }
    }
}
