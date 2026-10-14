# fetch-windows.ps1 - download and install Luxonis's prebuilt depthai-core
# win64 release for oak-cam on Windows 11 (MSVC, x64). macOS and Linux build
# from source with scripts/depthai/build.sh instead.
#
# Windows PowerShell 5.1 compatible; keep this file ASCII.
#
# Layout (default root %LOCALAPPDATA%\oak-cam\depthai, override with -Root or
# $env:OAK_DEPTHAI_ROOT so `cargo clean` never removes it):
#   <root>\downloads\          the pinned archives (removed after install unless -KeepDownloads)
#   <root>\<tag>\              install prefix: the release zip's contents (bin\, include\,
#                              lib\, lib\cmake\depthai, share\cmake), plus
#                              bin\opencv_world<ver>.dll, bin\libusb-1.0.dll (with
#                              share\licenses\libusb\COPYING) and oak-flags.json
#   <root>\opencv-<ver>\       the OpenCV headers, import library and CMake package the
#                              prebuilt depthai-core was built against (see below)
#   <root>\current             junction to the install prefix crates/depthai-sys/build.rs uses
#
# Options:
#   -Tag <vX.Y.Z>     depthai-core release tag (default: the pinned $DepthaiTag below)
#   -Sha256 <hex>     expected SHA-256 of that tag's win64 zip (required for any tag
#                     other than the pinned one)
#   -Root <dir>       cache root (default: $env:OAK_DEPTHAI_ROOT or
#                     %LOCALAPPDATA%\oak-cam\depthai)
#   -Clean            remove the install prefix and OpenCV tree first
#   -SkipProbe        do not run the flags probe after install
#   -KeepDownloads    keep the downloaded archives under <root>\downloads
#
# Why OpenCV: Luxonis builds the win64 release with DEPTHAI_OPENCV_SUPPORT=ON
# (and xtensor ON), unlike build.sh. depthai-core.dll imports
# opencv_world<ver>.dll, its depthaiConfig.cmake runs find_dependency(OpenCV 4),
# and its public headers include <opencv2/...> when DEPTHAI_HAVE_OPENCV_SUPPORT
# is defined. So the matching official OpenCV Windows package is pinned here
# too: its headers let the shim compile with the exported definitions, and its
# DLL is copied into <prefix>\bin so one directory on PATH (or beside
# oak-cam.exe) holds every runtime DLL. The xtensor/xtl/nlohmann/libnop headers
# ship inside the depthai zip.
#
# Why libusb: depthai-core.dll delay-loads libusb-1.0.dll for XLink's USB
# transport, but the release zip does not ship it. Without it XLink reports
# "USB protocol not available" and no USB device is listed.
# So the Luxonis libusb fork, at the commit depthai-core's own vcpkg port
# pins, is built here as a shared library with CMake + MSVC (which the probe
# already requires) and copied into <prefix>\bin beside the other DLLs, with
# its LGPL license text under <prefix>\share\licenses\libusb.
#
# The probe (scripts\depthai\probe\CMakeLists.txt) needs CMake and an MSVC C++
# toolchain (Visual Studio 2022 Build Tools with the C++ workload); it writes
# <prefix>\oak-flags.json with the include directories and definitions the shim
# is compiled with.
[CmdletBinding()]
param(
    [string]$Tag = "",
    [string]$Sha256 = "",
    [string]$Root = "",
    [switch]$Clean,
    [switch]$SkipProbe,
    [switch]$KeepDownloads
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
# Invoke-WebRequest's progress bar makes large downloads many times slower on 5.1.
$ProgressPreference = "SilentlyContinue"

# Pinned release. The hash is the SHA-256 of the GitHub release asset
# (it matches the digest GitHub reports for the asset).
$DepthaiTag = "v3.10.0"
$DepthaiRepo = "https://github.com/luxonis/depthai-core"
$DepthaiPinnedSha256 = @{
    "v3.10.0" = "4acfce6fafd9966c2cd0730e4f343e8f912a32a47e5f712738effbad621e0689"
}
# The OpenCV release depthai-core $DepthaiTag's win64 build links against
# (depthai-core.dll imports opencv_world4130.dll). Bump together with the tag.
$OpenCvVersion = "4.13.0"
$OpenCvUrl = "https://github.com/opencv/opencv/releases/download/4.13.0/opencv-4.13.0-windows.exe"
$OpenCvSha256 = "f0e98c302464d6860777a7015065e11b9b271b5394e6ba92663f0cf1fc303f2c"
$OpenCvWorld = "opencv_world" + ($OpenCvVersion -replace "\.", "")
# The libusb fork and commit depthai-core $DepthaiTag's vcpkg port builds
# against (cmake/ports/libusb/portfile.cmake, version 1.0.27). The hash is of
# GitHub's source archive for that commit. Bump together with the tag.
$LibusbRepo = "https://github.com/luxonis/libusb"
$LibusbRef = "745c272a2f0305c4ab162b7fb1b2edc7578c6b77"
$LibusbSha256 = "ef775cd3cb883c815da77f4745d8650f3a816ea0d14ffeb903d9e4cce2fb789b"

function Say([string]$msg) { Write-Host ">> $msg" }
function Die([string]$msg) { throw "error: $msg" }

if ($Tag -eq "") { $Tag = $DepthaiTag }
if ($Sha256 -eq "") {
    if (-not $DepthaiPinnedSha256.ContainsKey($Tag)) {
        Die "no pinned SHA-256 for depthai-core $Tag; pass -Sha256 <hex> (and check the OpenCV pin)"
    }
    $Sha256 = $DepthaiPinnedSha256[$Tag]
}
if ($Root -eq "") {
    if ($env:OAK_DEPTHAI_ROOT) {
        $Root = $env:OAK_DEPTHAI_ROOT
    } else {
        if (-not $env:LOCALAPPDATA) { Die "LOCALAPPDATA is not set; pass -Root <dir>" }
        $Root = Join-Path $env:LOCALAPPDATA "oak-cam\depthai"
    }
}

$ScriptDir = $PSScriptRoot
$Downloads = Join-Path $Root "downloads"
$Prefix = Join-Path $Root $Tag
$OpenCvRoot = Join-Path $Root "opencv-$OpenCvVersion"
$OpenCvDir = Join-Path $OpenCvRoot "build\x64\vc16\lib"
$Current = Join-Path $Root "current"
$Stamp = Join-Path $Prefix ".oak-fetch-stamp"
$LibusbStamp = Join-Path $Prefix ".oak-libusb-stamp"
New-Item -ItemType Directory -Force -Path $Root, $Downloads | Out-Null

# cmake (builds libusb, and runs the flags probe): a runner started before
# winget put CMake on PATH (or a service with the machine PATH from before the
# install) will not see it, so fall back to the usual install locations and
# prepend the one found.
if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
    $candidates = @(
        (Join-Path $env:ProgramFiles "CMake\bin"),
        (Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin"),
        (Join-Path $env:ProgramFiles "Microsoft Visual Studio\2022\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin"),
        (Join-Path $env:ProgramFiles "Microsoft Visual Studio\2022\Professional\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin")
    )
    $found = $candidates | Where-Object { Test-Path (Join-Path $_ "cmake.exe") } | Select-Object -First 1
    if ($found) {
        $env:Path = "$found;$env:Path"
        Say "cmake found at $found (not on PATH; added for this run)"
    } else {
        Die "cmake not found (winget install Kitware.CMake, or install Visual Studio's CMake component, then restart the runner so it sees the new PATH)"
    }
}

# Download $url to $out unless a file with the expected hash is already there.
function Get-Pinned([string]$url, [string]$out, [string]$sha) {
    if (Test-Path $out) {
        $have = (Get-FileHash -Algorithm SHA256 $out).Hash.ToLowerInvariant()
        if ($have -eq $sha.ToLowerInvariant()) {
            Say "using cached $out"
            return
        }
        Remove-Item -Force $out
    }
    Say "downloading $url"
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $url -OutFile $out -UseBasicParsing
    $have = (Get-FileHash -Algorithm SHA256 $out).Hash.ToLowerInvariant()
    if ($have -ne $sha.ToLowerInvariant()) {
        Remove-Item -Force $out
        Die "SHA-256 mismatch for ${url}: got $have, expected $sha"
    }
    Say "SHA-256 verified ($have)"
}

function Find-7z {
    $cmd = Get-Command 7z.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    foreach ($dir in @($env:ProgramFiles, ${env:ProgramFiles(x86)})) {
        if ($dir) {
            $p = Join-Path $dir "7-Zip\7z.exe"
            if (Test-Path $p) { return $p }
        }
    }
    return $null
}

if ($Clean) {
    foreach ($d in @($Prefix, $OpenCvRoot)) {
        if (Test-Path $d) { Say "removing $d"; Remove-Item -Recurse -Force $d }
    }
}

# --- depthai-core ------------------------------------------------------------
$zipName = "depthai-core-$Tag-win64.zip"
$zipUrl = "$DepthaiRepo/releases/download/$Tag/$zipName"
$stampText = "tag=$Tag`nsha256=$Sha256`nopencv=$OpenCvVersion`n"
$installed = (Test-Path $Stamp) -and ((Get-Content -Raw $Stamp) -eq $stampText) -and
    (Test-Path (Join-Path $Prefix "lib\cmake\depthai\depthaiConfig.cmake"))
if ($installed) {
    Say "depthai-core $Tag already installed at $Prefix"
} else {
    $zip = Join-Path $Downloads $zipName
    Get-Pinned $zipUrl $zip $Sha256
    if (Test-Path $Prefix) { Remove-Item -Recurse -Force $Prefix }
    $unpack = Join-Path $Root "unpack-$Tag"
    if (Test-Path $unpack) { Remove-Item -Recurse -Force $unpack }
    New-Item -ItemType Directory -Force -Path $unpack | Out-Null
    Say "extracting $zipName"
    # tar.exe (bsdtar, in Windows 10 1803+) is much faster than Expand-Archive.
    if (Get-Command tar.exe -ErrorAction SilentlyContinue) {
        & tar.exe -xf $zip -C $unpack
        if ($LASTEXITCODE -ne 0) { Die "tar -xf $zip failed ($LASTEXITCODE)" }
    } else {
        Expand-Archive -Path $zip -DestinationPath $unpack -Force
    }
    # The zip holds one top-level directory (depthai-core-<tag>-win64); it
    # becomes the prefix.
    $top = @(Get-ChildItem -Path $unpack -Directory)
    if ($top.Count -ne 1) { Die "expected one top-level directory in $zipName, found $($top.Count)" }
    Move-Item -Path $top[0].FullName -Destination $Prefix
    Remove-Item -Recurse -Force $unpack
    if (-not (Test-Path (Join-Path $Prefix "bin\depthai-core.dll"))) { Die "$Prefix\bin\depthai-core.dll missing after extract" }
}

# --- OpenCV ------------------------------------------------------------------
$worldDll = Join-Path $OpenCvRoot "build\x64\vc16\bin\$OpenCvWorld.dll"
if ((Test-Path (Join-Path $OpenCvDir "OpenCVConfig.cmake")) -and (Test-Path $worldDll)) {
    Say "OpenCV $OpenCvVersion already at $OpenCvRoot"
} else {
    $exe = Join-Path $Downloads "opencv-$OpenCvVersion-windows.exe"
    Get-Pinned $OpenCvUrl $exe $OpenCvSha256
    if (Test-Path $OpenCvRoot) { Remove-Item -Recurse -Force $OpenCvRoot }
    New-Item -ItemType Directory -Force -Path $OpenCvRoot | Out-Null
    # The package is a 7-Zip self-extractor holding opencv\build and
    # opencv\sources (~1.2 GB unpacked). Only the headers, the vc16 CMake
    # package + import libraries and the world DLLs are kept. The debug DLL
    # stays because OpenCVModules.cmake checks that every file it references
    # exists.
    $keep = @(
        "opencv\build\include",
        "opencv\build\x64\vc16\lib",
        "opencv\build\x64\vc16\bin\$OpenCvWorld.dll",
        "opencv\build\x64\vc16\bin\${OpenCvWorld}d.dll",
        "opencv\build\LICENSE"
    )
    $sevenZip = Find-7z
    $tmp = Join-Path $Root "unpack-opencv"
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    if ($sevenZip) {
        Say "extracting OpenCV $OpenCvVersion with $sevenZip"
        & $sevenZip x $exe "-o$tmp" -y @keep | Out-Null
        if ($LASTEXITCODE -ne 0) { Die "7z x $exe failed ($LASTEXITCODE)" }
    } else {
        Say "extracting OpenCV $OpenCvVersion (self-extractor; install 7-Zip to make this faster)"
        $p = Start-Process -FilePath $exe -ArgumentList @("-o`"$tmp`"", "-y") -Wait -PassThru
        if ($p.ExitCode -ne 0) { Die "$exe -o$tmp failed ($($p.ExitCode))" }
    }
    foreach ($rel in $keep) {
        $src = Join-Path $tmp $rel
        if (-not (Test-Path $src)) { Die "OpenCV package is missing $rel" }
        $dst = Join-Path $OpenCvRoot ($rel -replace "^opencv\\", "")
        New-Item -ItemType Directory -Force -Path (Split-Path $dst -Parent) | Out-Null
        Move-Item -Path $src -Destination $dst
    }
    Remove-Item -Recurse -Force $tmp
}
# One runtime directory: depthai-core.dll, dynamic_calibration.dll, the
# OpenCV world DLL and (below) libusb-1.0.dll side by side.
Copy-Item -Force $worldDll (Join-Path $Prefix "bin\$OpenCvWorld.dll")
Set-Content -Path $Stamp -Value $stampText -NoNewline -Encoding ASCII

# --- libusb --------------------------------------------------------------------
# Its own stamp, so a libusb pin bump rebuilds only libusb. The stamp lives in
# the prefix, so a depthai-core reinstall (which replaces the prefix) rebuilds
# it too.
$libusbDll = Join-Path $Prefix "bin\libusb-1.0.dll"
$libusbStampText = "ref=$LibusbRef`nsha256=$LibusbSha256`n"
$libusbInstalled = (Test-Path $LibusbStamp) -and
    ((Get-Content -Raw $LibusbStamp) -eq $libusbStampText) -and (Test-Path $libusbDll)
if ($libusbInstalled) {
    Say "libusb $LibusbRef already at $libusbDll"
} else {
    $libusbZipName = "libusb-$LibusbRef.zip"
    $libusbZip = Join-Path $Downloads $libusbZipName
    Get-Pinned "$LibusbRepo/archive/$LibusbRef.zip" $libusbZip $LibusbSha256
    $libusbTmp = Join-Path $Root "unpack-libusb"
    if (Test-Path $libusbTmp) { Remove-Item -Recurse -Force $libusbTmp }
    $libusbSrcRoot = Join-Path $libusbTmp "src"
    New-Item -ItemType Directory -Force -Path $libusbSrcRoot | Out-Null
    Say "extracting $libusbZipName"
    if (Get-Command tar.exe -ErrorAction SilentlyContinue) {
        & tar.exe -xf $libusbZip -C $libusbSrcRoot
        if ($LASTEXITCODE -ne 0) { Die "tar -xf $libusbZip failed ($LASTEXITCODE)" }
    } else {
        Expand-Archive -Path $libusbZip -DestinationPath $libusbSrcRoot -Force
    }
    # GitHub's archive holds one top-level directory (libusb-<ref>).
    $top = @(Get-ChildItem -Path $libusbSrcRoot -Directory)
    if ($top.Count -ne 1) { Die "expected one top-level directory in $libusbZipName, found $($top.Count)" }
    $libusbSrc = $top[0].FullName
    $libusbBuild = Join-Path $libusbTmp "build"
    $fwd = { param($p) $p -replace "\\", "/" }
    Say "building libusb $LibusbRef (shared, Release)"
    & cmake -S (& $fwd $libusbSrc) -B (& $fwd $libusbBuild) -A x64 "-DBUILD_SHARED_LIBS=ON"
    if ($LASTEXITCODE -ne 0) { Die "libusb cmake configure failed ($LASTEXITCODE)" }
    & cmake --build (& $fwd $libusbBuild) --config Release
    if ($LASTEXITCODE -ne 0) { Die "libusb cmake build failed ($LASTEXITCODE)" }
    $builtDll = Join-Path $libusbBuild "Release\libusb-1.0.dll"
    if (-not (Test-Path $builtDll)) { Die "$builtDll missing after build" }
    Copy-Item -Force $builtDll $libusbDll
    # libusb is LGPL-2.1-or-later; its license text travels with the DLL.
    $licenseDir = Join-Path $Prefix "share\licenses\libusb"
    New-Item -ItemType Directory -Force -Path $licenseDir | Out-Null
    Copy-Item -Force (Join-Path $libusbSrc "COPYING") (Join-Path $licenseDir "COPYING")
    Remove-Item -Recurse -Force $libusbTmp
    Set-Content -Path $LibusbStamp -Value $libusbStampText -NoNewline -Encoding ASCII
    Say "installed $libusbDll"
}

# --- current junction ----------------------------------------------------------
if (Test-Path $Current) {
    # Removes the junction itself, never the target's contents.
    (Get-Item $Current).Delete()
}
New-Item -ItemType Junction -Path $Current -Target $Prefix | Out-Null
Say "linked $Current -> $Prefix"

# --- probe ---------------------------------------------------------------------
if (-not $SkipProbe) {
    Say "probing exported compile flags"
    $probeBuild = Join-Path $Prefix ".probe-build"
    if (Test-Path $probeBuild) { Remove-Item -Recurse -Force $probeBuild }
    # Forward slashes: CMake paths, and oak-flags.json stays valid JSON.
    $fwd = { param($p) $p -replace "\\", "/" }
    $flagsOut = Join-Path $Prefix "oak-flags.json"
    & cmake -S (& $fwd (Join-Path $ScriptDir "probe")) -B (& $fwd $probeBuild) `
        "-DCMAKE_PREFIX_PATH=$(& $fwd $Prefix)" `
        "-DCMAKE_INSTALL_PREFIX=$(& $fwd $Prefix)" `
        "-DOpenCV_DIR=$(& $fwd $OpenCvDir)" `
        "-DOAK_FLAGS_OUT=$(& $fwd $flagsOut)"
    if ($LASTEXITCODE -ne 0) { Die "flags probe failed ($LASTEXITCODE)" }
    Remove-Item -Recurse -Force $probeBuild
    Say "wrote $flagsOut"
}

if (-not $KeepDownloads -and (Test-Path $Downloads)) {
    Remove-Item -Recurse -Force $Downloads
}

Write-Host ""
Write-Host "================================================================"
Write-Host "  depthai-core $Tag installed at $Prefix"
Write-Host "  cargo picks it up via $Current (or DEPTHAI_PREFIX=$Prefix)"
Write-Host "  runtime DLLs: put $Current\bin on PATH or copy them beside oak-cam.exe"
Write-Host "================================================================"
