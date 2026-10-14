#!/usr/bin/env bash
#
# setup-macos-runner.sh - one command, after cloning the repository on the
# signing Mac: check what oak-cam's macOS package job needs and register a
# GitHub Actions runner for oak-cam as a LaunchAgent in the owner's login
# session (that session is what gives codesign and notarytool their keychain).
# The macOS counterpart of setup-linux-runner.sh. Idempotent: safe to re-run.
#
# What it does:
#   - checks the Xcode command line tools (swiftc, codesign, notarytool,
#     hdiutil), rustup with the toolchain rust-toolchain.toml pins, and the
#     signing inputs the package job looks up at run time: a "Developer ID
#     Application" identity in the keychain and a provisioning profile named
#     --profile-name (default "OakCam Developer ID") in Xcode's profile store.
#     It never prints or exports keys; a missing input is a warning, the job
#     itself fails clearly.
#   - stamps an existing depthai-core install (~/.cache/oak-cam/depthai/current,
#     from scripts/depthai/build.sh) with the scripts/depthai tree id, so the
#     job reuses it instead of rebuilding for 20-60 minutes.
#   - the latest actions runner in ~/actions-runner-oak-cam (or --dir), SHA-256
#     checked against the release notes, registered for --repo (default
#     schofield/oak-cam) as <hostname>-oak-macos with the label "macos"
#     (GitHub adds self-hosted, macOS, ARM64/X64) plus --labels extras, using a
#     registration token minted with `gh api` (needs `gh auth login` once).
#     ~/.cargo/bin is on PATH when the runner is configured, so the runner's
#     saved PATH carries cargo and rustup into every job.
#   - a LaunchAgent (svc.sh install && svc.sh start) in the login session, so
#     jobs run whenever this user is logged in. --interactive skips the service
#     and prints the ./run.sh command instead.
#
# Re-running: when .runner already exists the registration is kept and the
# service is (re)started. When the runner is no longer listed for the
# repository (GitHub drops a runner after about two weeks offline), or with
# --reregister, the stale local registration is cleared and the runner
# registered again.
#
# Usage: scripts/ci/setup-macos-runner.sh [--repo owner/name] [--dir DIR]
#        [--name NAME] [--labels a,b] [--profile-name NAME] [--interactive]
#        [--reregister] [--skip-runner]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

REPO="schofield/oak-cam"
RUNNER_DIR="$HOME/actions-runner-oak-cam"
RUNNER_NAME="$(hostname -s)-oak-macos"
EXTRA_LABELS=""
PROFILE_NAME="OakCam Developer ID"
INTERACTIVE=0
REREGISTER=0
SKIP_RUNNER=0

while [ $# -gt 0 ]; do
    case "$1" in
        --repo) REPO="${2:?--repo needs owner/name}"; shift 2 ;;
        --dir) RUNNER_DIR="${2:?--dir needs a directory}"; shift 2 ;;
        --name) RUNNER_NAME="${2:?--name needs a value}"; shift 2 ;;
        --labels) EXTRA_LABELS="${2:?--labels needs a value}"; shift 2 ;;
        --profile-name) PROFILE_NAME="${2:?--profile-name needs a value}"; shift 2 ;;
        --interactive) INTERACTIVE=1; shift ;;
        --as-service) INTERACTIVE=0; shift ;;
        --reregister) REREGISTER=1; shift ;;
        --skip-runner) SKIP_RUNNER=1; shift ;;
        -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
        *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
    esac
done

step() { printf '\n==> %s\n' "$*"; }
ok()   { printf '    %s\n' "$*"; }
warn() { printf '    WARNING: %s\n' "$*" >&2; }
die()  { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "this script is for macOS (Linux: setup-linux-runner.sh, Windows: setup-windows-runner.ps1)"
[ "$(id -u)" -ne 0 ] || die "run as the user whose login keychain holds the signing identity, not root"

# --- host --------------------------------------------------------------------
step "Host"
ARCH="$(uname -m)"
case "$ARCH" in
    arm64) RUNNER_ARCH=osx-arm64 ;;
    x86_64) RUNNER_ARCH=osx-x64 ;;
    *) die "unexpected architecture $ARCH" ;;
esac
ok "macOS $(sw_vers -productVersion) ($ARCH), $(sysctl -n hw.ncpu) CPUs, $(( $(sysctl -n hw.memsize) / 1024 / 1024 / 1024 )) GB RAM"

