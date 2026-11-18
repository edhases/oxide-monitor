# Packages Oxide Monitor as a portable Windows x64 ZIP.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\package.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\package.ps1 -Version 0.1.0
#   powershell -ExecutionPolicy Bypass -File scripts\package.ps1 -SkipBuild
#
# Produces:
#   dist\oxide-monitor-<version>\oxide-monitor.exe
#   dist\oxide-monitor-<version>\oxide-settings.exe
#   dist\oxide-monitor-<version>\oxide-pawnio-host.exe
#   dist\oxide-monitor-<version>\PawnIO-NOTICE.md
#   dist\oxide-monitor-<version>\COPYING-PawnIO-Modules.txt
#   dist\oxide-monitor-<version>\config.toml
#   dist\oxide-monitor-<version>\README.md
#   dist\oxide-monitor-<version>\AUTOSTART.md
#   dist\oxide-monitor-<version>\PROVIDERS.md
#   dist\oxide-monitor-<version>-win-x64.zip
#
# The version defaults to [workspace.package].version in the root Cargo.toml.

[CmdletBinding()]
param(
    [string]$Version = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))

function Get-WorkspaceVersion {
    param([string]$Root)

    $cargoToml = Join-Path $Root "Cargo.toml"
    if (-not (Test-Path -LiteralPath $cargoToml -PathType Leaf)) {
        throw "Cargo.toml not found at $cargoToml"
    }

    $text = Get-Content -Raw -LiteralPath $cargoToml
    $section = [regex]::Match(
        $text,
        '(?ms)^\s*\[workspace\.package\]\s*(.*?)(?=^\s*\[|\z)')
    if (-not $section.Success) {
        throw "Could not find [workspace.package] in $cargoToml"
    }

    $versionMatch = [regex]::Match($section.Groups[1].Value, '(?m)^\s*version\s*=\s*"([^"]+)"')
    if (-not $versionMatch.Success) {
        throw "Could not find [workspace.package].version in $cargoToml"
    }

    return $versionMatch.Groups[1].Value
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $packageVersion = Get-WorkspaceVersion -Root $root
}
else {
    $packageVersion = $Version.Trim()
}

# Keep the version safe for a directory and zip file name.
if ($packageVersion -notmatch '^\d+\.\d+\.\d+([-.+][0-9A-Za-z.-]+)?$') {
    throw "Invalid package version '$packageVersion'. Expected a Cargo semver such as 0.1.0."
}

