#!/usr/bin/env bash
#
# package-macos.sh - wrap a signed, notarised oak-cam.app into the macOS test
# package: verify the signature, the stapled notarisation ticket and the
# Gatekeeper assessment, smoke-test a relocated copy, zip it with ditto (the
# only archiver that keeps the bundle's signatures and ticket intact), and
# write SHA256SUMS.txt + BUILD-INFO.json like package-linux.sh.
#
# The app comes from scripts/vcam/build-macos.sh, which is the signing
# workflow (Developer ID identity + provisioning profile + notarytool
# keychain profile on the build Mac):
#   scripts/vcam/build-macos.sh --check \
#       --provisionprofile <app profile> --notarize <notarytool profile>
#   scripts/ci/package-macos.sh            # reads dist/oak-cam.app
#
# Layout:
#   oak-cam-<version>-macos-<arch>-<sha7>/
#     oak-cam.app                  signed, notarised, stapled (extension inside)
#     LICENSE, THIRD_PARTY_LICENSES.md, README.txt, BUILD-INFO.json
#
# Output: dist/pkg-staging/{<pkg>.zip, <pkg>.dmg with --dmg, SHA256SUMS.txt,
# BUILD-INFO.json}; sha7/version/archive/dmg as step outputs when
# $GITHUB_OUTPUT is set.
#
# Options:
#   --app <path>        the app bundle (default dist/oak-cam.app)
#   --allow-unnotarized skip the stapler/Gatekeeper checks (ad-hoc or
#                       unnotarised builds; the README then says so)
#   --dmg               also write <pkg>.dmg (oak-cam.app + Applications link
#                       + README.txt, UDZO), signed with the app's Developer ID
#                       identity when it has one; listed in SHA256SUMS.txt.
#   --notarize <prof>   with --dmg: notarise the DMG itself with this
#                       notarytool keychain profile and staple it, so
#                       Gatekeeper accepts the downloaded image offline (the
#                       app inside already carries its own ticket).
set -euo pipefail

ws="${GITHUB_WORKSPACE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
app="$ws/dist/oak-cam.app"
allow_unnotarized=0
make_dmg=0
notarize_profile=""

die() { echo "error: $*" >&2; exit 1; }
say() { echo ">> $*"; }

# stapler validate looks the ticket up at Apple's CloudKit ticket service even
# when one is stapled, so a blip there fails an otherwise good bundle; retry
# before giving up, and show stapler's own message on each failure.
stapler_validate() {
    local target="$1" attempt out
    for attempt in 1 2 3; do
        if out="$(xcrun stapler validate "$target" 2>&1)"; then return 0; fi
        echo "   stapler validate failed (attempt $attempt): $(printf '%s\n' "$out" | tail -1)" >&2
        [ "$attempt" -lt 3 ] && sleep 5
    done
    return 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --app) app="${2:?--app needs a path}"; shift 2 ;;
        --allow-unnotarized) allow_unnotarized=1; shift ;;
        --dmg) make_dmg=1; shift ;;
        --notarize) notarize_profile="${2:?--notarize needs a notarytool keychain profile}"; shift 2 ;;
        -h|--help) sed -n '2,34p' "$0"; exit 0 ;;
        *) die "unknown argument '$1' (try --help)" ;;
    esac
done
[ -z "$notarize_profile" ] || [ "$make_dmg" = "1" ] || die "--notarize only applies to --dmg (the app is notarised by build-macos.sh)"

[ "$(uname -s)" = "Darwin" ] || die "package-macos.sh runs on macOS"
for tool in codesign ditto hdiutil lipo plutil shasum spctl xcrun; do
    command -v "$tool" >/dev/null || die "$tool not found"
