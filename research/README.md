# Дослідження Oxide Monitor

**Дата:** 17.11.2026  
**Мета:** спроєктувати безпечний, легкий Rust-аналог TrafficMonitor для taskbar Windows 10/11 без обов'язкового kernel-драйвера.

## Документи

| # | Файл | Що всередині | Статус |
|---|---|---|---|
| 01 | [`01_drivers_and_anticheat.md`](01_drivers_and_anticheat.md) | WinRing0, PawnIO, InpOutx64, вендорські драйвери; CVE, blocklist, EAC/BattlEye/Vanguard/FACEIT/Defender; матриця ризиків | ✅ |
| 02 | [`02_native_windows_sensors.md`](02_native_windows_sensors.md) | PDH, WMI, IP Helper, NVML, ADLX, NVMe/ATA SMART; Rust-крейти; чому Windows не віддає CPU temp | ✅ |
| 03 | [`03_shared_memory_providers.md`](03_shared_memory_providers.md) | HWiNFO SHM, AIDA64 SHM, LHM HTTP/WMI, MAHM, Core Temp; формати, ліцензії, overhead, `SensorProvider` | ✅ |
| 04 | [`04_taskbar_embedding_rust.md`](04_taskbar_embedding_rust.md) | DeskBand, `SetParent`, layered overlay, D2D/GDI, DPI, fullscreen, multi-monitor, prior art | ✅ |
| 05 | [`05_current_setup_audit.md`](05_current_setup_audit.md) | Аудит `E:\TrafficMonitor`: бінарники, LHM/PawnIO, конфіг, taskbar-вікно, безпека, продуктивність | ✅ |
| — | [`../ARCHITECTURE.md`](../ARCHITECTURE.md) | Фінальна архітектура, порівняльна таблиця, стек, модулі, план MVP, відкриті питання | ✅ draft |

## Головні висновки

1. **Поточна збірка TrafficMonitor — elevated MFC-застосунок**, який тримає LHM 0.9.6 + PawnIO в одному процесі. Це дає температури, але створює EoP-ризик (writable app dir + непідписані DLL), високу ціну (7.2 % одного ядра, 40/105 MB) і крихкість (один .NET crash за 5 місяців).
2. **Більшість метрик не потребує драйвера:** мережа (`GetIfEntry2`), CPU %/freq (PDH), RAM (`GlobalMemoryStatusEx`), GPU % (PDH `GPU Engine`), GPU temp NVIDIA/AMD (NVML/ADLX), disk activity (PDH `PhysicalDisk`), **NVMe temp (`StorageDeviceTemperatureProperty`, 41 °C без admin)**. Локально підтверджено. **У поточному профілі 7 із 8 елементів доступні без драйвера; єдина відсутня метрика — CPU temp** («HDD» у віджеті — це Apacer NVMe).
3. **WinRing0/InpOut — категорично ні:** CVE-2020-14979, Microsoft blocklist, Defender `VulnerableDriver/HackTool`, EAC/BattlEye/Vanguard/FACEIT.
4. **PawnIO 2.1.0+ — найкращий відкритий kernel-варіант, але не безкоштовний за ризиком:** WHQL/WHCP-підпис, не в blocklist, HVCI-сумісний, FACEIT-сумісний з 2.1.0; але device admin-only, bus factor 1, anti-cheat може змінити політику. Лише opt-in helper.
5. **Найбезпечніший шлях до температур — читати вже запущений провайдер:** HWiNFO Shared Memory (structured, low overhead, free 12 h limit) або LHM HTTP `/metrics` (free, admin, MPL 2.0; WMI видалено у 0.9.5+). AIDA64 — платний і з CVE-2024-26507; не рекомендується.
6. **Taskbar:** DeskBand мертвий; `SetParent` працює, але cross-process input/DPI і залежність від приватної ієрархії; **layered overlay** — найстабільніший default. Embed — experimental.
7. **Рендеринг:** Direct2D + DirectWrite у DIB + `UpdateLayeredWindowIndirect`; GDI fallback. Без egui/wgpu/WebView для віджета.
8. **ACPI thermal zone** (`Win32_PerfFormattedData_Counters_ThermalZoneInformation`) на цій машині дає ~47–48 °C без admin, але це зона платформи, не Tctl/Tdie. Лише окремий best-effort елемент, off by default.

## Наступний крок

Узгодити [`ARCHITECTURE.md`](../ARCHITECTURE.md), зокрема:

- температурну стратегію (safe-only / HWiNFO+LHM / helper);
- overlay vs embed;
- обсяг MVP (text-only чи з графіками);
- ліцензію UI та helper;
- цільові ОС і формат дистрибуції.

Після узгодження — Phase 0 spike: overlay + GDI + network/CPU/RAM на цій машині.