step "Xcode command line tools"
xcode-select -p >/dev/null 2>&1 || die "no developer directory; run: xcode-select --install"
for tool in swiftc codesign hdiutil plutil lipo ditto git curl; do
    command -v "$tool" >/dev/null 2>&1 || die "missing $tool (xcode-select --install)"
done
xcrun notarytool --version >/dev/null 2>&1 || die "xcrun notarytool not available (needs Xcode 13+ command line tools)"
ok "$(xcode-select -p); notarytool $(xcrun notarytool --version 2>/dev/null | head -1)"

for tool in cmake pkg-config; do
    command -v "$tool" >/dev/null 2>&1 || warn "$tool not found; a cold depthai-core build in the job needs it (brew install cmake ninja pkg-config)"
done
if [ "$ARCH" = "x86_64" ]; then
    warn "an Intel Mac carries the X64 label and would also be offered self-hosted-ci.yml's jobs,"
    warn "which are meant for the x86 box; give the box a dedicated label before using this runner"
fi

# --- rust --------------------------------------------------------------------
step "Rust toolchain from rust-toolchain.toml"
if [ ! -x "$HOME/.cargo/bin/rustup" ]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
        sh -s -- -y --profile minimal --default-toolchain none
fi
export PATH="$HOME/.cargo/bin:$PATH"
( cd "$REPO_ROOT" && rustup toolchain install && rustc --version && cargo --version )

# --- depthai-core ----------------------------------------------------------------
step "depthai-core install"
root="${OAK_DEPTHAI_ROOT:-$HOME/.cache/oak-cam/depthai}"
if [ -f "$root/current/oak-flags.json" ]; then
    want="$(git -C "$REPO_ROOT" rev-parse HEAD:scripts/depthai)"
    stamp="$root/current/.oak-scripts-tree"
    have="$(cat "$stamp" 2>/dev/null || true)"
    if [ "$have" = "$want" ]; then
        ok "$(readlink "$root/current" || echo "$root/current") is current (scripts/depthai $want)"
    elif [ -z "$have" ]; then
        # An install made by hand before the stamp existed: assume it matches
        # the checked-out scripts (delete the stamp to make the job rebuild).
        echo "$want" > "$stamp"
        ok "stamped $(readlink "$root/current" || echo "$root/current") as built from scripts/depthai $want"
    else
        warn "$root/current was built from scripts/depthai $have, not $want; the next job rebuilds it (20-60 min)"
    fi
else
    warn "no depthai-core install under $root/current; the first package job builds it (20-60 min),"
    warn "or run scripts/depthai/build.sh now"
fi

# --- signing inputs (checked, never printed) ---------------------------------
step "Signing inputs for the package job"
# build-macos.sh picks the "Developer ID Application: … (TEAM_ID)" identity.
TEAM_ID="${OAK_TEAM_ID:-5V2V8BX632}"
ids="$(security find-identity -v -p codesigning 2>/dev/null || true)"
if printf '%s\n' "$ids" | grep -q "Developer ID Application: .*($TEAM_ID)"; then
    ok "a Developer ID Application identity for team $TEAM_ID is in the keychain"
else
    warn "no 'Developer ID Application: … ($TEAM_ID)' identity in the keychain; the package job will fail at signing"