done
[ -d "$app/Contents/MacOS" ] || die "app bundle not found: $app (scripts/vcam/build-macos.sh --check builds it)"
app="$(cd "$app" && pwd -P)"
exe="$app/Contents/MacOS/oak-cam"
ext="$app/Contents/Library/SystemExtensions/com.surlysoft.oak-cam.camera-extension.systemextension"
ext_exe="$ext/Contents/MacOS/com.surlysoft.oak-cam.camera-extension"
[ -x "$exe" ] || die "no executable at $exe"
[ -d "$ext" ] || die "camera extension missing from the bundle: $ext"
[ -x "$ext_exe" ] || die "camera extension executable missing: $ext_exe"
stage="$ws/dist/pkg-staging"
case "$app" in "$stage"/*) die "--app must not point inside $stage (it is wiped first)" ;; esac

# --- version ([workspace.package] in Cargo.toml) + commit ---------------------
version="$(sed -n '/^\[workspace\.package\]/,/^\[/{ s/^[[:space:]]*version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p; }' "$ws/Cargo.toml" | head -1)"
[ -n "$version" ] || die "could not parse [workspace.package] version from Cargo.toml"
sha="$(git -C "$ws" rev-parse HEAD)"
sha7="${sha:0:7}"
bundle_version="$(plutil -extract CFBundleShortVersionString raw -o - "$app/Contents/Info.plist")"
bundle_build="$(plutil -extract CFBundleVersion raw -o - "$app/Contents/Info.plist")"
[ "$bundle_version" = "$version" ] || die "bundle says $bundle_version, Cargo.toml says $version"
ext_build="$(plutil -extract CFBundleVersion raw -o - "$ext/Contents/Info.plist")"
# build-macos.sh stamps CFBundleVersion as <major>.<minor>.<HEAD commit time>,
# so the bundle must come from the commit this package is labelled with.
expected_build="$(echo "$version" | cut -d. -f1,2).$(git -C "$ws" -c log.showSignature=false show -s --format=%ct HEAD)"
[ "$bundle_build" = "$expected_build" ] \
    || die "bundle CFBundleVersion $bundle_build is not $expected_build for HEAD $sha7: stale dist/oak-cam.app? (rebuild with build-macos.sh)"
if [ -n "$(git -C "$ws" status --porcelain --untracked-files=no 2>/dev/null)" ]; then
    echo "::warning::uncommitted changes in $ws; the package is labelled $sha7 but may not match it" >&2
fi

# Package arch = the oak-cam binary's slices (the extension may be universal).
arch="$(lipo -archs "$exe" | tr ' ' '\n' | sort | paste -sd+ -)"
case "$arch" in
    arm64+x86_64) arch=universal ;;
    arm64|x86_64) ;;
    *) die "unexpected architectures in $exe: $arch" ;;
esac
ext_arch="$(lipo -archs "$ext_exe" | tr ' ' '\n' | sort | paste -sd, -)"

depthai_version=""
root="${OAK_DEPTHAI_ROOT:-$HOME/.cache/oak-cam/depthai}"
if [ -f "$root/current/oak-flags.json" ]; then
    depthai_version="$(sed -n 's/^[[:space:]]*"version": "\([^"]*\)".*/\1/p' "$root/current/oak-flags.json" | head -1)"
fi

# --- signature, ticket, Gatekeeper --------------------------------------------
say "verifying the signature"
codesign --verify --deep --strict "$app" || die "codesign --verify --deep --strict failed on $app"
sig_info="$(codesign -dvv "$app" 2>&1)"
authority="$(printf '%s\n' "$sig_info" | sed -n 's/^Authority=\(Developer ID Application: .*\)$/\1/p' | head -1)"
notarized=no
if [ "$allow_unnotarized" = "1" ]; then
    say "notarisation not required (--allow-unnotarized)"
    if [ -z "$authority" ]; then
        if printf '%s\n' "$sig_info" | grep -q '^Signature=adhoc'; then
            authority="ad-hoc"
        else
            authority="$(printf '%s\n' "$sig_info" | sed -n 's/^Authority=\(.*\)$/\1/p' | head -1)"
        fi
    fi
    if xcrun stapler validate "$app" >/dev/null 2>&1; then
        notarized=yes
        say "a notarisation ticket is stapled after all"
    fi