Push-Location $root
try {
    $exePath = Join-Path $root "target\release\oxide-monitor.exe"
    $settingsExePath = Join-Path $root "target\release\oxide-settings.exe"
    $hostExePath = Join-Path $root "target\release\oxide-pawnio-host.exe"

    if (-not $SkipBuild) {
        Write-Host "==> cargo build --release --bin oxide-monitor --bin oxide-settings --bin oxide-pawnio-host"
        & cargo build --release --bin oxide-monitor --bin oxide-settings --bin oxide-pawnio-host
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed with exit code $LASTEXITCODE"
        }
    }
    else {
        Write-Host "==> Skipping cargo build (-SkipBuild)"
    }

    if (-not (Test-Path -LiteralPath $exePath -PathType Leaf)) {
        throw "Release binary not found: $exePath. Build it with: cargo build --release --bin oxide-monitor --bin oxide-settings --bin oxide-pawnio-host"
    }
    if (-not (Test-Path -LiteralPath $settingsExePath -PathType Leaf)) {
        throw "Release binary not found: $settingsExePath. Build it with: cargo build --release --bin oxide-monitor --bin oxide-settings --bin oxide-pawnio-host"
    }
    if (-not (Test-Path -LiteralPath $hostExePath -PathType Leaf)) {
        throw "Release helper not found: $hostExePath. Build it with: cargo build --release --bin oxide-monitor --bin oxide-settings --bin oxide-pawnio-host"
    }

    $distDir = Join-Path $root "dist"
    $stageName = "oxide-monitor-$packageVersion"
    $stageDir = Join-Path $distDir $stageName
    $zipPath = Join-Path $distDir "$stageName-win-x64.zip"

    if (-not (Test-Path -LiteralPath $distDir)) {
        New-Item -ItemType Directory -Path $distDir -Force | Out-Null
    }

    Write-Host "==> Staging $stageName"
    if (Test-Path -LiteralPath $stageDir) {
        Remove-Item -LiteralPath $stageDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $stageDir -Force | Out-Null

    Copy-Item -LiteralPath $exePath -Destination (Join-Path $stageDir "oxide-monitor.exe")
    Copy-Item -LiteralPath $settingsExePath -Destination (Join-Path $stageDir "oxide-settings.exe")
    Copy-Item -LiteralPath $hostExePath -Destination (Join-Path $stageDir "oxide-pawnio-host.exe")

    # LGPL-2.1 module notice + full license text travel with the helper binary.
    $pawnioNotice = Join-Path $root "crates\oxide-pawnio-host\assets\NOTICE.md"
    $pawnioLicense = Join-Path $root "crates\oxide-pawnio-host\assets\COPYING-PawnIO-Modules.txt"
    if (-not (Test-Path -LiteralPath $pawnioNotice)) {
        throw "PawnIO notice not found: $pawnioNotice"
    }
    if (-not (Test-Path -LiteralPath $pawnioLicense)) {
        throw "PawnIO LGPL text not found: $pawnioLicense"
    }
    Copy-Item -LiteralPath $pawnioNotice -Destination (Join-Path $stageDir "PawnIO-NOTICE.md")
    Copy-Item -LiteralPath $pawnioLicense -Destination (Join-Path $stageDir "COPYING-PawnIO-Modules.txt")

    $sampleConfig = Join-Path $root "resources\config.toml"
    if (-not (Test-Path -LiteralPath $sampleConfig -PathType Leaf)) {
        Write-Host "    resources\config.toml is missing; writing a default sample"
        $fallbackConfig = @'
version = 1

[general]
language = "en"
autostart = false
theme = "dark"
cpu_temp_source = "provider"
gpu_poll_ms = 3000

[taskbar]
show = true
width = 294
height = 35
rows = 2
item_space = 6

[format]
unit_byte = false
speed_unit = "auto"
digits = 4

[providers]
hwinfo = false
lhm_http = false
helper = false
lhm_url = "http://127.0.0.1:8085/metrics"

[[items]]
id = "up"
label = "UP:"
visible = true

[[items]]
id = "down"
label = "DOWN:"
visible = true

[[items]]
id = "cpu"
label = "CPU:"
visible = true

[[items]]
id = "memory"
label = "MEM:"
visible = true

[[items]]
id = "gpu"
label = "GPU:"
visible = true

[[items]]
id = "cpu_temp"
label = "CPU:"
visible = true

[[items]]
id = "gpu_temp"
label = "GPU:"
visible = true

[[items]]
id = "hdd_temp"
label = "HDD:"
visible = true
'@
        $resourcesDir = Split-Path -Parent $sampleConfig
        if (-not (Test-Path -LiteralPath $resourcesDir)) {
            New-Item -ItemType Directory -Path $resourcesDir -Force | Out-Null
        }
        $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
        [System.IO.File]::WriteAllText($sampleConfig, $fallbackConfig, $utf8NoBom)
    }
    Copy-Item -LiteralPath $sampleConfig -Destination (Join-Path $stageDir "config.toml")

    $readmePath = Join-Path $root "README.md"
    if (-not (Test-Path -LiteralPath $readmePath -PathType Leaf)) {
        throw "README.md not found at $readmePath"
    }
    Copy-Item -LiteralPath $readmePath -Destination (Join-Path $stageDir "README.md")

    $readmeUkPath = Join-Path $root "README.uk.md"
    if (Test-Path -LiteralPath $readmeUkPath -PathType Leaf) {
        Copy-Item -LiteralPath $readmeUkPath -Destination (Join-Path $stageDir "README.uk.md")
    }

    foreach ($licenseName in @("LICENSE-MIT", "LICENSE-APACHE", "NOTICE.md")) {
        $licensePath = Join-Path $root $licenseName
        if (Test-Path -LiteralPath $licensePath -PathType Leaf) {
            Copy-Item -LiteralPath $licensePath -Destination (Join-Path $stageDir $licenseName)
        }
    }

    $autostartDoc = Join-Path $root "docs\AUTOSTART.md"
    if (Test-Path -LiteralPath $autostartDoc -PathType Leaf) {
        Copy-Item -LiteralPath $autostartDoc -Destination (Join-Path $stageDir "AUTOSTART.md")
    }

    $providersDoc = Join-Path $root "docs\PROVIDERS.md"
    if (Test-Path -LiteralPath $providersDoc -PathType Leaf) {
        Copy-Item -LiteralPath $providersDoc -Destination (Join-Path $stageDir "PROVIDERS.md")
    }

    Get-ChildItem -LiteralPath $stageDir -File |
        ForEach-Object { Write-Host ("    {0} ({1} bytes)" -f $_.Name, $_.Length) }

    Write-Host "==> Compressing $stageName-win-x64.zip"
    if (Test-Path -LiteralPath $zipPath) {
        Remove-Item -LiteralPath $zipPath -Force
    }
    Compress-Archive -LiteralPath $stageDir -DestinationPath $zipPath -CompressionLevel Optimal
    if (-not (Test-Path -LiteralPath $zipPath -PathType Leaf)) {
        throw "Archive was not created: $zipPath"
    }

    $zip = Get-Item -LiteralPath $zipPath
    $exeHash = (Get-FileHash -LiteralPath $exePath -Algorithm SHA256).Hash.ToLowerInvariant()
    $zipHash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()

    Write-Host ""
    Write-Host "Package directory: $stageDir"
    Write-Host ("ZIP:               {0} ({1} bytes)" -f $zip.FullName, $zip.Length)
    Write-Host "SHA-256 (zip):     $zipHash"
    Write-Host "SHA-256 (exe):     $exeHash"
}
finally {
    Pop-Location
}