fi
found=""
for p in "$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles"/*.provisionprofile \
         "$HOME/Library/MobileDevice/Provisioning Profiles"/*.provisionprofile; do
    [ -f "$p" ] || continue
    n="$(security cms -D -i "$p" 2>/dev/null | plutil -extract Name raw -o - - 2>/dev/null || true)"
    if [ "$n" = "$PROFILE_NAME" ]; then found="$p"; break; fi
done
if [ -n "$found" ]; then
    ok "provisioning profile '$PROFILE_NAME' is in Xcode's profile store"
else
    warn "no provisioning profile named '$PROFILE_NAME' in Xcode's profile store; the package job will fail"
    warn "(download it from developer.apple.com and open it once, or pass --profile-name)"
fi
ok "the notarytool keychain profile is not checked here; the job reads it as the repository variable OAK_NOTARY_PROFILE (default sc-notary)"

if [ "$SKIP_RUNNER" -eq 1 ]; then
    step "Done"
    ok "toolchain checked; runner setup skipped (--skip-runner)"
    exit 0
fi

# --- gh -------------------------------------------------------------------------
step "GitHub CLI authentication"
command -v jq >/dev/null 2>&1 || die "jq is needed to check the runner download; brew install jq"
command -v gh >/dev/null 2>&1 || die "the GitHub CLI is needed to register the runner; brew install gh, then gh auth login"
ok "gh $(gh --version | head -1 | awk '{print $3}')"
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
    asset="actions-runner-${RUNNER_ARCH}-${version}.tar.gz"
    url="https://github.com/actions/runner/releases/download/v${version}/${asset}"
    tmpdir="$(mktemp -d)"
    tarball="$tmpdir/$asset"
    ok "downloading $url"
    curl -fsSL -o "$tarball" "$url"
    expected=$(jq -r .body <<<"$release" | grep -F "$asset" | grep -oE '[0-9a-f]{64}' | head -1 || true)
    actual=$(shasum -a 256 "$tarball" | awk '{print $1}')
    if [ -n "$expected" ]; then
        [ "$actual" = "$expected" ] || die "SHA-256 mismatch for $asset: $actual (expected $expected)"
        ok "SHA-256 verified"
    else
        warn "release notes carried no SHA-256 for $asset; downloaded hash $actual"
    fi
    tar -xzf "$tarball" -C "$RUNNER_DIR"
    rm -rf "$tmpdir"
    ok "runner $version unpacked"
else
    ok "runner already unpacked"
fi

cd "$RUNNER_DIR"

if [ -f .runner ]; then
    local_name=$(jq -r '.agentName // empty' .runner 2>/dev/null || true)
    [ -n "$local_name" ] && RUNNER_NAME="$local_name"
    if [ "$REREGISTER" -eq 0 ]; then
        # A failed listing must not be read as "not registered" (that path
        # deletes the local credentials).
        names="$(registered_names)" || die "could not list the runners for $REPO"
        if ! grep -qxF "$RUNNER_NAME" <<<"$names"; then
            warn "'$RUNNER_NAME' is configured here but not registered for $REPO any more; registering again"
            REREGISTER=1
        fi
    fi
    if [ "$REREGISTER" -eq 1 ]; then
        if [ -f .service ]; then
            ./svc.sh stop || true
            ./svc.sh uninstall || true
        fi
        rm -f .runner .credentials .credentials_rsaparams
        ok "cleared the local registration"
    fi
fi

labels="macos"
[ -n "$EXTRA_LABELS" ] && labels="$labels,$EXTRA_LABELS"

if [ -f .runner ]; then
    ok "runner '$RUNNER_NAME' already registered; keeping it"
else
    step "Registering '$RUNNER_NAME' (labels: self-hosted, macOS, $( [ "$ARCH" = arm64 ] && echo ARM64 || echo X64 ), $labels)"
    TOKEN=$(gh api -X POST "repos/$REPO/actions/runners/registration-token" --jq .token)
    [ -n "$TOKEN" ] || die "gh could not mint a registration token for $REPO (does the account have admin access?)"
    ./config.sh --unattended --url "https://github.com/$REPO" --token "$TOKEN" \
        --name "$RUNNER_NAME" --labels "$labels" --work _work --replace
    ok "registered"
fi

if [ "$INTERACTIVE" -eq 1 ]; then
    ok "start the runner with: $RUNNER_DIR/run.sh"
else
    step "LaunchAgent"
    # svc.sh install records the plist label in .service; on macOS it is a
    # per-user LaunchAgent (no sudo), which is what keeps the login keychain
    # reachable for codesign and notarytool.
    if [ ! -f .service ]; then
        ./svc.sh install
    fi
    # Refresh the PATH snapshot the service exports to jobs (.path), so a
    # later brew or rustup install reaches them without re-registering.
    [ -x ./env.sh ] && ./env.sh >/dev/null 2>&1 || true
    ./svc.sh stop >/dev/null 2>&1 || true
    ./svc.sh start
    ok "service $(cat .service) started"
    plist="$(cat .service)"
    if [ -f "$plist" ] && plutil -p "$plist" 2>/dev/null | grep -q '"SessionCreate" => 1'; then
        warn "the LaunchAgent sets SessionCreate, which gives jobs their own security session;"
        warn "if codesign fails with errSecInternalComponent, run the runner interactively ($RUNNER_DIR/run.sh)"
        warn "or allow codesign and notarytool on the key: security set-key-partition-list -S apple-tool:,apple: -s ~/Library/Keychains/login.keychain-db"
    fi
fi

step "Runners registered for $REPO"
gh api "repos/$REPO/actions/runners" --paginate \
    --jq '.runners[] | "    \(.name)  \(.status)  busy=\(.busy)  [\([.labels[].name] | join(","))]"'

step "Done"
ok "Trigger: gh workflow run package.yml -R $REPO -f os=macos"
