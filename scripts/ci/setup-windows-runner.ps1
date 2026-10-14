<#
.SYNOPSIS
    One command, after cloning the repository on the owner's box: install
    what oak-cam's self-hosted workflows need on Windows 11 and register a
    runner instance for oak-cam next to any existing runner (for example
    stream-concern's MECHAMARU in C:\actions-runner; personal-account runners
    are per-repository, so each project needs its own). Ported from
    stream-concern's scripts/ci/setup-windows-runner.ps1 and
    start-windows-runner.ps1. Idempotent: safe to re-run.

.DESCRIPTION
    Windows PowerShell 5.1 compatible; keep this file ASCII.

    Installed through winget:
      - Git for Windows (bash for the workflows' `shell: bash` steps)
      - Visual Studio 2022 Build Tools with the C++ workload and the Windows 11
        SDK (link.exe for Rust, cl.exe for the depthai-sys shim and the CMake
        flags probe in scripts\depthai\fetch-windows.ps1)
      - CMake (the flags probe)
      - 7-Zip (fetch-windows.ps1 unpacks the OpenCV package with it; the
        self-extractor is the slower fallback)
      - rustup, then the toolchain rust-toolchain.toml pins (rustfmt, clippy)
      - GitHub CLI (mints the runner registration token; `gh auth login` once)

    Also: long paths (system policy and git), and the CurrentUser execution
    policy RemoteSigned so the workflows can run the repository's .ps1 files.

    Runner: the latest actions/runner release is unpacked to
    C:\actions-runner-oak-cam (or -Dir), its SHA-256 checked against the
    release notes, and registered for -Repo (default schofield/oak-cam) as
    <computer name>-oak-windows with the label "windows" (GitHub adds
    self-hosted, Windows, X64) plus -ExtraLabels, with a token minted by
    `gh api`. The workflows target [self-hosted, windows].

    Service: by default config.cmd --runasservice installs and starts a
    Windows service that logs on as -ServiceAccount (default: you; rustup and
    the depthai install are per user), so jobs run from boot without a login.
    The account password is prompted for. -Interactive skips the service; then
    start C:\actions-runner-oak-cam\run.cmd from a logged-in desktop (creating
    the Media Foundation virtual cameras may need that session).

    Re-running: when .runner already exists the registration is kept and the
    service is (re)started. GitHub deletes a runner that has been offline for
    about two weeks (the box was booted into Linux, say); when the runner is no
    longer listed for the repository, or with -Reregister, the stale local
    registration and its service are removed and the runner registered again.

.PARAMETER Repo
    owner/name to register with. Default schofield/oak-cam.

.PARAMETER Dir
    Where to unpack the runner. Default C:\actions-runner-oak-cam.

.PARAMETER RunnerName
    Runner name shown on GitHub. Default: <computer name>-oak-windows.

.PARAMETER ExtraLabels
    Comma-separated labels added to "windows" (for example "nvidia").

.PARAMETER Interactive
    Do not install a service; print the run.cmd command instead.

.PARAMETER AsService
    The default; accepted for parity with stream-concern's script.

.PARAMETER ServiceAccount
    Account the service logs on as. Default: the current user.

.PARAMETER Reregister
    Force a fresh registration even when GitHub still lists the runner.

.PARAMETER SkipRunner
    Install the toolchain only; do not download or configure the runner.

.EXAMPLE
    # From an elevated PowerShell (Run as Administrator):
    git clone https://github.com/schofield/oak-cam; cd oak-cam
    powershell -ExecutionPolicy Bypass -File scripts\ci\setup-windows-runner.ps1
#>
[CmdletBinding()]
param(
    [string]$Repo = "schofield/oak-cam",
    [Alias("RunnerDir")]
    [string]$Dir = "C:\actions-runner-oak-cam",
    [string]$RunnerName = "$($env:COMPUTERNAME.ToLowerInvariant())-oak-windows",
    [string]$ExtraLabels = "",
    [switch]$Interactive,
    [switch]$AsService,
    [string]$ServiceAccount = "$env:USERDOMAIN\$env:USERNAME",
    [switch]$Reregister,
    [switch]$SkipRunner
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

function Step($msg) { Write-Host "`n==> $msg" -ForegroundColor Cyan }
function Ok($msg) { Write-Host "    $msg" -ForegroundColor Green }
function Warn($msg) { Write-Host "    $msg" -ForegroundColor Yellow }

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path

# --- preconditions -----------------------------------------------------------
$isAdmin = ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()
    ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    throw "Run this script from an elevated PowerShell (Run as Administrator)."
}
if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
    throw "winget is missing. Install 'App Installer' from the Microsoft Store, then rerun."
}
$os = Get-CimInstance Win32_OperatingSystem
Ok "Windows $($os.Caption) build $($os.BuildNumber), $([math]::Round($os.TotalVisibleMemorySize / 1MB)) GB RAM"

