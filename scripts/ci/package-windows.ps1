# package-windows.ps1 - assemble the native Windows test package for oak-cam,
# smoke-test it, zip it, and write SHA256SUMS.txt + BUILD-INFO.json. Called by
# .github/workflows/package.yml after `cargo build --release -p oak-cam`;
# works the same from a PowerShell prompt at the repository root.
#
# Windows PowerShell 5.1 compatible; keep this file ASCII.
#
# Layout:
#   oak-cam-<version>-windows-x86_64-<sha7>\
#     oak-cam.exe
#     depthai-core.dll, dynamic_calibration.dll, opencv_world*.dll, libusb-1.0.dll
#                                   release DLLs from <depthai root>\current\bin
#     oak_mf_vcam.dll               the Windows 11 virtual camera media source
#     register.ps1                  ADMINISTRATOR, once: registers oak_mf_vcam.dll
#     scripts\vcam\build-windows.ps1  what register.ps1 runs (package mode)
#     LICENSE, THIRD_PARTY_LICENSES.md, README.txt, BUILD-INFO.json
#     licenses\libusb\COPYING       the LGPL text for libusb-1.0.dll
#
# Output: dist\pkg-staging\{<pkg>.zip, SHA256SUMS.txt, BUILD-INFO.json};
# sha7/version/archive as step outputs when $env:GITHUB_OUTPUT is set.
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ws = $env:GITHUB_WORKSPACE
if (-not $ws) { $ws = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path }
$root = if ($env:OAK_DEPTHAI_ROOT) { $env:OAK_DEPTHAI_ROOT } else { Join-Path $env:LOCALAPPDATA "oak-cam\depthai" }
$binDir = Join-Path $root "current\bin"
$exe = Join-Path $ws "target\release\oak-cam.exe"
if (-not (Test-Path $exe)) { throw "release exe not found: $exe (cargo build --release -p oak-cam)" }
if (-not (Test-Path $binDir)) { throw "depthai install not found: $binDir (scripts\depthai\fetch-windows.ps1)" }

# --- version ([workspace.package] in Cargo.toml) + commit ---------------------
$version = $null
$inPkg = $false
foreach ($line in Get-Content (Join-Path $ws "Cargo.toml")) {
    if ($line -match '^\s*\[workspace\.package\]\s*$') { $inPkg = $true; continue }
    if ($line -match '^\s*\[') { $inPkg = $false; continue }
    if ($inPkg -and ($line -match '^\s*version\s*=\s*"([^"]+)"')) { $version = $Matches[1]; break }
}
if (-not $version) { throw "could not parse [workspace.package] version from Cargo.toml" }
$sha = (& git -C $ws rev-parse HEAD).Trim()
$sha7 = $sha.Substring(0, 7)
$depthaiVersion = "unknown"
$flagsPath = Join-Path $root "current\oak-flags.json"
if (Test-Path $flagsPath) {
    $flags = Get-Content -Raw $flagsPath | ConvertFrom-Json
    $depthaiVersion = $flags.version
}

$pkgName = "oak-cam-$version-windows-x86_64-$sha7"
$stage = Join-Path $ws "dist\pkg-staging"
$pkgDir = Join-Path $stage $pkgName
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $pkgDir | Out-Null
Write-Host ">> assembling $pkgName"

Copy-Item $exe (Join-Path $pkgDir "oak-cam.exe")

# --- depthai runtime DLLs (release only: skip the <name>d.dll debug twins) ----
$all = @(Get-ChildItem -Path $binDir -Filter *.dll -File)
$names = @($all | ForEach-Object { $_.Name.ToLowerInvariant() })
$dlls = @($all | Where-Object {
    $n = $_.Name.ToLowerInvariant()
    -not ($n -match '^(.+)d\.dll$' -and ($names -contains ($Matches[1] + ".dll")))
})
if ($dlls.Count -eq 0) { throw "no DLLs under $binDir" }
foreach ($d in $dlls) { Copy-Item $d.FullName (Join-Path $pkgDir $d.Name) }
Write-Host (">> depthai DLLs: " + (($dlls | ForEach-Object { $_.Name }) -join ", "))

# --- virtual camera -------------------------------------------------------------
# Always build (incremental): a DLL left in target\release by an older commit
# must never ship with this one.
Push-Location $ws
try {
    & cargo build --release --locked -p oak-mf-vcam
    if ($LASTEXITCODE -ne 0) { throw "cargo build -p oak-mf-vcam failed ($LASTEXITCODE)" }
} finally {
    Pop-Location
}
$vcamDll = Join-Path $ws "target\release\oak_mf_vcam.dll"
if (-not (Test-Path $vcamDll)) { throw "virtual camera DLL not found: $vcamDll" }
Copy-Item $vcamDll (Join-Path $pkgDir "oak_mf_vcam.dll")
$vcamScripts = Join-Path $pkgDir "scripts\vcam"
New-Item -ItemType Directory -Force -Path $vcamScripts | Out-Null
Copy-Item (Join-Path $ws "scripts\vcam\build-windows.ps1") (Join-Path $vcamScripts "build-windows.ps1")
# build-windows.ps1 finds the package root two levels up and, with
# oak_mf_vcam.dll there and no Cargo.toml, registers the packaged DLL.
$register = @'
# register.ps1 - ADMINISTRATOR, once: install and register the oak-cam virtual
# camera (oak_mf_vcam.dll) for this machine. Pass -Unregister to remove it.
param([switch]$Unregister)
$ErrorActionPreference = "Stop"
$script = Join-Path $PSScriptRoot "scripts\vcam\build-windows.ps1"
if ($Unregister) { & $script -Unregister } else { & $script -Register }
'@
Set-Content -Path (Join-Path $pkgDir "register.ps1") -Value $register -Encoding ASCII

