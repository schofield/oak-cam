#!/usr/bin/env bash
#
# build-macos.sh — build the oak-cam CoreMediaIO camera extension with swiftc
# alone (no Xcode project), assemble oak-cam.app around the oak-cam binary with
# the extension embedded, sign both, and (without --check) install the app to
# /Applications. Ported from stream-concern's scripts/vcam/build-macos.sh and
# the camera-extension half of its scripts/release/bundle-macos.sh.
#
# Produces:
#   <out>/oak-cam.app/Contents/
#     Info.plist                          scripts/vcam/macos/Info-app.plist.in
#     MacOS/oak-cam                       cargo build --release -p oak-cam
#     Resources/oak-cam.icns              assets/icon/oak-cam.icns
#     Frameworks/libdepthai-core.dylib    + libusb, from the depthai install
#     Library/SystemExtensions/com.surlysoft.oak-cam.camera-extension.systemextension
#       Contents/Info.plist               extensions/oak-camera-extension/Info.plist.in
#       Contents/MacOS/<bundle id>        the Swift executable (universal when
#                                         the x86_64 slice compiles)
#       Contents/embedded.provisionprofile   only with --vcam-provisionprofile
#     embedded.provisionprofile           only with --provisionprofile
#
# Options:
#   --check                build, test and sign everything into <out>, verify
#                          the signatures, but do not install (CI and local
#                          checks). Also runs the Swift host test.
#   --test                 build and run the Swift host test only (placeholder,
#                          format table, arbiter, badge, held frame, the
#                          OakCameras plist parse); no signing.
#   --typecheck            swiftc -typecheck the extension sources only.
#   --out <dir>            output directory (default dist).
#   --identity "<ID>"      codesign identity. Default: the "Developer ID
#                          Application: … (5V2V8BX632)" identity when it is in
#                          the keychain (stream-concern's team), else "-"
#                          (ad-hoc, with a warning: ad-hoc system extensions only
#                          activate with SIP off + `systemextensionsctl developer on`).
#   --team-id <TEAMID>     team prefix for the mach service name and app group.
#                          Default: parsed from --identity, else $OAK_TEAM_ID,
#                          else 5V2V8BX632.
#   --provisionprofile <p> embed <p> in the app and add the restricted
#                          com.apple.developer.system-extension.install
#                          entitlement + the team app group. Without it the app
#                          is built but CANNOT install its extension (AMFI kills
#                          an unprovisioned app that carries the entitlement).
#   --vcam-provisionprofile <p>  embed <p> in the extension.
#   --notarize <profile>   notarytool keychain profile: zip, submit --wait,
#                          staple. Needs a real --identity. macOS refuses to
#                          activate a Developer ID system extension from an app
#                          without a notarisation ticket.
#   --version <x.y.z>      CFBundleShortVersionString (default: Cargo.toml).
#   --build <n>            CFBundleVersion (default <major>.<minor>.<git commit
#                          count>). A rebuilt extension MUST carry a higher
#                          CFBundleVersion or macOS keeps running the old one;
#                          pass --build when reinstalling uncommitted changes.
#   --arch <arm64|x86_64|universal>   extension slices (default universal,
#                          falling back to the host arch).
#   --binary <path>        use this oak-cam binary instead of cargo build.
#   --install-dir <dir>    where to install the app (default /Applications).
#
# The extension's bundle id is fixed (the app activates it by id) and its
# device/stream UUIDs live in Info.plist.in (OakCameras); crates/vcam pins the
# same UUIDs and a unit test checks the two agree.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SRC_DIR="$REPO_ROOT/extensions/oak-camera-extension"

APP_NAME="oak-cam"
APP_BUNDLE_ID="com.surlysoft.oak-cam"
BUNDLE_ID="com.surlysoft.oak-cam.camera-extension"
MIN_MACOS="14.0"
DEFAULT_TEAM_ID="5V2V8BX632"

CHECK_ONLY=0
TEST_ONLY=0
TYPECHECK_ONLY=0
OUT="$REPO_ROOT/dist"
IDENTITY=""
TEAM_ID=""
VERSION=""
BUILD=""
ARCH="universal"
PROFILE=""
VCAM_PROFILE=""
NOTARIZE_PROFILE=""
BINARY=""
INSTALL_DIR="/Applications"

