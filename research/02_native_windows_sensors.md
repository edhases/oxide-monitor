# Нативні сенсори Windows для легкого таскбар-монітора

**Дата:** 2026-11-17  
**Цільова платформа:** Windows 10/11 x64; локальна перевірка — Windows 11 26H2, build 26300 (GA, не Insider)  
**Цільове залізо:** AMD Ryzen 5 5600H (Zen 3, 6C/12T, Radeon Vega iGPU), NVIDIA GTX 1650 Laptop (Turing, 4 GB), Apacer AS2280P4U 1 TB NVMe, HGST HTS725050A7E630 500 GB SATA HDD  
**Обмеження:** default path — pure user space, без стороннього kernel driver, бажано без прав адміністратора.

---

## 1. Головний висновок

У Windows 10/11 є повністю робочий **non-admin, user-mode** набір API для більшості метрик таскбар-монітора:

- **Мережа:** `GetIfTable2`/`GetIfEntry2` (`MIB_IF_ROW2.InOctets`/`OutOctets`) або PDH `\Network Interface(*)\Bytes Received/sec` / `Bytes Sent/sec`.
- **CPU load:** PDH `\Processor Information(_Total)\% Processor Utility` (те, що показує Task Manager), `GetSystemTimes` (простіше), `NtQuerySystemInformation(SystemProcessorPerformanceInformation)` (per-core).
- **RAM:** `GlobalMemoryStatusEx` + `GetPerformanceInfo`; PDH `\Memory\*` як альтернатива.
- **GPU NVIDIA:** NVML через `nvml-wrapper`; **GPU AMD:** ADLX (`amdadlx64.dll`); **vendor-neutral fallback:** PDH `\GPU Engine(*)\Utilization Percentage` та `\GPU Adapter Memory(*)\Dedicated Usage`.
- **Диск:** NVMe-температура через `IOCTL_STORAGE_QUERY_PROPERTY` + `StorageDeviceTemperatureProperty` **працює без адміністратора** (перевірено: 41 °C на Apacer); активність — PDH `\PhysicalDisk(*)\*`.
- **ACPI-температура:** `Win32_PerfFormattedData_Counters_ThermalZoneInformation` / PDH `\Thermal Zone Information(*)\High Precision Temperature` працює non-admin (перевірено: 3212 = 48.05 °C), але це **не core temperature**.

Чого немає в чистому user space:

- **CPU core temperature** (Intel DTS / AMD SMN) — тільки ring0/kernel driver; офіційного user-mode API немає.
- **SATA HDD/SSD temperature** — Windows не віддає її через `StorageDeviceTemperatureProperty`; потрібен ATA PASS-THROUGH/SMART з адміністратором.
- **Vendor-neutral GPU temperature** — WDDM/PDH не мають температурного counter; `D3DKMT_ADAPTER_PERFDATA.Temperature` існує, але залежить від KMD і на практиці часто недоступний.
- **`Get-StorageReliabilityCounter`** на тестовій машині без адміністратора дав `Access to a CIM resource was not available to the client.`

Тому реалістична архітектура: **основний non-admin процес** + **опційний elevated helper** лише для SATA SMART/HDD temperature (і, за бажанням, vendor WMI). CPU core temp у default path краще не обіцяти; показувати ACPI zone з явним label.

---

## 2. Локальна валідація (фактичні заміри)

Перевірка виконувалася звичайним користувачем (`Admin=False`) на HP gaming laptop, Windows 11 26H2 build 26300, без elevated helper.

| Метрика / API | Фактичний результат | Висновок |
|---|---|---|
| PDH `\Processor Information(_Total)\% Processor Utility` | **6.37 %** | non-admin працює; це Task Manager-семантика |
| PDH `\Processor Information(_Total)\% Processor Time` | **10.07 %** | time-based, не те, що показує Task Manager |
| PDH `\Processor Information(_Total)\% Processor Performance` | 58–60 % | відповідає CurrentMhz 2011 / base 3301 |
| PDH `\Processor Information(_Total)\Processor Frequency` | 2011 MHz | non-admin; поточний P-state |
| `CallNtPowerInformation(ProcessorInformation)` | MaxMhz 3301, CurrentMhz 2011, MhzLimit 3301 | non-admin; boost 4.2 GHz не видно |
| `GetSystemTimes` | busy 13.33 % (окремий запуск) | cheap, total-only, kernel includes idle |
| `NtQuerySystemInformation(8)` | 12 cores, ret=576 bytes, per-core 4.6–21.9 % | non-admin; per-core працює |
| `GlobalMemoryStatusEx` | total 32 097 MB, avail 5 078 MB, load 84 % | швидко, non-admin |
| `GetPerformanceInfo` | commit 36 314 / 37 755 MB (96 %), physical avail 5 057 MB | committed vs physical |
| PDH `\Memory\Available MBytes` | 4506 MB | трохи інший момент/кеш |
| NVML (`nvml-wrapper`) | GTX 1650: **42 °C**, util 0 %, 786 MB / 4096 MB, power ≈4.4 W | non-admin працює |
| ADLX (`amdadlx64.dll`) | Radeon iGPU: **47 °C**, usage 5 %, 1800/1600 MHz, 6 W, VRAM 423 MB | non-admin працює |
| `IOCTL_STORAGE_QUERY_PROPERTY` + `StorageDeviceTemperatureProperty` (NVMe) | **41 °C**, 3 sensors, critical 95 °C, warning 90 °C | non-admin працює |
| NVMe log page 0x02 (`NVMeDataTypeLogPage`) | 40.85 °C, percentage used 2 % | non-admin працює |
| `StorageDeviceTemperatureProperty` (SATA HDD) | `ERROR_INVALID_FUNCTION` (1) | HDD temp так не читається |
| `IOCTL_ATA_PASS_THROUGH` (HDD, non-admin) | `ERROR_ACCESS_DENIED` (5) | потрібен адміністратор |
| `Get-StorageReliabilityCounter` | **Access denied / CIM resource unavailable** | non-admin не працює |
| `MSAcpi_ThermalZoneTemperature` (root\WMI) | **Access denied** | non-admin не працює |
| `Win32_PerfFormattedData_Counters_ThermalZoneInformation` | **3212** (tenths K) = **48.05 °C**, `_TZ.THRM` | non-admin працює, але ACPI zone |
| PDH `\Thermal Zone Information(*)\High Precision Temperature` | 3212 | те саме через PDH |
| `sysinfo` 0.40.4 | CPU 2011 MHz; Components **0**; GPU usage **None**, used 0; network includes `vEthernet (Default Switch)` | обмеження бібліотеки |
| `sysinfo` Disks | C/E SSD, F HDD; volumes, не physical disks | немає temp/activity |

Окремо варто зафіксувати поведінку CPU: під 100 % навантаженням (12 spinning threads) `CurrentMhz` все одно 2011 MHz, `% Processor Performance` ≈60 %, `% Processor Utility` ≈60 %. Це не баг: ноутбук/профіль живлення обмежує частоту приблизно 2.0 GHz, а Utility нормалізується до номінальних 3.3 GHz. **Task Manager у такому стані теж покаже менше 100 %**, хоча всі ядра зайняті.

---

## 3. Мережа: throughput up/down

### 3.1 IP Helper: `GetIfTable2` / `GetIfEntry2`

`GetIfTable2` повертає `MIB_IF_TABLE2`, що містить `NumEntries` і масив `MIB_IF_ROW2`. Пам'ять виділяє сама IP Helper, звільняти треба `FreeMibTable`. Для одного адаптера швидше `GetIfEntry2`: заповніть `MIB_IF_ROW2.InterfaceLuid` або `InterfaceIndex`, і функція поверне один row.

Ключові поля `MIB_IF_ROW2`:

- `InterfaceLuid`, `InterfaceIndex`, `InterfaceGuid` — ідентифікатори; `InterfaceIndex` може змінитися після disable/enable, `LUID` стабільніший.
- `Alias` (friendly name, наприклад `Wi-Fi`), `Description` (driver name), `Type` (`IF_TYPE_ETHERNET_CSMACD` = 6, `IF_TYPE_IEEE80211` = 71, `IF_TYPE_TUNNEL` = 131, `IF_TYPE_SOFTWARE_LOOPBACK` = 24), `TunnelType`.
- `OperStatus`, `MediaConnectState`, `InterfaceAndOperStatusFlags` (`HardwareInterface`, `FilterInterface`, `ConnectorPresent`, `NotMediaConnected`, `Paused`, `LowPower`, `EndPointInterface`).
- `TransmitLinkSpeed`, `ReceiveLinkSpeed` — біти за секунду, не байти.
- `InOctets`, `OutOctets` — `ULONG64`, **лічильники з моменту ініціалізації адаптера**, включно з L2 framing. Це не те саме, що «корисна швидкість» HTTP; для таскбара це нормально, але підпис має бути «throughput адаптера», а не «швидкість інтернету».
- `InUcastPkts`, `InNUcastPkts`, `OutUcastPkts`, `OutNUcastPkts`, `InErrors`, `OutErrors`, `InDiscards`, `OutDiscards`.

Приклад (спрощено, `windows` crate 0.62):

```rust
use windows::Win32::NetworkManagement::IpHelper::{GetIfEntry2, MIB_IF_ROW2};
use windows::Win32::Foundation::NO_ERROR;

let mut row = MIB_IF_ROW2::default();
row.InterfaceLuid = luid;
if unsafe { GetIfEntry2(&mut row) } == NO_ERROR {
    let rx_bps = (row.InOctets - prev_in) as f64 * 8.0 / elapsed.as_secs_f64();
    let tx_bps = (row.OutOctets - prev_out) as f64 * 8.0 / elapsed.as_secs_f64();
}
```

Практичні деталі:

1. **Вибір активного адаптера.** Для таскбара не потрібно показувати всі інтерфейси. Фільтр: `OperStatus == IfOperStatusUp`, `MediaConnectState == MediaConnectStateConnected`, `InterfaceAndOperStatusFlags.HardwareInterface == 1`, `!FilterInterface`, `Type` = Ethernet або Wi-Fi, `PhysicalAddressLength > 0`. Далі виберіть інтерфейс з default route через `GetBestInterfaceEx(dest, &ifindex)` (наприклад, destination 8.8.8.8) або найбільший `ReceiveLinkSpeed`/`TransmitLinkSpeed` серед підключених. `GetAdaptersAddresses` дає `IP_ADAPTER_ADDRESSES` з `IfType`, `OperStatus`, `FirstGatewayAddress`, `Luid`, `FriendlyName` — зручно, щоб відрізнити реальний default gateway від virtual adapter.
2. **VPN.** Фізичний адаптер передає вже зашифрований трафік, VPN-адаптер — той самий трафік ще раз. Якщо сумувати `Wi-Fi + WireGuard/TAP/PPP`, отримаєте подвійний облік. Показуйте або фізичний adapter, або VPN, але не обидва; у tooltip можна дати обидва ряди.
3. **Reset і overflow.** `InOctets`/`OutOctets` — 64-bit, практичного wrap немає, але adapter reset/disable/enable/driver reload скидає лічильники. Якщо `new < old`, це reset — пропустіть один інтервал, не показуйте від'ємне/гігантське значення. Не використовуйте `SystemTime` для elapsed; беріть `QueryPerformanceCounter` або `Instant`, бо NTP-корекція дасть спайк.
4. **1 Hz.** Достатньо для таскбара і збігається з Task Manager. 2 Hz — максимум для плавності; 10 Hz — зайве навантаження і шум. Перший sample завжди пропускайте.
5. **Без адміністратора.** `GetIfTable2`/`GetIfEntry2` не потребують elevation. Це найдешевший і найточніший шлях для absolute counters.

### 3.2 PDH: `\Network Interface(*)\Bytes Received/sec`

Counter set `Network Interface` (і паралельний `Network Adapter`) дає:

- `\Network Interface(*)\Bytes Received/sec`
- `\Network Interface(*)\Bytes Sent/sec`
- `\Network Interface(*)\Packets Received/sec`, `Packets Sent/sec`
- `\Network Interface(*)\Output Queue Length`

PDH сам рахує rate за два sample, обробляє counter wrap і дає готові bytes/sec. Мінуси: names локалізовані; wildcard повертає масив instances; перший `PdhCollectQueryData` не дає валідного rate; потрібен `PdhAddEnglishCounterW`, інакше на українській/російській Windows English path не знайдеться.

На тестовій системі `Get-Counter -ListSet 'Network Interface'` не знайшов set через локалізацію, але `PdhAddEnglishCounterW` з тим самим English path працював. Це важливий практичний висновок: **не орієнтуйтеся на `Get-Counter`/localized names, використовуйте English PDH API**.

Приклад:

```rust
let mut q = PDH_HQUERY::default();
PdhOpenQueryW(None, 0, &mut q);
let mut c = PDH_HCOUNTER::default();
PdhAddEnglishCounterW(q, w!(r"\Network Interface(*)\Bytes Received/sec"), 0, &mut c);
PdhCollectQueryData(q);
// через 1 s:
PdhCollectQueryData(q);
// PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, ...) -> PDH_FMT_COUNTERVALUE_ITEM_W[]
```

`PDH_FMT_DOUBLE` — це `0x00000200`, а не `0x00000100` (`PDH_FMT_LONG`). Якщо переплутати, `PDH_FMT_COUNTERVALUE.Anonymous.doubleValue` прочитає long як double і дасть 0/денормальне число. `PDH_FMT_NOCAP100` (`0x00008000`) потрібен, якщо хочете бачити значення >100 % для CPU Utility. Для wildcard використовуйте `PdhGetFormattedCounterArrayW` з двома викликами: перший повертає `PDH_MORE_DATA` (`0x800007D2`) і розмір, другий — дані.

### 3.3 `sysinfo` network API

`sysinfo::Networks::new_with_refreshed_list()` повертає `HashMap<OsString, NetworkData>`. `NetworkData` має `received()`, `transmitted()` — байти **з моменту останнього refresh**, `total_received()`, `total_transmitted()` — з моменту старту, `packets_received()`, `packets_transmitted()`, `errors_on_received()`, `mac_address()`, `ip_networks()`, `mtu()`, `operational_state()`, `receive_link_speed()`, `transmit_link_speed()`.

На Windows sysinfo використовує `GetIfTable2` і фільтрує:

- `TransmitLinkSpeed != 0 || ReceiveLinkSpeed != 0`
- `MediaConnectState != MediaConnectStateDisconnected`
- `PhysicalAddressLength != 0`
- дедуплікація за GUID (щоб прибрати software interfaces з тим самим GUID).

Це непогано, але **недостатньо**:

- у нашому запуску в список потрапив `vEthernet (Default Switch)` з link speed 10 Gbps; це Hyper-V virtual switch, і його трафік може дублювати фізичний;
- VPN/TAP/WireGuard можуть мати `PhysicalAddressLength == 0` і бути відфільтровані або, навпаки, залишитися залежно від драйвера;
- `received()` — це delta за інтервал, але бібліотека не дає elapsed; ви маєте самі ділити на фактичний час і обробляти reset;
- `NetworkData` не розрізняє Ethernet/Wi-Fi/VPN семантично, тільки `operational_state` і link speed.

Тому для таскбара краще взяти `GetIfEntry2` + власну логіку вибору, а `sysinfo` — як швидкий прототип.

### 3.4 Taskbar / Task Manager caveats

- У Windows 11 **немає публічного API таскбара для відображення throughput**. Іконка мережі у flyout показує connectivity, а не швидкість. Власний widget — це окреме topmost-вікно/AppBar (`SHAppBarMessage`, `SetWindowPos`) або tray icon; DeskBands у Windows 11 прибрані.
- Task Manager показує **per-adapter** графік, а не суму всіх інтерфейсів; VPN і фізичний adapter — окремі рядки. Якщо ваша цифра сумує all interfaces, вона буде більшою за Task Manager.
- Task Manager семплує приблизно раз на секунду; графік у Mbps — це bytes/sec × 8. «Пікові» значення на суб-секундному інтервалі можуть відрізнятися у 2–3 рази.
- `Network Interface` counters включають L2 headers; корисна пропускна здатність буде трохи меншою. Для таскбара це прийнятно, але не називайте це «швидкістю завантаження файлу».

---

## 4. CPU load і частота

### 4.1 `GetSystemTimes`

`GetSystemTimes(&idle, &kernel, &user)` повертає сумарні `FILETIME` по всіх процесорах. `kernel` **включає** `idle`, тому:

```text
busy% = 100 * (1 - Δidle / (Δkernel + Δuser))
```

Плюси: один дешевий виклик, без адміністратора, стабільний API. Мінуси: тільки total, не per-core; це time-based, а не frequency-normalized; на системах з >64 logical CPUs є processor groups, і `GetSystemTimes` не дає per-group розбивку.

### 4.2 PDH `% Processor Utility` vs `% Processor Time`

З Windows 8 Task Manager і Performance Monitor для CPU % використовують:

- `\Processor Information(_Total)\% Processor Utility`
- `\Processor Information(_Total)\% Privileged Utility`

а не `% Processor Time`/`% Privileged Time`, як у Windows 7. Це прямо описано в Microsoft KB 3200459: utility counters враховують processor performance state і Turbo Boost, тому показують «скільки роботи реально виконано», а time counters — «скільки часу процесор був зайнятий». Utility може перевищувати 100 % при turbo.

На тестовій машині:

- `% Processor Utility` = **6.37 %**
- `% Processor Time` = **10.07 %**

Різниця очікувана: CPU працює на ~2.0 GHz проти номінальних 3.3 GHz, тому utility ≈ 10.07 × (2.0/3.3) ≈ 6.1 %. Task Manager покаже ближче до 6.37 %, ніж до 10.07 %.

Для таскбара:

- хочете «як Task Manager» — беріть Utility, clamp 0..100 для смужки, у tooltip показуйте raw (може бути >100 при turbo);
- хочете просту time-based завантаженість — `GetSystemTimes`, але підпис має бути «time busy», а не «Task Manager CPU»;
- не змішуйте ці дві метрики в одному графіку.

PDH також дає per-core `\Processor Information(0,0)\% Processor Utility`; instance names — `0,0`, `0,1`, …, `0,_Total`. Це зручно, якщо не хочете тягнути `NtQuerySystemInformation`.

### 4.3 `NtQuerySystemInformation(SystemProcessorPerformanceInformation)`

`NtQuerySystemInformation` з class `SystemProcessorPerformanceInformation` (8) повертає масив `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION` — по одному на logical CPU:

```c
typedef struct _SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION {
    LARGE_INTEGER IdleTime;
    LARGE_INTEGER KernelTime;
    LARGE_INTEGER UserTime;
    LARGE_INTEGER DpcTime;
    LARGE_INTEGER InterruptTime;
    ULONG InterruptCount;
} SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION; // 48 bytes on x64
```

`KernelTime` так само включає `IdleTime`. Per-core busy = `1 - ΔIdle / (ΔKernel + ΔUser)`.

На тестовій машині 12 CPU дали `ret=576` bytes (12 × 48), status 0, без адміністратора; per-core значення 4.6–21.9 %. Це один syscall, десятки мікросекунд, дешевше за 12 окремих PDH counters. Недолік: клас і структура **напівдокументовані**; Microsoft може їх змінити. Для production краще мати fallback на `GetSystemTimes`/PDH per-core.

У `windows` crate: `windows::Wdk::System::SystemInformation::NtQuerySystemInformation`, feature `Wdk_System_SystemInformation`; структуру `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION` часто доводиться оголошувати самостійно (`#[repr(C)]`).

### 4.4 `sysinfo` CPU

`sysinfo` 0.40.4 на Windows:

- `System::new_all()` тепер повертає `Result<System, Error>` (API змінився; старі приклади з `System::new_all()` без `?` не компілюються);
- `Cpu::usage()` (раніше `cpu_usage()`);
- для global CPU використовує PDH `\Processor(_Total)\% Idle Time`, для per-core — `\Processor(N)\% Idle Time`;
- `MINIMUM_CPU_UPDATE_INTERVAL = 200 ms`;
- frequency бере з `CallNtPowerInformation(ProcessorInformation).CurrentMhz`.

