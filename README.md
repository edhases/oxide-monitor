# Oxide Monitor

[Українська версія](README.uk.md)

Lightweight taskbar system monitor for Windows 10/11, written in Rust.

**Driver-free and unelevated by default.** No kernel driver, no administrator rights, no
injection into Explorer. Optional temperature sources are opt-in: external HWiNFO/LHM
providers or an explicitly enabled PawnIO CPU-temperature helper.

## Features

- **Taskbar widget** with a classic 294×35 two-row layout and dynamic sizing:
  content-based, fixed DIP, or exact pixels; scale 50–300 %; Compact/Normal/Large/TV
  presets; “fit to taskbar” so short taskbars and TVs never clip.
- Network up/down, CPU %, RAM %, GPU %, CPU/GPU/NVMe temperatures, disk activity
  (SATA/mainboard temperatures via optional providers); multi-GPU and multi-disk
  discovery; per-core and history views in the Flyout.
- **Flyout** with CPU/per-core, memory, every GPU, every disk, network, source badges and
  localized actions; tray icon with localized menu; `--show` reveals a running instance.
- **Settings GUI** (`oxide-settings.exe`) for every Config v2 section with pixel-accurate
  Live Preview, Apply / Cancel / Reset, IPC and 2 s config hot reload. The TOML file
  remains an advanced/fallback path.
- **Autostart** through a per-user Windows Task Scheduler task (`Oxide Monitor`, at logon,
  15 s delay, limited privileges — no admin/UAC). Live status is shown in Settings.
- **External temperature providers**: HWiNFO shared memory and loopback
  LibreHardwareMonitor HTTP. Both are off by default and can be enabled live.
- **Official version checks** for LibreHardwareMonitor and PawnIO. LHM can stage a
  verified portable update under `%LOCALAPPDATA%\OxideMonitor\providers\LHM`; it is never
  executed automatically and an external LHM installation is never modified.
- **Optional PawnIO CPU-temperature helper**: opt-in, elevated only after an explicit
  Enable action and UAC. CPU Tctl/Tdie only; SuperIO/SATA-via-PawnIO are out of scope.
- Fluent localization: **Ukrainian (default), English, Russian**.
- x64 release build and `cargo check --target aarch64-pc-windows-msvc` are part of the
  gate (no x86 assembly).

## Requirements

- Windows 10 or 11 (x64; ARM64 is compile-checked, physical ARM64 hardware is not yet
  tested).
- Rust stable toolchain and the **Desktop development with C++** workload (MSVC linker,
  Windows SDK).
- No administrator rights for normal use. PawnIO, if enabled, requires a one-time
  administrator/UAC approval for its helper; PawnIO itself is installed manually.

## Install (Windows)