function Install-Winget($id, $extra = @()) {
    $listed = winget list --id $id --exact --accept-source-agreements 2>$null | Select-String -SimpleMatch $id
    if ($listed) {
        Ok "$id already installed"
        return
    }
    Step "Installing $id"
    $wingetArgs = @("install", "--id", $id, "--exact", "--silent",
        "--accept-package-agreements", "--accept-source-agreements") + $extra
    & winget @wingetArgs
    if ($LASTEXITCODE -ne 0) {
        # winget returns package-specific non-zero codes for "already installed"
        # and "newer version present"; the checks below catch a real failure.
        Warn "winget install $id exited with $LASTEXITCODE; continuing"
    }
}

# --- toolchain ---------------------------------------------------------------
Install-Winget "Git.Git"
Install-Winget "GitHub.cli"
Install-Winget "Kitware.CMake"
Install-Winget "7zip.7zip"

# Visual Studio 2022 Build Tools with the C++ workload and the Windows 11 SDK.
# One string: winget passes --override through as a single argument.
$vsOverride = "--quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.Windows11SDK.22621 --includeRecommended"
Install-Winget "Microsoft.VisualStudio.2022.BuildTools" @("--override", $vsOverride)

# winget skips --override when Build Tools (or any Visual Studio) is already
# installed, leaving the C++ workload out; add it through the installer then.
$vsInstaller = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer"
$vswhere = Join-Path $vsInstaller "vswhere.exe"
$vsSetup = Join-Path $vsInstaller "setup.exe"
if (Test-Path $vswhere) {
    $vc = & $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
    if ($vc) {
        Ok "MSVC C++ toolchain present at $vc"
    } else {
        $vs = & $vswhere -products * -property installationPath 2>$null | Select-Object -First 1
        if ($vs -and (Test-Path $vsSetup)) {
            Step "Adding the C++ workload to the Visual Studio at $vs"
            & $vsSetup modify --installPath "$vs" --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.Windows11SDK.22621 --includeRecommended --passive --norestart --wait
            if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 3010) { Warn "Visual Studio installer exited with $LASTEXITCODE; add 'Desktop development with C++' by hand" }
        } else {
            Warn "no MSVC C++ toolchain found: open Visual Studio Installer, Modify, and add 'Desktop development with C++'"
        }
    }
} else {
    Warn "vswhere.exe not found: Build Tools did not install; rerun, or add 'Desktop development with C++' by hand"
}

Install-Winget "Rustlang.Rustup"

# Refresh PATH for this session so the freshly installed tools resolve.
$env:Path = [Environment]::GetEnvironmentVariable("Path", "Machine") + ";" +
            [Environment]::GetEnvironmentVariable("Path", "User")

Step "Rust toolchain from rust-toolchain.toml"
Push-Location $RepoRoot
try {
    & rustup toolchain install
    if ($LASTEXITCODE -ne 0) { throw "rustup toolchain install failed ($LASTEXITCODE)" }
    & rustc --version
    & cargo --version
} finally {
    Pop-Location
}

