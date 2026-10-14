# build-windows.ps1 - build the oak-cam Windows 11 virtual camera
# (ported from stream-concern): the Media Foundation media source DLL oak_mf_vcam.dll from
# extensions/oak-mf-vcam.
#
# Windows PowerShell 5.1 compatible; keep this file ASCII.
#
# Usage (from the repository root):
#   .\scripts\vcam\build-windows.ps1                  # release build; the DLL
#                                                     # lands in target\release
#                                                     # beside oak-cam.exe
#   .\scripts\vcam\build-windows.ps1 -AppDir <dir>    # also copy it into <dir>
#                                                     # (a packaged app folder)
#   .\scripts\vcam\build-windows.ps1 -Register        # ADMINISTRATOR: install
#                                                     # and register for the machine
#   .\scripts\vcam\build-windows.ps1 -Unregister      # ADMINISTRATOR: remove it
#
# Why -Register needs an administrator: the Frame Server services (Local
# System / Local Service) load the media source, and they only see the
# machine registration (HKLM\Software\Classes\CLSID) and only read files
# outside user profiles. -Register therefore copies the DLL to
# %ProgramFiles%\oak-cam\vcam\ and runs regsvr32 on that copy. It is a
# one-time step; the app creates and removes the camera itself (no admin).
#
# -NoBuild skips cargo (register a DLL you already built). -Dll <path>
# registers that file instead. In a packaged app (this script ships as
# scripts\vcam\build-windows.ps1 inside the package, with oak_mf_vcam.dll at
# the package root and no Cargo.toml) the packaged DLL is used and nothing is
# built, so the same command works from the package folder:
#   .\scripts\vcam\build-windows.ps1 -Register

param(
    [string]$AppDir = "",
    [string]$Dll = "",
    [switch]$Register,
    [switch]$Unregister,
    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"
$Root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$Clsid = "{38EE9B00-B255-4538-A39A-9DC8B3D0C64D}"
$DllName = "oak_mf_vcam.dll"
$InstallDir = Join-Path $env:ProgramFiles "oak-cam\vcam"

function Test-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $p = New-Object Security.Principal.WindowsPrincipal($id)
    return $p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Invoke-Regsvr32([string[]]$ArgList) {
    $proc = Start-Process -FilePath "$env:SystemRoot\System32\regsvr32.exe" `
        -ArgumentList $ArgList -Wait -PassThru -NoNewWindow
    return $proc.ExitCode
}

if ($Unregister) {
    if (-not (Test-Admin)) { throw "-Unregister needs an administrator PowerShell." }
    $installed = Join-Path $InstallDir $DllName
    if (Test-Path $installed) {
        $code = Invoke-Regsvr32 @("/u", "/s", "`"$installed`"")
        Write-Host "regsvr32 /u exit code $code"
    }
    Remove-Item -Path "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Classes\CLSID\$Clsid" `
        -Recurse -ErrorAction SilentlyContinue
    Write-Host "Unregistered the oak-cam virtual camera ($Clsid)."
    exit 0
}

$Built = Join-Path $Root "target\release\$DllName"
$Packaged = (Test-Path (Join-Path $Root $DllName)) -and -not (Test-Path (Join-Path $Root "Cargo.toml"))
if ($Dll -ne "") {
    $Built = (Resolve-Path $Dll).Path
    $NoBuild = $true
} elseif ($Packaged) {
    $Built = Join-Path $Root $DllName
    $NoBuild = $true
}
if (-not $NoBuild) {
    Push-Location $Root
    try {
        & cargo build --release -p oak-mf-vcam
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
}
if (-not (Test-Path $Built)) { throw "$Built not found; build it first (omit -NoBuild)." }
Write-Host "Built $Built"

if ($AppDir -ne "") {
    New-Item -ItemType Directory -Force -Path $AppDir | Out-Null
    Copy-Item -Force $Built (Join-Path $AppDir $DllName)
    Write-Host "Copied $DllName into $AppDir"
}

if ($Register) {
    if (-not (Test-Admin)) {
        throw "-Register needs an administrator PowerShell (the Frame Server reads the machine registration)."
    }
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $target = Join-Path $InstallDir $DllName
    try {
        Copy-Item -Force $Built $target
    } catch {
        # The Frame Server keeps a loaded source DLL open; install beside it
        # under a new name and register that one instead.
        $stamp = Get-Date -Format "yyyyMMddHHmmss"
        $target = Join-Path $InstallDir "oak_mf_vcam-$stamp.dll"
        Copy-Item -Force $Built $target
        Write-Host "The previous DLL is in use; installed as $target"
    }
    $code = Invoke-Regsvr32 @("/s", "`"$target`"")
    if ($code -ne 0) { throw "regsvr32 failed with exit code $code" }
    $key = "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Classes\CLSID\$Clsid\InprocServer32"
    $registered = (Get-ItemProperty -Path $key -ErrorAction Stop).'(default)'
    Write-Host "Registered $Clsid -> $registered"
    # The camera services keep a loaded source DLL until they stop; stop them
    # so the next camera loads this one (they restart on demand).
    foreach ($svc in @("FrameServer", "FrameServerMonitor")) {
        Stop-Service -Name $svc -Force -ErrorAction SilentlyContinue
    }
    Write-Host "Stopped FrameServer and FrameServerMonitor (they restart on demand)."
    Write-Host "Run 'oak-cam vcam test-pattern'; other apps then list 'OAK RGB'."
}
