#requires -Version 5.1
<#
.SYNOPSIS
    Runs the local Oxide Monitor verification suite: formatting, x64 build
    checks, unit tests, and the ARM64 compile check.

.DESCRIPTION
    Mirrors the GitHub Actions gates in .github/workflows/ci.yml:

      cargo fmt --all -- --check
      cargo check --workspace --all-targets
      cargo test --workspace
      cargo check --workspace --target aarch64-pc-windows-msvc

    The ARM64 step is check-only. Linking an ARM64 executable needs the MSVC
    ARM64 build tools and ARM64 Windows SDK libraries; see docs/ARM64.md.

    No administrator rights, vendor DLLs (NVML/ADLX), or physical GPU are
    required. Unit tests are pure data/format tests and do not touch hardware.

    With -Clippy, also runs:
      cargo clippy --workspace --all-targets -- -D warnings
    That switch is off by default because the core crates currently have
    pre-existing warnings; the CI clippy step is advisory for the same reason.

.PARAMETER Clippy
    Also run clippy with warnings denied. Currently expected to fail until the
    pre-existing warnings in oxide-core/oxide-ui are fixed.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\check-all.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\check-all.ps1 -Clippy

.NOTES
    Exits 0 only when every selected step passed.
#>
[CmdletBinding()]
param(
    [switch]$Clippy
)

$ErrorActionPreference = "Stop"
$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$arm64Target = "aarch64-pc-windows-msvc"

$script:results = @()
$script:failures = 0

function Invoke-CargoStep {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name,

        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    Write-Host ""
    Write-Host "==> $Name" -ForegroundColor Cyan
    Write-Host ("    cargo {0}" -f ($Arguments -join " ")) -ForegroundColor DarkGray

    & cargo @Arguments
    $exitCode = $LASTEXITCODE

    if ($exitCode -eq 0) {
        Write-Host "    PASS" -ForegroundColor Green
        $status = "PASS"
    }
    else {
        Write-Host ("    FAIL (exit {0})" -f $exitCode) -ForegroundColor Red
        $status = "FAIL"
        $script:failures++
    }

    $script:results += [pscustomobject]@{
        Step     = $Name
        Status   = $status
        ExitCode = $exitCode
    }
}

Push-Location $root
try {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "cargo was not found on PATH. Install Rust stable (MSVC): https://rustup.rs"
    }

    if (-not (Get-Command rustc -ErrorAction SilentlyContinue)) {
        throw "rustc was not found on PATH. Install Rust stable (MSVC): https://rustup.rs"
    }

    $rustVersion = (& rustc --version) -join " "
    Write-Host "Oxide Monitor verification"
    Write-Host "  Workspace: $root"
    Write-Host "  $rustVersion"

    if (Get-Command rustup -ErrorAction SilentlyContinue) {
        $installedTargets = & rustup target list --installed
        if ($LASTEXITCODE -eq 0 -and $installedTargets -notcontains $arm64Target) {
            Write-Host ""
            Write-Host ("  warning: target {0} is not installed." -f $arm64Target) -ForegroundColor Yellow
            Write-Host ("           Run: rustup target add {0}" -f $arm64Target) -ForegroundColor Yellow
        }
    }

    Invoke-CargoStep -Name "Formatting (cargo fmt --check)" -Arguments @(
        "fmt", "--all", "--", "--check"
    )

    Invoke-CargoStep -Name "Check (x64)" -Arguments @(
        "check", "--workspace", "--all-targets"
    )

    Invoke-CargoStep -Name "Test (x64)" -Arguments @(
        "test", "--workspace"
    )

    Invoke-CargoStep -Name "Check (ARM64, check only)" -Arguments @(
        "check", "--workspace", "--target", $arm64Target
    )

    Write-Host ""
    Write-Host "==> Dependency isolation audit (oxide-app clean, oxide-settings DX12-only)" -ForegroundColor Cyan
    $appTree = & cargo tree -p oxide-app
    $badApp = $appTree | Select-String -Pattern "\b(egui|eframe|wgpu|glow|ash|vulkan|oxide-settings)\b"
    # `oxide-autostart` is intentionally linked into the monitor (Task Scheduler COM,
    # no GUI/GPU dependencies). The settings app must never link it or a backend crate.
    $setTree = & cargo tree -p oxide-settings
    $badSet = $setTree | Select-String -Pattern "\b(glow|egui_glow|glutin|ash|vulkan|khronos|gles|oxide-ui|oxide-sensors|oxide-gpu|oxide-providers|oxide-autostart)\b"
    if ($badApp -or $badSet) {
        Write-Host "    FAIL (dependency boundary violated)" -ForegroundColor Red
        if ($badApp) { Write-Host "    oxide-app: $badApp" -ForegroundColor Red }
        if ($badSet) { Write-Host "    oxide-settings: $badSet" -ForegroundColor Red }
        $script:failures++
        $script:results += [pscustomobject]@{ Step = "Dependency isolation audit"; Status = "FAIL"; ExitCode = 1 }
    }
    else {
        Write-Host "    PASS" -ForegroundColor Green
        $script:results += [pscustomobject]@{ Step = "Dependency isolation audit"; Status = "PASS"; ExitCode = 0 }
    }

    if ($Clippy) {
        Invoke-CargoStep -Name "Clippy (warnings denied)" -Arguments @(
            "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"
        )
    }

    Write-Host ""
    Write-Host "==================== Summary ====================" -ForegroundColor Cyan
    foreach ($result in $script:results) {
        $color = if ($result.ExitCode -eq 0) { "Green" } else { "Red" }
        Write-Host ("  [{0}] {1} (exit {2})" -f $result.Status, $result.Step, $result.ExitCode) -ForegroundColor $color
    }

    $passed = $script:results.Count - $script:failures
    Write-Host ("  {0} passed, {1} failed" -f $passed, $script:failures)
    Write-Host "=================================================" -ForegroundColor Cyan

    if ($script:failures -gt 0) {
        Write-Host ""
        Write-Host "Verification FAILED." -ForegroundColor Red
        exit 1
    }

    Write-Host ""
    Write-Host "Verification passed." -ForegroundColor Green
    exit 0
}
catch {
    Write-Host ""
    Write-Host ("Verification could not run: {0}" -f $_.Exception.Message) -ForegroundColor Red
    exit 2
}
finally {
    Pop-Location
}
