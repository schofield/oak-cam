// Host-side test for the placeholder frame, compiled and run by
// scripts/vcam/build-macos.sh --check / --test. It compiles the CMIO-free
// sources with this tiny main — no signing, no CMIO provider, no
// installed extension — so CI on any Mac catches the class of bug that made
// Photo Booth hang: a sample buffer whose format description does not match its
// image buffer, which CMSampleBufferCreateForImageBuffer rejects with -12743.

import CoreMedia
import CoreVideo
import Foundation

private func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("placeholder-test FAIL: \(message)\n".utf8))
    exit(1)
}

private func check(_ condition: Bool, _ message: String) {
    if !condition { fail(message) }
}

/// Build a placeholder at `width`x`height`, take one sample buffer and assert it
/// is well-formed: NV12 (video range) at the requested dimensions, and — the
/// check that would have caught the bug — its format description matches its
/// image buffer.
private func checkSize(_ width: Int32, _ height: Int32) {
    var placeholder = Placeholder(width: width, height: height)
    guard let sample = placeholder.sampleBuffer(hostTimeNs: 1_000_000_000) else {
        fail("sampleBuffer(\(width)x\(height)) returned nil")
    }
    guard let image = CMSampleBufferGetImageBuffer(sample) else {
        fail("sample buffer (\(width)x\(height)) has no image buffer")
    }
    guard let desc = CMSampleBufferGetFormatDescription(sample) else {
        fail("sample buffer (\(width)x\(height)) has no format description")
    }
    let dims = CMVideoFormatDescriptionGetDimensions(desc)
    check(
        dims.width == width && dims.height == height,
        "format description dimensions \(dims.width)x\(dims.height) != \(width)x\(height)")
    let subtype = CMFormatDescriptionGetMediaSubType(desc)
    check(
        subtype == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
        "pixel format \(subtype) != NV12 (420YpCbCr8BiPlanarVideoRange)")
    check(
        CMVideoFormatDescriptionMatchesImageBuffer(desc, imageBuffer: image),
        "format description does not match image buffer (CMSampleBufferCreateForImageBuffer would fail -12743)")
}

// Every NV12 size either camera kind advertises (the placeholder is built at
// the kind's default and resized to whatever the app publishes).
for kind in [CameraKind.rgb, CameraKind.mono] {
    for format in FormatTable.entries(for: kind)
    where format.pixelFormat == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange {
        checkSize(format.width, format.height)
    }
}

// ---------------------------------------------------------------------------
// FormatTable per kind.
// ---------------------------------------------------------------------------

func dims(_ entries: [CameraFormat]) -> [String] {
    entries.map { "\($0.pixelFormat == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange ? "420v" : "other") \($0.width)x\($0.height)" }
}
check(
    dims(Array(FormatTable.entries(for: .rgb).prefix(3))) == ["420v 1920x1080", "420v 1280x720", "420v 3840x2160"],
    "rgb NV12 entries \(dims(FormatTable.entries(for: .rgb)))")
check(
    dims(Array(FormatTable.entries(for: .mono).prefix(3))) == ["420v 1280x800", "420v 1280x720", "420v 640x400"],
    "mono NV12 entries \(dims(FormatTable.entries(for: .mono)))")
check(FormatTable.placeholder(for: .rgb).width == 1920, "rgb default is 1080p")
check(FormatTable.placeholder(for: .mono).height == 800, "mono default is 1280x800")
check(
    FormatTable.entries(for: .rgb).contains(
        CameraFormat(pixelFormat: kCVPixelFormatType_32BGRA, width: 1920, height: 1080)),
    "rgb offers BGRA 1080p")
check(
    FormatTable.entries(for: .mono).contains(
        CameraFormat(pixelFormat: kCVPixelFormatType_422YpCbCr8, width: 1280, height: 800)),
    "mono offers 2vuy 1280x800")
check(
    FormatTable.index(for: .mono, pixelFormat: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, width: 640, height: 400)
        == 2, "mono index of 640x400")
check(
    FormatTable.index(for: .rgb, pixelFormat: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, width: 640, height: 400)
        == nil, "rgb does not advertise 640x400")
check(Placeholder.caption(for: "OAK RGB") == "OAK RGB — waiting for oak-cam", "placeholder caption")

// ---------------------------------------------------------------------------
// ExtensionIdentity: the OakCameras array of Info.plist.in (path in argv[1])
// parses into three distinct cameras, RGB then Left then Right.
// ---------------------------------------------------------------------------

