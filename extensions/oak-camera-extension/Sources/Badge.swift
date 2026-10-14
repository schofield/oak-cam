import CoreGraphics
import CoreText
import CoreVideo
import Foundation
import os.log

private let log = OSLog(subsystem: "com.surlysoft.oak-cam.camera-extension", category: "badge")

/// A caption drawn onto a frame: a dark rounded-rect plate with white text,
/// bottom-centre, sized to the frame. Used for the held-frame "Reconnecting…"
/// badge and the idle placeholder's "no signal" caption.
///
/// Every entry point is a pure function over a `CVPixelBuffer` (no CoreMediaIO)
/// so the host test can exercise it. The glyph coverage is rendered once with
/// CoreText into an 8-bit grey bitmap; the caller composites it into the frame
/// in the frame's own pixel format. CoreText with the system UI font works
/// inside the extension's sandbox (system fonts are readable), which the host
/// test confirms links and runs.
enum Badge {
    /// Video-range luma of the plate backing (near black) and the text.
    private static let backingLuma: Double = 24
    private static let textLuma: Double = 235
    /// How opaque the dark plate is over the picture (the video shows through a
    /// little so a held frame still reads as the last picture).
    private static let backingAlpha: Double = 0.72

    /// An 8-bit grey coverage bitmap: 0 where empty, 255 where a glyph is solid.
    struct GlyphBitmap {
        let bytes: [UInt8]
        let width: Int
        let height: Int
    }

    /// Render `text` into a coverage bitmap whose height is about `heightPx`
    /// (white text on black, so the byte value is the glyph coverage). Returns
    /// nil if CoreText or CoreGraphics is unavailable for the string/size.
    static func glyphBitmap(text: String, heightPx: Int) -> GlyphBitmap? {
        guard heightPx > 0, !text.isEmpty else { return nil }
        let fontSize = CGFloat(heightPx)
        let font =
            CTFontCreateUIFontForLanguage(.system, fontSize, nil)
            ?? CTFontCreateWithName("Helvetica" as CFString, fontSize, nil)
        // CoreText attribute keys (AppKit's `.font` / `.foregroundColor` are not
        // available with Foundation + CoreText alone).
        let attributes: [NSAttributedString.Key: Any] = [
            NSAttributedString.Key(kCTFontAttributeName as String): font,
            NSAttributedString.Key(kCTForegroundColorAttributeName as String): CGColor(gray: 1.0, alpha: 1.0),
        ]
        let attributed = NSAttributedString(string: text, attributes: attributes)
        let line = CTLineCreateWithAttributedString(attributed)
        var ascent: CGFloat = 0
        var descent: CGFloat = 0
        var leading: CGFloat = 0
        let typographicWidth = CTLineGetTypographicBounds(line, &ascent, &descent, &leading)
        let padX = max(2, heightPx / 6)
        let width = Int(typographicWidth.rounded(.up)) + 2 * padX
        let height = max(1, Int((ascent + descent).rounded(.up)))
        guard width > 0, height > 0 else { return nil }

        let stride = width  // 1 byte per pixel, tightly packed
        var bytes = [UInt8](repeating: 0, count: stride * height)
        let grey = CGColorSpaceCreateDeviceGray()
        let ok = bytes.withUnsafeMutableBytes { raw -> Bool in
            guard
                let ctx = CGContext(
                    data: raw.baseAddress,
                    width: width,
                    height: height,
                    bitsPerComponent: 8,
                    bytesPerRow: stride,
                    space: grey,
                    bitmapInfo: CGImageAlphaInfo.none.rawValue)
            else {
                return false
            }
            ctx.setAllowsAntialiasing(true)
            ctx.setShouldAntialias(true)
            // CoreText's origin is the baseline; place it above the descent.
            ctx.textPosition = CGPoint(x: CGFloat(padX), y: descent)
            CTLineDraw(line, ctx)
            return true
        }
        guard ok else {
            os_log(.error, log: log, "glyphBitmap: CGContext creation failed (%dx%d)", width, height)
            return nil
        }
        return GlyphBitmap(bytes: bytes, width: width, height: height)
    }

