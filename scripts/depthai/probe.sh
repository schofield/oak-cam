#!/usr/bin/env bash
# probe.sh — run the flags probe against an installed depthai-core prefix and
# write <prefix>/oak-flags.json. Called by build.sh; safe to re-run.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix) PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
        *) echo "error: unknown argument '$1'" >&2; exit 2 ;;
    esac
done
[[ -n "$PREFIX" ]] || PREFIX="${OAK_DEPTHAI_ROOT:-$HOME/.cache/oak-cam/depthai}/current"
PREFIX="$(cd "$PREFIX" && pwd -P)"
[[ -d "$PREFIX/lib/cmake/depthai" ]] || { echo "error: $PREFIX/lib/cmake/depthai not found (run build.sh first)" >&2; exit 1; }
BUILD="$PREFIX/.probe-build"
rm -rf "$BUILD"
cmake -S "$SCRIPT_DIR/probe" -B "$BUILD" \
    -DCMAKE_PREFIX_PATH="$PREFIX" \
    -DCMAKE_INSTALL_PREFIX="$PREFIX" \
    -DOAK_FLAGS_OUT="$PREFIX/oak-flags.json" >/dev/null
rm -rf "$BUILD"
echo ">> wrote $PREFIX/oak-flags.json"