Download the latest `OxideMonitor-<version>-Setup.exe` from
[GitHub Releases](https://github.com/edhases/oxide-monitor/releases). The installer is
**per-user and does not require administrator rights**; it installs to
`%LOCALAPPDATA%\OxideMonitor`, creates Start Menu shortcuts and an uninstaller. A portable
ZIP (`oxide-monitor-<version>-win-x64.zip`) is available from the same release.

The binaries and installer are **unsigned**. Windows SmartScreen may warn; verify the
SHA-256 against the release's `SHA256SUMS` asset before running. PawnIO and
LibreHardwareMonitor are not bundled.

## Build and run

```powershell
cargo build -p oxide-app -p oxide-settings
cargo run -p oxide-app
```

Release (binaries are placed side by side in `target\release\`):

```powershell
cargo build --release --bin oxide-monitor --bin oxide-settings --bin oxide-pawnio-host
target\release\oxide-monitor.exe
```

`oxide-pawnio-host.exe` is optional at runtime, but must stay beside
`oxide-monitor.exe` if the PawnIO CPU-temperature helper is enabled.

Useful modes:

```powershell
oxide-monitor.exe --headless --once   # print one sensor snapshot, no UI
oxide-monitor.exe --headless          # three snapshots, one per second
oxide-monitor.exe --show              # reveal the widget of an already running instance
oxide-settings.exe                    # settings GUI (IPC or standalone)
```

Run the gates:

```powershell
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace --target aarch64-pc-windows-msvc
```

`scripts/check-all.ps1` runs the full local gate (including dependency isolation);
`scripts/package.ps1` builds and copies the release files.

## Configuration

- **Primary path:** Settings GUI (`oxide-settings.exe`, opened from the tray menu or the
  Flyout card). It covers all Config v2 sections with Live Preview and
  Apply / Cancel / Reset.
- **Advanced/fallback:** `%APPDATA%\OxideMonitor\config.toml` (portable mode:
  `config.toml` next to the executable). It is hot-reloaded every 2 s; saves use
  `ReplaceFileW` with a `.bak` copy.
- `resources/config.toml` is the sample.
- `config.general.language = "uk" | "en" | "ru"`.
- `config.general.cpu_temp_source = "auto" | "acpi" | "provider" | "off"`.
- ACPI CPU temperature is a platform zone, not Tctl/Tdie; the UI appends `~`.

### Autostart

`general.autostart = true` registers the per-user Task Scheduler task `Oxide Monitor`
(at logon, 15 s delay, limited privileges, no admin/UAC) that runs
`oxide-monitor.exe --autostart`. It is reconciled on startup, on Settings → Apply and on
config hot reload; the legacy `HKCU\...\Run` value is removed automatically. The result is
published to `%APPDATA%\OxideMonitor\autostart.toml` and shown in Settings.

Remove the task with:

```powershell
Unregister-ScheduledTask -TaskName 'Oxide Monitor'
```

See [`docs/AUTOSTART.md`](docs/AUTOSTART.md).

### External temperature providers

CPU/mainboard/SATA temperatures can come from **HWiNFO shared memory** or a
**loopback LibreHardwareMonitor** web server. Both are off by default, need no
administrator rights or driver from Oxide, are toggled live, and report health
(`connected` / `not_running` / `stale` / `error`) in Settings.

- HWiNFO: enable **Settings → Safety → Shared Memory Support**, start HWiNFO, then enable
  the HWiNFO source in Settings.
- LHM: start LibreHardwareMonitor, enable **Options → Remote Web Server → Run** (default
  `http://127.0.0.1:8085/metrics`), then enable the LHM source in Settings. Only
  loopback URLs are accepted.
- LHM auto-update is optional (`providers.lhm_auto_update`, default off). It downloads
  only official GitHub releases, verifies the published SHA-256, stages a portable copy,
  and never executes it or touches an external LHM installation. The official LHM
  executable is not Authenticode-signed; the staged artifact is verified by hash and is
  not run automatically.

See [`docs/PROVIDERS.md`](docs/PROVIDERS.md) and
[`crates/oxide-pawnio-host/README.md`](crates/oxide-pawnio-host/README.md).

### PawnIO CPU temperature (opt-in)

PawnIO is a signed kernel driver, installed and updated manually from
[`pawnio.eu`](https://pawnio.eu). Oxide Monitor never installs or updates it.

Phase 1 supports **CPU Tctl/Tdie only** on AMD family 17h–1Ah. When `providers.pawnio` is
enabled from Settings, the monitor starts `oxide-pawnio-host.exe` through UAC; the helper
loads one hash-pinned official module, reads the SMN temperature, and stops on Stop, Game
Mode, app exit or pipe loss. The monitor and settings remain unelevated. The helper is
unsigned, so UAC shows “Unknown publisher”; kernel drivers can be flagged by some
anti-cheat systems. Motherboard/SuperIO, SATA-via-PawnIO and automatic driver updates are
not included.

## Architecture and research

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — decisions (ADR-01..ADR-14), provider comparison,
  module schema, roadmap.
- [`docs/SETTINGS.md`](docs/SETTINGS.md) — isolated settings GUI (DX12-only, Live Preview).
- [`docs/IPC.md`](docs/IPC.md) — named-pipe IPC, single-writer config, conflict detection.
- [`docs/AUTOSTART.md`](docs/AUTOSTART.md) — Task Scheduler autostart.
- [`docs/PROVIDERS.md`](docs/PROVIDERS.md) — HWiNFO/LHM/PawnIO provider details.
- [`docs/UI_CONTRACT.md`](docs/UI_CONTRACT.md) — UI crate public contract.
- [`research/`](research/) — driver/anti-cheat audit, native sensors, shared memory,
  taskbar embedding and local TrafficMonitor audit.
- [`PHASE0.md`](PHASE0.md) — acceptance checklist.
- [`docs/BUILD.md`](docs/BUILD.md), [`docs/ARM64.md`](docs/ARM64.md).

## Safety

The default build loads no kernel driver, requires no elevation, opens no network listener
and sends no telemetry. External providers are read-only and disabled by default; LHM URLs
are restricted to loopback. PawnIO is the only optional kernel-driver path and is disabled
by default, visibly opt-in, and clearly marked as an anti-cheat risk.

## License

Application code is available under **MIT OR Apache-2.0**. The embedded PawnIO module is
**LGPL-2.1-or-later** and is distributed with its notice and source link; the PawnIO
driver/installer is not redistributed. See `LICENSE-MIT`, `LICENSE-APACHE` and `NOTICE.md`,
plus `crates/oxide-pawnio-host/assets/`.