die() { echo "error: $*" >&2; exit 1; }
say() { echo ">> $*"; }
warn() { echo ">> WARNING: $*" >&2; }
usage() { sed -n '2,61p' "$0"; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --check)            CHECK_ONLY=1; shift ;;
        --test)             TEST_ONLY=1; shift ;;
        --typecheck)        TYPECHECK_ONLY=1; shift ;;
        --out)              OUT="${2:?--out needs a directory}"; shift 2 ;;
        --identity)         IDENTITY="${2:?--identity needs a value}"; shift 2 ;;
        --team-id)          TEAM_ID="${2:?--team-id needs a value}"; shift 2 ;;
        --version)          VERSION="${2:?--version needs a value}"; shift 2 ;;
        --build)            BUILD="${2:?--build needs a value}"; shift 2 ;;
        --arch)             ARCH="${2:?--arch needs a value}"; shift 2 ;;
        --provisionprofile) PROFILE="${2:?--provisionprofile needs a path}"; shift 2 ;;
        --vcam-provisionprofile) VCAM_PROFILE="${2:?--vcam-provisionprofile needs a path}"; shift 2 ;;
        --notarize)         NOTARIZE_PROFILE="${2:?--notarize needs a keychain profile}"; shift 2 ;;
        --binary)           BINARY="${2:?--binary needs a path}"; shift 2 ;;
        --install-dir)      INSTALL_DIR="${2:?--install-dir needs a directory}"; shift 2 ;;
        -h|--help)          usage; exit 0 ;;
        *) echo "error: unknown argument '$1' (try --help)" >&2; exit 2 ;;
    esac
done

for tool in swiftc xcrun plutil codesign lipo otool install_name_tool; do
    command -v "$tool" >/dev/null || die "$tool not found (install the Xcode command line tools)"
done

SDK="$(xcrun --sdk macosx --show-sdk-path)"
[[ -d "$SDK" ]] || die "macOS SDK not found via xcrun"

