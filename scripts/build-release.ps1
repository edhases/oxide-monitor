# Builds the complete Windows release: binaries, portable ZIP, installer and SHA256SUMS.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\build-release.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\build-release.ps1 -Version 0.1.0
#   powershell -ExecutionPolicy Bypass -File scripts\build-release.ps1 -SkipBuild
#
# Output: dist\OxideMonitor-<version>-Setup.exe
#         dist\oxide-monitor-<version>-win-x64.zip
#         dist\SHA256SUMS

[CmdletBinding()]
param(
    [string]$Version = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))

if ([string]::IsNullOrWhiteSpace($Version)) {
    $cargoToml = Join-Path $root "Cargo.toml"
    $text = Get-Content -Raw -LiteralPath $cargoToml
    $section = [regex]::Match($text, '(?ms)^\s*\[workspace\.package\]\s*(.*?)(?=^\s*\[|\z)')
    if (-not $section.Success) { throw "Could not find [workspace.package] in $cargoToml" }
    $match = [regex]::Match($section.Groups[1].Value, '(?m)^\s*version\s*=\s*"([^"]+)"')
    if (-not $match.Success) { throw "Could not find [workspace.package].version in $cargoToml" }
    $Version = $match.Groups[1].Value
}

if (-not $SkipBuild) {
    Write-Host "==> cargo build --release (monitor, settings, PawnIO helper)"
    Push-Location $root
    try {
        & cargo build --release --bin oxide-monitor --bin oxide-settings --bin oxide-pawnio-host
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    }
    finally {
        Pop-Location
    }
}

Write-Host "==> Portable package"
& (Join-Path $root "scripts\package.ps1") -Version $Version -SkipBuild
if ($LASTEXITCODE -ne 0) { throw "Portable package failed" }

Write-Host "==> Per-user installer"
& (Join-Path $root "scripts\build-installer.ps1") -Version $Version
if ($LASTEXITCODE -ne 0) { throw "Installer build failed" }

$dist = Join-Path $root "dist"
$setup = Join-Path $dist "OxideMonitor-$Version-Setup.exe"
$zip = Join-Path $dist "oxide-monitor-$Version-win-x64.zip"
foreach ($asset in @($setup, $zip)) {
    if (-not (Test-Path -LiteralPath $asset -PathType Leaf)) {
        throw "Release asset missing: $asset"
    }
}

$sums = Join-Path $dist "SHA256SUMS"
$lines = @()
foreach ($asset in @($setup, $zip)) {
    $hash = (Get-FileHash -LiteralPath $asset -Algorithm SHA256).Hash.ToLowerInvariant()
    $lines += "$hash  $([System.IO.Path]::GetFileName($asset))"
}
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText($sums, (($lines -join "`n") + "`n"), $utf8NoBom)

Write-Host ""
Write-Host "Release assets:"
Get-ChildItem -LiteralPath $dist -File |
    Where-Object { $_.Name -in @((Split-Path $setup -Leaf), (Split-Path $zip -Leaf), "SHA256SUMS") } |
    ForEach-Object { Write-Host ("  {0}  {1:N0} bytes" -f $_.Name, $_.Length) }
Write-Host "SHA256SUMS: $sums"