Step "Machine settings"
# Long paths: cargo's target tree plus the runner's _work tree can exceed 260
# characters. Both switches are needed (system policy and git).
Set-ItemProperty -Path "HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem" `
    -Name LongPathsEnabled -Value 1 -Type DWord
& git config --system core.longpaths true
Ok "long paths enabled"
# Set-ExecutionPolicy throws ExecutionPolicyOverride when a more specific
# scope (Process, when launched with -ExecutionPolicy Bypass) already wins.
# The CurrentUser value is still written in that case, so treat it as done.
try {
    Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy RemoteSigned -Force -ErrorAction Stop
} catch [System.Security.SecurityException] {
    if ($_.FullyQualifiedErrorId -notlike "ExecutionPolicyOverride*") { throw }
}
Ok ("execution policy (CurrentUser): " + (Get-ExecutionPolicy -Scope CurrentUser))

if ($SkipRunner) {
    Ok "Toolchain ready; runner setup skipped (-SkipRunner)."
    Ok "Optional: warm the depthai-core install with .\scripts\depthai\fetch-windows.ps1"
    exit 0
}

# --- gh --------------------------------------------------------------------------
Step "GitHub CLI authentication"
# gh prints its status on stderr; under "Stop", Windows PowerShell 5.1 turns a
# redirected native stderr line into a terminating error.
$ErrorActionPreference = "Continue"
& gh auth status *> $null
$ghLoggedIn = ($LASTEXITCODE -eq 0)
$ErrorActionPreference = "Stop"
if (-not $ghLoggedIn) {
    Write-Host "gh is not logged in. Run this once, then rerun this script:" -ForegroundColor Red
    Write-Host "    gh auth login --hostname github.com --git-protocol https --web" -ForegroundColor Red
    Write-Host "(the account needs admin access to $Repo to register runners)" -ForegroundColor Red
    exit 1
}
Ok "gh is logged in"

function Get-RegisteredNames {
    $names = & gh api "repos/$Repo/actions/runners" --paginate --jq ".runners[].name"
    if ($LASTEXITCODE -ne 0) { throw "gh api repos/$Repo/actions/runners failed ($LASTEXITCODE)" }
    return @($names)
}

# --- runner ------------------------------------------------------------------
Step "GitHub Actions runner in $Dir for $Repo"
New-Item -ItemType Directory -Force -Path $Dir | Out-Null
if (-not (Test-Path (Join-Path $Dir "config.cmd"))) {
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/actions/runner/releases/latest" `
        -Headers @{ "User-Agent" = "oak-cam-setup" }
    $version = $release.tag_name.TrimStart("v")
    $assetName = "actions-runner-win-x64-$version.zip"
    $asset = $release.assets | Where-Object { $_.name -eq $assetName }
    if (-not $asset) { throw "Release $version has no asset named $assetName" }
    # The release notes list "<sha256>  <asset>" lines; verify against them.
    $shaLine = ($release.body -split "`n") | Where-Object { $_ -match $assetName -and $_ -match "[0-9a-fA-F]{64}" } | Select-Object -First 1
    $expectedSha = if ($shaLine) { ([regex]::Match($shaLine, "[0-9a-fA-F]{64}")).Value.ToUpperInvariant() } else { $null }
    $zip = Join-Path $env:TEMP $assetName
    Ok "downloading $assetName"
    Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zip -UseBasicParsing
    $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash
    if ($expectedSha) {
        if ($actual -ne $expectedSha) { throw "SHA-256 mismatch for ${assetName}: $actual (expected $expectedSha)" }
        Ok "SHA-256 verified"
    } else {
        Warn "release notes carried no SHA-256 for $assetName; downloaded hash $actual"
    }
    Expand-Archive -Path $zip -DestinationPath $Dir -Force
    Remove-Item $zip
    Ok "runner $version unpacked"
} else {
    Ok "runner already unpacked in $Dir"
}