SOURCES=("$SRC_DIR"/Sources/*.swift)
[[ ${#SOURCES[@]} -gt 0 && -f "${SOURCES[0]}" ]] || die "no Swift sources under $SRC_DIR/Sources"

# Frameworks the extension links. The Swift runtime comes from /usr/lib/swift
# on every macOS this targets, so nothing is embedded.
SWIFT_COMMON=(
    -swift-version 5
    -sdk "$SDK"
    -module-name OakCameraExtension
    -framework Foundation
    -framework CoreMedia
    -framework CoreVideo
    -framework CoreMediaIO
    -framework CoreGraphics
    -framework CoreText
)

BUILD_DIR="$OUT/build"

# Host-side test: compile the CMIO-free sources with Tests/main.swift and run
# it against Info.plist.in. No signing, no CMIO provider, no installed
# extension. It asserts the placeholder sample buffer's format description
# matches its image buffer at every advertised NV12 size (the pairing
# CMSampleBufferCreateForImageBuffer rejects with -12743), the per-kind format
# tables, the FrameArbiter states and monotonic clock, held-frame construction,
# CoreText badge coverage, and that OakCameras parses into three cameras.
run_host_test() {
    local test_main="$SRC_DIR/Tests/main.swift"
    [[ -f "$test_main" ]] || die "host test source missing at $test_main"
    local host_arch
    host_arch="$(uname -m)"
    [[ "$host_arch" == "aarch64" ]] && host_arch="arm64"
    mkdir -p "$BUILD_DIR"
    local bin="$BUILD_DIR/extension-host-test"
    say "building the extension host test ($host_arch, macOS $MIN_MACOS)"
    swiftc -O "${SWIFT_COMMON[@]}" -target "$host_arch-apple-macos$MIN_MACOS" \
        "$SRC_DIR/Sources/FormatTable.swift" \
        "$SRC_DIR/Sources/ExtensionIdentity.swift" \
        "$SRC_DIR/Sources/FrameArbiter.swift" \
        "$SRC_DIR/Sources/Badge.swift" \
        "$SRC_DIR/Sources/Placeholder.swift" \
        "$SRC_DIR/Sources/HeldFrame.swift" \
        "$test_main" -o "$bin"
    say "running the extension host test"
    "$bin" "$SRC_DIR/Info.plist.in"
    say "extension host test passed"
}

typecheck() {
    say "type-checking ${#SOURCES[@]} Swift sources (arm64, macOS $MIN_MACOS)"
    swiftc -typecheck "${SWIFT_COMMON[@]}" -target "arm64-apple-macos$MIN_MACOS" "${SOURCES[@]}"
    say "camera extension sources type-check"
}

if [[ "$TYPECHECK_ONLY" == "1" ]]; then
    typecheck
    exit 0
fi
if [[ "$TEST_ONLY" == "1" ]]; then
    run_host_test
    exit 0
fi

# ----------------------------------------------------------------------------
# identity / version / team
# ----------------------------------------------------------------------------
if [[ -z "$IDENTITY" ]]; then
    IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null \
        | sed -n "s/.*\"\(Developer ID Application: .*($DEFAULT_TEAM_ID)\)\"$/\1/p" | head -1)"
    if [[ -z "$IDENTITY" ]]; then
        IDENTITY="-"
        warn "no 'Developer ID Application: … ($DEFAULT_TEAM_ID)' identity in the keychain; signing ad-hoc."
        warn "An ad-hoc system extension only activates with SIP off and"
        warn "'systemextensionsctl developer on'; pass --identity for a real build."
    fi
fi

if [[ -z "$VERSION" ]]; then
    VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)"
    [[ -n "$VERSION" ]] || die "could not read version from Cargo.toml"
fi
# Apple caps CFBundleVersion at three integers, so the commit count is the
# third field; it only grows with history, so a rebuilt extension from a later
# commit replaces the installed one.
if [[ -z "$BUILD" ]]; then
    if count="$(git -C "$REPO_ROOT" rev-list --count HEAD 2>/dev/null)" && [[ -n "$count" ]]; then
        BUILD="$(echo "$VERSION" | cut -d. -f1,2).$count"
        if [[ -n "$(git -C "$REPO_ROOT" status --porcelain 2>/dev/null)" ]]; then
            warn "uncommitted changes: CFBundleVersion $BUILD may equal an installed build;"
            warn "pass --build <higher n> before installing a changed extension."
        fi
    else
        BUILD="$VERSION"
    fi
fi

if [[ -z "$TEAM_ID" ]]; then
    if [[ "$IDENTITY" =~ \(([A-Z0-9]{10})\)$ ]]; then
        TEAM_ID="${BASH_REMATCH[1]}"
    else
        TEAM_ID="${OAK_TEAM_ID:-$DEFAULT_TEAM_ID}"
    fi
fi
[[ "$TEAM_ID" =~ ^[A-Z0-9]{10}$ ]] || die "team id '$TEAM_ID' is not a 10-character Apple team identifier"
[[ -z "$NOTARIZE_PROFILE" || "$IDENTITY" != "-" ]] || die "--notarize needs a real --identity"
say "$APP_NAME $VERSION ($BUILD)   team $TEAM_ID   identity: $IDENTITY"

SIGN_COMMON=(--force --options runtime --timestamp)
if [[ "$IDENTITY" == "-" ]]; then
    SIGN_COMMON=(--force --options runtime)
fi

rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"

typecheck
run_host_test

# ----------------------------------------------------------------------------
# 1. the camera extension (one slice per arch, then lipo)
# ----------------------------------------------------------------------------
HOST_ARCH="$(uname -m)"
[[ "$HOST_ARCH" == "aarch64" ]] && HOST_ARCH="arm64"
case "$ARCH" in
    universal) ARCHES=(arm64 x86_64) ;;
    arm64|x86_64) ARCHES=("$ARCH") ;;
    *) die "--arch must be arm64, x86_64 or universal" ;;
esac

SLICES=()
for arch in "${ARCHES[@]}"; do
    slice="$BUILD_DIR/$arch/$BUNDLE_ID"
    mkdir -p "$BUILD_DIR/$arch"
    say "compiling the extension $arch slice"
    if swiftc -O "${SWIFT_COMMON[@]}" -target "$arch-apple-macos$MIN_MACOS" "${SOURCES[@]}" -o "$slice"; then
        SLICES+=("$slice")
    elif [[ "$ARCH" == "universal" && "$arch" != "$HOST_ARCH" ]]; then
        say "  $arch slice did not compile; building the host arch only"
    else
        die "swiftc failed for $arch"
    fi
done
[[ ${#SLICES[@]} -gt 0 ]] || die "no slice compiled"

EXT="$BUILD_DIR/$BUNDLE_ID.systemextension"
EXT_CONTENTS="$EXT/Contents"
mkdir -p "$EXT_CONTENTS/MacOS"
if [[ ${#SLICES[@]} -gt 1 ]]; then
    lipo -create "${SLICES[@]}" -output "$EXT_CONTENTS/MacOS/$BUNDLE_ID"
else
    install -m 0755 "${SLICES[0]}" "$EXT_CONTENTS/MacOS/$BUNDLE_ID"
fi
say "extension slices: $(lipo -archs "$EXT_CONTENTS/MacOS/$BUNDLE_ID")"

sed -e "s|@BUNDLE_ID@|$BUNDLE_ID|g" \
    -e "s|@VERSION@|$VERSION|g" \
    -e "s|@BUILD@|$BUILD|g" \
    -e "s|@MIN_MACOS@|$MIN_MACOS|g" \
    -e "s|@TEAM_ID@|$TEAM_ID|g" \
    "$SRC_DIR/Info.plist.in" > "$EXT_CONTENTS/Info.plist"
plutil -lint "$EXT_CONTENTS/Info.plist" >/dev/null || die "generated extension Info.plist failed plutil -lint"

EXT_ENTITLEMENTS="$BUILD_DIR/entitlements-extension.plist"
sed -e "s|@TEAM_ID@|$TEAM_ID|g" "$SRC_DIR/entitlements.plist.in" > "$EXT_ENTITLEMENTS"
plutil -lint "$EXT_ENTITLEMENTS" >/dev/null || die "generated extension entitlements failed plutil -lint"

if [[ -n "$VCAM_PROFILE" ]]; then
    [[ -f "$VCAM_PROFILE" ]] || die "extension provisioning profile not found: $VCAM_PROFILE"
    cp "$VCAM_PROFILE" "$EXT_CONTENTS/embedded.provisionprofile"
fi

say "signing the extension"
codesign "${SIGN_COMMON[@]}" --entitlements "$EXT_ENTITLEMENTS" --sign "$IDENTITY" "$EXT"
codesign --verify --strict "$EXT" || die "codesign --verify failed on the extension"
MACH="$(plutil -extract CMIOExtension.CMIOExtensionMachServiceName raw -o - "$EXT_CONTENTS/Info.plist")"
[[ "$MACH" == "$TEAM_ID.$BUNDLE_ID" ]] || die "mach service name '$MACH' != '$TEAM_ID.$BUNDLE_ID'"
codesign -d --entitlements - --xml "$EXT" 2>/dev/null > "$BUILD_DIR/signed-extension-entitlements.plist"
GROUP="$(/usr/libexec/PlistBuddy -c 'Print :com.apple.security.application-groups:0' "$BUILD_DIR/signed-extension-entitlements.plist")"
[[ "$MACH" == "$GROUP"* ]] || die "mach service name '$MACH' is not prefixed by app group '$GROUP'"
CAMS="$(/usr/libexec/PlistBuddy -c 'Print :OakCameras' "$EXT_CONTENTS/Info.plist" | grep -c 'DeviceUUID')"
[[ "$CAMS" == "3" ]] || die "extension Info.plist declares $CAMS cameras, expected 3"
say "mach service $MACH   app group $GROUP   cameras $CAMS"

# ----------------------------------------------------------------------------
# 2. the oak-cam binary and its depthai libraries
# ----------------------------------------------------------------------------
if [[ -z "$BINARY" ]]; then
    say "cargo build --release -p oak-cam"
    (cd "$REPO_ROOT" && cargo build --release -p oak-cam)
    BINARY="$REPO_ROOT/target/release/oak-cam"
fi
[[ -x "$BINARY" ]] || die "oak-cam binary not found at $BINARY"

# The absolute rpath the build baked in (crates/oak-cam/build.rs) is where
# libdepthai-core lives; bundle every dylib from there.
DEPTHAI_LIBDIR=""
while IFS= read -r rp; do
    if [[ "$rp" == /* && -f "$rp/libdepthai-core.dylib" ]]; then
        DEPTHAI_LIBDIR="$rp"
    fi
done < <(otool -l "$BINARY" | awk '/cmd LC_RPATH/{getline; getline; print $2}')
[[ -n "$DEPTHAI_LIBDIR" ]] || die "cannot find libdepthai-core.dylib via the binary's LC_RPATH entries"

# ----------------------------------------------------------------------------
# 3. assemble oak-cam.app
# ----------------------------------------------------------------------------
APP="$OUT/$APP_NAME.app"
CONTENTS="$APP/Contents"
say "assembling $APP"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Frameworks" "$CONTENTS/Resources" "$CONTENTS/Library/SystemExtensions"
install -m 0755 "$BINARY" "$CONTENTS/MacOS/oak-cam"
# App icon (CFBundleIconFile in Info-app.plist.in); regenerate it with
# scripts/release/build-icons.sh.
ICNS="$REPO_ROOT/assets/icon/oak-cam.icns"
[[ -f "$ICNS" ]] || die "app icon not found at $ICNS (scripts/release/build-icons.sh regenerates it)"
install -m 0644 "$ICNS" "$CONTENTS/Resources/oak-cam.icns"
for lib in "$DEPTHAI_LIBDIR"/*.dylib; do
    [[ -L "$lib" ]] && continue
    install -m 0644 "$lib" "$CONTENTS/Frameworks/"
done
# Drop build-machine rpaths so the bundle only loads its own Frameworks.
while IFS= read -r rp; do
    if [[ "$rp" == /* ]]; then
        install_name_tool -delete_rpath "$rp" "$CONTENTS/MacOS/oak-cam"
    fi
done < <(otool -l "$CONTENTS/MacOS/oak-cam" | awk '/cmd LC_RPATH/{getline; getline; print $2}')
cp -a "$EXT" "$CONTENTS/Library/SystemExtensions/"

sed -e "s|@APP_BUNDLE_ID@|$APP_BUNDLE_ID|g" \
    -e "s|@VERSION@|$VERSION|g" \
    -e "s|@BUILD@|$BUILD|g" \
    -e "s|@MIN_MACOS@|$MIN_MACOS|g" \
    "$SCRIPT_DIR/macos/Info-app.plist.in" > "$CONTENTS/Info.plist"
printf 'APPL????' > "$CONTENTS/PkgInfo"
plutil -lint "$CONTENTS/Info.plist" >/dev/null || die "generated app Info.plist failed plutil -lint"

APP_ENTITLEMENTS="$BUILD_DIR/entitlements-app.plist"
plutil -convert xml1 -o "$APP_ENTITLEMENTS" "$SCRIPT_DIR/macos/entitlements-app.plist" \
    || die "could not convert the app entitlements"
INSTALLABLE=0
if [[ -n "$PROFILE" ]]; then
    [[ -f "$PROFILE" ]] || die "provisioning profile not found: $PROFILE"
    cp "$PROFILE" "$CONTENTS/embedded.provisionprofile"
    /usr/libexec/PlistBuddy \
        -c "Add :com.apple.developer.system-extension.install bool true" \
        -c "Add :com.apple.security.application-groups array" \
        -c "Add :com.apple.security.application-groups:0 string $TEAM_ID.$APP_BUNDLE_ID" \
        "$APP_ENTITLEMENTS" >/dev/null || die "could not add the system-extension entitlements"
    INSTALLABLE=1
    say "app entitlements include com.apple.developer.system-extension.install + app group $TEAM_ID.$APP_BUNDLE_ID"
else
    warn "no --provisionprofile: leaving out com.apple.developer.system-extension.install"
    warn "(AMFI kills an unprovisioned app that carries it), so this app CANNOT install"
    warn "its camera extension; 'oak-cam vcam install' will fail with a missing-entitlement error."
fi
# Ad-hoc signatures carry no Team ID, so hardened-runtime library validation
# refuses the ad-hoc libdepthai-core ("mapping process and mapped file have
# different Team IDs") and dyld aborts. stream-concern's app entitlements carry
# disable-library-validation for the same reason; add it only for ad-hoc
# builds so Developer ID builds keep library validation.
if [[ "$IDENTITY" == "-" ]]; then
    /usr/libexec/PlistBuddy \
        -c "Add :com.apple.security.cs.disable-library-validation bool true" \
        "$APP_ENTITLEMENTS" >/dev/null || die "could not add disable-library-validation"
    say "ad-hoc build: app entitlements include com.apple.security.cs.disable-library-validation"
fi
plutil -lint "$APP_ENTITLEMENTS" >/dev/null || die "generated app entitlements failed plutil -lint"

# ----------------------------------------------------------------------------
# 4. sign inside-out: dylibs, main executable, app seal (no --deep: the
#    extension keeps the signature and entitlements it was given above)
# ----------------------------------------------------------------------------
say "signing the depthai dylibs"
for lib in "$CONTENTS/Frameworks"/*.dylib; do
    codesign "${SIGN_COMMON[@]}" --sign "$IDENTITY" "$lib"
done
say "signing the main executable and sealing the app"
codesign "${SIGN_COMMON[@]}" --entitlements "$APP_ENTITLEMENTS" --sign "$IDENTITY" "$CONTENTS/MacOS/oak-cam"
codesign "${SIGN_COMMON[@]}" --entitlements "$APP_ENTITLEMENTS" --sign "$IDENTITY" "$APP"

codesign --verify --deep --strict "$APP" || die "codesign --verify --deep --strict failed on $APP"
"$CONTENTS/MacOS/oak-cam" --version >/dev/null || die "the bundled oak-cam does not launch"
say "bundle verified; bundled oak-cam launches ($("$CONTENTS/MacOS/oak-cam" --version))"

# ----------------------------------------------------------------------------
# 5. notarisation (only with --notarize)
# ----------------------------------------------------------------------------
if [[ -n "$NOTARIZE_PROFILE" ]]; then
    ZIP="$BUILD_DIR/$APP_NAME-$VERSION-notarize.zip"
    say "notarising with keychain profile $NOTARIZE_PROFILE"
    /usr/bin/ditto -c -k --keepParent "$APP" "$ZIP"
    xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARIZE_PROFILE" --wait
    xcrun stapler staple "$APP"
    xcrun stapler validate "$APP"
fi

echo
echo "================================================================"
echo "  built:     $APP"
echo "  identity:  $IDENTITY"
echo "  extension: $BUNDLE_ID $VERSION ($BUILD)"
if [[ "$INSTALLABLE" == "1" ]]; then
    echo "  install entitlement: yes (provisioned)"
else
    echo "  install entitlement: NO (no --provisionprofile; cannot activate the extension)"
fi
[[ -n "$NOTARIZE_PROFILE" ]] && echo "  notarised: yes" || echo "  notarised: no"
echo "================================================================"

if [[ "$CHECK_ONLY" == "1" ]]; then
    exit 0
fi

# ----------------------------------------------------------------------------
# 6. install (system extensions only activate from /Applications unless
#    developer mode is on)
# ----------------------------------------------------------------------------
DEST="$INSTALL_DIR/$APP_NAME.app"
say "installing $DEST"
if [[ -e "$DEST" ]]; then
    mv "$DEST" "$HOME/.Trash/$APP_NAME-$(date +%Y%m%d-%H%M%S).app" || die "cannot move the old $DEST to the Trash"
fi
/usr/bin/ditto "$APP" "$DEST"
echo
echo "Next:"
echo "  $DEST/Contents/MacOS/oak-cam vcam install"
echo "  approve it under System Settings › General › Login Items & Extensions › Camera Extensions"
echo "  $DEST/Contents/MacOS/oak-cam vcam status"