if CommandLine.arguments.count > 1 {
    let url = URL(fileURLWithPath: CommandLine.arguments[1])
    guard let data = try? Data(contentsOf: url),
        let plist = try? PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any],
        let cameras = plist["OakCameras"] as? [[String: Any]]
    else {
        fail("cannot read OakCameras from \(url.path)")
    }
    let identities = cameras.enumerated().map { ExtensionIdentity.parse($1, index: $0) }
    check(identities.map(\.deviceName) == ["OAK RGB", "OAK Left", "OAK Right"], "camera names/order")
    check(identities.map(\.kind) == [.rgb, .mono, .mono], "camera kinds")
    let uuids = identities.flatMap { [$0.deviceUUID, $0.sourceStreamUUID, $0.sinkStreamUUID] }
    check(Set(uuids).count == 9, "all nine device/stream UUIDs are distinct")
    print("identity-test: OK (3 cameras from \(url.lastPathComponent))")
}

// The same instance, resized (as DeviceSource does to follow the canvas), must
// rebuild a matching buffer/description pair at the new size.
var resizable = Placeholder(width: 1920, height: 1080)
check(resizable.sampleBuffer(hostTimeNs: 0) != nil, "1080p sample buffer before resize")
resizable.resize(width: 1280, height: 720)
guard let resized = resizable.sampleBuffer(hostTimeNs: 0),
    let resizedImage = CMSampleBufferGetImageBuffer(resized),
    let resizedDesc = CMSampleBufferGetFormatDescription(resized)
else {
    fail("sample buffer after resize to 1280x720 was not well-formed")
}
let resizedDims = CMVideoFormatDescriptionGetDimensions(resizedDesc)
check(
    resizedDims.width == 1280 && resizedDims.height == 720,
    "resized dimensions \(resizedDims.width)x\(resizedDims.height) != 1280x720")
check(
    CMVideoFormatDescriptionMatchesImageBuffer(resizedDesc, imageBuffer: resizedImage),
    "resized format description does not match image buffer (-12743)")

// A consumer shares the placeholder's IOSurface, so attachments it sets land on
// OUR buffer. Changing a colour attachment makes the cached format description
// stop matching; the next tick must re-derive it rather than fail with -12743
// for as long as that consumer stays connected (seen live on 2026-09-19).
CVBufferSetAttachment(
    resizedImage, kCVImageBufferYCbCrMatrixKey, kCVImageBufferYCbCrMatrix_ITU_R_601_4, .shouldPropagate)
guard let healed = resizable.sampleBuffer(hostTimeNs: 1),
    let healedImage = CMSampleBufferGetImageBuffer(healed),
    let healedDesc = CMSampleBufferGetFormatDescription(healed)
else {
    fail("placeholder did not recover after a consumer changed the buffer's attachments")
}
check(
    CMVideoFormatDescriptionMatchesImageBuffer(healedDesc, imageBuffer: healedImage),
    "re-derived placeholder description does not match its buffer")

print("placeholder-test: OK (rgb + mono NV12 sizes, resize 1080p->720p, attachment change)")

// ---------------------------------------------------------------------------
// FrameArbiter: state transitions and the monotonic clock.
// ---------------------------------------------------------------------------

private let second: UInt64 = 1_000_000_000

func checkArbiterStates() {
    let a = FrameArbiter()
    // Nothing published yet: idle while a consumer is open, none without one.
    check(a.tick(nowNs: 10 * second, consumers: 0) == .none, "no consumer -> none")
    check(a.tick(nowNs: 10 * second, consumers: 1) == .idle, "never relayed -> idle")

    // A relayed frame moves to relaying; a fresh tick relays.
    a.noteRelay(hostNs: 10 * second)
    check(a.state == .relaying, "noteRelay -> relaying")
    check(a.tick(nowNs: 10 * second, consumers: 1) == .relay, "fresh relay -> relay")

    // Past the one-second stall window with no stop: hold indefinitely.
    check(a.tick(nowNs: 11 * second + 100_000_000, consumers: 1) == .held, "stall -> held")
    check(a.state == .held, "stall state is held")
    check(a.tick(nowNs: 200 * second, consumers: 1) == .held, "unclean loss holds indefinitely")

    // A new relay switches straight back with no idle in between.
    a.noteRelay(hostNs: 201 * second)
    check(a.tick(nowNs: 201 * second, consumers: 1) == .relay, "relay resumes -> relay")

    // A clean stop holds, then times out to idle after 60 s.
    a.publisherStopped(clean: true, nowNs: 201 * second)
    check(a.state == .held, "clean stop -> held")
    check(a.tick(nowNs: 230 * second, consumers: 1) == .held, "held within timeout")
    check(
        a.tick(nowNs: 201 * second + FrameArbiter.holdTimeoutNanoseconds, consumers: 1) == .idle,
        "held past clean-stop timeout -> idle")
    check(a.state == .idle, "timed-out hold state is idle")

    // Before anything was relayed, a stop falls straight to idle.
    let b = FrameArbiter()
    b.publisherStopped(clean: true, nowNs: 5 * second)
    check(b.tick(nowNs: 5 * second, consumers: 1) == .idle, "stop before any relay -> idle")
}

