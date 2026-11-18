# Testing strategy — universal Tier 0

## Automated gates

```powershell
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo check --workspace --target aarch64-pc-windows-msvc
```

`scripts/check-all.ps1` runs the same sequence. CI mirrors it (`.github/workflows/ci.yml`).

## Mock-data tests (no hardware required)

The universal layer must be testable without NVIDIA/AMD/Intel/Qualcomm hardware and without
admin rights. Each parser/aggregator is a pure function with fixtures:

| Area | What is mocked | Examples |
|---|---|---|
| GPU PDH | instance names with several LUIDs and `engtype_*` values | 2 adapters, 3D + Copy, malformed names |
| GPU merge | DXGI adapter list + optional vendor data | NVIDIA dGPU + AMD iGPU + Microsoft Basic Render |
| GPU absence | missing `nvml.dll`/`amdadlx64.dll`/`ze_loader.dll` | returns `None`, no panic, no log spam |
| Storage temp | `STORAGE_TEMPERATURE_DATA_DESCRIPTOR` bytes | composite index 0, fallback sensor, truncated buffer |
| NVMe log page | synthetic 512-byte SMART/health log | composite Kelvin 314.15 → 41 °C |
| Storage device | `STORAGE_DEVICE_DESCRIPTOR` bytes | NVMe/SATA/USB bus types, bad string offsets |
| Disk activity | PDH instance names | `0 C:`, `1 D:`, `_Total`, malformed |
| Network | adapter lists | Wi-Fi vs vEthernet/WSL/Tailscale/Bluetooth, explicit disconnected, USB WWAN |
| ACPI | temperature/load history | frozen value + changing load → stale; moving value → good |
| CPU | frequency values | 0, NaN, 12000 MHz, normal 3301 MHz |
| i18n | `.ftl` resources | EN/UK/RU keys, unknown language, missing key |

## Graceful-degradation invariant

Every test that simulates a missing DLL/API must assert:

1. the function returns `None`/empty, not `Err` that aborts the snapshot;
2. no panic;
3. at most one debug/warn log per state change;
4. the rest of the snapshot (network/CPU/RAM/disk activity) remains valid.

## Manual matrix (when hardware is available)

| Axis | Values |
|---|---|
| OS | Win10 22H2, Win11 23H2/24H2/25H2/26H2 |
| Arch | x64, ARM64 (Snapdragon) |
| CPU | Intel, AMD, ARM64, VM |
| GPU | NVIDIA dGPU, AMD dGPU/iGPU, Intel iGPU/Arc, Qualcomm Adreno, Hyper-V/Basic Render |
| Storage | NVMe (1–3), SATA SSD/HDD, USB, RAID/Storage Spaces |
| Network | Ethernet, Wi-Fi, USB WWAN, VPN, sleep/resume |
| Display | 100–200 % DPI, multi-monitor, taskbar bottom/top/left/right |
| Theme | dark/light, HDR, transparency |

Acceptance: on an unknown configuration, the widget still starts and shows network, CPU %, RAM %,
disk activity, and `—` for unavailable values.
