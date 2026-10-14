#!/usr/bin/env bash
#
# build.sh — build and install depthai-core (Luxonis) as a shared library for
# oak-cam on macOS and Linux. Windows uses scripts/depthai/fetch-windows.ps1.
#
# Layout (default root ~/.cache/oak-cam/depthai, override with --root or
# $OAK_DEPTHAI_ROOT so `cargo clean` never removes it):
#   <root>/src-<tag>      shallow clone of luxonis/depthai-core at <tag>
#   <root>/build-<tag>    CMake build tree (vcpkg bootstraps itself inside)
#   <root>/<tag>          install prefix (lib/, include/, lib/cmake/depthai, oak-flags.json)
#   <root>/current        symlink to the install prefix crates/depthai-sys/build.rs uses
#
# Options:
#   --tag <vX.Y.Z>   depthai-core tag (default: the pinned DEPTHAI_TAG below)
#   --root <dir>     cache root (default: $OAK_DEPTHAI_ROOT or ~/.cache/oak-cam/depthai)
#   --jobs <n>       parallel jobs (default: all cores)
#   --clean          remove the build tree first
#   --skip-probe     do not run the flags probe after install
#
# The exact CMake options are pinned here on purpose: crates/depthai-sys is
# compiled against this configuration, and changing an option that alters the
# public headers (OpenCV, xtensor) changes the ABI the shim is built against.
set -euo pipefail

DEPTHAI_TAG="v3.10.0"
DEPTHAI_REPO="https://github.com/luxonis/depthai-core"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="${OAK_DEPTHAI_ROOT:-$HOME/.cache/oak-cam/depthai}"
TAG="$DEPTHAI_TAG"
JOBS=""
CLEAN=0
PROBE=1

die() { echo "error: $*" >&2; exit 1; }
say() { echo ">> $*"; }
usage() { sed -n '2,22p' "$0"; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --tag)        TAG="${2:?--tag needs a value}"; shift 2 ;;
        --root)       ROOT="${2:?--root needs a directory}"; shift 2 ;;
        --jobs)       JOBS="${2:?--jobs needs a number}"; shift 2 ;;
        --clean)      CLEAN=1; shift ;;
        --skip-probe) PROBE=0; shift ;;
        -h|--help)    usage; exit 0 ;;
        *) echo "error: unknown argument '$1' (try --help)" >&2; exit 2 ;;
    esac
done

command -v cmake >/dev/null || die "cmake not found (macOS: brew install cmake; Linux: apt install cmake)"
command -v git   >/dev/null || die "git not found"
# vcpkg ports (bzip2, libarchive, ...) need pkg-config even on macOS.
command -v pkg-config >/dev/null || die "pkg-config not found (macOS: brew install pkg-config; Linux: apt install pkg-config)"
if [[ "$(uname -s)" == "Linux" ]]; then
    pkg-config --exists libudev 2>/dev/null || die "libudev headers not found (apt install libudev-dev)"
fi
GENERATOR=()
if command -v ninja >/dev/null; then GENERATOR=(-G Ninja); fi
if [[ -z "$JOBS" ]]; then
    JOBS="$( (command -v nproc >/dev/null && nproc) || sysctl -n hw.ncpu )"
fi

SRC="$ROOT/src-$TAG"
BUILD="$ROOT/build-$TAG"
PREFIX="$ROOT/$TAG"
mkdir -p "$ROOT"

if [[ ! -d "$SRC/.git" ]]; then
    say "cloning depthai-core $TAG into $SRC"
    git clone --depth 1 --branch "$TAG" --recursive "$DEPTHAI_REPO" "$SRC"
else
    say "using existing clone at $SRC ($(git -C "$SRC" describe --tags --always))"
fi

if [[ "$CLEAN" == "1" ]]; then
    say "removing $BUILD"
    rm -rf "$BUILD"
fi

