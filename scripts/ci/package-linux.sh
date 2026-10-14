#!/usr/bin/env bash
#
# package-linux.sh - assemble the native Linux test package for oak-cam,
# smoke-test a relocated copy, tar.gz it, and write SHA256SUMS.txt +
# BUILD-INFO.json. Called by .github/workflows/package.yml after
# `cargo build --release -p oak-cam`; works the same from a shell.
#
# Layout (flat; oak-cam carries an $ORIGIN rpath, crates/oak-cam/build.rs):
#   oak-cam-<version>-linux-x86_64-<sha7>/
#     oak-cam                      the release binary
#     libdepthai-core.so* ...      every shared library from the depthai install's lib/
#     80-movidius.rules            udev rule for the OAK (USB vendor 03e7)
#     oak-cam-v4l2loopback.conf    modprobe options for three loopback devices
#     LICENSE, THIRD_PARTY_LICENSES.md, README.txt, BUILD-INFO.json
#
# Output: dist/pkg-staging/{<pkg>.tar.gz, SHA256SUMS.txt, BUILD-INFO.json};
# sha7/version/archive as step outputs when $GITHUB_OUTPUT is set.
set -euo pipefail

ws="${GITHUB_WORKSPACE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
root="${OAK_DEPTHAI_ROOT:-$HOME/.cache/oak-cam/depthai}"
prefix="$(readlink -f "$root/current")"
exe="$ws/target/release/oak-cam"

die() { echo "error: $*" >&2; exit 1; }
say() { echo ">> $*"; }

[ -x "$exe" ] || die "release binary not found: $exe (cargo build --release -p oak-cam)"
[ -d "$prefix/lib" ] || die "depthai install not found under $root/current (scripts/depthai/build.sh)"

# --- version ([workspace.package] in Cargo.toml) + commit ---------------------
version="$(sed -n '/^\[workspace\.package\]/,/^\[/{ s/^[[:space:]]*version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p; }' "$ws/Cargo.toml" | head -1)"
[ -n "$version" ] || die "could not parse [workspace.package] version from Cargo.toml"
sha="$(git -C "$ws" rev-parse HEAD)"
sha7="${sha:0:7}"
depthai_version="$(sed -n 's/^[[:space:]]*"version": "\([^"]*\)".*/\1/p' "$prefix/oak-flags.json" | head -1)"

pkg="oak-cam-${version}-linux-x86_64-${sha7}"
stage="$ws/dist/pkg-staging"
pkgdir="$stage/$pkg"
rm -rf "$stage"
mkdir -p "$pkgdir"
say "assembling $pkg"

install -m 0755 "$exe" "$pkgdir/oak-cam"

# --- depthai shared libraries (keep the version symlinks) ---------------------
shopt -s nullglob
count=0
for so in "$prefix"/lib/*.so*; do
    cp -a "$so" "$pkgdir/"
    count=$((count + 1))
done
shopt -u nullglob
[ "$count" -gt 0 ] || die "no shared libraries under $prefix/lib"
say "copied $count library entries from $prefix/lib"

# The build also records the absolute install libdir as an rpath (for
# `cargo run`); a package only needs $ORIGIN.
if command -v patchelf >/dev/null 2>&1; then
    # shellcheck disable=SC2016  # $ORIGIN is for the loader, not the shell
    patchelf --set-rpath '$ORIGIN' "$pkgdir/oak-cam"
    say "rpath: $(patchelf --print-rpath "$pkgdir/oak-cam")"
else
    echo "::warning::patchelf not found; oak-cam keeps the build machine's absolute rpath next to \$ORIGIN" >&2
fi

# --- udev rule and modprobe options -------------------------------------------
# TODO(M8): move both to packaging/linux/ and the
# modprobe options to what scripts/vcam/linux-setup.sh (M4b) settles on.
if [ -f "$ws/packaging/linux/80-movidius.rules" ]; then
    cp "$ws/packaging/linux/80-movidius.rules" "$pkgdir/"
else
    cat > "$pkgdir/80-movidius.rules" <<'EOF'
# Luxonis OAK (Movidius Myriad X, USB vendor 03e7): let logged-in users open it.
# Install: sudo cp 80-movidius.rules /etc/udev/rules.d/ && sudo udevadm control --reload-rules && sudo udevadm trigger
SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"
EOF
fi
if [ -f "$ws/packaging/linux/oak-cam-v4l2loopback.conf" ]; then
    cp "$ws/packaging/linux/oak-cam-v4l2loopback.conf" "$pkgdir/"
else
    cat > "$pkgdir/oak-cam-v4l2loopback.conf" <<'EOF'
# PLACEHOLDER until M4b (Linux multi-loopback): three v4l2loopback devices for
# OAK RGB / OAK Left / OAK Right. Install to /etc/modprobe.d/ and add
# v4l2loopback to /etc/modules-load.d/.
options v4l2loopback devices=3 video_nr=10,11,12 card_label="OAK RGB","OAK Left","OAK Right" exclusive_caps=1,1,1
EOF
fi

cp "$ws/LICENSE" "$ws/THIRD_PARTY_LICENSES.md" "$pkgdir/"

rustc_ver="$(rustc --version)"
built="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

cat > "$pkgdir/README.txt" <<EOF
oak-cam $version test build for Linux x86_64
Commit: $sha
depthai-core: ${depthai_version:-unknown}

UNSIGNED, UNTESTED test build from CI.

1. Extract the whole tarball and keep the files together (oak-cam finds the
   bundled libdepthai-core beside itself).
2. Once: sudo cp 80-movidius.rules /etc/udev/rules.d/ &&
   sudo udevadm control --reload-rules && sudo udevadm trigger
3. ./oak-cam --list      ./oak-cam --probe
   ./oak-cam --headless --no-vcam --stats

The v4l2loopback virtual cameras are not finished on Linux yet (M4b);
oak-cam-v4l2loopback.conf is a placeholder.
EOF

cat > "$pkgdir/BUILD-INFO.json" <<EOF
{
  "name": "oak-cam",
  "version": "$version",
  "commit": "$sha",
  "commit_short": "$sha7",
  "os": "linux",
  "target": "x86_64-unknown-linux-gnu",
  "rustc": "$rustc_ver",
  "depthai_core": "${depthai_version:-unknown}",
  "built_utc": "$built",
  "workflow_run_id": "${GITHUB_RUN_ID:-}"
}
EOF

# --- smoke test from a relocated copy (proves the $ORIGIN rpath) --------------
say "smoke test: oak-cam --version from a relocated copy"
reloc="$(mktemp -d)"
trap 'rm -rf "$reloc"' EXIT
cp -a "$pkgdir" "$reloc/"
smoke="$("$reloc/$pkg/oak-cam" --version)" || die "the relocated binary did not run"
echo "   $smoke"
printf '%s\n' "$smoke" | grep -q "$version" || die "--version did not report $version: $smoke"

archive="$pkg.tar.gz"
( cd "$stage" && tar czf "$archive" "$pkg" )
cp "$pkgdir/BUILD-INFO.json" "$stage/BUILD-INFO.json"
( cd "$stage" && sha256sum "$archive" BUILD-INFO.json > SHA256SUMS.txt )
say "wrote $stage/$archive"
cat "$stage/SHA256SUMS.txt"

if [ -n "${GITHUB_OUTPUT:-}" ]; then
    {
        echo "sha7=$sha7"
        echo "version=$version"
        echo "archive=$archive"
    } >> "$GITHUB_OUTPUT"
fi
