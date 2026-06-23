# isol8 Windows Installer
# Downloads and installs the latest release (isol8.exe + isol8-winhook.dll) from GitHub.
#
# Usage examples:
#   .\install.ps1
#   .\install.ps1 -Version 0.2.4
#   .\install.ps1 -v v0.2.4
#   .\install.ps1 -InstallDir "$env:LOCALAPPDATA\Programs\isol8"
#   irm https://raw.githubusercontent.com/mdnmdn/isol8/main/install.ps1 | iex
#
#Requires -Version 5.1

[CmdletBinding()]
param(
    [Alias("v")]
    [string]$Version,

    [Alias("d")]
    [string]$InstallDir = (Join-Path $env:USERPROFILE ".local\bin"),

    [Alias("h")]
    [switch]$Help
)

$ErrorActionPreference = "Stop"

$Repo = "mdnmdn/isol8"
$Platform = "windows-x64"
$ExeName = "isol8.exe"
$DllName = "isol8-winhook.dll"

function Write-Info($Message)  { Write-Host "[INFO] $Message" -ForegroundColor Blue }
function Write-Ok($Message)    { Write-Host "[SUCCESS] $Message" -ForegroundColor Green }
function Write-Warn($Message)  { Write-Host "[WARNING] $Message" -ForegroundColor Yellow }
function Write-Err($Message)   { Write-Host "[ERROR] $Message" -ForegroundColor Red; exit 1 }

function Show-Help {
    @"
Usage: .\install.ps1 [OPTIONS]

Options:
  -Version, -v VERSION     Install a specific release (e.g. 0.2.4 or v0.2.4)
  -InstallDir, -d DIR      Installation directory (default: %USERPROFILE%\.local\bin)
  -Help, -h                Show this help message

Installs both isol8.exe and isol8-winhook.dll into the same directory. Path grants
are enforced only when the hook DLL is beside the binary.
"@
    exit 0
}

function Get-ReleaseVersion {
    param([string]$Requested)

    if ($Requested) {
        if ($Requested.StartsWith("v")) {
            $Requested = $Requested.Substring(1)
        }
        Write-Info "Using specified version: $Requested"
        return $Requested
    }

    Write-Info "Fetching latest release information..."
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest"
    $tag = $release.tag_name
    if ($tag.StartsWith("v")) {
        $tag = $tag.Substring(1)
    }
    return $tag
}

function Test-InstallDir {
    param([string]$Dir)

    if (-not (Test-Path -LiteralPath $Dir)) {
        New-Item -ItemType Directory -Path $Dir -Force | Out-Null
    }
}

function Install-Isol8 {
    param(
        [string]$Version,
        [string]$Dir
    )

    $downloadUrl = "https://github.com/$Repo/releases/download/v$Version/$Platform.zip"
    $tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("isol8-install-" + [guid]::NewGuid().ToString("n"))
    $zipPath = Join-Path $tempRoot "$Platform.zip"
    $extractDir = Join-Path $tempRoot "extract"

    try {
        New-Item -ItemType Directory -Path $tempRoot -Force | Out-Null
        Write-Info "Downloading isol8 $Version for $Platform..."
        Write-Info "URL: $downloadUrl"

        Invoke-WebRequest -Uri $downloadUrl -OutFile $zipPath -UseBasicParsing

        if (-not (Test-Path -LiteralPath $zipPath) -or ((Get-Item -LiteralPath $zipPath).Length -eq 0)) {
            Write-Err "Downloaded file is missing or empty"
        }

        Write-Info "Extracting archive..."
        New-Item -ItemType Directory -Path $extractDir -Force | Out-Null
        Expand-Archive -LiteralPath $zipPath -DestinationPath $extractDir -Force

        $exeSrc = Get-ChildItem -Path $extractDir -Recurse -Filter $ExeName -File | Select-Object -First 1
        $dllSrc = Get-ChildItem -Path $extractDir -Recurse -Filter $DllName -File | Select-Object -First 1

        if (-not $exeSrc) {
            Write-Err "$ExeName not found in the downloaded archive"
        }
        if (-not $dllSrc) {
            Write-Err "$DllName not found in the downloaded archive (path enforcement requires the hook DLL)"
        }

        Test-InstallDir -Dir $Dir
        Write-Info "Installing to $Dir ..."

        Copy-Item -LiteralPath $exeSrc.FullName -Destination (Join-Path $Dir $ExeName) -Force
        Copy-Item -LiteralPath $dllSrc.FullName -Destination (Join-Path $Dir $DllName) -Force
    }
    finally {
        if (Test-Path -LiteralPath $tempRoot) {
            Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

function Test-PathInUserPath {
    param([string]$Dir)

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not $userPath) { return $false }

    $normalized = $Dir.TrimEnd('\')
    foreach ($entry in $userPath -split ';') {
        if ($entry.TrimEnd('\').Equals($normalized, [StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }
    return $false
}

function Show-PathHint {
    param([string]$Dir)

    $exePath = Join-Path $Dir $ExeName
    if (Test-PathInUserPath -Dir $Dir) {
        Write-Ok "$Dir is already in your user PATH"
        return
    }

    Write-Warn "$Dir is not in your user PATH"
    Write-Host ""
    Write-Host "You can still run isol8 directly from:"
    Write-Host "  $exePath"
    Write-Host ""
    Write-Host "To use 'isol8' without a full path, add the install directory to your user PATH:"
    Write-Host ""
    Write-Host "  [Environment]::SetEnvironmentVariable("
    Write-Host "      'Path',"
    Write-Host "      [Environment]::GetEnvironmentVariable('Path', 'User') + ';$Dir',"
    Write-Host "      'User')"
    Write-Host ""
    Write-Host "Restart your terminal after updating PATH."
}

function Test-Installation {
    param([string]$Dir)

    $exePath = Join-Path $Dir $ExeName
    $dllPath = Join-Path $Dir $DllName

    if (-not (Test-Path -LiteralPath $exePath)) {
        Write-Err "Installation verification failed: $exePath is missing"
    }
    if (-not (Test-Path -LiteralPath $dllPath)) {
        Write-Err "Installation verification failed: $dllPath is missing"
    }

    try {
        $versionOutput = & $exePath @version 2>&1 | Select-Object -First 1
        Write-Ok "Installation verified: $versionOutput"
    }
    catch {
        Write-Err "Installation verification failed: could not run $exePath @version"
    }
}

if ($Help) { Show-Help }

Write-Host "isol8 Windows Installer"
Write-Host "======================="
Write-Host ""

$version = Get-ReleaseVersion -Requested $Version
if (-not $version) {
    Write-Err "Failed to get version information"
}

Write-Info "Version to install: $version"
Write-Info "Install directory: $InstallDir"

Install-Isol8 -Version $version -Dir $InstallDir
Show-PathHint -Dir $InstallDir
Test-Installation -Dir $InstallDir

Write-Host ""
Write-Ok "Installation complete!"
Write-Host ""
Write-Host "Installed files:"
Write-Host "  $(Join-Path $InstallDir $ExeName)"
Write-Host "  $(Join-Path $InstallDir $DllName)"
Write-Host ""
Write-Host "Quick check:"
Write-Host "  $(Join-Path $InstallDir $ExeName) @version"
Write-Host ""
Write-Host "Next steps:"
Write-Host "  isol8 --help"
Write-Host "  isol8 --show-profiles"
Write-Host ""
Write-Host "For more information, visit: https://github.com/$Repo"