Push-Location $Dir
try {
    $serviceFile = Join-Path $Dir ".service"
    $runnerFile = Join-Path $Dir ".runner"

    # An existing local registration whose runner GitHub no longer lists
    # (deleted after ~2 weeks offline) cannot connect; clear it (and its
    # service) and register again.
    # The runner writes its service name to .service; fall back to the
    # actions.runner.<owner>-<repo>.<name> naming.
    function Find-RunnerService {
        if (Test-Path $serviceFile) { return (Get-Content -Raw $serviceFile).Trim() }
        $svc = Get-Service | Where-Object { $_.Name -like "actions.runner.*" -and $_.Name -like "*$RunnerName*" } | Select-Object -First 1
        if ($svc) { return $svc.Name }
        return $null
    }

    if (Test-Path $runnerFile) {
        # .runner is JSON written with a UTF-8 BOM.
        $local = (Get-Content -Raw $runnerFile).TrimStart([char]0xFEFF) | ConvertFrom-Json
        if ($local.agentName) { $RunnerName = $local.agentName }
        if (-not $Reregister -and -not ((Get-RegisteredNames) -contains $RunnerName)) {
            Warn "'$RunnerName' is configured here but not registered for $Repo any more; registering again"
            $Reregister = $true
        }
        if ($Reregister) {
            $svcName = Find-RunnerService
            if ($svcName) {
                Stop-Service -Name $svcName -Force -ErrorAction SilentlyContinue
                & sc.exe delete $svcName | Out-Null
                if (Test-Path $serviceFile) { Remove-Item -Force $serviceFile }
                Ok "removed service $svcName"
            }
            foreach ($f in @(".runner", ".credentials", ".credentials_rsaparams")) {
                $p = Join-Path $Dir $f
                if (Test-Path $p) { Remove-Item -Force $p }
            }
            Ok "cleared the local registration"
        }
    }

    $labels = "windows"
    if ($ExtraLabels) { $labels += "," + $ExtraLabels }

    if (Test-Path $runnerFile) {
        Ok "runner '$RunnerName' already registered; keeping it"
    } else {
        Step "Registering '$RunnerName' (labels: self-hosted, Windows, X64, $labels)"
        $token = (& gh api -X POST "repos/$Repo/actions/runners/registration-token" --jq .token)
        if (-not $token) { throw "gh could not mint a registration token for $Repo (does the account have admin access?)" }
        $cfgArgs = @("--unattended", "--url", "https://github.com/$Repo", "--token", $token,
            "--name", $RunnerName, "--labels", $labels, "--work", "_work", "--replace")
        if (-not $Interactive) {
            $secure = Read-Host -AsSecureString "Password for $ServiceAccount (the runner service logs on as this user; a PIN does not count)"
            $plain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR(
                [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure))
            if ([string]::IsNullOrEmpty($plain)) {
                throw "The runner rejects an empty password for $ServiceAccount; rerun with the account password, or with -Interactive and start $Dir\run.cmd from the desktop."
            }
            $cfgArgs += @("--runasservice", "--windowslogonaccount", $ServiceAccount, "--windowslogonpassword", $plain)
        }
        # config.cmd only unblocks the downloaded files and then calls the
        # listener through cmd.exe, which mangles ^ % & and quotes in a
        # password; do the unblock here and call the listener directly.
        Get-ChildItem -LiteralPath $Dir -Recurse -File | Unblock-File -ErrorAction SilentlyContinue
        & .\bin\Runner.Listener.exe configure @cfgArgs
        if ($LASTEXITCODE -ne 0) { throw "runner configure failed with exit code $LASTEXITCODE" }
        Ok "registered"
    }

    $svcName = if ($Interactive) { $null } else { Find-RunnerService }
    if ($Interactive) {
        Ok "Start the runner from a logged-in desktop with: $Dir\run.cmd"
    } elseif ($svcName) {
        Restart-Service -Name $svcName -Force
        Ok "service $svcName is $((Get-Service $svcName).Status)"
    } else {
        Warn "no service is installed for this runner (it was registered with -Interactive); rerun with -Reregister to install one"
    }
} finally {
    Pop-Location
}

Step "Runners registered for $Repo"
& gh api "repos/$Repo/actions/runners" --paginate --jq '.runners[] | "    \(.name)  \(.status)  busy=\(.busy)  [\([.labels[].name] | join(","))]"'

Step "Done"
Ok "Trigger: gh workflow run hardware.yml -R $Repo -f os=windows"