Тобто `sysinfo` дає **time-based** CPU %, а не `% Processor Utility`. На тестовій машині global CPU був 16–17 % у моменти, коли PDH Utility був 7.5–11 %; різниця закономірна. Якщо потрібно «як Task Manager», `sysinfo` не підходить.

### 4.5 Частота CPU

`CallNtPowerInformation(ProcessorInformation, ...)` повертає масив `PROCESSOR_POWER_INFORMATION`:

```c
typedef struct _PROCESSOR_POWER_INFORMATION {
    ULONG Number;
    ULONG MaxMhz;
    ULONG CurrentMhz;
    ULONG MhzLimit;
    ULONG MaxIdleState;
    ULONG CurrentIdleState;
} PROCESSOR_POWER_INFORMATION;
```

На тестовій машині: `MaxMhz=3301`, `CurrentMhz=2011`, `MhzLimit=3301` для всіх 12 CPU. Це без адміністратора. Але:

- `CurrentMhz` — це поточний P-state від OS power manager, а не точний boost clock; він квантований, може відставати і на різних ядрах показувати однакове значення;
- `MaxMhz` — номінальна/базова частота, не maximum turbo; для Ryzen 5 5600H base 3.3 GHz, boost до 4.2 GHz, але `MaxMhz` = 3301;
- на деяких системах `CurrentMhz` = 0 або завжди max; перевіряйте sanity;
- registry `HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0\~MHz` — це статична базова частота (3301), не поточна; не використовуйте для графіка;
- PDH `\Processor Information(_Total)\Processor Frequency` дає поточні MHz (2011) і узгоджується з `CurrentMhz`; `% Processor Performance` — відношення до nominal (58–60 %).

Рекомендація: показуйте «CPU 2.01 GHz» з tooltip «base 3.30 GHz, boost до 4.2 GHz (не видно через OS)». Не називайте `MaxMhz` максимумом turbo.

---

## 5. RAM: physical vs committed

### 5.1 `GlobalMemoryStatusEx`