else
    [ -n "$authority" ] || die "not signed with a Developer ID Application identity (pass --allow-unnotarized for a test build)"
    say "signed by $authority"
    say "validating the stapled notarisation ticket"
    [ -f "$app/Contents/CodeResources" ] || die "no notarisation ticket stapled to $app (build-macos.sh --notarize)"
    stapler_validate "$app" || die "the ticket stapled to $app does not validate (build-macos.sh --notarize)"
    say "Gatekeeper assessment"
    assess="$(spctl --assess --type execute --verbose=2 "$app" 2>&1)" || die "Gatekeeper rejected $app: $assess"
    echo "   $assess" | tr '\n' ' '; echo
    printf '%s\n' "$assess" | grep -q 'source=Notarized Developer ID' || die "Gatekeeper did not see a notarised Developer ID app: $assess"
    notarized=yes
fi
install_entitlement=no
ent_plist="$(mktemp)"
codesign -d --entitlements - --xml "$app" > "$ent_plist" 2>/dev/null || true
if [ "$(/usr/libexec/PlistBuddy -c 'Print :com.apple.developer.system-extension.install' "$ent_plist" 2>/dev/null)" = "true" ]; then
    install_entitlement=yes
fi
rm -f "$ent_plist"
say "extension $ext_build ($ext_arch)   install entitlement: $install_entitlement   notarised: $notarized"

# --- stage ----------------------------------------------------------------
pkg="oak-cam-${version}-macos-${arch}-${sha7}"
pkgdir="$stage/$pkg"
rm -rf "$stage"
mkdir -p "$pkgdir"
say "assembling $pkg"
ditto "$app" "$pkgdir/oak-cam.app"
cp "$ws/LICENSE" "$ws/THIRD_PARTY_LICENSES.md" "$pkgdir/"

rustc_ver="$("${CARGO_HOME:-$HOME/.cargo}/bin/rustc" --version 2>/dev/null || rustc --version 2>/dev/null || echo unknown)"
built="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
macos_ver="$(sw_vers -productVersion)"

if [ "$notarized" = "yes" ]; then
    trust_note="Signed with $authority and notarised by Apple (ticket stapled); Gatekeeper opens it after the usual first-launch confirmation."
else
    trust_note="NOT notarised: macOS will refuse to activate the camera extension from this build. Test build only."
fi
if [ "$install_entitlement" = "yes" ]; then
    ext_note="The app carries the system-extension install entitlement (provisioned), so step 2 works."
else
    ext_note="The app was built WITHOUT a provisioning profile, so step 2 fails with a missing-entitlement error."
fi

cat > "$pkgdir/README.txt" <<EOF
oak-cam $version test build for macOS ($arch)
Commit: $sha
Bundle: oak-cam.app $bundle_version ($bundle_build), camera extension $ext_build ($ext_arch)
depthai-core: ${depthai_version:-unknown}

$trust_note
$ext_note

1. Drag oak-cam.app to /Applications (system extensions only activate from
   there), then open it once.
2. /Applications/oak-cam.app/Contents/MacOS/oak-cam vcam install
   and approve "OAK Cameras" under System Settings > General >
   Login Items & Extensions > Camera Extensions (macOS 14: System Settings >
   Privacy & Security).
3. /Applications/oak-cam.app/Contents/MacOS/oak-cam vcam status   -> active
   /Applications/oak-cam.app/Contents/MacOS/oak-cam --list
   /Applications/oak-cam.app/Contents/MacOS/oak-cam --probe
   Grant Camera access to whatever runs oak-cam (Terminal, or the app) when
   macOS asks. Quit and reopen Photo Booth or any camera app that was
   already running.

Replacing an installed build: macOS keeps the old extension unless the new
one carries a higher CFBundleVersion; this build's is $ext_build.
EOF

