#Requires -Version 5.0
<#
.SYNOPSIS
    Installs Huragok into Halo: Campaign Evolved.

.DESCRIPTION
    Copies the dwmapi proxy next to the game and drops huragok.dll into the game's
    mods folder. Run this from the folder you extracted the release into, so
    huragok.dll sits next to the script.

.PARAMETER GamePath
    The game's install folder (the one that contains "Meteorite"). Omit it to
    auto-detect the game from your Steam libraries.

.PARAMETER DwmapiPath
    The dwmapi.dll to use as the proxy. Defaults to the copy already on this PC.
    This is Windows' own DLL and is never shipped with Huragok.

.EXAMPLE
    .\install.ps1

.EXAMPLE
    .\install.ps1 -GamePath "D:\Games\Halo Campaign Evolved"
#>
[CmdletBinding()]
param(
    [string]$GamePath,
    [string]$DwmapiPath = (Join-Path $env:SystemRoot 'System32\dwmapi.dll')
)

$ErrorActionPreference = 'Stop'

function Write-Step($message) { Write-Host "==> $message" -ForegroundColor Cyan }

# --- files this script installs -------------------------------------------
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$modDll = Join-Path $scriptDir 'huragok.dll'
if (-not (Test-Path $modDll)) {
    throw "huragok.dll is not next to this script ($scriptDir). Run install.ps1 from the folder you extracted the release into."
}
if (-not (Test-Path $DwmapiPath)) {
    throw "No dwmapi.dll at '$DwmapiPath'. Pass -DwmapiPath to point at one."
}

# --- find the game --------------------------------------------------------
function Get-SteamRoot {
    foreach ($key in 'HKCU:\Software\Valve\Steam', 'HKLM:\SOFTWARE\WOW6432Node\Valve\Steam', 'HKLM:\SOFTWARE\Valve\Steam') {
        try {
            $root = (Get-ItemProperty -Path $key -ErrorAction Stop).InstallPath
            if ($root -and (Test-Path $root)) { return $root }
        } catch { }
    }
    return $null
}

function Get-SteamLibraries($steamRoot) {
    $libraries = @($steamRoot)
    $vdf = Join-Path $steamRoot 'steamapps\libraryfolders.vdf'
    if (Test-Path $vdf) {
        foreach ($line in Get-Content $vdf) {
            if ($line -match '"path"\s+"([^"]+)"') {
                $libraries += ($matches[1] -replace '\\\\', '\')
            }
        }
    }
    return $libraries | Select-Object -Unique
}

function Find-GamePath {
    $steam = Get-SteamRoot
    if (-not $steam) { return $null }
    foreach ($library in Get-SteamLibraries $steam) {
        $common = Join-Path $library 'steamapps\common'
        if (-not (Test-Path $common)) { continue }
        foreach ($dir in Get-ChildItem -Path $common -Directory -ErrorAction SilentlyContinue) {
            if (Test-Path (Join-Path $dir.FullName 'Meteorite\Binaries\Win64')) {
                return $dir.FullName
            }
        }
    }
    return $null
}

if (-not $GamePath) {
    Write-Step 'Looking for Halo: Campaign Evolved in your Steam libraries'
    $GamePath = Find-GamePath
}
if (-not $GamePath) {
    throw "Could not find the game automatically. Re-run with -GamePath pointing at the folder that contains Meteorite."
}

$win64 = Join-Path $GamePath 'Meteorite\Binaries\Win64'
if (-not (Test-Path $win64)) {
    throw "'$win64' does not exist. Point -GamePath at the game's install folder (the one that contains Meteorite)."
}

# --- install --------------------------------------------------------------
Write-Step "Game folder: $win64"

$proxyDest = Join-Path $win64 'dwmapi.dll'
Write-Step "Copying the proxy to $proxyDest"
Copy-Item -Path $DwmapiPath -Destination $proxyDest -Force

$modsDir = Join-Path $win64 'mods'
if (-not (Test-Path $modsDir)) {
    Write-Step "Creating $modsDir"
    New-Item -ItemType Directory -Path $modsDir | Out-Null
}

$modDest = Join-Path $modsDir 'huragok.dll'
Write-Step "Copying huragok.dll to $modDest"
Copy-Item -Path $modDll -Destination $modDest -Force

Write-Host ''
Write-Host 'Done. Launch the game, load a mission, and press Ctrl+B to open the panel.' -ForegroundColor Green
