# Phase 0/1 — universal native acceptance checklist

Дата: 18.11.2026  
Ціль: stable Tier 0 on Windows 10/11 x64 + ARM64, no kernel driver, no admin, no vendor DLL required.

## Build / gates

```powershell
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo check --workspace --target aarch64-pc-windows-msvc
```

`scripts/check-all.ps1` runs the same. All gates must pass without admin rights or vendor DLLs.

## Headless sensor check

```powershell
cargo run -p oxide-app -- --headless --once
```

Expected on the reference laptop:

| Field | Expected |
|---|---|
| `network.adapter` | Wi-Fi (not vEthernet/Tailscale/WSL) |
| `cpu.usage_pct` | 0–100, Task Manager parity |
| `cpu.temp_c` | ACPI zone ~47–70 °C, `TempSource::Acpi` |
| `gpu` | primary NVIDIA GTX 1650, `GpuVendor::Nvidia`, NVML temp ~40–50 °C |
| `gpus` | AMD Radeon iGPU + NVIDIA dGPU + Microsoft Basic Render, no duplicates |
| `disk` | Apacer NVMe, `is_nvme=true`, `TempSource::Nvme` ~41 °C |
| `disks` | HGST SATA (`temp_c=None`) + Apacer NVMe |
| `seq` | increments once per poll |

On unknown hardware/VM, at minimum: network, CPU %, RAM %, disk activity; temperatures/GPU `—`.

## GUI acceptance

```powershell
cargo run -p oxide-app
```

- [ ] 294×35 overlay left of `TrayNotifyWnd`, two rows, 8 items.
- [ ] Column order: Up/CPU/GPU/GPU temp; Down/MEM/CPU temp/HDD temp.
- [ ] `CPU ... ~` marks ACPI best-effort; `—` for unavailable values.
- [ ] Tray menu is localized (Ukrainian by default; English/Russian via `config.toml`).
- [ ] Hide shows a one-time localized balloon; left-click tray shows; `--show` on a running instance reveals.
- [ ] Explorer restart, fullscreen, DPI/theme changes do not crash the widget.
- [ ] No `SetParent`, no injection, process is `asInvoker`.

## Performance

```powershell
$p = Get-Process oxide-monitor
$c1 = $p.CPU; Start-Sleep 20; $p.Refresh(); $c2 = $p.CPU
"CPU: {0:N2}% одного ядра" -f (($c2-$c1)/20*100)
"WS: {0:N1} MB" -f ($p.WorkingSet64/1MB)
```

Target: **< 0.3–0.5 % одного ядра**, **< 45 MB WS / < 30 MB private**, 1 Hz.

## Localized UI

- `uk` is default; `en`, `ru` available.
- Fluent resources: `crates/oxide-core/locales/*.ftl`.
- Do not hard-code menu/balloon strings; use `Localizer::text`.
- `config.general.language = "uk" | "en" | "ru"`.

## Known gaps / next phases

- AMD GPU temperature is probe-only (ADLX vtable mismatch risk); AMD adapters get load/VRAM via DXGI+PDH, temp `—`.
- SATA/USB/RAID temperature `—` without admin; NVMe native.
- CPU core temp (Tctl/Tdie) only via optional external provider/helper; ACPI is a platform zone.
- HWiNFO/LHM provider toggles and Game Mode are the final phase, disabled by default.
- Secondary taskbars, auto-hide animation, graphs/history/skins are later.
