#Requires -Version 5.0
<#
.SYNOPSIS
    Installs Huragok into the game.

.DESCRIPTION
    Copies the dwmapi proxy next to the game and drops huragok.dll into the game's
    mods folder. Run this from the folder you extracted the release into, so
    huragok.dll sits next to the script.

.PARAMETER GamePath
    The game's install folder, Content folder, or binaries folder. Omit it to
    auto-detect the game from your Steam libraries.

.PARAMETER DwmapiPath
    The dwmapi.dll to use as the proxy. Defaults to the copy already on this PC.
    This is Windows' own DLL and is never shipped with Huragok.

.EXAMPLE
    .\install.ps1

.EXAMPLE
    .\install.ps1 -GamePath "D:\Games\Campaign Evolved"
#>
[CmdletBinding()]
param(
    [string]$GamePath,
    [string]$DwmapiPath = (Join-Path $env:SystemRoot 'System32\dwmapi.dll')
)

$ErrorActionPreference = 'Stop'
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }

# --- Presentation ---------------------------------------------------------
$Glyph = @{
    Bullet = [char]0x25CF  # filled circle
    Check  = [char]0x2713  # check mark
    Cross  = [char]0x2717  # cross
    Arrow  = [char]0x2192  # arrow
}

function Write-Header {
    Write-Host ''
    Write-Host '  ' -NoNewline
    Write-Host $Glyph.Bullet -ForegroundColor Cyan -NoNewline
    Write-Host ' Huragok' -ForegroundColor White
    Write-Host '    In-game gameplay toolbox and control panel' -ForegroundColor DarkGray
    Write-Host ''
}

function Write-Task($Message) {
    Write-Host '  ' -NoNewline
    Write-Host $Glyph.Arrow -ForegroundColor Cyan -NoNewline
    Write-Host " $Message" -ForegroundColor Gray
}

function Write-Done($Label, $Detail) {
    Write-Host '  ' -NoNewline
    Write-Host $Glyph.Check -ForegroundColor Green -NoNewline
    Write-Host ' ' -NoNewline
    Write-Host $Label.PadRight(8) -ForegroundColor Gray -NoNewline
    Write-Host $Detail -ForegroundColor DarkGray
}

function Write-Failure($Message) {
    Write-Host ''
    Write-Host '  ' -NoNewline
    Write-Host $Glyph.Cross -ForegroundColor Red -NoNewline
    Write-Host " $Message" -ForegroundColor Red
    Write-Host ''
}

# --- Steam discovery ------------------------------------------------------
function Get-SteamRoot {
    foreach ($key in 'HKCU:\Software\Valve\Steam', 'HKLM:\SOFTWARE\WOW6432Node\Valve\Steam', 'HKLM:\SOFTWARE\Valve\Steam') {
        try {
            $root = (Get-ItemProperty -Path $key -ErrorAction Stop).InstallPath
            if ($root -and (Test-Path $root)) { return $root }
        } catch { }
    }
    return $null
}

function Get-SteamLibraries($SteamRoot) {
    $libraries = @($SteamRoot)
    $vdf = Join-Path $SteamRoot 'steamapps\libraryfolders.vdf'
    if (Test-Path $vdf) {
        foreach ($line in Get-Content $vdf) {
            if ($line -match '"path"\s+"([^"]+)"') {
                $libraries += ($matches[1] -replace '\\\\', '\')
            }
        }
    }
    return $libraries | Select-Object -Unique
}

function Resolve-GameBinaryDirectory([string]$Path) {
    if (-not $Path) { return $null }

    # Steam's current package puts the executable under Content\Meteorite, while
    # earlier Steam installs used Meteorite directly. Accept either layout, as well
    # as a path supplied directly to the binaries directory.
    $candidates = @(
        $Path,
        (Join-Path $Path 'Content\Meteorite\Binaries\Win64'),
        (Join-Path $Path 'Content\Meteorite\Binaries\WinGDK'),
        (Join-Path $Path 'Meteorite\Binaries\Win64'),
        (Join-Path $Path 'Meteorite\Binaries\WinGDK')
    ) | Select-Object -Unique

    foreach ($candidate in $candidates) {
        if (Test-Path (Join-Path $candidate 'HaloCampaignEvolved.exe')) {
            return $candidate
        }
    }
    return $null
}

function Find-GameBinaryDirectory {
    $steam = Get-SteamRoot
    if (-not $steam) { return $null }
    foreach ($library in Get-SteamLibraries $steam) {
        $common = Join-Path $library 'steamapps\common'
        if (-not (Test-Path $common)) { continue }
        foreach ($dir in Get-ChildItem -Path $common -Directory -ErrorAction SilentlyContinue) {
            $binaryDirectory = Resolve-GameBinaryDirectory $dir.FullName
            if ($binaryDirectory) { return $binaryDirectory }
        }
    }
    return $null
}

# --- Install --------------------------------------------------------------
Write-Header

try {
    # The mod DLL ships next to this script.
    $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
    $modDll = Join-Path $scriptDir 'huragok.dll'
    if (-not (Test-Path $modDll)) {
        throw "Huragok.dll is not next to this script. Run install.ps1 from the folder you extracted the release into."
    }
    if (-not (Test-Path $DwmapiPath)) {
        throw "No dwmapi.dll at '$DwmapiPath'. Pass -DwmapiPath to point at one."
    }

    if (-not $GamePath) {
        Write-Task 'Searching your Steam libraries for the game'
        $win64 = Find-GameBinaryDirectory
    }
    if (-not $win64) {
        $win64 = Resolve-GameBinaryDirectory $GamePath
    }
    if (-not $win64) {
        throw "Could not find HaloCampaignEvolved.exe. Re-run with -GamePath pointing at the game, Content, or Binaries folder."
    }
    Write-Done 'Game' $win64

    # Copy Windows' own dwmapi.dll in as the proxy the mod loads through.
    $proxyDest = Join-Path $win64 'dwmapi.dll'
    Copy-Item -Path $DwmapiPath -Destination $proxyDest -Force
    Write-Done 'Proxy' $proxyDest

    # Drop the mod into the mods folder, creating it if needed.
    $modsDir = Join-Path $win64 'mods'
    if (-not (Test-Path $modsDir)) {
        New-Item -ItemType Directory -Path $modsDir | Out-Null
    }
    $modDest = Join-Path $modsDir 'huragok.dll'
    Copy-Item -Path $modDll -Destination $modDest -Force
    Write-Done 'Mod' $modDest

    Write-Host ''
    Write-Host '  ' -NoNewline
    Write-Host $Glyph.Check -ForegroundColor Green -NoNewline
    Write-Host ' Installation complete' -ForegroundColor Green
    Write-Host ''
    Write-Host '  Launch the game, load a mission, and press ' -ForegroundColor Gray -NoNewline
    Write-Host 'Ctrl+B' -ForegroundColor White -NoNewline
    Write-Host ' to open the panel.' -ForegroundColor Gray
    Write-Host ''
}
catch {
    Write-Failure $_.Exception.Message
    exit 1
}
