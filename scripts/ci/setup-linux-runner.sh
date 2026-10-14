#!/usr/bin/env bash
#
# setup-linux-runner.sh - one command, after cloning the repository on the
# owner's box: install what oak-cam's self-hosted workflows need and register
# a runner instance for oak-cam next to any existing runner (for example
# stream-concern's in ~/actions-runner; personal-account runners are
# per-repository, so each project needs its own). Ported from
# stream-concern's scripts/ci/setup-linux-runner.sh and start-linux-runner.sh;
# the Linux counterpart of setup-windows-runner.ps1. Idempotent: safe to re-run.
#
# What it does:
#   - apt: build-essential, cmake, ninja-build, pkg-config, git, curl, jq,
#     patchelf, libudev-dev and libusb-1.0-0-dev (the depthai-core build in
#     scripts/depthai/build.sh), v4l2loopback-dkms and v4l-utils (the Linux
#     virtual cameras, hardware.yml's v4l2-ctl), and the GitHub CLI from its
#     apt repository.
#   - --nvidia: the recommended NVIDIA driver through ubuntu-drivers (not
#     needed by oak-cam itself; for a box that also serves GPU projects).
#   - the Movidius udev rule (USB vendor 03e7, /etc/udev/rules.d/80-movidius.rules)
#     so the runner user can open the OAK without root.
#   - rustup with the toolchain rust-toolchain.toml pins (+ rustfmt, clippy).
#   - the latest actions runner in ~/actions-runner-oak-cam (or --dir),
#     SHA-256 checked against the release notes, registered for --repo
#     (default schofield/oak-cam) as <hostname>-oak-linux with the label
#     "linux" (GitHub adds self-hosted, Linux, X64) plus --labels extras, using
#     a registration token minted with `gh api` (needs `gh auth login` once).
#   - a systemd service (svc.sh install && svc.sh start), so jobs run from boot
#     without a desktop login. --interactive skips the service and prints the
#     ./run.sh command instead.
#
# Re-running: when .runner already exists the registration is kept and the
# service is (re)started. GitHub deletes a runner that has been offline for
# about two weeks (the box was booted into Windows, say); when the runner is
# no longer listed for the repository, or with --reregister, the stale local
# registration is cleared and the runner registered again.
#
# Usage: scripts/ci/setup-linux-runner.sh [--repo owner/name] [--dir DIR]
#        [--name NAME] [--labels a,b] [--nvidia] [--interactive]
#        [--reregister] [--skip-runner]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

REPO="schofield/oak-cam"
RUNNER_DIR="$HOME/actions-runner-oak-cam"
RUNNER_NAME="$(hostname -s)-oak-linux"
EXTRA_LABELS=""
NVIDIA=0
INTERACTIVE=0
REREGISTER=0
SKIP_RUNNER=0

while [ $# -gt 0 ]; do
    case "$1" in
        --repo) REPO="${2:?--repo needs owner/name}"; shift 2 ;;
        --dir) RUNNER_DIR="${2:?--dir needs a directory}"; shift 2 ;;
        --name) RUNNER_NAME="${2:?--name needs a value}"; shift 2 ;;
        --labels) EXTRA_LABELS="${2:?--labels needs a value}"; shift 2 ;;
        --nvidia) NVIDIA=1; shift ;;
        --interactive) INTERACTIVE=1; shift ;;
        --as-service) INTERACTIVE=0; shift ;;  # the default; kept for parity with stream-concern
        --reregister) REREGISTER=1; shift ;;
        --skip-runner) SKIP_RUNNER=1; shift ;;
        -h|--help) sed -n '2,41p' "$0"; exit 0 ;;
        *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
    esac
done