# Platform flags.
PLATFORM_FLAGS=()
case "$(uname -s)" in
    Darwin)
        PLATFORM_FLAGS+=(-DCMAKE_OSX_DEPLOYMENT_TARGET=14.0)
        PLATFORM_FLAGS+=(-DCMAKE_INSTALL_RPATH=@loader_path)
        ;;
    Linux)
        PLATFORM_FLAGS+=('-DCMAKE_INSTALL_RPATH=$ORIGIN')
        ;;
esac

# What is switched off, and why:
#   OPENCV / XTENSOR / PCL / RTABMAP / BASALT  optional integrations oak-cam never
#                                             uses; OpenCV alone is an hour of vcpkg.
#   REMOTE_CONNECTION / EVENTS_MANAGER / CURL  the visualizer web server and the
#                                             Luxonis Hub client (pulls in curl, protobuf, websockets).
#   PROTOBUF                                  only needed by the remote connection.
#   MP4V2                                     on-device recording helper.
#   APRIL_TAG / KOMPUTE                       nodes oak-cam does not create.
#   DYNAMIC_CALIBRATION                       a downloaded prebuilt library we would
#                                             otherwise have to ship.
#   BACKWARD                                  crash stack traces via libdw/binutils.
#   DEVICE_RVC4_FW                            OAK-D Pro is RVC2; drop the RVC4 firmware blob.
#   BINARIES_RESOURCE_COMPILE=ON              embed the RVC2 firmware so the app never
#                                             downloads at runtime.
say "configuring in $BUILD (jobs=$JOBS)"
cmake -S "$SRC" -B "$BUILD" "${GENERATOR[@]}" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$PREFIX" \
    -DBUILD_SHARED_LIBS=ON \
    -DDEPTHAI_INSTALL=ON \
    -DDEPTHAI_BUILD_EXAMPLES=OFF \
    -DDEPTHAI_BUILD_TESTS=OFF \
    -DDEPTHAI_BUILD_PYTHON=OFF \
    -DDEPTHAI_BUILD_DOCS=OFF \
    -DDEPTHAI_BUILD_ZOO_HELPER=OFF \
    -DDEPTHAI_CLANG_FORMAT=OFF \
    -DDEPTHAI_OPENCV_SUPPORT=OFF \
    -DDEPTHAI_XTENSOR_SUPPORT=OFF \
    -DDEPTHAI_PCL_SUPPORT=OFF \
    -DDEPTHAI_RTABMAP_SUPPORT=OFF \
    -DDEPTHAI_BASALT_SUPPORT=OFF \
    -DDEPTHAI_ENABLE_REMOTE_CONNECTION=OFF \
    -DDEPTHAI_ENABLE_EVENTS_MANAGER=OFF \
    -DDEPTHAI_ENABLE_CURL=OFF \
    -DDEPTHAI_ENABLE_PROTOBUF=OFF \
    -DDEPTHAI_ENABLE_MP4V2=OFF \
    -DDEPTHAI_ENABLE_APRIL_TAG=OFF \
    -DDEPTHAI_ENABLE_KOMPUTE=OFF \
    -DDEPTHAI_DYNAMIC_CALIBRATION_SUPPORT=OFF \
    -DDEPTHAI_ENABLE_BACKWARD=OFF \
    -DDEPTHAI_ENABLE_DEVICE_RVC4_FW=OFF \
    -DDEPTHAI_BINARIES_RESOURCE_COMPILE=ON \
    "${PLATFORM_FLAGS[@]}"

say "building"
cmake --build "$BUILD" --parallel "$JOBS"

say "installing to $PREFIX"
cmake --install "$BUILD"

ln -sfn "$PREFIX" "$ROOT/current"
say "linked $ROOT/current -> $PREFIX"

if [[ "$PROBE" == "1" ]]; then
    say "probing exported compile flags"
    "$SCRIPT_DIR/probe.sh" --prefix "$PREFIX"
fi

echo
echo "================================================================"
echo "  depthai-core $TAG installed at $PREFIX"
echo "  cargo picks it up via $ROOT/current (or DEPTHAI_PREFIX=$PREFIX)"
echo "================================================================"