func checkArbiterMonotonic() {
    let a = FrameArbiter()
    var last: UInt64 = 0
    // A scripted run: idle, then relay whose PTS jumps backwards (the reconnect
    // bug), then held and idle. Every stamp must strictly increase.
    let script: [(OutputSource, UInt64)] = [
        (.idle, 100 * second),
        (.idle, 100 * second + 33_000_000),
        (.relay, 50 * second),  // PTS earlier than the last idle send
        (.relay, 50 * second + 33_000_000),
        (.held, 60 * second),
        (.held, 60 * second),  // same preferred: still must advance
        (.idle, 40 * second),  // preferred jumps back again
        (.relay, 200 * second),
    ]
    var sawDiscontinuity = false
    var prevSource: OutputSource? = nil
    for (source, preferred) in script {
        let (stamp, discontinuity) = a.stampForSend(source: source, preferred: preferred)
        check(stamp > last, "monotonic stamp: \(stamp) > \(last) for \(source)")
        last = stamp
        if prevSource != nil && prevSource != source {
            check(discontinuity, "source switch \(prevSource!) -> \(source) flags discontinuity")
            sawDiscontinuity = true
        }
        prevSource = source
    }
    check(sawDiscontinuity, "at least one source switch was flagged")
}

// ---------------------------------------------------------------------------
// Badge: CoreText glyph coverage renders inside the extension's frameworks.
// ---------------------------------------------------------------------------

func checkBadgeGlyphCoverage() {
    guard let glyph = Badge.glyphBitmap(text: HeldFrame.badgeText, heightPx: 48) else {
        fail("glyphBitmap returned nil (CoreText/CoreGraphics unavailable)")
    }
    check(glyph.width > 0 && glyph.height > 0, "glyph bitmap has size")
    let coverage = glyph.bytes.reduce(0) { $0 + Int($1) }
    check(coverage > 0, "glyph bitmap has non-empty coverage")
}

// ---------------------------------------------------------------------------
// HeldFrame: copy independence, matching description, badge region.
// ---------------------------------------------------------------------------

/// A locked NV12 buffer's Y byte at (x, y).
func yByte(_ pb: CVPixelBuffer, _ x: Int, _ y: Int) -> UInt8 {
    CVPixelBufferLockBaseAddress(pb, .readOnly)
    defer { CVPixelBufferUnlockBaseAddress(pb, .readOnly) }
    let base = CVPixelBufferGetBaseAddressOfPlane(pb, 0)!.assumingMemoryBound(to: UInt8.self)
    let stride = CVPixelBufferGetBytesPerRowOfPlane(pb, 0)
    return base[y * stride + x]
}

func makeNV12(width: Int, height: Int, y: UInt8, uv: UInt8) -> CVPixelBuffer {
    let attributes: [CFString: Any] = [kCVPixelBufferIOSurfacePropertiesKey: [CFString: Any]()]
    var buffer: CVPixelBuffer?
    let status = CVPixelBufferCreate(
        kCFAllocatorDefault, width, height, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
        attributes as CFDictionary, &buffer)
    guard status == kCVReturnSuccess, let buffer = buffer else {
        fail("makeNV12 CVPixelBufferCreate failed: \(status)")
    }
    CVPixelBufferLockBaseAddress(buffer, [])
    let yBase = CVPixelBufferGetBaseAddressOfPlane(buffer, 0)!.assumingMemoryBound(to: UInt8.self)
    let yStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
    for row in 0..<height { yBase.advanced(by: row * yStride).initialize(repeating: y, count: width) }
    let uvBase = CVPixelBufferGetBaseAddressOfPlane(buffer, 1)!.assumingMemoryBound(to: UInt8.self)
    let uvStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)
    let uvRows = CVPixelBufferGetHeightOfPlane(buffer, 1)
    uvBase.initialize(repeating: uv, count: uvStride * uvRows)
    CVPixelBufferUnlockBaseAddress(buffer, [])
    CVBufferSetAttachment(
        buffer, kCVImageBufferColorPrimariesKey, kCVImageBufferColorPrimaries_ITU_R_709_2, .shouldPropagate)
    CVBufferSetAttachment(
        buffer, kCVImageBufferTransferFunctionKey, kCVImageBufferTransferFunction_ITU_R_709_2, .shouldPropagate)
    CVBufferSetAttachment(
        buffer, kCVImageBufferYCbCrMatrixKey, kCVImageBufferYCbCrMatrix_ITU_R_709_2, .shouldPropagate)
    return buffer
}

