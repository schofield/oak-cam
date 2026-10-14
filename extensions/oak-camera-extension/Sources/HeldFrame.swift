import CoreMedia
import CoreVideo
import Foundation
import os.log

private let log = OSLog(subsystem: "com.surlysoft.oak-cam.camera-extension", category: "held")

/// The last relayed picture, copied once into an extension-owned buffer with a
/// "Reconnecting…" badge drawn on it, then re-sent at the nominal rate while the
/// publisher is gone.
///
/// The copy matters: while relaying, `DeviceSource` only retains a reference to
/// the app's most recent pooled buffer (one of the sink pool's six). On entering
/// the held state it copies that buffer ONCE here into a fresh IOSurface-backed
/// `CVPixelBuffer`, so the app's pool buffer is released and the app may
/// overwrite or free it. Steady state therefore holds at most one of the app's
/// pooled buffers (released the moment this copy is made), and the held state
/// itself allocates exactly one buffer per stall — no per-frame copies.
///
/// Built once per held picture (like `Placeholder`): the pixel buffer and its
/// format description are fixed, and each tick wraps them in a fresh
/// `CMSampleBuffer` with a new timestamp.
struct HeldFrame {
    private let pixelBuffer: CVPixelBuffer
    private let formatDescription: CMFormatDescription

    /// The "Reconnecting…" caption drawn on the held picture.
    static let badgeText = "Reconnecting…"

    /// Copy the image buffer out of `sample`, draw the badge, and derive a
    /// matching format description. Returns nil only when the sample carries no
    /// image buffer or the copy cannot be allocated; an unsupported badge format
    /// still yields a (badge-less) held frame.
    init?(from sample: CMSampleBuffer) {
        guard let source = CMSampleBufferGetImageBuffer(sample) else {
            os_log(.error, log: log, "held: sample has no image buffer")
            return nil
        }
        guard let copy = HeldFrame.copy(source) else {
            return nil
        }
        if !Badge.composite(text: HeldFrame.badgeText, into: copy) {
            os_log(
                .info, log: log, "held: no badge for pixel format %d; holding last frame without a badge",
                CVPixelBufferGetPixelFormatType(copy))
        }
        var description: CMFormatDescription?
        let status = CMVideoFormatDescriptionCreateForImageBuffer(
            allocator: kCFAllocatorDefault, imageBuffer: copy, formatDescriptionOut: &description)
        guard status == noErr, let description = description else {
            os_log(.error, log: log, "held: CMVideoFormatDescriptionCreateForImageBuffer failed: %d", status)
            return nil
        }
        self.pixelBuffer = copy
        self.formatDescription = description
    }

    /// Wrap the held picture in a sample buffer presented at `hostTimeNs`.
    func sampleBuffer(hostTimeNs: UInt64) -> CMSampleBuffer? {
        // Consumers share this IOSurface and may change its attachments; see
        // `FormatTable.matchingDescription`. (Not cached back: this is a value
        // type read through a copy, and the re-derive only runs after a change.)
        guard
            let formatDescription = FormatTable.matchingDescription(
                for: pixelBuffer, cached: formatDescription)
        else {
            os_log(.error, log: log, "held: no format description matches the held buffer")
            return nil
        }
        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: FormatTable.nominalFrameRate),
            presentationTimeStamp: CMTime(value: CMTimeValue(hostTimeNs), timescale: 1_000_000_000),
            decodeTimeStamp: .invalid)
        var sampleBuffer: CMSampleBuffer?
        let status = CMSampleBufferCreateForImageBuffer(
            allocator: kCFAllocatorDefault,
            imageBuffer: pixelBuffer,
            dataReady: true,
            makeDataReadyCallback: nil,
            refcon: nil,
            formatDescription: formatDescription,
            sampleTiming: &timing,
            sampleBufferOut: &sampleBuffer)
        if status != noErr {
            os_log(.error, log: log, "held: CMSampleBufferCreateForImageBuffer failed: %d", status)
            return nil
        }
        return sampleBuffer
    }

    /// Deep-copy `source` into a fresh IOSurface-backed pixel buffer of the same
    /// format and size, propagating the colour attachments so a description
    /// derived from the copy matches it and travels to the consumer.
    private static func copy(_ source: CVPixelBuffer) -> CVPixelBuffer? {
        let width = CVPixelBufferGetWidth(source)
        let height = CVPixelBufferGetHeight(source)
        let format = CVPixelBufferGetPixelFormatType(source)
        let attributes: [CFString: Any] = [
            kCVPixelBufferIOSurfacePropertiesKey: [CFString: Any]()
        ]
        var buffer: CVPixelBuffer?
        let status = CVPixelBufferCreate(
            kCFAllocatorDefault, width, height, format, attributes as CFDictionary, &buffer)
        guard status == kCVReturnSuccess, let buffer = buffer else {
            os_log(.error, log: log, "held: CVPixelBufferCreate failed: %d", status)
            return nil
        }

        CVPixelBufferLockBaseAddress(source, .readOnly)
        CVPixelBufferLockBaseAddress(buffer, [])
        defer {
            CVPixelBufferUnlockBaseAddress(buffer, [])
            CVPixelBufferUnlockBaseAddress(source, .readOnly)
        }
        if CVPixelBufferIsPlanar(source) {
            let planes = CVPixelBufferGetPlaneCount(source)
            for p in 0..<planes {
                copyPlane(
                    src: CVPixelBufferGetBaseAddressOfPlane(source, p),
                    srcStride: CVPixelBufferGetBytesPerRowOfPlane(source, p),
                    dst: CVPixelBufferGetBaseAddressOfPlane(buffer, p),
                    dstStride: CVPixelBufferGetBytesPerRowOfPlane(buffer, p),
                    rows: min(
                        CVPixelBufferGetHeightOfPlane(source, p), CVPixelBufferGetHeightOfPlane(buffer, p)))
            }
        } else {
            copyPlane(
                src: CVPixelBufferGetBaseAddress(source),
                srcStride: CVPixelBufferGetBytesPerRow(source),
                dst: CVPixelBufferGetBaseAddress(buffer),
                dstStride: CVPixelBufferGetBytesPerRow(buffer),
                rows: min(height, CVPixelBufferGetHeight(buffer)))
        }

        // Propagate the colour attachments (BT.709 tags and friends) so the
        // derived description matches and the consumer decodes correctly.
        CVBufferPropagateAttachments(source, buffer)
        return buffer
    }

    /// Copy `rows` rows of the smaller stride between two locked planes.
    private static func copyPlane(
        src: UnsafeMutableRawPointer?, srcStride: Int,
        dst: UnsafeMutableRawPointer?, dstStride: Int, rows: Int
    ) {
        guard let src = src, let dst = dst else { return }
        let bytes = min(srcStride, dstStride)
        if srcStride == dstStride {
            dst.copyMemory(from: src, byteCount: bytes * rows)
            return
        }
        let s = src.assumingMemoryBound(to: UInt8.self)
        let d = dst.assumingMemoryBound(to: UInt8.self)
        for row in 0..<rows {
            d.advanced(by: row * dstStride).update(from: s.advanced(by: row * srcStride), count: bytes)
        }
    }
}