step() { printf '\n==> %s\n' "$*"; }
ok()   { printf '    %s\n' "$*"; }
warn() { printf '    WARNING: %s\n' "$*" >&2; }
die()  { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" -ne 0 ] || die "run as the user that will own the runner, not root (sudo is used where needed)"

# --- host --------------------------------------------------------------------
step "Host"
if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    ok "${PRETTY_NAME:-?}, $(nproc) CPUs, $(awk '/MemTotal/ {printf "%d GB", $2/1024/1024}' /proc/meminfo) RAM"
    if [ "${ID:-}" != "ubuntu" ] || [ "${VERSION_ID:-}" != "24.04" ]; then
        warn "written for Ubuntu 24.04 (the hosted CI image); continuing on ${ID:-?} ${VERSION_ID:-?}"
    fi
fi

# --- packages ----------------------------------------------------------------
step "apt packages"
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
    build-essential curl git ca-certificates gnupg jq cmake ninja-build \
    pkg-config patchelf zip unzip tar libudev-dev libusb-1.0-0-dev \
    v4l2loopback-dkms v4l-utils "linux-headers-$(uname -r)"
ok "build dependencies installed"

if ! command -v gh >/dev/null 2>&1; then
    step "GitHub CLI (apt repository)"
    sudo install -dm 755 /etc/apt/keyrings
    curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg |
        sudo tee /etc/apt/keyrings/githubcli-archive-keyring.gpg >/dev/null
    sudo chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" |
        sudo tee /etc/apt/sources.list.d/github-cli.list >/dev/null
    sudo apt-get update && sudo apt-get install -y gh
fi
ok "gh $(gh --version | head -1 | awk '{print $3}')"

if [ "$NVIDIA" -eq 1 ]; then
    step "NVIDIA driver (ubuntu-drivers)"
    sudo apt-get install -y ubuntu-drivers-common
    sudo ubuntu-drivers install || warn "ubuntu-drivers install failed; install the driver by hand"
    warn "reboot afterwards so the driver loads"
fi

# --- OAK udev rule -------------------------------------------------------------
step "Movidius udev rule (OAK, USB vendor 03e7)"
RULE=/etc/udev/rules.d/80-movidius.rules
RULE_TEXT='SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"'
if [ "$(cat "$RULE" 2>/dev/null)" != "$RULE_TEXT" ]; then
    echo "$RULE_TEXT" | sudo tee "$RULE" >/dev/null
    sudo udevadm control --reload-rules
    sudo udevadm trigger
    ok "wrote $RULE (replug the OAK once)"
else
    ok "$RULE present"
fi

# TODO(M4b): scripts/vcam/linux-setup.sh will own the v4l2loopback options
# (three devices, video_nr=10,11,12); until then only the module is installed.
if modinfo v4l2loopback >/dev/null 2>&1; then
    ok "v4l2loopback module available ($(modinfo -F version v4l2loopback 2>/dev/null || echo '?'))"
else
    warn "v4l2loopback module not built for $(uname -r); check the DKMS output above (Secure Boot may need the MOK enrolled)"
fi

# --- rust --------------------------------------------------------------------
step "Rust toolchain from rust-toolchain.toml"
if [ ! -x "$HOME/.cargo/bin/rustup" ]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
        sh -s -- -y --profile minimal --default-toolchain none
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
( cd "$REPO_ROOT" && rustup toolchain install && rustc --version && cargo --version )

if [ "$SKIP_RUNNER" -eq 1 ]; then
    step "Done"
    ok "toolchain ready; runner setup skipped (--skip-runner)"
    ok "optional warm-up of the depthai-core install (20-60 min): scripts/depthai/build.sh"
    exit 0
fi

# --- gh -------------------------------------------------------------------------
step "GitHub CLI authentication"
if ! gh auth status >/dev/null 2>&1; then
    echo "gh is not logged in. Run this once, then rerun this script:" >&2
    echo "    gh auth login --hostname github.com --git-protocol https --web" >&2
    echo "(the account needs admin access to $REPO to register runners)" >&2
    exit 1
fi
ok "$(gh auth status 2>&1 | grep -m1 -E 'Logged in' | sed 's/^[[:space:]]*//')"

registered_names() {
    gh api "repos/$REPO/actions/runners" --paginate --jq '.runners[].name'
}

# --- runner ------------------------------------------------------------------
step "GitHub Actions runner in $RUNNER_DIR for $REPO"
mkdir -p "$RUNNER_DIR"
if [ ! -x "$RUNNER_DIR/config.sh" ]; then
    release=$(curl -fsSL https://api.github.com/repos/actions/runner/releases/latest)
    version=$(jq -r .tag_name <<<"$release" | sed 's/^v//')
    asset="actions-runner-linux-x64-${version}.tar.gz"
    url="https://github.com/actions/runner/releases/download/v${version}/${asset}"
    tmpdir="$(mktemp -d)"
    tarball="$tmpdir/$asset"
    ok "downloading $url"
    curl -fsSL -o "$tarball" "$url"
    expected=$(jq -r .body <<<"$release" | grep -F "$asset" | grep -oE '[0-9a-f]{64}' | head -1 || true)
    actual=$(sha256sum "$tarball" | awk '{print $1}')
    if [ -n "$expected" ]; then
        [ "$actual" = "$expected" ] || die "SHA-256 mismatch for $asset: $actual (expected $expected)"
        ok "SHA-256 verified"
    else
        warn "release notes carried no SHA-256 for $asset; downloaded hash $actual"
    fi
    tar -xzf "$tarball" -C "$RUNNER_DIR"
    rm -rf "$tmpdir"
    sudo "$RUNNER_DIR/bin/installdependencies.sh"
    ok "runner $version unpacked"
else
    ok "runner already unpacked"
fi

cd "$RUNNER_DIR"

# An existing local registration whose runner GitHub no longer lists (deleted
# after ~2 weeks offline) cannot connect; clear it and register again.
if [ -f .runner ]; then
    local_name=$(jq -r '.agentName // empty' .runner 2>/dev/null || true)
    [ -n "$local_name" ] && RUNNER_NAME="$local_name"
    if [ "$REREGISTER" -eq 0 ] && ! registered_names | grep -qxF "$RUNNER_NAME"; then
        warn "'$RUNNER_NAME' is configured here but not registered for $REPO any more; registering again"
        REREGISTER=1
    fi
    if [ "$REREGISTER" -eq 1 ]; then
        if [ -f .service ]; then
            sudo ./svc.sh stop || true
        fi
        rm -f .runner .credentials .credentials_rsaparams
        ok "cleared the local registration"
    fi
fi

labels="linux"
[ -n "$EXTRA_LABELS" ] && labels="$labels,$EXTRA_LABELS"

if [ -f .runner ]; then
    ok "runner '$RUNNER_NAME' already registered; keeping it"
else
    step "Registering '$RUNNER_NAME' (labels: self-hosted, Linux, X64, $labels)"
    TOKEN=$(gh api -X POST "repos/$REPO/actions/runners/registration-token" --jq .token)
    [ -n "$TOKEN" ] || die "gh could not mint a registration token for $REPO (does the account have admin access?)"
    ./config.sh --unattended --url "https://github.com/$REPO" --token "$TOKEN" \
        --name "$RUNNER_NAME" --labels "$labels" --work _work --replace
    ok "registered"
fi

if [ "$INTERACTIVE" -eq 1 ]; then
    ok "start the runner with: $RUNNER_DIR/run.sh"
else
    step "systemd service"
    # svc.sh install records the unit name in .service.
    if [ ! -f .service ]; then
        sudo ./svc.sh install "$USER"
    fi
    sudo ./svc.sh stop >/dev/null 2>&1 || true
    sudo ./svc.sh start
    ok "service $(cat .service) started"
fi

step "Runners registered for $REPO"
gh api "repos/$REPO/actions/runners" --paginate \
    --jq '.runners[] | "    \(.name)  \(.status)  busy=\(.busy)  [\([.labels[].name] | join(","))]"'

step "Done"
ok "Trigger: gh workflow run hardware.yml -R $REPO -f os=linux"
