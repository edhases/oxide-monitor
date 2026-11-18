# ARM64 (Windows on ARM)

Oxide Monitor targets Windows 10/11 on x64 and is **compile-checked** for
`aarch64-pc-windows-msvc` on every CI run. ARM64 is not a shipped package yet:
the release ZIP from `scripts\package.ps1` is `win-x64` only.

The CI gate for ARM64 is `cargo check`, not `cargo build` or `cargo test`.
The full gate set is:

| Gate | Command | Needs |
|---|---|---|
| Formatting | `cargo fmt --all -- --check` | rustfmt |
| x64 check | `cargo check --workspace --all-targets` | MSVC x64 toolchain |
| x64 tests | `cargo test --workspace` | MSVC x64 toolchain |
| ARM64 check | `cargo check --workspace --target aarch64-pc-windows-msvc` | Rust std for ARM64 |
| ARM64 link (manual) | `cargo build --release --target aarch64-pc-windows-msvc` | MSVC ARM64 build tools + ARM64 SDK libs |

The workflow is `.github/workflows/ci.yml` (job **Check (ARM64)**), and
`scripts\check-all.ps1` runs the same checks locally.

## 1. Install the target

A normal x64 Rust installation can cross-check ARM64 without any Visual Studio
ARM64 components:

```powershell
rustup target add aarch64-pc-windows-msvc
rustup target list --installed
```

`cargo check` produces metadata only; it never invokes `link.exe`, so the ARM64
linker and the ARM64 Windows SDK libraries are **not** required for the CI gate.

Run the check from the repository root:

```powershell
cargo check --workspace --target aarch64-pc-windows-msvc
```

Or run everything (formatting, x64 check, x64 tests, ARM64 check):

```powershell
powershell -ExecutionPolicy Bypass -File scripts\check-all.ps1
```

Neither command needs administrator rights, a physical GPU, or vendor DLLs.
Build scripts still run during `cargo check`: `crates\oxide-app\build.rs`
invokes `winres`, which shells out to the **host** `rc.exe` from the Windows
SDK. That resource library is architecture-neutral, so the x64 icon/manifest
embedding is exercised by the ARM64 check as well. This was verified on
2026-11-18 with Rust 1.99.1 (an ARM64-target `cargo check -p oxide-app`
reran the build script, produced `resource.lib`, and succeeded).

## 2. Build a native ARM64 binary (optional, not CI)

Linking an ARM64 executable requires the ARM64 MSVC toolchain and ARM64 Windows
SDK libraries. In the Visual Studio Installer, modify **Visual Studio Build
Tools 2022** and add:

- **Desktop development with C++**
- **MSVC v143 - VS 2022 C++ ARM64/ARM64EC build tools** (latest)
- **Windows 11 SDK** (10.0.22621 or newer; ARM64 libraries are included)

Then build from a normal terminal (Cargo locates MSVC through `vswhere`):

```powershell
cargo build --release --target aarch64-pc-windows-msvc -p oxide-app
```

Output:

```text
target\aarch64-pc-windows-msvc\release\oxide-monitor.exe
```

If `link.exe` is missing or the linker reports a machine-type mismatch, the
ARM64 MSVC component is not installed; a plain x64 Build Tools installation is
not enough.

### Known packaging gaps

The ARM64 build is not distributable yet:

- `resources\app.manifest` declares
  `processorArchitecture="amd64"` in the application identity. An ARM64 or
  architecture-neutral manifest is needed before shipping an ARM64 executable.
- `scripts\package.ps1` packages `target\release\oxide-monitor.exe` (x64) into
  `oxide-monitor-<version>-win-x64.zip` and has no ARM64 option.
- No ARM64 code signing or installer exists.

## 3. Runtime fallbacks on ARM64

The application is designed so that every optional backend fails soft: a
missing driver, DLL, counter, or sensor leaves the affected item as `—` and
never fails the process or requires elevation.