func sampleFrom(_ pb: CVPixelBuffer) -> CMSampleBuffer {
    var desc: CMFormatDescription?
    let dstatus = CMVideoFormatDescriptionCreateForImageBuffer(
        allocator: kCFAllocatorDefault, imageBuffer: pb, formatDescriptionOut: &desc)
    guard dstatus == noErr, let desc = desc else { fail("sampleFrom: description failed \(dstatus)") }
    var timing = CMSampleTimingInfo(
        duration: CMTime(value: 1, timescale: 30),
        presentationTimeStamp: CMTime(value: 0, timescale: 1_000_000_000), decodeTimeStamp: .invalid)
    var sample: CMSampleBuffer?
    let status = CMSampleBufferCreateForImageBuffer(
        allocator: kCFAllocatorDefault, imageBuffer: pb, dataReady: true, makeDataReadyCallback: nil,
        refcon: nil, formatDescription: desc, sampleTiming: &timing, sampleBufferOut: &sample)
    guard status == noErr, let sample = sample else { fail("sampleFrom: sample failed \(status)") }
    return sample
}

func checkHeldFrame() {
    let width = 1280
    let height = 720
    let source = makeNV12(width: width, height: height, y: 0x60, uv: 0x80)
    let sourceCornerY = yByte(source, 4, 4)  // far corner, away from the bottom badge
    let sample = sampleFrom(source)

    guard let held = HeldFrame(from: sample) else { fail("HeldFrame(from:) returned nil") }

    // Mutate the source AFTER constructing the held frame: the copy must be
    // independent (this is what lets the app free its pooled buffer).
    CVPixelBufferLockBaseAddress(source, [])
    let yBase = CVPixelBufferGetBaseAddressOfPlane(source, 0)!.assumingMemoryBound(to: UInt8.self)
    let yStride = CVPixelBufferGetBytesPerRowOfPlane(source, 0)
    for row in 0..<height { yBase.advanced(by: row * yStride).initialize(repeating: 0x11, count: width) }
    CVPixelBufferUnlockBaseAddress(source, [])

    guard let heldSample = held.sampleBuffer(hostTimeNs: 1_000_000_000),
        let heldImage = CMSampleBufferGetImageBuffer(heldSample),
        let heldDesc = CMSampleBufferGetFormatDescription(heldSample)
    else {
        fail("held sample buffer was not well-formed")
    }

    // The far corner is byte-identical to the ORIGINAL source (copy is
    // independent of the later mutation, and the badge is bottom-centre only).
    check(
        yByte(heldImage, 4, 4) == sourceCornerY,
        "held copy far corner \(yByte(heldImage, 4, 4)) != original source \(sourceCornerY)")

    // The badge region (bottom-centre) differs from the flat source picture.
    let badgeY = yByte(heldImage, width / 2, height - height / 20)
    check(badgeY != 0x60, "held badge region \(badgeY) should differ from the source 0x60")

    // The format description matches the image buffer (else -12743 on send) and
    // carries the copied geometry.
    check(
        CMVideoFormatDescriptionMatchesImageBuffer(heldDesc, imageBuffer: heldImage),
        "held format description does not match its image buffer")
    let dims = CMVideoFormatDescriptionGetDimensions(heldDesc)
    check(dims.width == 1280 && dims.height == 720, "held dimensions \(dims.width)x\(dims.height) != 1280x720")
    check(
        CMFormatDescriptionGetMediaSubType(heldDesc) == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
        "held pixel format is not NV12")

    // Same shared-IOSurface hazard as the placeholder: a consumer changing an
    // attachment must not stop the held picture.
    CVBufferSetAttachment(
        heldImage, kCVImageBufferYCbCrMatrixKey, kCVImageBufferYCbCrMatrix_ITU_R_601_4, .shouldPropagate)
    guard let heldAgain = held.sampleBuffer(hostTimeNs: 2_000_000_000),
        let heldAgainImage = CMSampleBufferGetImageBuffer(heldAgain),
        let heldAgainDesc = CMSampleBufferGetFormatDescription(heldAgain)
    else {
        fail("held frame did not recover after a consumer changed the buffer's attachments")
    }
    check(
        CMVideoFormatDescriptionMatchesImageBuffer(heldAgainDesc, imageBuffer: heldAgainImage),
        "re-derived held description does not match its buffer")
}

checkArbiterStates()
checkArbiterMonotonic()
checkBadgeGlyphCoverage()
checkHeldFrame()

print("host-test: OK (placeholder, arbiter states + monotonic clock, badge coverage, held frame)")
