# Tier 0 — universal native support matrix

Мета: працювати на Windows 10/11 **x64 і ARM64** без kernel-драйверів, без admin, на будь-якому залізі. Vendor DLL є опційними; їх відсутність — штатний стан.

| Підсистема | Основне джерело | Fallback | Відсутність → |
|---|---|---|---|
| CPU load | PDH `\Processor Information(_Total)\% Processor Utility` | `GetSystemTimes` | `0 %`/`—` |
| CPU freq | PDH `Processor Frequency` / `% Processor Performance` × max MHz | `CallNtPowerInformation` (валідація 0/сміття) | `—` |
| CPU temp | ACPI thermal zone via PDH / `Win32_PerfFormattedData_Counters_ThermalZoneInformation` | frozen-value detector, `~` | `—` |
| GPU list | DXGI `IDXGIFactory1::EnumAdapters1` | PDH LUID-only | порожній список |
| GPU load | PDH `\GPU Engine(*)\Utilization Percentage` per LUID | vendor API | `—` |
| GPU temp NVIDIA | NVML `nvml.dll` (dynamic, D3cold-aware) | — | `—` |
| GPU temp AMD | ADLX `amdadlx64.dll` (dynamic probe; temperature currently `—` until a vtable-safe SDK binding exists) | PDH usage only | `—` |
| GPU temp Intel | Level Zero Sysman `ze_loader.dll` (dynamic; basic) | PDH usage only | `—` |
| GPU temp Qualcomm/VM | — | DXGI/PDH usage | `—` |
| Disks | `PhysicalDrive0..N` + `StorageDeviceProperty` | — | пропуск диска |
| Disk temp NVMe | `StorageDeviceTemperatureProperty` (52) | NVMe log page 0x02 | `—` |
| Disk temp SATA/USB/RAID | — | — | `—` (без warn-спаму) |
| Disk activity | PDH `\PhysicalDisk(*)\% Idle Time` per disk | `_Total` | `—` |
| Network | `GetIfTable2`/`GetIfEntry2` (64-bit) | `GetAdaptersAddresses`/`GetBestInterfaceEx` | 0 b/s |
| Network filters | default route + physical preference | keyword filter Hyper-V/WSL/VPN/Bluetooth | перший Up |
| RAM | `GlobalMemoryStatusEx` | — | `0 %` |

## Architecture rules

- **No kernel driver, no admin, no injection, no telemetry.**
- Every sampler returns `Option`/`—` on failure; one failing source must not abort the snapshot.
- Vendor DLLs are loaded with `LoadLibraryExW(..., LOAD_LIBRARY_SEARCH_SYSTEM32)` and are optional.
- Multi-GPU (`Snapshot.gpus`) and multi-disk (`Snapshot.disks`) are first-class; the widget shows the primary.
- Any new parser/aggregator must have mock-data unit tests (no physical hardware required).
- CI gate: `cargo check --workspace --target aarch64-pc-windows-msvc` and `cargo test --workspace` on x64.

## Minimum guaranteed output

Even with no vendor DLLs, no ACPI zone and no NVMe temp, the widget must start and show:

- network up/down;
- CPU %;
- RAM %;
- disk activity;
- `—` for unavailable temperatures/GPU.

This is the acceptance criterion for any unknown device or VM.