    /// Composite a caption reading `text` into `pixelBuffer`, bottom-centre, at
    /// a plate height of about 6 % of the frame. Returns false when the frame's
    /// pixel format is not one the badge writer handles (the caller then holds
    /// the frame without a badge).
    @discardableResult
    static func composite(text: String, into pixelBuffer: CVPixelBuffer) -> Bool {
        let width = CVPixelBufferGetWidth(pixelBuffer)
        let height = CVPixelBufferGetHeight(pixelBuffer)
        guard width >= 16, height >= 16 else { return false }
        let format = CVPixelBufferGetPixelFormatType(pixelBuffer)
        guard supports(format) else { return false }

        // Plate ~6 % of frame height (text ~60 % of the plate), centred, sitting
        // a little above the bottom edge.
        let plateHeight = max(16, height * 6 / 100)
        let textHeight = max(8, plateHeight * 3 / 5)
        guard let glyphs = glyphBitmap(text: text, heightPx: textHeight) else { return false }

        let padX = max(4, plateHeight / 3)
        var plateWidth = glyphs.width + 2 * padX
        plateWidth = min(plateWidth, width - 8)
        let plateX = (width - plateWidth) / 2
        let bottomMargin = max(8, height * 4 / 100)
        let plateY = height - plateHeight - bottomMargin
        guard plateX >= 0, plateY >= 0 else { return false }
        let rect = Rect(x: plateX, y: plateY, w: plateWidth, h: plateHeight)
        let radius = min(plateHeight / 2, 24)

        // The glyph is centred within the plate.
        let glyphX = plateX + (plateWidth - glyphs.width) / 2
        let glyphY = plateY + (plateHeight - glyphs.height) / 2

        CVPixelBufferLockBaseAddress(pixelBuffer, [])
        defer { CVPixelBufferUnlockBaseAddress(pixelBuffer, []) }

        switch format {
        case kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange:
            compositeNV12(pixelBuffer, rect: rect, radius: radius, glyphs: glyphs, glyphX: glyphX, glyphY: glyphY)
        case kCVPixelFormatType_422YpCbCr8:  // '2vuy' UYVY
            compositePacked422(
                pixelBuffer, rect: rect, radius: radius, glyphs: glyphs, glyphX: glyphX, glyphY: glyphY,
                lumaOffset: 1)
        case kCVPixelFormatType_422YpCbCr8_yuvs:  // 'yuvs' YUY2
            compositePacked422(
                pixelBuffer, rect: rect, radius: radius, glyphs: glyphs, glyphX: glyphX, glyphY: glyphY,
                lumaOffset: 0)
        case kCVPixelFormatType_32BGRA:
            compositeBGRA(pixelBuffer, rect: rect, radius: radius, glyphs: glyphs, glyphX: glyphX, glyphY: glyphY)
        default:
            return false
        }
        return true
    }

    /// Whether [`composite`] can draw a badge in `format`.
    static func supports(_ format: OSType) -> Bool {
        switch format {
        case kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            kCVPixelFormatType_422YpCbCr8,
            kCVPixelFormatType_422YpCbCr8_yuvs,
            kCVPixelFormatType_32BGRA:
            return true
        default:
            return false
        }
    }

    // MARK: Geometry

    private struct Rect {
        let x: Int
        let y: Int
        let w: Int
        let h: Int
    }

    /// Coverage (0…1) of the rounded-rect plate at plate-local `(lx, ly)`,
    /// anti-aliased one pixel at the rounded corners.
    private static func plateCoverage(lx: Int, ly: Int, rect: Rect, radius: Int) -> Double {
        if radius <= 0 { return 1.0 }
        let r = Double(radius)
        // Distance into the nearest corner's quarter-disc; full coverage away
        // from the corners.
        let dxLeft = Double(radius - lx)
        let dxRight = Double(lx - (rect.w - 1 - radius))
        let dyTop = Double(radius - ly)
        let dyBottom = Double(ly - (rect.h - 1 - radius))
        let dx = max(0.0, max(dxLeft, dxRight))
        let dy = max(0.0, max(dyTop, dyBottom))
        if dx == 0 || dy == 0 { return 1.0 }
        let dist = (dx * dx + dy * dy).squareRoot()
        return max(0.0, min(1.0, r - dist + 0.5))
    }

    /// The badge luma and total coverage at plate-local `(lx, ly)`: the dark
    /// plate under an optional glyph, both anti-aliased. `glyph…` place the
    /// coverage bitmap within the plate.
    private static func sample(
        lx: Int, ly: Int, rect: Rect, radius: Int, glyphs: GlyphBitmap, glyphOffsetX: Int, glyphOffsetY: Int
    ) -> (luma: Double, coverage: Double) {
        let plate = plateCoverage(lx: lx, ly: ly, rect: rect, radius: radius)
        if plate <= 0 { return (0, 0) }
        var luma = backingLuma
        var coverage = plate * backingAlpha
        let gx = lx - glyphOffsetX
        let gy = ly - glyphOffsetY
        if gx >= 0, gx < glyphs.width, gy >= 0, gy < glyphs.height {
            // A CGBitmapContext stores memory top-down (row 0 is the visual top)
            // while drawing with a bottom-left origin, so the rendered glyph band
            // is already upright in memory: row `gy` from the top maps directly.
            let g = Double(glyphs.bytes[gy * glyphs.width + gx]) / 255.0
            if g > 0 {
                luma = luma * (1 - g) + textLuma * g
                coverage = max(coverage, g)
            }
        }
        return (min(textLuma, max(0, luma)), min(1.0, coverage))
    }

