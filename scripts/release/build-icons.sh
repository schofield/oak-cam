#!/usr/bin/env bash
#
# build-icons.sh — regenerate the app icons from assets/icon/oak-cam.svg.
#
# macOS only: it renders the SVG through AppKit (NSImage reads SVG on macOS
# 14+; a small Swift program compiled on the fly draws it into transparent
# bitmaps) and packs the .icns with iconutil. The .ico is written by a small
# python3 (stdlib only) PNG-in-ICO writer. Quick Look (qlmanage -t) is not
# used: it flattens thumbnails onto white and loses the transparent corners.
#
# Usage: scripts/release/build-icons.sh      (no arguments; run from anywhere)
#
# Writes, next to the SVG:
#   assets/icon/oak-cam.icns     macOS bundle icon (scripts/vcam/build-macos.sh)
#   assets/icon/oak-cam.ico      Windows exe icon (crates/oak-cam/build.rs),
#                                16/32/48/64/128/256
#   assets/icon/oak-cam-256.png  window icon (crates/oak-cam/src/gui/mod.rs)
#                                and the README image
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
ICON_DIR="$REPO_ROOT/assets/icon"
SVG="$ICON_DIR/oak-cam.svg"

die() { echo "error: $*" >&2; exit 1; }
say() { echo ">> $*"; }

[[ "$(uname -s)" == "Darwin" ]] || die "build-icons.sh needs macOS (AppKit, iconutil)"
for tool in swiftc iconutil python3; do
    command -v "$tool" >/dev/null || die "$tool not found"
done
[[ -f "$SVG" ]] || die "icon source not found: $SVG"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Render with AppKit: each size is drawn into a cleared RGBA bitmap so the
# margin outside the squircle stays transparent.
cat > "$TMP/render.swift" <<'SWIFT'
import AppKit
let args = CommandLine.arguments
guard args.count > 3, let image = NSImage(contentsOf: URL(fileURLWithPath: args[1])) else {
    fputs("usage: render <svg> <out dir> <size>...; cannot load the SVG\n", stderr)
    exit(1)
}
let outDir = args[2]
for a in args[3...] {
    guard let n = Int(a), n > 0 else { fputs("bad size \(a)\n", stderr); exit(1) }
    guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: n, pixelsHigh: n,
                                     bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                     colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
          let ctx = NSGraphicsContext(bitmapImageRep: rep) else { exit(1) }
    rep.size = NSSize(width: n, height: n)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = ctx
    ctx.cgContext.clear(CGRect(x: 0, y: 0, width: n, height: n))
    ctx.imageInterpolation = .high
    image.draw(in: NSRect(x: 0, y: 0, width: n, height: n), from: .zero, operation: .sourceOver, fraction: 1.0)
    NSGraphicsContext.restoreGraphicsState()
    guard let png = rep.representation(using: .png, properties: [:]) else { exit(1) }
    do { try png.write(to: URL(fileURLWithPath: "\(outDir)/oak-cam-\(n).png")) } catch { exit(1) }
}
SWIFT
swiftc -O -o "$TMP/render" "$TMP/render.swift" >/dev/null || die "cannot compile the renderer"
# CoreSVG logs a harmless error for the SVG's drop-shadow filter.
"$TMP/render" "$SVG" "$TMP" 16 32 48 64 128 256 512 1024 2>/dev/null || die "render failed"
# The corner outside the squircle must be transparent.
python3 - "$TMP/oak-cam-256.png" <<'PY'
import struct, sys, zlib
d = open(sys.argv[1], "rb").read()
i, idat, ct = 8, b"", None
while i < len(d):
    (l,) = struct.unpack(">I", d[i:i + 4]); t = d[i + 4:i + 8]; c = d[i + 8:i + 8 + l]
    if t == b"IHDR": ct = c[9]
    if t == b"IDAT": idat += c
    i += 12 + l
raw = zlib.decompress(idat)
if ct != 6 or raw[4] != 0:
    sys.exit("error: the render is not transparent at the top-left corner")
PY
say "rendered $SVG at 16..1024"

ICONSET="$TMP/oak-cam.iconset"
mkdir -p "$ICONSET"
cp "$TMP/oak-cam-16.png"   "$ICONSET/icon_16x16.png"
cp "$TMP/oak-cam-32.png"   "$ICONSET/icon_16x16@2x.png"
cp "$TMP/oak-cam-32.png"   "$ICONSET/icon_32x32.png"
cp "$TMP/oak-cam-64.png"   "$ICONSET/icon_32x32@2x.png"
cp "$TMP/oak-cam-128.png"  "$ICONSET/icon_128x128.png"
cp "$TMP/oak-cam-256.png"  "$ICONSET/icon_128x128@2x.png"
cp "$TMP/oak-cam-256.png"  "$ICONSET/icon_256x256.png"
cp "$TMP/oak-cam-512.png"  "$ICONSET/icon_256x256@2x.png"
cp "$TMP/oak-cam-512.png"  "$ICONSET/icon_512x512.png"
cp "$TMP/oak-cam-1024.png" "$ICONSET/icon_512x512@2x.png"
iconutil -c icns "$ICONSET" -o "$ICON_DIR/oak-cam.icns"
say "wrote $ICON_DIR/oak-cam.icns"

# ICO: a 6-byte header, one 16-byte directory entry per size, then the PNG
# blobs in the same order. Width/height 0 means 256.
python3 - "$TMP" "$ICON_DIR/oak-cam.ico" <<'PY'
import struct
import sys

tmp, out = sys.argv[1], sys.argv[2]
sizes = [16, 32, 48, 64, 128, 256]
pngs = []
for n in sizes:
    with open(f"{tmp}/oak-cam-{n}.png", "rb") as f:
        pngs.append(f.read())
header = struct.pack('<HHH', 0, 1, len(sizes))
offset = len(header) + 16 * len(sizes)
entries = b""
for n, png in zip(sizes, pngs):
    w = h = 0 if n == 256 else n
    entries += struct.pack('<BBBBHHII', w, h, 0, 0, 1, 32, len(png), offset)
    offset += len(png)
with open(out, "wb") as f:
    f.write(header + entries + b"".join(pngs))
PY
say "wrote $ICON_DIR/oak-cam.ico"

cp "$TMP/oak-cam-256.png" "$ICON_DIR/oak-cam-256.png"
say "wrote $ICON_DIR/oak-cam-256.png"