`MEMORYSTATUSEX` (обов'язково `dwLength = sizeof(MEMORYSTATUSEX)`):

- `ullTotalPhys` — фізична пам'ять, видима OS (без hardware-reserved);
- `ullAvailPhys` — available physical, **включає standby**;
- `ullTotalPageFile`, `ullAvailPageFile` — commit limit/available;
- `ullTotalVirtual`, `ullAvailVirtual`, `ullAvailExtendedVirtual`;
- `dwMemoryLoad` — приблизний відсоток physical in use.

Це найшвидший шлях (<1 µs), без адміністратора.

На тестовій машині: total 32 097 MB, avail 5 078 MB, `dwMemoryLoad=84 %`. «In use» = total − avail ≈ 27 019 MB.

### 5.2 `GetPerformanceInfo`

`PERFORMANCE_INFORMATION` (psapi.h) має `cb` першим полем; його треба виставити перед викликом. Поля:

- `CommitTotal`, `CommitLimit`, `CommitPeak` — у pages;
- `PhysicalTotal`, `PhysicalAvailable` — у pages;
- `SystemCache`, `KernelTotal`, `KernelPaged`, `KernelNonpaged`, `PageSize`;
- `HandleCount`, `ProcessCount`, `ThreadCount`.

Розмір page — `PageSize` (зазвичай 4096). `CommitTotal * PageSize` — committed bytes; `CommitLimit * PageSize` — commit limit. На тестовій машині: commit 36 314 MB / limit 37 755 MB (96 %), physical available 5 057 MB. Це саме те, що Task Manager показує в рядку «Committed».

`GetPerformanceInfo` / `K32GetPerformanceInfo` не потребує адміністратора. У `windows` crate — feature `Win32_System_ProcessStatus`.

### 5.3 PDH `\Memory\*`

- `\Memory\Available MBytes` — available physical, MB;
- `\Memory\Committed Bytes`, `\Memory\Commit Limit`, `\Memory\% Committed Bytes In Use`;
- `\Memory\Cache Bytes`, `\Memory\Pool Paged Bytes`, `\Memory\Pool Nonpaged Bytes`.

На тестовій машині `Available MBytes` = 4506 MB, що трохи відрізняється від `GlobalMemoryStatusEx` 5 078 MB через момент семплування/кеш. PDH зручний, якщо вже є query для CPU/disk, але для RAM дешевше прямий API.

### 5.4 WMI `Win32_OperatingSystem`

`Win32_OperatingSystem.TotalVisibleMemorySize` і `FreePhysicalMemory` — у KB; `TotalVirtualMemorySize`, `FreeVirtualMemory`, `SizeStoredInPagingFiles`. Це ті самі дані, але через WMI: 5–50 ms, залежність від WMI service, складніший error handling. Для 1 Hz не використовуйте; для inventory/одноразового читання — можна. `wmi` crate 0.18.4.

### 5.5 Що показує Task Manager

- Заголовний відсоток RAM — **physical in use** = `total - available` (available включає standby).
- «Committed» — `CommitTotal / CommitLimit`, може перевищувати 100 % physical; це окремий показник.
- «Cached» — system cache/standby, не те саме, що «available».
- Тому в таскбарі краще два рядки: `RAM 84 %` (physical) і `Commit 36.3/37.8 GB` (tooltip). Не змішуйте `\Memory\% Committed Bytes In Use` з фізичним відсотком.

---

## 6. GPU: NVML, ADLX, Level Zero, WDDM

### 6.1 NVIDIA NVML

NVML — офіційна C API NVIDIA, встановлюється разом з display driver. На Windows `nvml.dll` і `nvidia-smi.exe` лежать у `C:\Windows\System32` (перевірено: nvml.dll version 8.17.15.9282, driver 592.82). `nvml-wrapper` 0.13.0 завантажує DLL динамічно через `libloading`, тому програма може працювати на машинах без NVIDIA: `Nvml::init()` поверне `NvmlError::LibraryNotFound`.

Ключові функції NVML:

- `nvmlInit_v2()` / `nvmlShutdown()`;
- `nvmlDeviceGetHandleByIndex_v2(index, &handle)` / by UUID / by PCI;
- `nvmlDeviceGetUtilizationRates(handle, &nvmlUtilization_t { gpu, memory })` — відсотки;
- `nvmlDeviceGetTemperature(handle, NVML_TEMPERATURE_GPU /* 0 */, &temp_c)`;
- `nvmlDeviceGetMemoryInfo(handle, &nvmlMemory_t { total, free, used })` — bytes;
- `nvmlDeviceGetPowerUsage(handle, &mw)` — milliwatts;
- `nvmlDeviceGetEnforcedPowerLimit`, `nvmlDeviceGetPowerManagementLimit`;
- `nvmlDeviceGetClockInfo(handle, NVML_CLOCK_GRAPHICS/SM/MEM, &mhz)`;
- `nvmlDeviceGetFanSpeed`, `nvmlDeviceGetPcieThroughput`, `nvmlDeviceGetPerformanceState`;
- `nvmlDeviceGetProcessUtilization`, `nvmlDeviceGetComputeRunningProcesses`, `nvmlDeviceGetGraphicsRunningProcesses`.

`nvml-wrapper` API:

```rust
use nvml_wrapper::{Nvml, enum_wrappers::device::TemperatureSensor};

let nvml = Nvml::init()?;
let dev = nvml.device_by_index(0)?;
let temp = dev.temperature(TemperatureSensor::Gpu)?; // °C
let util = dev.utilization_rates()?;                  // { gpu, memory }
let mem  = dev.memory_info()?;                        // { total, free, used }
let pwr  = dev.power_usage()?;                        // mW
```

Локально: GTX 1650, 42 °C, GPU util 0 %, memory 786 MB / 4096 MB, power 4.4 W, без адміністратора.

Обмеження:

- тільки NVIDIA; на AMD/Intel `Nvml::init()` не знайде DLL або не знайде device;
- NVML віддає **GPU die temperature** (`NVML_TEMPERATURE_GPU`), не hotspot/memory junction; `nvml-wrapper` 0.13 має лише `TemperatureSensor::Gpu`;
- laptop dGPU під Optimus може бути повністю вимкнений (D3cold). NVML або не перелічить його, або поверне `NVML_ERROR_GPU_IS_LOST` / timeout. Не «будіть» dGPU кожним poll: це збільшує споживання і температуру. Політика: poll раз на 2–5 s, при помилці показуйте «dGPU asleep», не робіть retry storm;
- на деяких GeForce `power_usage` або fan speed недоступні (`NVML_ERROR_NOT_SUPPORTED`); перевіряйте `Result`;
- NVML не потребує адміністратора (перевірено), але потребує сумісного driver; старі драйвери можуть не мати потрібних символів. `nvml-wrapper` поверне помилку при `init`, якщо DLL/версія несумісні.

### 6.2 AMD ADLX

ADLX (AMD Device Library eXtra) — сучасна AMD API для display/3D/performance monitoring/tuning. Офіційна версія — **2.0 (вересень 2026)**, до неї 1.5 (квітень 2026). DLL називається **`amdadlx64.dll`** (не `adlx.dll`), лежить у `C:\Windows\System32` (перевірено: version 1.1.0.56 на тестовій машині).

Ініціалізація (C ABI, COM-подібні vtable-інтерфейси):

```c
HMODULE dll = LoadLibraryW(L"amdadlx64.dll");
ADLXInitialize2(ADLX_FULL_VERSION, &system, &adlMapping); // або ADLXInitialize
// ...
ADLXTerminate();
```

Основні інтерфейси:

- `IADLXSystem::GetGPUs(IADLXGPUList**)`, `IADLXSystem::GetPerformanceMonitoringServices(...)`;
- `IADLXGPUList::At(index, IADLXGPU**)`, `IADLXGPU::Name`, `VendorId`, `TotalVRAM`;
- `IADLXPerformanceMonitoringServices::GetCurrentGPUMetrics(IADLXGPU*, IADLXGPUMetrics**)`;
- `IADLXGPUMetricsSupport` — `IsSupportedGPUUsage`, `IsSupportedGPUTemperature`, `IsSupportedGPUHotspotTemperature`, `IsSupportedGPUPower`, `IsSupportedGPUClockSpeed`, `IsSupportedGPUVRAMClockSpeed`, `IsSupportedGPUFanSpeed`, `IsSupportedGPUVRAM`, `IsSupportedGPUVoltage`, `IsSupportedGPUTotalBoardPower`, `IsSupportedGPUIntakeTemperature`;
- `IADLXGPUMetrics` — `GPUUsage`, `GPUTemperature`, `GPUHotspotTemperature`, `GPUPower`, `GPUTotalBoardPower`, `GPUClockSpeed`, `GPUVRAMClockSpeed`, `GPUFanSpeed`, `GPUVRAM`, `GPUVoltage`, `GPUIntakeTemperature`, `TimeStamp`.

Локально (Radeon iGPU у 5600H): **47 °C**, usage 5 %, clocks 1800/1600 MHz, power 6 W, VRAM 423 MB, voltage 1050 mV, без адміністратора.

Важливий нюанс: sample викликав `IsSupportedGPUTemperature` і отримав **false**, але `GPUTemperature` все одно повернув 47.0 °C; аналогічно clock/power support queries казали «not supported», але значення були. Тобто **не можна жорстко gate-ити всі метрики лише на `IsSupported*`**; перевіряйте `ADLX_RESULT` і sanity range (0–120 °C, 0–100 %), а `IsSupported*` використовуйте як підказку для UI.

Rust:

- офіційні bindings `amd-adlx` v0.1.0 лежать у SDK repo (`Samples/rust/amd-adlx`), але **не опубліковані на crates.io** станом на 2026-11-17; їх можна взяти vendor-ом/path dependency;
- `adlx` crate — `0.0.0-alpha.1` (2024), фактично закинутий;
- `adlx-lite` 1.0.0 (2026-10-23) — дуже малий adoption (39 downloads), не варто як єдина залежність;
- практично: `libloading` 0.9.1 + власні `#[repr(C)]` vtable structs, або SDK-шний `amd-adlx`.

Обмеження: тільки AMD; APU support залежить від driver/ADLX; на hybrid laptop треба коректно вибрати iGPU vs dGPU (за `IADLXGPU::Name`/PCI); init дорожчий за NVML; ADLX DLL може бути відсутній на старих драйверах; температури APU можуть бути відсутні або еквівалентні ACPI zone.

### 6.3 Intel oneAPI Level Zero (для повноти)

`ze_loader.dll` + Sysman:

- `zesInit(0)`, `zesDriverGet`, `zesDeviceGet`;
- `zesDeviceEnumTemperatureSensors`, `zesTemperatureGetState` — Celsius double;
- `zesDeviceEnumEngineGroups`, `zesEngineGetActivity` — activeTime/timestamp для utilization;
- `zesDeviceEnumMemoryModules`, `zesMemoryGetState` — total/free.

Це Intel-only; на Intel iGPU temperature sensor часто відсутній, на Arc dGPU працює. Для цільового AMD 5600H не потрібно; у Rust немає зрілого crate, тільки raw FFI через `libloading`.

### 6.4 WDDM/PDH — vendor-neutral fallback

Counter sets (доступні без адміністратора):

- `\GPU Engine(*)\Utilization Percentage`
- `\GPU Engine(*)\Running Time`
- `\GPU Adapter Memory(*)\Dedicated Usage`
- `\GPU Adapter Memory(*)\Shared Usage`
- `\GPU Adapter Memory(*)\Total Committed`
- `\GPU Process Memory(*)\Dedicated Usage`, `Shared Usage`, `Total Committed`
- `\GPU Local Adapter Memory(*)`, `\GPU Non Local Adapter Memory(*)` (залежить від WDDM/драйвера)

Instance name для `GPU Engine`:

```text
pid_<pid>_luid_0x<high>_0x<low>_phys_<n>_eng_<n>_engtype_<Type>
```

Наприклад: `pid_4840_luid_0x00000000_0x000104FB_phys_0_eng_0_engtype_3D`.

На тестовій машині `\GPU Engine(*)\Utilization Percentage` повернув **416 instances** і buffer ~65 KB; це нормально для hybrid AMD+NVIDIA+virtual adapters, але парсинг і агрегація помітні. `\GPU Adapter Memory(*)\Dedicated Usage` повернув 3 instances:

- `luid_0x00000000_0x000104FB_phys_0` ≈ 441 MB (AMD iGPU/UMA carveout);
- `luid_0x00000000_0x11AD6_phys_0` = 0 (software/basic render);
- `luid_0x00000000_0x11B4C_phys_0` ≈ 790 MB (NVIDIA GTX 1650, збігається з NVML 786 MB).

Агрегація:

- **Загальний GPU %** — не сума всіх engine instances. Сума легко >100 %, бо 3D, Copy, VideoDecode, Compute працюють паралельно. Task Manager показує найбільш завантажений engine/engine group; для таскбара можна взяти `max` по `engtype` для конкретного LUID, або середнє по 3D-engines. Якщо потрібен «загальний GPU», `max` краще корелює з Task Manager, ніж `sum`.
- **Per-process GPU** — сумуйте `Utilization Percentage` за `pid` (але це теж може >100 % для процесу з кількома engines); для списку процесів це прийнятно.
- **VRAM** — `Dedicated Usage` за LUID; `Shared Usage` — system RAM, яку використовує GPU; `Total Committed` — сума. На iGPU «dedicated» — це carveout, а не окрема VRAM.
- **Mapping LUID → adapter** — DXGI: `IDXGIFactory6::EnumAdapters1`, `IDXGIAdapter::GetDesc1().AdapterLuid`, `IDXGIAdapter3::QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, ...)`; або `windows` crate `Win32_Graphics_Dxgi`.

Чому **температури немає** в WDDM/PDH:

- `GPU Engine` і `GPU Adapter Memory` — це scheduler/memory accounting counters від `Dxgkrnl`; вони не є thermal telemetry.
- У PDH немає counter `GPU Temperature`; на тестовій машині `GPU Engine` set містить лише 2 counters (`Utilization Percentage`, `Running Time`), `GPU Adapter Memory` — 3.
- WDDM не має стандартизованого user-mode thermal interface. Температура належить vendor KMD/EC/ACPI.
- Існує `D3DKMT_ADAPTER_PERFDATA` з полем `Temperature` (deci-Celsius) і `KMTQAITYPE_ADAPTERPERFDATA`, але це WDK-структура, яку KMD **може** заповнювати або ні; на практиці NVIDIA/AMD часто повертають 0/unsupported для consumer GPU. Наша спроба `D3DKMTOpenAdapterFromLuid` + `D3DKMTQueryAdapterInfo(KMTQAITYPE_ADAPTERPERFDATA)` повернула `0xC000000D` (STATUS_INVALID_PARAMETER) для всіх трьох LUID; це ілюструє, чому D3DKMT не можна робити default path.
- `D3DKMTQueryStatistics` дає engine running time, але не температуру; він напівдокументований і складний.

Тому для температури GPU залишаються NVML (NVIDIA), ADLX (AMD), Level Zero (Intel) або ACPI thermal zone (якщо вона взагалі є для GPU). Vendor-neutral temperature без vendor API — ненадійна.

### 6.5 Task Manager GPU caveats

- Task Manager GPU % — це не `sum(GPU Engine Utilization)`; це евристика на основі найбільш завантаженого engine.
- «Dedicated GPU memory» — `GPU Adapter Memory\Dedicated Usage`; «Shared GPU memory» — `Shared Usage`/system RAM.
- Task Manager не дає vendor-neutral температуру; якщо вона десь і з'являється, це vendor/OEM integration, а не публічний WDDM counter.
- На laptop з Optimus Task Manager може показувати кілька GPU (iGPU + dGPU), і dGPU може бути asleep; не інтерпретуйте 0 % як «немає GPU».

---

## 7. Диски: NVMe, SATA SMART, активність

### 7.1 Inventory без адміністратора

Відкрийте `\\.\PhysicalDriveN` з `CreateFileW(path, 0, FILE_SHARE_READ | FILE_SHARE_WRITE, ...)` (0 desired access) і надішліть `IOCTL_STORAGE_QUERY_PROPERTY` з `STORAGE_PROPERTY_QUERY { PropertyId = StorageDeviceProperty, QueryType = PropertyStandardQuery }`. Output — `STORAGE_DEVICE_DESCRIPTOR`:

- `Version`, `Size`, `DeviceType`, `DeviceTypeModifier`, `RemovableMedia`, `CommandQueueing`;
- `VendorIdOffset`, `ProductIdOffset`, `ProductRevisionOffset`, `SerialNumberOffset` — offsets у `RawDeviceProperties`;
- `BusType` — 7 = USB, 8 = RAID, 11 = SATA, 17 = NVMe;
- `RawPropertiesLength`, `RawDeviceProperties[1]`.

Локально: drive 0 — `BusType=11` (SATA) `HGST HTS725050A7E630`; drive 1 — `BusType=17` (NVMe) `Apacer AS2280P4U 1TB`. Обидва descriptor-и читаються **без адміністратора**.

### 7.2 NVMe temperature: `StorageDeviceTemperatureProperty`

Найпростіший шлях:

```c
STORAGE_PROPERTY_QUERY q = {0};
q.PropertyId = StorageDeviceTemperatureProperty; // 52
q.QueryType  = PropertyStandardQuery;
DeviceIoControl(h, IOCTL_STORAGE_QUERY_PROPERTY, &q, sizeof(q),
                buf, buf_size, &ret, NULL);
```

Output — `STORAGE_TEMPERATURE_DATA_DESCRIPTOR`:

```c
DWORD Version, Size;
SHORT CriticalTemperature, WarningTemperature;
WORD  InfoCount;
BYTE  Reserved0[2];
DWORD Reserved1[2];
STORAGE_TEMPERATURE_INFO TemperatureInfo[];
```

`STORAGE_TEMPERATURE_INFO`:

```c
WORD  Index;          // 0 = composite
SHORT Temperature;    // Celsius
SHORT OverThreshold, UnderThreshold;
BOOLEAN OverThresholdChangable, UnderThresholdChangable, EventGenerated;
BYTE Reserved0;
DWORD Reserved1;
```

Локально Apacer NVMe: `InfoCount=3`, `Temperature=41 °C`, `CriticalTemperature=95`, `WarningTemperature=90`, **без адміністратора**. Це найкращий default для NVMe.

`StorageAdapterTemperatureProperty` (51) — для controller/adapter; на NVMe теж спрацював, на SATA HDD — `ERROR_IO_DEVICE` (1117).

### 7.3 NVMe log page 0x02 (SMART/Health)

Якщо `StorageDeviceTemperatureProperty` не підтримується драйвером, можна напряму:

- `PropertyId = StorageDeviceProtocolSpecificProperty`
- `STORAGE_PROTOCOL_SPECIFIC_DATA`:
  - `ProtocolType = ProtocolTypeNvme` (3)
  - `DataType = NVMeDataTypeLogPage` (2)
  - `ProtocolDataRequestValue = 0x02` (SMART/Health log)
  - `ProtocolDataRequestSubValue = 0`
  - `ProtocolDataOffset = sizeof(STORAGE_PROTOCOL_SPECIFIC_DATA)` (40)
  - `ProtocolDataLength = 512`
- Output — `STORAGE_PROTOCOL_DATA_DESCRIPTOR` + `NVME_HEALTH_INFO_LOG`.

`NVME_HEALTH_INFO_LOG` (512 bytes):

- byte 0 — `CriticalWarning`;
- bytes 1–2 — `Temperature` (Kelvin, little-endian, composite);
- byte 3 — `AvailableSpare`; byte 4 — `AvailableSpareThreshold`; byte 5 — `PercentageUsed`;
- bytes 200–215 — `TemperatureSensor1..8` (Kelvin, 16-bit each; sensor 1 at offset 200).

Локально: composite 314 K = **40.85 °C**, `PercentageUsed=2 %`, 3 sensors. Теж без адміністратора.

У `windows` crate: `Win32::System::Ioctl::{IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_PROPERTY_QUERY, STORAGE_TEMPERATURE_DATA_DESCRIPTOR, STORAGE_TEMPERATURE_INFO, StorageDeviceTemperatureProperty, PropertyStandardQuery}` + `Win32::Storage::FileSystem::CreateFileW` + `Win32::System::IO::DeviceIoControl`. Features: `Win32_System_Ioctl`, `Win32_Storage_FileSystem`, `Win32_System_IO`.

### 7.4 SATA SSD/HDD: ATA PASS-THROUGH / SMART

Windows не транслює `StorageDeviceTemperatureProperty` у ATA SMART для більшості SATA-дисків. Локально HGST HDD дав `ERROR_INVALID_FUNCTION` (1), adapter — `ERROR_IO_DEVICE` (1117).

Робочий шлях — ATA PASS-THROUGH:

- `IOCTL_ATA_PASS_THROUGH` / `IOCTL_ATA_PASS_THROUGH_DIRECT`;
- `ATA_PASS_THROUGH_EX`:
  - `Length = sizeof(ATA_PASS_THROUGH_EX)`
  - `AtaFlags = ATA_FLAGS_DATA_IN | ATA_FLAGS_DRDY_REQUIRED`
  - `DataTransferLength = 512`
  - `DataBufferOffset` — offset у буфері
  - `CurrentTaskFile[8]`:
    - `[0] Features = 0xD0`
    - `[1] SectorCount = 0x01`
    - `[2] LBA Low = 0x4F`
    - `[3] LBA Mid = 0xC2`
    - `[4] LBA High = 0x4F`
    - `[5] Drive/Head = 0xA0`
    - `[6] Command = 0xB0` (SMART)
    - `[7]` — не використовується
- 512-byte SMART READ DATA; атрибут 194 (Temperature Celsius) або 190 (Airflow Temperature) у vendor-specific raw bytes. У HGST зазвичай 194.

Альтернатива — legacy `SMART_RCV_DRIVE_DATA` (саме така назва константи в `ntdddisk.h`; не `SMART_RECEIVE_DRIVE_DATA`) з `SENDCMDINPARAMS`/`SENDCMDOUTPARAMS`. Вона теж потребує admin.

Ключове: `IOCTL_ATA_PASS_THROUGH` має `FILE_READ_ACCESS | FILE_WRITE_ACCESS` у `CTL_CODE`, тому `CreateFile` на `\\.\PhysicalDriveN` має бути з read+write, а це elevation. Локально:

- `CreateFile(..., 0, ...)` → `DeviceIoControl` → `ERROR_ACCESS_DENIED` (5);
- `CreateFile(..., GENERIC_READ, ...)` → `ERROR_ACCESS_DENIED` (5);
- `CreateFile(..., GENERIC_READ|GENERIC_WRITE, ...)` → `ERROR_ACCESS_DENIED` (5).

Тобто **HDD/SATA temperature у default non-admin path недоступна**. Варіанти:

1. Опційний elevated helper (окремий процес, запускається за потреби), який робить ATA PASS-THROUGH і повертає лише temp.
2. `Get-StorageReliabilityCounter` з адміністратором (якщо Storage provider підтримує).
3. Взагалі не показувати HDD temp; показувати SMART health/wear, якщо доступно.

### 7.5 `Get-StorageReliabilityCounter` / `MSFT_StorageReliabilityCounter`

`MSFT_StorageReliabilityCounter` (root\Microsoft\Windows\Storage) має:

- `Temperature` (UInt8, °C), `TemperatureMax` (UInt8);
- `Wear`, `PowerOnHours`, `StartStopCycleCount`, `LoadUnloadCycleCount`;
- `ReadErrorsTotal/Corrected/Uncorrected`, `WriteErrorsTotal/...`, `ReadLatencyMax`, `WriteLatencyMax`, `FlushLatencyMax`;
- `ManufactureDate`, `DeviceId`.

PowerShell: `Get-PhysicalDisk | Get-StorageReliabilityCounter`. Локально без адміністратора:

```text
Get-StorageReliabilityCounter : Access to a CIM resource was not available to the client.
```

Тобто **не розраховуйте на non-admin**. З адміністратором цей шлях може працювати для NVMe і SATA, але залежить від Storage Management provider; на USB bridges/RAID часто повертає 0/unsupported.

### 7.6 Disk activity через PDH

Counter set `PhysicalDisk`:

- `\PhysicalDisk(*)\% Disk Time`
- `\PhysicalDisk(*)\Disk Read Bytes/sec`
- `\PhysicalDisk(*)\Disk Write Bytes/sec`
- `\PhysicalDisk(*)\Disk Bytes/sec` (read+write)
- `\PhysicalDisk(*)\Disk Reads/sec`, `Disk Writes/sec`
- `\PhysicalDisk(*)\Avg. Disk Queue Length`
- `\PhysicalDisk(*)\% Idle Time`

На тестовій машині instances: `0 F:`, `1 C: E:`, `_Total`; `Disk Bytes/sec` ≈ 340 KB/s, `% Disk Time` ≈ 2 %. Без адміністратора.

Нюанси:

- `% Disk Time` може перевищувати 100 % (Microsoft це документує для counter definition); для смужки 0–100 використовуйте `100 - % Idle Time` або clamp.
- На NVMe/RAID `% Disk Time` іноді зависає на 100 % або показує 0; надійніше комбінувати `Disk Bytes/sec` + `Avg. Disk Queue Length`.
- Instance names прив'язані до disk numbers; після зміни disk order вони можуть змінитися. Періодично пере-енумеруйте.
- `PhysicalDisk` counters не дають температуру; тільки I/O.

### 7.7 USB bridges / RAID

- **USB-SATA bridge:** `StorageDeviceProperty` працює, `BusType=7`; ATA PASS-THROUGH/SMART часто блокується bridge firmware або повертає vendor-specific data; `Get-StorageReliabilityCounter` може не бачити disk. NVMe у USB enclosure — ще гірше, NVMe passthrough зазвичай недоступний.
- **RAID/Storage Spaces:** фізичні диски приховані за virtual disk; `MSFT_PhysicalDisk` може показати pool disk, але SMART/temperature членів масиву недоступні. Intel RST/AMD RAID можуть не пропускати IOCTL до member disks.
- **NVMe RAID:** Windows Storage Spaces/AMD RAIDXpert можуть не віддавати temperature; `StorageDeviceTemperatureProperty` на virtual disk може повернути 0/unsupported.
- **USB HDD:** температура недоступна без vendor-specific bridge protocol; не обіцяйте.

---

## 8. CPU temperature: чому немає офіційного user-mode API

### 8.1 Що є офіційно

1. **`Win32_TemperatureProbe`** (root\cimv2) — WMI-клас temperature sensor. Microsoft прямо пише: більшість даних з SMBIOS, а `CurrentReading` **не заповнюється** поточними реалізаціями WMI; поле зарезервоване на майбутнє. Тобто це не робочий API для CPU temp.
2. **`MSAcpi_ThermalZoneTemperature`** (root\WMI) — ACPI thermal zone:
   - `CurrentTemperature` — `uint32` у **tenths of Kelvin**;
   - conversion: `°C = CurrentTemperature / 10.0 - 273.15`;
   - на практиці потребує адміністратора (локально — `Access denied`);
   - часто повертає одну зону (`TZ00`, `TZ01`, `_TZ.THRM`), це температура **платформи/EC/корпусу**, не core;
   - може бути 0, 3000 (26.85 °C, «заглушка») або не змінюватися під навантаженням.
3. **`Win32_PerfFormattedData_Counters_ThermalZoneInformation`** / PDH `\Thermal Zone Information(*)\High Precision Temperature`:
   - локально **без адміністратора**: `_TZ.THRM` = `HighPrecisionTemperature=3212` (tenths K) = **48.05 °C**, `Temperature=321` K;
   - це той самий ACPI thermal zone, але через performance counter provider; зручніше, ніж root\WMI, і не потребує elevation у нашому тесті;
   - все одно не core temp; значення може бути статичним/застарілим (у нас не змінилося під навантаженням).
4. **`IOCTL_THERMAL_READ_TEMPERATURE`** — це IOCTL, який ACPI driver надсилає temperature sensor driver; він не є публічним user-mode інтерфейсом для читання core temp.

### 8.2 Intel DTS

Intel Digital Thermal Sensor:

- `IA32_THERM_STATUS` — MSR `0x19C` (per-core);
- `IA32_PACKAGE_THERM_STATUS` — MSR `0x1B1` (package);
- `MSR_TEMPERATURE_TARGET` — MSR `0x1A2` (TjMax offsets);
- digital readout = `TjMax - (MSR & 0x7F0000) >> 16` (спрощено; точні біти залежать від моделі).

MSR доступний лише в ring0. З user mode `rdmsr` неможливий; потрібен kernel driver, який виконує `__readmsr`/`HalGetBusData` або прямий `rdmsr`.

### 8.3 AMD Zen SMN

Для Zen (у т.ч. Renoir/Cezanne, 5600H) температура кристала читається через SMN (System Management Network), а не через CPUID:

- SMN index/data registers (PCI config indirect) або SMU mailbox;
- Linux `k10temp` використовує `F17H_M01H_THM_TCON_CUR_TMP = 0x00059800`, поле `CUR_TEMP` (bits 21..31), range select;
- доступ до SMN — ring0; з user mode потрібен driver (або vendor service).

`raw-cpuid` не допоможе: це CPUID, а не MSR/SMN.

### 8.4 Vendor WMI

Lenovo/ASUS/Dell/HP/Alienware мають власні WMI/EC класи:

- Lenovo — `root\WMI` vendor classes, Lenovo Vantage/service;
- ASUS — `ASUSWMI`/`AsusAtkWmi`, ATK service;
- Dell — `root\WMI` Dell classes, Dell Command/SupportAssist;
- HP — `root\WMI` HP BIOS/EC classes, HP service.

Проблеми: model-specific, залежить від vendor service/driver, часто потребує admin, немає єдиної схеми, значення можуть бути EC skin/board, не core. Для портативного продукту це неприйнятно як default.

### 8.5 Чому Microsoft не дає user-mode core temp

- MSR/SMN — привілейовані інструкції/простори; user mode не має доступу.
- ACPI thermal zones — це абстракція платформи (skin/EC/board), а не DTS core; Windows OSPM не гарантує, що `_TMP` відповідає core.
- `Win32_TemperatureProbe` офіційно не заповнюється; іншого офіційного WMI-класу для core temp немає.
- Kernel driver для MSR — це сторонній driver (WinRing0, InpOut32, LibreHardwareMonitorDriver), який:
  - потребує підпису/attestation;
  - часто блокується Memory Integrity/HVCI;
  - входить у Microsoft vulnerable driver blocklist (WinRing0 відомий);
  - збільшує attack surface і ризик BSOD.
- Тому «чистий user space + без драйвера» фізично не може дати core temp. Максимум — ACPI zone.

**Рекомендація:** у таскбарі показуйте CPU temp як `ACPI 48 °C` (з tooltip «thermal zone, не core»), або не показуйте взагалі. Якщо продукту потрібна core temp — це окремий optional компонент з підписаним driver, що суперечить default-констрейнту.

---

## 9. Rust crates: що брати

| Crate | Версія (2026-11-17) | Що дає | Обмеження | Стан |
|---|---:|---|---|---|
| `windows` | **0.62.2** | Типізовані Win32/WDK API: IP Helper, PDH, Power, Ioctl, DXGI, D3DKMT; `Result`, `PCWSTR`, feature-gated modules | Великий compile time; features треба вмикати точково; WDK-модулі окремі | Active, Microsoft |
| `windows-sys` | **0.61.2** | Сирі C-подібні bindings, менший/швидший за `windows` | Немає `Result`/RAII; більше unsafe; треба самому перевіряти status | Active, Microsoft |
| `sysinfo` | **0.40.4** | CPU/RAM/disks/networks/processes; Windows GPU API (нове); components | CPU — `% Idle Time`, не Utility; Components=0 non-admin; GPU usage None/0 на тесті; network includes vEthernet; disks — volumes; API churn | Active, GuillaumeGomez; Rust 1.95 |
| `nvml-wrapper` | **0.13.0** | NVIDIA: util, temp, memory, power, clocks, fan, PCIe, processes | NVIDIA only; GPU die temp only; dGPU D3cold/lost; driver-dependent | Active |
| `wmi` | **0.18.4** | COM/WMI queries: `Win32_OperatingSystem`, `MSFT_PhysicalDisk`, `MSAcpi_ThermalZoneTemperature` | Latency 5–50 ms; root\WMI часто admin; serde mapping; WMI service | Active |
| `raw-cpuid` | **11.6.0** | CPUID: vendor, brand, features, cache, topology | Не читає MSR/SMN; температур немає | Active |
| `winreg` | **0.56.0** | Registry: `~MHz`, CPU name, power settings | `~MHz` статичний base, не current; registry may be virtualized | Active |
| `windows-result` | **0.100.0** | `HRESULT`, `Result<T>`, error types для `windows` | Допоміжний, не сенсорний | Active |
| `libloading` | **0.9.1** | Динамічне завантаження `nvml.dll`, `amdadlx64.dll`, `ze_loader.dll` | Треба явно шукати DLL у System32, не CWD; vtable/ABI — на ваш ризик | Active |
| `heim` | **0.1.0-rc.1** (2020) | Колись cross-platform system info | Закинутий з 2020; не використовувати | Dead |
| `adlx` | **0.0.0-alpha.1** (2024) | AMD ADLX bindings | Alpha, закинутий; API неповний | Dead/alpha |
| `adlx-lite` | **1.0.0** (2026-10-23) | AMD ADLX bindings | Дуже малий adoption (39 downloads); невідома якість | New/low adoption |
| `amd-adlx` | **0.1.0** (SDK repo) | Офіційні ADLX Rust bindings + loader | Не на crates.io станом на 2026-11-17; треба vendor/path | Vendor SDK |
| `pdh` | — | — | **Немає crate на crates.io**; PDH реалізується через `windows`/`windows-sys` | N/A |
| `wmidev` | — | — | **Немає опублікованого crate**; не використовувати | N/A |
| `nvapi` | 0.1.4 (2022) | NVIDIA NVAPI bindings | Закинутий; NVML краще | Dead |

### 9.1 `windows` features, які реально потрібні

- `Win32_Foundation`
- `Win32_NetworkManagement_IpHelper`
- `Win32_NetworkManagement_Ndis`
- `Win32_Networking_WinSock`
- `Win32_System_Performance` (PDH)
- `Win32_System_Power` (`CallNtPowerInformation`)
- `Win32_System_Threading` (`GetSystemTimes`)
- `Win32_System_SystemInformation` (`GlobalMemoryStatusEx`, `MEMORYSTATUSEX`)
- `Win32_System_ProcessStatus` (`K32GetPerformanceInfo`)
- `Win32_System_Memory`
- `Win32_System_IO` (`DeviceIoControl`)
- `Win32_System_Ioctl` (`IOCTL_STORAGE_QUERY_PROPERTY`, `STORAGE_*`)
- `Win32_Storage_FileSystem` (`CreateFileW`, `FILE_SHARE_*`)
- `Win32_System_Registry` (optional)
- `Wdk_System_SystemInformation` (`NtQuerySystemInformation`)
- `Wdk_Graphics_Direct3D` (`D3DKMTOpenAdapterFromLuid`, `D3DKMTQueryAdapterInfo`)
- `Win32_Graphics_Dxgi` (LUID → adapter, VRAM budget)
- `Win32_Devices_DeviceAndDriverInstallation` (SetupAPI, PCI mapping; optional)
- `Win32_System_Com`, `Win32_System_Ole`, `Win32_System_Variant`, `Win32_System_Wmi` (if using WMI directly)

`windows-sys` has the same feature names but flat functions; choose it if compile time/binary size matters and you are comfortable with raw handles.

### 9.2 `sysinfo` 0.40.4 — конкретні обмеження

- `System::new_all() -> Result<System, Error>`; `Cpu::usage()` (не `cpu_usage()`); `MINIMUM_CPU_UPDATE_INTERVAL = 200 ms`.
- Global CPU — PDH `\Processor(_Total)\% Idle Time`; per-core — `\Processor(N)\% Idle Time`; **не** `% Processor Utility`.
- CPU frequency — `CallNtPowerInformation(ProcessorInformation).CurrentMhz`; на тесті 2011 MHz, base 3301.
- Components — `MSAcpi_ThermalZoneTemperature`; non-admin → 0 components (`Access denied`).
- Networks — `GetIfTable2`; includes `vEthernet (Default Switch)`; no elapsed time, no reset handling, no VPN classification.
- Disks — volumes, not physical disks; `DiskKind` HDD/SSD через `StorageDeviceSeekPenaltyProperty`; no temperature, no I/O counters.
- GPU — new 0.40 API; on test 2 adapters, `usage() = None`, `used_memory() = 0`; VRAM total only. Не production-ready для GPU telemetry.
- Memory — `used_memory() = total - available`, добре для Task Manager-подібного RAM.

---

## 10. Capability matrix

| Metric | Primary API | Admin? | Vendor-specific? | Accuracy / latency | Rust crate |
|---|---|---|---|---|---|
| Network RX/TX bytes/s | `GetIfEntry2` + `MIB_IF_ROW2.InOctets/OutOctets` | No | No | 64-bit absolute; 1 Hz; L2 bytes; reset guard | `windows` |
| Network RX/TX bytes/s | PDH `\Network Interface(*)\Bytes Received/sec`, `Bytes Sent/sec` | No | No | PDH rate; 1 Hz; localized names → `PdhAddEnglishCounterW` | `windows` |
| Active adapter / default route | `GetBestInterfaceEx`, `GetAdaptersAddresses` | No | No | Route lookup; ms | `windows` |
| CPU total % (Task Manager) | PDH `\Processor Information(_Total)\% Processor Utility` | No | No | Frequency-normalized; can >100 with turbo; ~1 Hz | `windows` |
| CPU total % (time-based) | `GetSystemTimes` | No | No | Sum all CPUs; kernel includes idle; <1 µs | `windows` |
| CPU per-core % | `NtQuerySystemInformation(SystemProcessorPerformanceInformation)` | No | No | 48 B/core; ~µs; semi-documented | `windows` / `windows-sys` |
| CPU per-core % | PDH `\Processor Information(0,N)\% Processor Utility` | No | No | 1 Hz; many counters | `windows` |
| CPU frequency | `CallNtPowerInformation(ProcessorInformation)` → `CurrentMhz` | No | No | Quantized P-state; boost not visible; 0 possible | `windows` / `sysinfo` |
| CPU frequency | PDH `Processor Frequency` / `% Processor Performance` | No | No | 1 Hz; ratio to nominal | `windows` |
| CPU base MHz | Registry `HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0\~MHz` | No | No | Static base; not current | `winreg` |
| CPU core temp (Intel DTS) | MSR `0x19C`/`0x1B1` + `0x1A2` | **Yes (ring0)** | Intel | Driver-only; no official user API | none / vendor driver |
| CPU core temp (AMD Zen) | SMN `0x00059800` (`THM_TCON_CUR_TMP`) | **Yes (ring0)** | AMD | Driver-only; no official user API | none / vendor driver |
| CPU/ACPI temp | `MSAcpi_ThermalZoneTemperature` | Admin in practice | No | Tenths K; zone not core; often unavailable | `wmi` |
| ACPI temp (non-admin) | `Win32_PerfFormattedData_Counters_ThermalZoneInformation`, PDH `\Thermal Zone Information(*)\High Precision Temperature` | No (observed) | No | Tenths K; ACPI zone; may be stale | `windows` / `wmi` |
| RAM total/available | `GlobalMemoryStatusEx` | No | No | <1 µs; avail includes standby | `windows` / `sysinfo` |
| RAM committed | `GetPerformanceInfo` (`CommitTotal`, `CommitLimit`, `PageSize`) | No | No | ~µs; Task Manager committed | `windows` |
| RAM via WMI | `Win32_OperatingSystem` | No | No | 5–50 ms; KB units; WMI | `wmi` |
| GPU NVIDIA util/temp/VRAM/power | NVML (`nvmlDeviceGet*`) | No | NVIDIA | 10–100 ms init; 1 Hz; die temp only; dGPU sleep | `nvml-wrapper` |
| GPU AMD util/temp/clocks/power | ADLX (`IADLXGPUMetrics`) | No | AMD | Init ~100 ms; ms queries; APU support variable | `amd-adlx` (vendor), `adlx-lite` |
| GPU Intel util/temp | Level Zero Sysman (`zesTemperatureGetState`, `zesEngineGetActivity`) | No | Intel | Driver/loader; iGPU temp often unsupported | raw FFI |
| GPU util vendor-neutral | PDH `\GPU Engine(*)\Utilization Percentage` | No | No | 1 Hz; 100s instances; aggregate by LUID/engine; sum >100 | `windows` / `sysinfo` |
| GPU VRAM | PDH `\GPU Adapter Memory(*)\Dedicated Usage`; DXGI `QueryVideoMemoryInfo` | No | No | 1 Hz; dedicated/shared; LUID mapping | `windows` |
| GPU temp vendor-neutral | `D3DKMTQueryAdapterInfo(KMTQAITYPE_ADAPTERPERFDATA)` → `D3DKMT_ADAPTER_PERFDATA.Temperature` | No | KMD-dependent | Deci-C; often 0/unsupported; fragile | `windows` (Wdk) |
| Disk inventory/type | `IOCTL_STORAGE_QUERY_PROPERTY(StorageDeviceProperty)` | No | No | Fast; BusType 11/17/7 | `windows` |
| Disk capacity/usage | `GetDiskFreeSpaceEx`, `GetVolumeInformation` | No | No | Fast; per volume | `windows` / `sysinfo` |
| Disk activity | PDH `\PhysicalDisk(*)\% Disk Time`, `Disk Bytes/sec` | No | No | 1 Hz; `% Disk Time` can >100; use idle | `windows` |
| NVMe temp | `StorageDeviceTemperatureProperty` → `STORAGE_TEMPERATURE_DATA_DESCRIPTOR` | No (observed) | No | Celsius; composite + sensors; fast | `windows` |
| NVMe temp/SMART | NVMe log page 0x02 via `StorageDeviceProtocolSpecificProperty` | No (observed) | No | Kelvin; 512 B; percentage used | `windows` |
| SATA SSD/HDD temp | ATA PASS-THROUGH SMART 0xB0 / `SMART_RCV_DRIVE_DATA` | **Yes** | No | 512 B SMART; attr 194/190; USB/RAID fail | `windows` |
| SATA/NVMe temp | `Get-StorageReliabilityCounter` → `MSFT_StorageReliabilityCounter.Temperature` | Admin in practice | No | UInt8 °C; provider-dependent; slow | `wmi` |
| Disk health/wear | `MSFT_StorageReliabilityCounter` (`Wear`, `PowerOnHours`) | Admin in practice | No | Provider-dependent | `wmi` |

---

## 11. Рекомендована архітектура

### 11.1 Один non-admin процес, 1 Hz

- **Tick 1 s:** мережа, CPU load, RAM, GPU util/VRAM, disk activity.
- **Tick 2–5 s:** NVMe/HDD temperature, GPU temperature (NVML/ADLX), CPU frequency, ACPI zone.
- **Tick 30–60 s:** inventory (adapters, disks, GPUs), health/wear.

Один PDH query на CPU/network/disk/GPU; але GPU wildcard з 400+ instances краще окремим query, щоб не гальмувати основний tick. `PdhCollectQueryData` один раз на tick для всіх counters.

### 11.2 Startup / hotplug

1. Enumerate adapters (`GetIfTable2`), disks (`PhysicalDrive0..N` + `IOCTL_STORAGE_QUERY_PROPERTY`), GPUs (DXGI + NVML/ADLX presence).
2. Select active network adapter by default route/physical flags; store LUID.
3. Select disk by volume/disk number; map `C:`/`E:`/`F:` to physical disk.
4. On `GetIfEntry2` failure / disk IOCTL failure → re-enumerate (adapter disabled, USB removed, sleep/resume).
5. On resume, reset all deltas (counters may reset).

### 11.3 GPU strategy

- If `nvml.dll` exists and NVIDIA adapter present → NVML for NVIDIA.
- If `amdadlx64.dll` exists and AMD adapter present → ADLX for AMD.
- Always run WDDM PDH as fallback/cross-check (util/VRAM).
- If NVML returns `GpuLost` → mark dGPU asleep, retry after 30 s, don't busy-loop.
- For hybrid laptop, show two GPU rows (iGPU + dGPU) rather than one combined number; otherwise user can't tell which GPU is loaded.

### 11.4 Storage strategy

- NVMe: `StorageDeviceTemperatureProperty` first; if fails, NVMe log page 0x02.
- SATA: no non-admin temp; show `—`/`requires admin`; optional elevated helper.
- Disk activity: PDH `PhysicalDisk` by disk number; display bytes/s and queue; `% Disk Time` clamp.
- HDD vs SSD: `STORAGE_DEVICE_DESCRIPTOR.BusType`/`MSFT_PhysicalDisk.MediaType`/`SpindleSpeed`; `sysinfo::DiskKind` for volumes.

### 11.5 CPU temp strategy

- Default: ACPI zone via `Win32_PerfFormattedData_Counters_ThermalZoneInformation` or PDH thermal counter, label `ACPI`, update 5 s; if unavailable, `—`.
- Do not label it `CPU core`; do not color it red/green as if core.
- If product later allows optional driver, expose core temp as separate opt-in metric.

### 11.6 Security / DLL loading

- Load vendor DLLs from `%SystemRoot%\System32` explicitly: `LoadLibraryExW(path, NULL, LOAD_LIBRARY_SEARCH_SYSTEM32)`; do not rely on CWD/PATH (DLL hijack).
- Verify `nvml.dll`/`amdadlx64.dll` existence and architecture (x64).
- Treat vendor API failures as expected: missing DLL, old driver, dGPU asleep, unsupported metric.
- Do not run the UI process elevated; if elevated helper is used, communicate via a narrow IPC (named pipe) and validate input.

### 11.7 Taskbar UI notes

- Windows 11 taskbar is XAML; no public API for adding a native widget. Use a borderless topmost window positioned over the taskbar, or a tray icon with flyout.
- `SHAppBarMessage(ABM_NEW/ABM_SETPOS)` can reserve space, but Windows 11 taskbar may ignore/overlap; test on 26H2.
- Per-monitor DPI: use `GetDpiForWindow`/`WM_DPICHANGED`; taskbar height changes with scaling.
- Don't block the UI thread on PDH/WMI/NVML; use a worker thread and `Arc<Mutex<Snapshot>>` or channel.
- 1 Hz repaint is enough; avoid 60 Hz animations for a taskbar widget.

---

## 12. Висновки по кожному пункту

1. **Мережа:** `GetIfEntry2` — найкращий non-admin primary; PDH — зручний fallback. `sysinfo` — прототип, але включає virtual adapters і не рахує rate. Task Manager/Taskbar не дають публічного API; Task Manager per-adapter, 1 Hz.
2. **CPU load:** `% Processor Utility` — Task Manager-семантика; `GetSystemTimes` — простіше; `NtQuerySystemInformation` — per-core. `sysinfo` використовує `% Idle Time`, тому не Task Manager. Частота — `CallNtPowerInformation.CurrentMhz`, але boost не гарантований.
3. **RAM:** `GlobalMemoryStatusEx` + `GetPerformanceInfo`; physical in-use vs committed — різні речі; Task Manager показує обидва.
4. **GPU:** NVML/ADLX дають temp і util, WDDM — тільки util/VRAM. WDDM/PDH не мають temperature; D3DKMT perf data існує, але unreliable. Laptop dGPU може спати.
5. **Storage:** NVMe temp — non-admin через `StorageDeviceTemperatureProperty` (41 °C); SATA/HDD temp — admin SMART; `Get-StorageReliabilityCounter` — admin у практиці; USB/RAID — часто fail. Disk activity — PDH.
6. **CPU temp:** офіційного user-mode core temp немає; ACPI zone — максимум без driver; Intel DTS/AMD SMN — ring0; vendor WMI — model-specific. `MSAcpi` — admin, tenths K; ACPI perf counter — non-admin, але не core.
7. **Crates:** `windows` 0.62.2 + `nvml-wrapper` 0.13.0 + `wmi` 0.18.4 + `sysinfo` 0.40.4 (обережно) + `libloading` 0.9.1; `pdh`/`wmidev` crates відсутні; `heim`/`adlx` закинуті.
8. **Capability matrix:** див. розділ 10.

---

## Sources

### Microsoft Learn / Win32

1. `GetIfTable2` — https://learn.microsoft.com/en-us/windows/win32/api/netioapi/nf-netioapi-getiftable2
2. `MIB_IF_ROW2` — https://learn.microsoft.com/en-us/windows/win32/api/netioapi/ns-netioapi-mib_if_row2
3. `GetIfEntry2` — https://learn.microsoft.com/en-us/windows/win32/api/netioapi/nf-netioapi-getifentry2
4. `GetBestInterfaceEx` — https://learn.microsoft.com/en-us/windows/win32/api/netioapi/nf-netioapi-getbestinterfaceex
5. `GetAdaptersAddresses` — https://learn.microsoft.com/en-us/windows/win32/api/iphlpapi/nf-iphlpapi-getadaptersaddresses
6. Network-Related Performance Counters — https://learn.microsoft.com/en-us/windows-server/networking/technologies/network-subsystem/net-sub-performance-counters
7. `GetSystemTimes` — https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getsystemtimes
8. `NtQuerySystemInformation` — https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntquerysysteminformation
9. `PROCESSOR_POWER_INFORMATION` — https://learn.microsoft.com/en-us/windows/win32/power/processor-power-information-str
10. `CallNtPowerInformation` — https://learn.microsoft.com/en-us/windows/win32/api/powerbase/nf-powerbase-callntpowerinformation
11. CPU usage over 100% / `% Processor Utility` — https://learn.microsoft.com/en-us/troubleshoot/windows-client/performance/cpu-usage-exceeds-100
12. `GlobalMemoryStatusEx` — https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-globalmemorystatusex
13. `MEMORYSTATUSEX` — https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/ns-sysinfoapi-memorystatusex
14. `GetPerformanceInfo` — https://learn.microsoft.com/en-us/windows/win32/api/psapi/nf-psapi-getperformanceinfo
15. `PERFORMANCE_INFORMATION` — https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-performance_information
16. `PdhAddEnglishCounterW` — https://learn.microsoft.com/en-us/windows/win32/api/pdh/nf-pdh-pdhaddenglishcounterw
17. `PdhCollectQueryData` — https://learn.microsoft.com/en-us/windows/win32/api/pdh/nf-pdh-pdhcollectquerydata
18. `PdhGetFormattedCounterValue` — https://learn.microsoft.com/en-us/windows/win32/api/pdh/nf-pdh-pdhgetformattedcountervalue
19. `PdhGetFormattedCounterArrayW` — https://learn.microsoft.com/en-us/windows/win32/api/pdh/nf-pdh-pdhgetformattedcounterarrayw
20. `PDH_FMT_COUNTERVALUE` — https://learn.microsoft.com/en-us/windows/win32/api/pdh/ns-pdh-pdh_fmt_countervalue
21. `PDH_FMT_COUNTERVALUE_ITEM_W` — https://learn.microsoft.com/en-us/windows/win32/api/pdh/ns-pdh-pdh_fmt_countervalue_item_w
22. Using PDH functions — https://learn.microsoft.com/en-us/windows/win32/perfctrs/using-the-pdh-functions-to-consume-counter-data
23. `IOCTL_STORAGE_QUERY_PROPERTY` — https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-ioctl_storage_query_property
24. `STORAGE_TEMPERATURE_DATA_DESCRIPTOR` — https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-storage_temperature_data_descriptor
25. `STORAGE_TEMPERATURE_INFO` — https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-storage_temperature_info
26. `STORAGE_PROPERTY_ID` — https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ne-winioctl-storage_property_id
27. Working with NVMe Drives — https://learn.microsoft.com/en-us/windows/win32/fileio/working-with-nvme-devices
28. `STORAGE_DEVICE_DESCRIPTOR` — https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-storage_device_descriptor
29. `ATA_PASS_THROUGH_EX` — https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntddscsi/ns-ntddscsi-_ata_pass_through_ex
30. `MSFT_StorageReliabilityCounter` — https://learn.microsoft.com/en-us/windows-hardware/drivers/storage/msft-storagereliabilitycounter
31. `Get-StorageReliabilityCounter` — https://learn.microsoft.com/en-us/powershell/module/storage/get-storagereliabilitycounter
32. `MSFT_PhysicalDisk` — https://learn.microsoft.com/en-us/windows-hardware/drivers/storage/msft-physicaldisk
33. `Win32_TemperatureProbe` — https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/win32-temperatureprobe
34. `IOCTL_THERMAL_READ_TEMPERATURE` (kernel-to-driver) — https://learn.microsoft.com/en-us/windows-hardware/design/device-experiences/ioctl-thermal-read-temperature
35. `KMTQUERYADAPTERINFOTYPE` — https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ne-d3dkmthk-_kmtqueryadapterinfotype
36. `D3DKMT_ADAPTER_PERFDATA` — https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_adapter_perfdata
37. `DXGK_ADAPTER_PERFDATACAPS` — https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmdt/ns-d3dkmdt-_dxgk_adapter_perfdatacaps
38. `D3DKMTQueryAdapterInfo` — https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtqueryadapterinfo
39. `IDXGIAdapter3::QueryVideoMemoryInfo` — https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_4/nf-dxgi1_4-idxgiadapter3-queryvideomemoryinfo
40. Windows 11 release information (26H2, build 26300) — https://learn.microsoft.com/en-us/windows/release-health/windows11-release-information
41. Microsoft vulnerable driver blocklist — https://learn.microsoft.com/en-us/windows/security/application-security/application-control/app-control-for-business/design/microsoft-recommended-driver-block-rules
42. `SHAppBarMessage` — https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shappbarmessage

### NVIDIA / AMD / Intel

43. NVML API documentation — https://docs.nvidia.com/deploy/nvml-api/latest/index.html
44. NVML `nvmlDeviceGetTemperature` / device queries — https://docs.nvidia.com/deploy/nvml-api/latest/index.html
45. AMD ADLX SDK — https://gpuopen.com/adlx/
46. ADLX GitHub repository — https://github.com/GPUOpen-LibrariesAndSDKs/ADLX
47. ADLX `IPerformanceMonitoring.h` — https://github.com/GPUOpen-LibrariesAndSDKs/ADLX/blob/main/SDK/Include/IPerformanceMonitoring.h
48. ADLX Rust sample — https://github.com/GPUOpen-LibrariesAndSDKs/ADLX/tree/main/Samples/rust
49. Intel oneAPI Level Zero — https://github.com/oneapi-src/level-zero
50. Linux `k10temp.c` (AMD SMN `THM_TCON_CUR_TMP`) — https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/drivers/hwmon/k10temp.c
51. Intel SDM (MSR 0x19C/0x1B1/0x1A2) — https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html

### Rust crates / docs

52. `sysinfo` 0.40.4 docs — https://docs.rs/sysinfo/0.40.4/sysinfo/
53. `sysinfo::NetworkData` — https://docs.rs/sysinfo/0.40.4/sysinfo/struct.NetworkData.html
54. `sysinfo::Gpu` — https://docs.rs/sysinfo/0.40.4/sysinfo/struct.Gpu.html
55. `nvml-wrapper` 0.13.0 — https://docs.rs/nvml-wrapper/0.13.0/nvml_wrapper/
56. `nvml-wrapper` `Device` — https://docs.rs/nvml-wrapper/0.13.0/nvml_wrapper/device/struct.Device.html
57. `wmi` 0.18.4 — https://docs.rs/wmi/0.18.4/wmi/
58. `raw-cpuid` 11.6.0 — https://docs.rs/raw-cpuid/11.6.0/raw_cpuid/
59. `winreg` 0.56.0 — https://docs.rs/winreg/0.56.0/winreg/
60. `windows-result` 0.100.0 — https://docs.rs/windows-result/0.100.0/windows_result/
61. `heim` 0.1.0-rc.1 — https://docs.rs/heim/0.1.0-rc.1/heim/
62. `windows` 0.62.2 docs — https://microsoft.github.io/windows-docs-rs/doc/windows/
63. `PdhAddEnglishCounterW` (windows crate) — https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/System/Performance/fn.PdhAddEnglishCounterW.html
64. `GetIfTable2` (windows crate) — https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/NetworkManagement/IpHelper/fn.GetIfTable2.html
65. `NtQuerySystemInformation` (windows crate) — https://microsoft.github.io/windows-docs-rs/doc/windows/Wdk/System/SystemInformation/fn.NtQuerySystemInformation.html
66. `IOCTL_STORAGE_QUERY_PROPERTY` (windows crate) — https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/System/Ioctl/constant.IOCTL_STORAGE_QUERY_PROPERTY.html

---

### Примітка про метод

Версії crate-ів і дати оновлення перевірені через crates.io API 2026-11-17; API-деталі — через Microsoft Learn, docs.rs, microsoft.github.io/windows-docs-rs, NVIDIA NVML docs, GPUOpen ADLX і локальні Windows SDK headers (`winioctl.h`, `ntddscsi.h`, `ntdddisk.h`, `nvme.h`, `winnt.h`, build 10.0.26100.0). Локальні виміри — на зазначеному HP laptop, non-admin, Windows 11 26H2 build 26300; вони ілюстративні, не є benchmark-ом.