| Data | Mechanism | ARM64 behavior |
|---|---|---|
| CPU load | `CallNtPowerInformation(ProcessorInformation)` | Works (standard Win32). |
| Memory | `GlobalMemoryStatusEx` | Works. |
| Network throughput | IP Helper `GetIfTable2`/`GetIfEntry2`, default-route selection | Works; same adapters as x64, including USB/Wi-Fi. |
| GPU discovery | DXGI adapter enumeration | Works; Qualcomm Adreno (and any emulated/virtual adapter) appears as a normal DXGI adapter. |
| GPU utilization | PDH `\GPU Engine(*)\Utilization Percentage` (English counters) | Works when the GPU driver publishes GPU Engine counters; otherwise the item stays `—`. |
| NVIDIA GPU name/temp | NVML, loaded at runtime from `System32\nvml.dll` with `LoadLibraryExW(LOAD_LIBRARY_SEARCH_SYSTEM32)` and resolved by export name | No `nvml.dll` on a typical ARM64 device; initialization fails once, is logged at debug level, and the NVML backend stays disabled. No NVIDIA GPU ⇒ no NVML data. |
| AMD GPU temp | ADLX probe only (`amdadlx64.dll`) | The backend intentionally stops at a readable DLL/export probe and never calls the versioned C++ vtables, so AMD temperature stays `—` on every architecture. ARM64 has no ADLX DLL, so the probe simply fails; AMD load/VRAM still come from DXGI and PDH. |
| Intel GPU temp/util | oneAPI Level Zero Sysman via `ze_loader.dll`, loaded at runtime with plain C exports | Probed only when DXGI reports an Intel adapter. No Intel GPU or loader on ARM64, so the backend stays disabled; Intel adapters still get DXGI enumeration and PDH utilization when present. |
| CPU temperature | PDH `\Thermal Zone Information(*)\High Precision Temperature` (ACPI `THRM` preferred), only with `cpu_temp_source = "auto"`/`"acpi"` | Works if the platform exposes thermal zone counters; otherwise `—`. This is a platform thermal zone, not a per-core DTS. |
| Disk activity | PDH `\PhysicalDisk(_Total)\% Idle Time` | Works. |
| Disk/NVMe temperature | `IOCTL_STORAGE_QUERY_PROPERTY` + `StorageDeviceTemperatureProperty` | Non-admin user-mode query; depends on the storage driver. Unsupported devices return an error that is ignored, leaving `—`. |
| External sensors | Optional HWiNFO shared memory (`Global\HWiNFO_SENS_SM2`) or LibreHardwareMonitor loopback HTTP | Both are off by default and only read; a missing provider simply yields no readings. Loopback HTTP is restricted to localhost, so `ureq` is built with `default-features = false` and no TLS stack. |
| UI | Win32 layered taskbar overlay + GDI/DWM, tray icon | Arch-neutral; no x86 intrinsics or assembly anywhere in the workspace. |

No component installs a kernel driver or a Windows service, and nothing
requests elevation: the manifest is `asInvoker`, and the app remains fully
functional (with fewer data sources) on a locked-down ARM64 device. The only
persistent change the app can make is an opt-in **per-user scheduled task**
(`Oxide Monitor`, logon trigger, 15 s delay, limited privileges) created while
`general.autostart = true`; disabling it removes the task again and the legacy
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value is cleaned up
unconditionally. Neither path needs elevation. See
[`AUTOSTART.md`](AUTOSTART.md).

## 4. Dependencies verified for ARM64

`cargo check --workspace --target aarch64-pc-windows-msvc` compiles every
dependency for ARM64. The relevant ones:

| Dependency | Version | ARM64 notes |
|---|---|---|
| `windows` | 0.62 | Win32 bindings; no arch-specific code in the used features. `Win32_System_LibraryLoader` provides the vendor-DLL loader. |
| `ureq` | 3.5, `default-features = false` | Plain HTTP only, used for loopback LHM polling; avoids native TLS/crypto dependencies that complicate cross-arch builds. |
| `fluent-bundle` + `unic-langid` | 0.16 / 0.9 | Pure Rust localization; architecture-independent. |
| Vendor GPU libraries | none linked | NVML/ADLX/Level Zero are resolved at runtime from `System32` only; there are no import libraries or SDK crates to compile, and absent DLLs just disable that backend. |

No x86-only assembly, intrinsics, or build-time code generation is used, so
there is no architecture-specific code to port.

## 5. Testing

Unit tests run on x64 in CI (`cargo test --workspace`). The ARM64 target cannot
execute on an x64 CI runner without emulation, so ARM64 remains a compile gate.
The tests are pure data/format tests (config round-trips, PDH parsing, HWiNFO
snapshot parsing, layout math), so they do not exercise architecture-specific
behavior.

On a real Windows on ARM device, run:

```powershell
cargo test --workspace
cargo build --release -p oxide-app
```

## 6. Troubleshooting

| Symptom | Fix |
|---|---|
| `error[E0463]: can't find crate for 'std'` / `can't find crate for 'core'` when using `--target aarch64-pc-windows-msvc` | `rustup target add aarch64-pc-windows-msvc`. |
| `link.exe not found` or `LNK1112: module machine type 'ARM64' conflicts with target machine type 'x86'` during an ARM64 **build** | Install **MSVC v143 ARM64/ARM64EC build tools** and the Windows 11 SDK; a check-only build does not need them. |
| `rc.exe not found` while checking `oxide-app` | Install the Windows SDK (any recent 10.x). `winres` locates it through the registry. |
| `rc.exe not found` when building **natively on an ARM64 host** | Cross-compiling from x64 is the verified path. `winres` 0.1 chooses `rc.exe` from the build-script host architecture (non-x64 hosts fall back to the SDK `x86` tools); install those SDK tools or point `winres` at a valid toolkit with `set_toolkit_path`. |
| GPU utilization is `—` on ARM64 | The GPU driver does not publish `GPU Engine` performance counters; DXGI still reports the adapter name. |
| GPU/CPU temperature is `—` | Expected on most ARM64 devices: no NVML runtime, no AMD/Intel vendor temperature source, and the ACPI thermal counter may be absent. This is a data-source limitation, not a crash. |
| Tests fail only when a provider is enabled | Providers are opt-in and need the external tool to be running; CI never enables them. See `docs\BUILD.md` for configuration. |
| `cargo fmt --check` fails | Run `cargo fmt --all` once the working tree is stable; do not mix formatting with functional changes. |