cat > "$pkgdir/BUILD-INFO.json" <<EOF
{
  "name": "oak-cam",
  "version": "$version",
  "bundle_version": "$bundle_build",
  "extension_version": "$ext_build",
  "commit": "$sha",
  "commit_short": "$sha7",
  "os": "macos",
  "arch": "$arch",
  "extension_arch": "$ext_arch",
  "signing_authority": "$(printf '%s' "$authority" | sed 's/\\/\\\\/g; s/"/\\"/g')",
  "notarized": $([ "$notarized" = "yes" ] && echo true || echo false),
  "install_entitlement": $([ "$install_entitlement" = "yes" ] && echo true || echo false),
  "rustc": "$rustc_ver",
  "depthai_core": "${depthai_version:-unknown}",
  "built_on_macos": "$macos_ver",
  "built_utc": "$built",
  "workflow_run_id": "${GITHUB_RUN_ID:-}"
}
EOF
# plutil -lint only takes plists; python3 ships with the Xcode command line tools.
python3 -I -c 'import json, sys; json.load(open(sys.argv[1]))' "$pkgdir/BUILD-INFO.json" \
    || die "BUILD-INFO.json is not valid JSON"

# --- smoke test from a relocated copy -----------------------------------------
say "smoke test: oak-cam --version from a relocated copy"
reloc="$(mktemp -d)"
mnt=""
# Detach a still-mounted image (a die or a cancel while it is attached) before
# removing the temp dir, or rm would walk into a read-only volume.
cleanup() {
    if [ -n "$mnt" ] && mount | grep -qF " on $mnt "; then
        hdiutil detach "$mnt" -force -quiet || true
    fi
    rm -rf "$reloc"
}
trap cleanup EXIT
ditto "$pkgdir" "$reloc/$pkg"
smoke="$("$reloc/$pkg/oak-cam.app/Contents/MacOS/oak-cam" --version)" || die "the relocated bundle did not run"
echo "   $smoke"
printf '%s\n' "$smoke" | grep -q "$version" || die "--version did not report $version: $smoke"
codesign --verify --deep --strict "$reloc/$pkg/oak-cam.app" || die "the copied bundle's signature broke"

# --- archive --------------------------------------------------------------
# ditto keeps the Mach-O signatures, _CodeSignature and the stapled ticket
# (Contents/CodeResources) intact; they are plain files, so the resource
# forks and extended attributes (provenance, quarantine) can be left out,
# which also avoids a __MACOSX folder for people who unzip from a shell.
archive="$pkg.zip"
( cd "$stage" && ditto -c -k --norsrc --noextattr --noqtn --keepParent "$pkg" "$archive" )

# What ships is the zip, so check the bundle after extracting it too.
say "verifying the bundle extracted from $archive"
ditto -x -k "$stage/$archive" "$reloc/unzipped"
shipped="$reloc/unzipped/$pkg/oak-cam.app"
[ -d "$shipped" ] || die "the archive did not extract to $pkg/oak-cam.app"
codesign --verify --deep --strict "$shipped" || die "the extracted bundle's signature is broken"
if [ "$notarized" = "yes" ]; then
    # The ticket is the plain file Contents/CodeResources; an offline
    # byte-for-byte comparison with the source app proves the zip kept it.
    cmp -s "$app/Contents/CodeResources" "$shipped/Contents/CodeResources" \
        || die "the extracted bundle's stapled ticket differs from the source app's"
    if ! stapler_validate "$shipped"; then
        echo "::warning::stapler could not validate the extracted copy (ticket identical to the source, which validated; likely Apple's ticket service)" >&2
    fi
    spctl --assess --type execute "$shipped" || die "Gatekeeper rejected the extracted bundle"
fi
"$shipped/Contents/MacOS/oak-cam" --version >/dev/null || die "the extracted oak-cam does not run"
say "extracted bundle verified"

