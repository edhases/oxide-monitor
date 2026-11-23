# Builds the per-user Oxide Monitor installer with Inno Setup 6.
#
# The compiler is downloaded from the official Inno Setup GitHub release and
# unpacked in portable mode (no admin/UAC, no registry uninstall entry).
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1 -Version 0.1.0
#   powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1 -SkipDownload
#
# Output: dist\OxideMonitor-<version>-Setup.exe

[CmdletBinding()]
param(
    [string]$Version = "",
    [switch]$SkipDownload
)

$ErrorActionPreference = "Stop"
$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))

function Get-WorkspaceVersion {
    param([string]$Root)
    $cargoToml = Join-Path $Root "Cargo.toml"
    $text = Get-Content -Raw -LiteralPath $cargoToml
    $section = [regex]::Match($text, '(?ms)^\s*\[workspace\.package\]\s*(.*?)(?=^\s*\[|\z)')
    if (-not $section.Success) { throw "Could not find [workspace.package] in $cargoToml" }
    $match = [regex]::Match($section.Groups[1].Value, '(?m)^\s*version\s*=\s*"([^"]+)"')
    if (-not $match.Success) { throw "Could not find [workspace.package].version in $cargoToml" }
    return $match.Groups[1].Value
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $Version = Get-WorkspaceVersion -Root $root
}
if ($Version -notmatch '^\d+\.\d+\.\d+([-.+][0-9A-Za-z.-]+)?$') {
    throw "Invalid version '$Version'. Expected a Cargo semver such as 0.1.0."
}

$innoVersion = "6.7.3"
$innoHash = "9C73C3BAE7ED48D44112A0F48E66742C00090BDB5BEF71D9D3C056C66E97B732"
$toolRoot = Join-Path $env:TEMP "oxide-inno-$innoVersion"
$iscc = Join-Path $toolRoot "ISCC.exe"

if (-not (Test-Path -LiteralPath $iscc -PathType Leaf)) {
    if ($SkipDownload) {
        throw "ISCC.exe not found at $iscc and -SkipDownload was specified."
    }
    New-Item -ItemType Directory -Path $toolRoot -Force | Out-Null
    $setup = Join-Path $toolRoot "innosetup-$innoVersion.exe"
    $url = "https://github.com/jrsoftware/issrc/releases/download/is-6_7_3/innosetup-$innoVersion.exe"
    Write-Host "==> Downloading Inno Setup $innoVersion (official GitHub release)"
    Invoke-WebRequest -Uri $url -OutFile $setup -UseBasicParsing
    $actual = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash
    if ($actual -ne $innoHash) {
        throw "Inno Setup SHA-256 mismatch: got $actual, expected $innoHash"
    }
    Write-Host "==> Unpacking Inno Setup portable (no admin/UAC)"
    $p = Start-Process -FilePath $setup -Wait -PassThru -ArgumentList @(
        "/VERYSILENT",
        "/SUPPRESSMSGBOXES",
        "/CURRENTUSER",
        "/PORTABLE=1",
        "/NOICONS",
        "/DIR=`"$toolRoot`""
    )
    if ($p.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $iscc -PathType Leaf)) {
        throw "Inno Setup unpack failed (exit $($p.ExitCode)); expected $iscc"
    }
    Remove-Item -LiteralPath $setup -Force -ErrorAction SilentlyContinue
}

$script = Join-Path $root "installer\oxide-monitor.iss"
$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Path $dist -Force | Out-Null

Write-Host "==> Compiling $script"
& $iscc "/DMyAppVersion=$Version" "/O$dist" $script
if ($LASTEXITCODE -ne 0) {
    throw "ISCC failed with exit code $LASTEXITCODE"
}

$setupPath = Join-Path $dist "OxideMonitor-$Version-Setup.exe"
if (-not (Test-Path -LiteralPath $setupPath -PathType Leaf)) {
    throw "Installer was not produced: $setupPath"
}
$hash = (Get-FileHash -LiteralPath $setupPath -Algorithm SHA256).Hash
$file = Get-Item -LiteralPath $setupPath
Write-Host ""
Write-Host "Installer: $($file.FullName)"
Write-Host ("Size:      {0:N0} bytes" -f $file.Length)
Write-Host "SHA-256:   $hash"