Copy-Item (Join-Path $ws "LICENSE") (Join-Path $pkgDir "LICENSE")
Copy-Item (Join-Path $ws "THIRD_PARTY_LICENSES.md") (Join-Path $pkgDir "THIRD_PARTY_LICENSES.md")
# libusb-1.0.dll is LGPL-2.1-or-later: its license text ships with the DLL.
$libusbLicense = Join-Path $root "current\share\licenses\libusb\COPYING"
if (-not (Test-Path $libusbLicense)) { throw "libusb license not found: $libusbLicense (rerun scripts\depthai\fetch-windows.ps1)" }
$libusbLicenseDir = Join-Path $pkgDir "licenses\libusb"
New-Item -ItemType Directory -Force -Path $libusbLicenseDir | Out-Null
Copy-Item $libusbLicense (Join-Path $libusbLicenseDir "COPYING")

$rustc = (& rustc --version).Trim()
$built = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")

$readme = @"
oak-cam $version test build for Windows x86_64
Commit: $sha
depthai-core: $depthaiVersion

UNSIGNED, UNTESTED test build from the self-hosted CI box. SmartScreen may
warn on first launch (More info -> Run anyway).

1. Extract the whole ZIP to a folder outside your user profile if you will
   register the virtual camera, and keep every DLL beside oak-cam.exe.
2. .\oak-cam.exe --list      .\oak-cam.exe --probe
   .\oak-cam.exe --headless --no-vcam --stats
3. Virtual camera, once, from an ADMINISTRATOR PowerShell in this folder:
     powershell -ExecutionPolicy Bypass -File .\register.ps1
   (copies oak_mf_vcam.dll under Program Files and registers it; Windows 11).

Needs the Microsoft Visual C++ 2015-2022 x64 runtime
(https://aka.ms/vs/17/release/vc_redist.x64.exe) if VCRUNTIME140.dll is missing.
"@
Set-Content -Path (Join-Path $pkgDir "README.txt") -Value $readme -Encoding ASCII

$info = [ordered]@{
    name = "oak-cam"
    version = $version
    commit = $sha
    commit_short = $sha7
    os = "windows"
    target = "x86_64-pc-windows-msvc"
    rustc = $rustc
    depthai_core = $depthaiVersion
    built_utc = $built
    workflow_run_id = "$env:GITHUB_RUN_ID"
}
$infoJson = New-Object PSObject -Property $info | ConvertTo-Json
Set-Content -Path (Join-Path $pkgDir "BUILD-INFO.json") -Value $infoJson -Encoding ASCII

# --- smoke test: launch from the package directory -------------------------------
Write-Host ">> smoke test: oak-cam.exe --version from the package directory"
Push-Location $pkgDir
try {
    $smoke = (& .\oak-cam.exe --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { throw "oak-cam.exe --version exited with $LASTEXITCODE (a missing DLL shows as -1073741515)" }
} finally {
    Pop-Location
}
Write-Host "   $smoke"
if ($smoke -notmatch [regex]::Escape($version)) { throw "--version did not report ${version}: $smoke" }

$archive = "$pkgName.zip"
$archivePath = Join-Path $stage $archive
Compress-Archive -Path $pkgDir -DestinationPath $archivePath -Force
Copy-Item (Join-Path $pkgDir "BUILD-INFO.json") (Join-Path $stage "BUILD-INFO.json")
$sums = foreach ($f in @($archive, "BUILD-INFO.json")) {
    $h = (Get-FileHash -Algorithm SHA256 (Join-Path $stage $f)).Hash.ToLowerInvariant()
    "$h  $f"
}
# sha256sum -c format, LF line endings.
[IO.File]::WriteAllText((Join-Path $stage "SHA256SUMS.txt"), (($sums -join "`n") + "`n"))
Write-Host ">> wrote $archivePath"
Get-Content (Join-Path $stage "SHA256SUMS.txt")

if ($env:GITHUB_OUTPUT) {
    Add-Content -Path $env:GITHUB_OUTPUT -Value "sha7=$sha7"
    Add-Content -Path $env:GITHUB_OUTPUT -Value "version=$version"
    Add-Content -Path $env:GITHUB_OUTPUT -Value "archive=$archive"
}