    // MARK: Per-format writers

    private static func compositeNV12(
        _ pb: CVPixelBuffer, rect: Rect, radius: Int, glyphs: GlyphBitmap, glyphX: Int, glyphY: Int
    ) {
        guard let yBase = CVPixelBufferGetBaseAddressOfPlane(pb, 0),
            let uvBase = CVPixelBufferGetBaseAddressOfPlane(pb, 1)
        else { return }
        let yStride = CVPixelBufferGetBytesPerRowOfPlane(pb, 0)
        let uvStride = CVPixelBufferGetBytesPerRowOfPlane(pb, 1)
        let uvRows = CVPixelBufferGetHeightOfPlane(pb, 1)
        let y = yBase.assumingMemoryBound(to: UInt8.self)
        let uv = uvBase.assumingMemoryBound(to: UInt8.self)
        for ly in 0..<rect.h {
            let py = rect.y + ly
            for lx in 0..<rect.w {
                let px = rect.x + lx
                let s = sample(
                    lx: lx, ly: ly, rect: rect, radius: radius, glyphs: glyphs, glyphOffsetX: glyphX - rect.x,
                    glyphOffsetY: glyphY - rect.y)
                if s.coverage <= 0 { continue }
                let idx = py * yStride + px
                let orig = Double(y[idx])
                y[idx] = clampVideo(orig * (1 - s.coverage) + s.luma * s.coverage)
                // Neutral chroma under the plate; NV12 chroma is half-res.
                let cx = (px / 2) * 2
                let cy = py / 2
                if cy < uvRows {
                    let uvIdx = cy * uvStride + cx
                    uv[uvIdx] = 128
                    uv[uvIdx + 1] = 128
                }
            }
        }
    }

    /// Packed 4:2:2: `lumaOffset` 1 is UYVY ('2vuy'), 0 is YUY2 ('yuvs'). Each
    /// 4-byte group holds two luma samples and one shared U/V pair.
    private static func compositePacked422(
        _ pb: CVPixelBuffer, rect: Rect, radius: Int, glyphs: GlyphBitmap, glyphX: Int, glyphY: Int,
        lumaOffset: Int
    ) {
        guard let base = CVPixelBufferGetBaseAddress(pb) else { return }
        let stride = CVPixelBufferGetBytesPerRow(pb)
        let p = base.assumingMemoryBound(to: UInt8.self)
        let chromaOffset = lumaOffset == 0 ? 1 : 0  // the other two bytes of the group
        for ly in 0..<rect.h {
            let py = rect.y + ly
            let row = py * stride
            for lx in 0..<rect.w {
                let px = rect.x + lx
                let s = sample(
                    lx: lx, ly: ly, rect: rect, radius: radius, glyphs: glyphs, glyphOffsetX: glyphX - rect.x,
                    glyphOffsetY: glyphY - rect.y)
                if s.coverage <= 0 { continue }
                let yIdx = row + px * 2 + lumaOffset
                let orig = Double(p[yIdx])
                p[yIdx] = clampVideo(orig * (1 - s.coverage) + s.luma * s.coverage)
                // Neutral chroma for the 2-pixel group.
                let group = row + (px / 2) * 4
                p[group + chromaOffset] = 128
                p[group + chromaOffset + 2] = 128
            }
        }
    }

    private static func compositeBGRA(
        _ pb: CVPixelBuffer, rect: Rect, radius: Int, glyphs: GlyphBitmap, glyphX: Int, glyphY: Int
    ) {
        guard let base = CVPixelBufferGetBaseAddress(pb) else { return }
        let stride = CVPixelBufferGetBytesPerRow(pb)
        let p = base.assumingMemoryBound(to: UInt8.self)
        for ly in 0..<rect.h {
            let py = rect.y + ly
            let row = py * stride
            for lx in 0..<rect.w {
                let px = rect.x + lx
                let s = sample(
                    lx: lx, ly: ly, rect: rect, radius: radius, glyphs: glyphs, glyphOffsetX: glyphX - rect.x,
                    glyphOffsetY: glyphY - rect.y)
                if s.coverage <= 0 { continue }
                let idx = row + px * 4
                let grey = UInt8(max(0, min(255, s.luma)))
                for c in 0..<3 {  // B, G, R
                    let orig = Double(p[idx + c])
                    p[idx + c] = UInt8(max(0, min(255, orig * (1 - s.coverage) + Double(grey) * s.coverage)))
                }
            }
        }
    }

    /// Clamp to BT.709 video-range luma (16…235).
    private static func clampVideo(_ v: Double) -> UInt8 {
        UInt8(max(16, min(235, v.rounded())))
    }
}