# --- disk image (--dmg) ---------------------------------------------------
# Same recipe as stream-concern's bundle-macos.sh --dmg: the app, an
# Applications link to drag it onto, README.txt; UDZO; signed and notarised
# as its own item so Gatekeeper accepts the download without going online.
dmg=""
dmg_notarized=no
if [ "$make_dmg" = "1" ]; then
    dmg="$pkg.dmg"
    say "building $dmg"
    dmg_stage="$reloc/dmg"
    mkdir -p "$dmg_stage"
    ditto "$pkgdir/oak-cam.app" "$dmg_stage/oak-cam.app"
    ln -s /Applications "$dmg_stage/Applications"
    cp "$pkgdir/README.txt" "$dmg_stage/README.txt"
    rm -f "$stage/$dmg"
    # Spotlight or Finder can hold the new volume for a moment; retry.
    created=0
    for attempt in 1 2 3; do
        if hdiutil create -volname "oak-cam $version" -srcfolder "$dmg_stage" -fs HFS+ -format UDZO "$stage/$dmg" >/dev/null; then
            created=1; break
        fi
        echo "   hdiutil create failed (attempt $attempt); retrying" >&2
        rm -f "$stage/$dmg"; sleep 3
    done
    [ "$created" = "1" ] || die "hdiutil create failed for $dmg"
    case "$authority" in
        "Developer ID Application: "*)
            say "signing the disk image"
            codesign --force --sign "$authority" --timestamp "$stage/$dmg"
            codesign --verify --strict "$stage/$dmg" || die "codesign --verify failed on $dmg"
            if [ -n "$notarize_profile" ]; then
                say "notarising the disk image with keychain profile $notarize_profile"
                submit_log="$reloc/notary-submit.log"
                xcrun notarytool submit "$stage/$dmg" --keychain-profile "$notarize_profile" --wait --timeout 45m 2>&1 | tee "$submit_log" || true
                if ! grep -q '^ *status: Accepted' "$submit_log"; then
                    sub_id="$(sed -n 's/^ *id: //p' "$submit_log" | head -1)"
                    if [ -n "$sub_id" ]; then
                        echo "notary log for submission $sub_id:" >&2
                        xcrun notarytool log "$sub_id" --keychain-profile "$notarize_profile" >&2 || true
                    fi
                    die "Apple did not accept $dmg"
                fi
                xcrun stapler staple "$stage/$dmg"
                stapler_validate "$stage/$dmg" || die "the ticket stapled to $dmg does not validate"
                assess="$(spctl --assess --type open --context context:primary-signature --verbose=2 "$stage/$dmg" 2>&1)" \
                    || die "Gatekeeper rejected $dmg: $assess"
                echo "   $assess" | tr '\n' ' '; echo
                printf '%s\n' "$assess" | grep -q 'source=Notarized Developer ID' || die "Gatekeeper did not see a notarised Developer ID image: $assess"
                dmg_notarized=yes
            else
                echo "::warning::$dmg is signed but not notarised (pass --notarize <profile>); Gatekeeper will warn on download" >&2
            fi
            ;;
        *)
            echo "::warning::$dmg is unsigned (the app is not Developer ID signed)" >&2
            ;;
    esac
    # The image must open and hold the same, still-valid bundle.
    mnt="$reloc/mnt"
    mkdir -p "$mnt"
    hdiutil attach "$stage/$dmg" -mountpoint "$mnt" -nobrowse -readonly -quiet || die "the disk image does not mount"
    if [ -d "$mnt/oak-cam.app" ] && codesign --verify --deep --strict "$mnt/oak-cam.app" && [ -L "$mnt/Applications" ]; then
        say "mounted $dmg: oak-cam.app signature intact, Applications link present"
    else
        hdiutil detach "$mnt" -quiet || true
        die "the mounted image does not hold a valid oak-cam.app and Applications link"
    fi
    detached=0
    for attempt in 1 2 3; do
        if hdiutil detach "$mnt" -quiet; then detached=1; break; fi
        sleep 2
    done
    if [ "$detached" = "0" ]; then
        hdiutil detach "$mnt" -force -quiet || die "could not detach $mnt"
    fi
    mnt=""
    say "dmg: $stage/$dmg (notarised: $dmg_notarized)"
fi

cp "$pkgdir/BUILD-INFO.json" "$stage/BUILD-INFO.json"
( cd "$stage" && shasum -a 256 "$archive" ${dmg:+"$dmg"} BUILD-INFO.json > SHA256SUMS.txt )
say "wrote $stage/$archive"
cat "$stage/SHA256SUMS.txt"

if [ -n "${GITHUB_OUTPUT:-}" ]; then
    {
        echo "sha7=$sha7"
        echo "version=$version"
        echo "archive=$archive"
        echo "dmg=$dmg"
    } >> "$GITHUB_OUTPUT"
fi
