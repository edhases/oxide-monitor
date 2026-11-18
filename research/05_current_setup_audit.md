# 05. Аудит поточної збірки TrafficMonitor (`E:\TrafficMonitor`)

**Дата аудиту:** 17.11.2026  
**Середовище:** Windows 11 Pro, build **26300** (x64), AMD Ryzen 5 5600H (6C/12T, Zen 3 APU), NVIDIA GTX 1650 Laptop + AMD Radeon iGPU, 32 GB RAM, NVMe Apacer AS2280P4U 1 TB + HDD HGST HTS725050A7E630, 1920×1080, taskbar height 53 px, DPI 106.

> Усі висновки нижче зроблені з файлів і живого процесу на цій машині. Там, де висновок є припущенням, це позначено явно.

---

## 1. Короткий підсумок (TL;DR)

1. Це **кастомна збірка TrafficMonitor 1.86 x64** (C++/MFC, компіляція 29.03.2026), у якій штатний міст `OpenHardwareMonitorApi.dll` перезібраний під **LibreHardwareMonitorLib 0.9.6**, а сенсори температури читаються через **PawnIO 2.1.0** (окремий kernel-драйвер, встановлений у систему).
2. `TrafficMonitor.exe` має маніфест **`requireAdministrator`**. Через це весь UI, рендеринг taskbar-віджета й опитування сенсорів працюють в одному **elevated-процесі**. Автозапуск зроблено через **Task Scheduler** (`Autorun for proed`), а не через `HKCU\...\Run`.
3. У поточному конфігу ввімкнено **всі 4 групи hardware monitoring** (`hardware_monitor_item = 15`: CPU/GPU/HDD/MBD) і **8 елементів** у taskbar (`tbar_display_item = 255`): up, down, CPU %, MEM %, GPU %, CPU temp, GPU temp, HDD temp. Вікно — **два рядки** по 4 елементи, ~294×35 px, одразу ліворуч від системного трею.
4. Taskbar-віджет на Win11 реалізовано не через DeskBand, а через **`SetParent(hwnd, Shell_TrayWnd)`**: layered popup-вікно класу `#32770` стає «дитиною» taskbar, позиціонується від `TrayNotifyWnd`/`Start` і малюється GDI (у конфігу `disable_d2d = true`). На цій самій збірці Win11 26300 це працює.
5. **Мережа, CPU load/freq, RAM, GPU load, disk activity і NVMe-температура вже читаються без kernel-драйвера**: `GetIfTable`, `GetSystemTimes`/PDH, `GlobalMemoryStatusEx`, PDH `\GPU Engine(*)\Utilization Percentage`, PDH `\PhysicalDisk(*)\% Idle Time`, `IOCTL_STORAGE_QUERY_PROPERTY` + `StorageDeviceTemperatureProperty`. PawnIO/LHM потрібні для **температури CPU, материнської плати, SATA SMART і частини GPU/CPU freq**.
6. Головні проблеми поточної архітектури:
   - elevated UI + завантаження **непідписаних DLL** з каталогу, який **може писати будь-який authenticated user** (`E:\TrafficMonitor` → `Authenticated Users:(M)`) → локальний шлях підвищення привілеїв;
   - LHM працює **in-process** (C++/CLI + .NET Framework), тому будь-який збій у .NET/драйвері валить весь монітор (у лозі є один повний crash dump від 30.10.2026 з кадрами `clr.dll`/`RaiseException`);
   - висока для такого застосунку ціна: **~7.2 % одного ядра** (≈0.6 % усіх 12 потоків) і **40 MB working set / 105 MB private** у стані спокою;
   - Win11-вбудовування спирається на незадокументовану ієрархію вікон Explorer і ламається при змінах taskbar (центровані іконки, widgets, secondary taskbars);
   - DPI awareness лише `dpiAware=true` (system DPI), не Per-Monitor V2.
7. Для нового Rust-застосунку доцільно **зберегти функціональний паритет UI/конфігу**, але змінити архітектуру: **unelevated UI + окремий опціональний sensor-helper** (або читання shared memory у вже запущеного WHQL-монітора), **process isolation**, **Per-Monitor V2**, **Direct2D/DirectWrite**, **плагінна система без C++ ABI**.

---

## 2. Інвентаризація каталогу

### 2.1. Виконувані файли та ключові бібліотеки

| Файл | Версія | Підпис | Розмір | Роль |
|---|---:|---|---:|---|
| `TrafficMonitor.exe` | 1.8.6.0 | **NotSigned** | 1 934 336 B | Основний GUI (C++/MFC, x64). Маніфест: `requireAdministrator`, `dpiAware=true`. |
| `OpenHardwareMonitorApi.dll` | 1.0.0.3 | **NotSigned** | 271 872 B | C++/CLI mixed-mode міст до `LibreHardwareMonitorLib.dll`; імпортує `mscoree.dll`. |
| `LibreHardwareMonitorLib.dll` | 0.9.6.0 | **NotSigned** | 1 203 200 B | Керована бібліотека сенсорів; всередині — PawnIO-модулі (`AMDFamily17.bin`, `RyzenSMU.bin`, `IntelMSR.bin`, `Smbus*.bin`). |
| `LibreHardwareMonitor.exe` | 0.9.6.0 | **NotSigned** | 4 458 496 B | Standalone GUI LHM; у процесах **не запущений**. Використовується, ймовірно, лише для діагностики. |
| `LibreHardwareMonitor.config` | — | — | 29 279 B | Конфіг standalone LHM; зокрема `listenerPort=8085`, `authenticationEnabled=false`. |
| `PawnIO.sys` (system-wide) | 2.1.0 | **WHQL** (Microsoft Windows Hardware Compatibility Publisher) | 136 496 B | Kernel-драйвер у `DriverStore`; служба `PawnIO` — Manual, зараз Running. |
| `PawnIOLib.dll` (system-wide) | 2.1.0 | Authenticode namazso.eu (GLOBALTRUST) | 39 248 B | User-mode API до драйвера (`pawnio_open/load/execute`). |
| `Aga.Controls.dll` | 1.7.0.0 | NotSigned | 145 920 B | TreeView-контрол для діалогів. |
| `OxyPlot.dll`, `OxyPlot.WindowsForms.dll` | 2.2.0.0 | NotSigned | 700 928 / 32 768 B | Графіки історії трафіку. |
| `Microsoft.Win32.TaskScheduler.dll` | 2.12.2.0 | NotSigned | 333 312 B | Створення scheduled task для автозапуску. |
| `HidSharp.dll` | 2.6.4.0 | Valid (Illusory Studios) | 262 792 B | HID-доступ (частина LHM-залежностей). |
| `RAMSPDToolkit-NDD.dll` | 1.4.2 | NotSigned | 233 472 B | SPD/DDR-сенсори LHM. |
| `DiskInfoToolkit.dll` | 1.1.2 | NotSigned | 903 680 B | Дискова інформація (LHM). |
| `BlackSharp.Core.dll` | 1.0.7 | NotSigned | 32 768 B | Допоміжна .NET-бібліотека. |

Інші 28 DLL — переважно .NET-залежності (`System.*`, `Microsoft.Bcl.*`) і MFC/CRT runtime. Повний список — у Додатку A.

**SHA-256 (для відтворюваності):**

| Файл | SHA-256 |
|---|---|
| `TrafficMonitor.exe` | `22CAD4F87C7B063BFE8651CCA88FA51BF9B172F53A3F988065ABB0BA4AE4965C` |
| `OpenHardwareMonitorApi.dll` | `E82D15A2320BEEA8BAE3CB9EDECB2D40B40FEF32B2E52EB26C9CD56F09ACC28E` |
| `LibreHardwareMonitorLib.dll` | `6EBC194316536BA61AF5BE24508AD9FCBB2ECC685E716C12E787C79530F66BF0` |
| `PawnIO.sys` (2.1.0) | `F92DE04E5A02256E86FFA1B4252FE669282198B7329227F40E51BCA9267D133D` |

### 2.2. Ресурси

- **13 скінів** у `skins\` (BMP/PNG + `skin.ini` або `skin.xml`), включно з двома варіантами (light/dark) для кожного; підтримуються `layout_l`/`layout_s` (повний/компактний), `preview_*`, per-item кольори, `text_color`, `font_name/size/style`, `no_text`.
- **11 мов** у `language\` (English, German, Hebrew, Hungarian, Italian, Polish, Portuguese_Brazilian, Russian, Simplified/Traditional Chinese, Turkish). Поточна мова — **1049 (російська)**.
- `history_traffic.dat` (3 477 B) + `.bak` — власний бінарний формат історії трафіку по днях; `error.log` (44 KB) — журнал подій/збоїв.
- Каталогу `plugins\` **немає**, зовнішні плагіни не встановлені (`plugin_disabled` порожній). Але plugin API v8 присутній у коді й може бути критичним для міграції.

### 2.3. Маніфест і залежності

`TrafficMonitor.exe` (PE32+ x64, subsystem GUI) імпортує:

| DLL | Що це дає |
|---|---|
| `OpenHardwareMonitorApi.dll` | Єдина точка входу до LHM (2 функції). |
| `pdh.dll` | CPU/GPU/disk performance counters. |
| `IPHLPAPI.DLL` | `GetIfTable` для швидкості мережі. |
| `d2d1.dll`, `DWrite.dll`, `dxgi.dll`, `d3d10_1.dll` | Опціональний Direct2D/DirectComposition рендеринг. |
| `dwmapi.dll` | DWM-ефекти/транспарентність. |
| `gdiplus.dll` | PNG-скіни, іконки. |
| `mfc140u.dll`, `MSVCP140.dll`, `VCRUNTIME140*.dll` | MFC/CRT. |
| `dbghelp.dll` | Мінідамп при збої. |
| `POWRPROF.dll` (delay-load) | Живлення/енергозбереження. |

Маніфест (повністю):

```xml
<requestedExecutionLevel level="requireAdministrator" uiAccess="false"/>
<dpiAware>true</dpiAware>
```

Тобто застосунок **завжди** стартує з UAC-підвищенням, а DPI awareness — лише system-level.

---

## 3. Модель виконання та безпека

### 3.1. Один elevated-процес на все

Схема поточної збірки:

```
Task Scheduler ("Autorun for proed")
        │  (запуск від імені адміністратора, без вікна)
        ▼
TrafficMonitor.exe  (elevated, MFC, x64)
   ├─ UI: main window + taskbar child window (GDI/D2D)
   ├─ network/CPU/RAM/GPU-load/disk-load: PDH + GetIfTable + GlobalMemoryStatusEx
   ├─ OpenHardwareMonitorApi.dll (C++/CLI, in-process)
   │      └─ LibreHardwareMonitorLib.dll (.NET Framework 4.x)
   │             └─ PawnIOLib.dll → \\.\PawnIO → PawnIO.sys (WHQL)
   └─ history_traffic.dat, config.ini, error.log
```

Наслідки:

- **Будь-який збій** у C++/CLI, .NET або PawnIO-модулі завершує весь монітор. У лозі є один повний мінідамп (`2026/10/30 18:20:03`) із кадрами `KERNELBASE!RaiseException` → `clr.dll`; поруч — стандартна порада автора «вимкніть hardware monitoring і плагіни». Це підтверджує ризик in-process сенсорів.
- **Elevated UI** означає, що drag-and-drop з Explorer, взаємодія з іншими застосунками та UIPI-сценарії працюють з обмеженнями; будь-яка вразливість у рендерингу/парсингу скінів стає admin-вразливістю.
- **Автозапуск через Task Scheduler** обрано саме тому, що звичайний `Run`-ключ не працює для `requireAdministrator`. Це створює задачу з найвищими привілеями, яку користувач не бачить у звичайному списку автозапуску.

### 3.2. Критична знахідка: writable app directory + unsigned DLL

```
E:\TrafficMonitor  BUILTIN\Administrators:(F)
                   NT AUTHORITY\SYSTEM:(F)
                   NT AUTHORITY\Authenticated Users:(M)   ← Modify
                   BUILTIN\Users:(RX)
```

`TrafficMonitor.exe` вимагає admin і завантажує `OpenHardwareMonitorApi.dll` **за іменем** зі свого каталогу (стандартний DLL search order: application directory перший). Будь-який authenticated user, який може писати в `E:\TrafficMonitor`, може підмінити DLL і дочекатися, поки адміністратор запустить монітор → **local privilege escalation**. Це не гіпотетична вразливість конкретної версії, а наслідок архітектурного рішення «elevated GUI + DLL поруч із записуваним конфігом».

Для нового застосунку це неприйнятно: UI має бути **unelevated**, сенсорний helper — **окремим процесом** із вузьким IPC-контрактом, а всі DLL — або підписані, або завантажуватися з захищеного каталогу (`Program Files`) з перевіркою хешу/підпису.

### 3.3. PawnIO: що саме встановлено

- Служба: `PawnIO`, `Start = 3 (Manual)`, `Type = 1 (kernel driver)`, шлях — `C:\Windows\System32\DriverStore\FileRepository\pawnio.inf_amd64_a92d2610b0f4c4c8\PawnIO.sys`.
- **Драйвер WHQL-підписаний** (Microsoft Windows Hardware Compatibility Publisher; сертифікат підпису має NotAfter 14.07.2026, але підпис timestamped, тому `Get-AuthenticodeSignature` повертає `Valid`). Це важливо: поширене твердження «PawnIO — самопідписаний драйвер» для версії 2.1.0 на цій машині **не відповідає дійсності**. Самопідпис може стосуватися старих версій або `.dll`/інсталятора; user-mode `PawnIOLib.dll` має звичайний Authenticode від namazso.eu.
- У локальному `C:\Windows\System32\CodeIntegrity\driversipolicy.p7b` (оновлено 13.11.2026) серед імен драйверів присутні `WinRing0.sys`, `HWiNFO64A.SYS`, `HWiNFO64I.SYS`, `HwRwDrv.sys`, `cpuz.sys`, `gdrv.sys`, `MSI`-драйвери, але **`PawnIO.sys` не знайдено**. Це не гарантія «дозволеності» для античитів — лише факт, що Microsoft-блокування драйверів його наразі не покриває.
- Модель PawnIO: kernel-драйвер надає IOCTL-інтерфейс, а вся логіка доступу до MSR/PCI/SMBus виконується **підписаними bytecode-модулями** (`AMDFamily17.bin`, `RyzenSMU.bin`, `IntelMSR.bin`, `Smbus*.bin`), які LHM вбудовує як ресурси. Це суттєво безпечніше за WinRing0 (немає довільного `MmMapIoSpace`/`WRMSR` з user-mode), але це все одно **загальний kernel-примітив**, який античити можуть блокувати за назвою/сертифікатом/хешем.

### 3.4. Інші спостереження безпеки

- `LibreHardwareMonitor.config` містить `listenerPort=8085`, `authenticationEnabled=false`, `listenerIp="?"`. Якщо користувач запускає standalone `LibreHardwareMonitor.exe`, його HTTP-сервер віддає сенсори **без автентифікації** (за замовчуванням — на всі інтерфейси). Зараз процес не запущений і порт 8085 закритий, але це типовий приклад небезпечного дефолту, який новий застосунок не повинен повторювати.
- `config.ini` лежить у тому ж writable-каталозі; цілісність конфігу не захищена. Для нового застосунку — `%LOCALAPPDATA%` або `%APPDATA%` з ACL поточного користувача, atomic write, backup, версіонування схеми.
- `error.log` пише локалізовані повідомлення в системній кодувальній сторінці (тут CP1251) — частина рядків перетворюється на `?`. Для нового застосунку — UTF-8 + структурований лог.

---

## 4. Звідки беруться метрики

### 4.1. Карта джерел даних

| Метрика | API / джерело | Реалізація в TrafficMonitor | Потрібен admin? |
|---|---|---|---|
| Upload/Download | `GetIfTable` (IPHLPAPI) | `CTrafficMonitorDlg::OnTimer`, дельта `dwInOctets/dwOutOctets` за `monitor_time_span` (1000 ms) | Ні |
| CPU % | `GetSystemTimes` або PDH `\Processor Information(_Total)\% Processor Utility` | `CPdhCPUUsage` / `CCPUUsage`; вибір у налаштуваннях (`cpu_usage_acquire_method = 1`) | Ні* |
| CPU freq | PDH `\Processor Information(_Total)\% Processor Performance` × `MaxMhz` з `CallNtPowerInformation(ProcessorInformation)` | `CPdhCpuFreq` | Ні* |
| RAM | `GlobalMemoryStatusEx` | `dwMemoryLoad`, `ullTotalPhys - ullAvailPhys` | Ні |
| GPU usage | PDH `\GPU Engine(*)\Utilization Percentage` | `CPdhGPUUsage`: сума по `engtype`, потім **max** (як Task Manager) | Ні* |
| Disk activity | PDH `\PhysicalDisk(*)\% Idle Time` | `CPdhDiskUsage`: `100 - idle`, з clamp для NVMe (idle може бути >100 % через черги) | Ні* |
| CPU temp | LHM: усі `SensorType.Temperature` вузла CPU, середнє або вибране ядро | `GetCpuTemperature`; у конфігу вибрано `Средняя температура` | **Так** |
| CPU temp (ACPI best-effort) | Perf-counter `Win32_PerfFormattedData_Counters_ThermalZoneInformation` (`\_TZ.THRM`) | На цій машині без admin: `3202–3212` деци-Kelvin = **~47–48 °C**; `MSAcpi_ThermalZoneTemperature` — `Access denied` без admin | **Ні** (perf-counter) |
| GPU temp | LHM: `GPU Core` для NVIDIA/AMD/Intel; fallback — середнє | `GetHardwareTemperature` | **Так** |
| NVMe SSD temp | `IOCTL_STORAGE_QUERY_PROPERTY` + `StorageDeviceTemperatureProperty` (52) | На цій машині Apacer: **41 °C**, 3 sensors, без admin | **Ні** |
| SATA HDD/SSD temp | LHM / `IOCTL_ATA_PASS_THROUGH` + SMART | На цій машині HGST: `StorageDeviceTemperatureProperty` → `ERROR_INVALID_FUNCTION`; ATA SMART → `Access denied` без admin | **Так** |
| Mainboard temp | LHM: сенсори motherboard | `GetHardwareTemperature` | **Так** |
| CPU temp (fallback) | LHM CPU `Load`/`Clock` | `GetCpuUsage`, `GetCPUFreq` | **Так** |
| Today/history traffic | власний `history_traffic.dat` + лічильники за день | `HistoryTrafficFile` | Ні |
| Total speed | сума up+down | обчислення | Ні |

\* PDH-лічильники зазвичай доступні й без підвищення, але на цій машині `typeperf`/частина `Get-Counter` шляхів без admin не віддаються через локалізацію/дозволи; у коді TrafficMonitor є fallback на `PdhAddEnglishCounter`, а для CPU — на `GetSystemTimes`. Деталі — у звіті `02_native_windows_sensors.md`.

### 4.2. Таймери та навантаження

- `MONITOR_TIMER` = `monitor_time_span` (у конфігу **1000 ms**) — опитування мережі, CPU, RAM, GPU, дисків, LHM.
- `MAIN_TIMER` = 1000 ms — оновлення головного вікна.
- `TASKBAR_TIMER` = 1000 ms у `CTaskBarDlg::OnInitDialog`, плюс 100 ms таймер у головному вікні для перерахунку розмірів/позиції taskbar-вікна.
- LHM `GetHardwareInfo()` викликається **в тому ж 1-секундному таймері**, під критичною секцією, з `__try/__except` навколо виклику. Це ще один аргумент за окремий процес: SEH не захищає від логічних винятків .NET і не ізолює memory corruption.

### 4.3. Виміряне споживання (живий процес)

| Показник | Значення |
|---|---|
| Working set | **40.3 MB** |
| Private bytes | **105 MB** |
| Threads / handles | 11 / 738 |
| CPU за 20 s | **1.438 s** → **7.18 % одного ядра** (≈0.60 % усіх 12 потоків) |
| Uptime на момент заміру | 53.4 год |
| GDI/D2D | `disable_d2d = true` → поточний рендер GDI; у лозі одна подія `RecreateAll` (перестворення D2D-девайсів) |

Для застосунку, який малює 294×35 px раз на секунду, 7 % одного ядра — забагато. Ймовірні причини: elevated MFC-процес, 100 ms таймер, перерахунок layout, GDI-малювання без dirty-rect, .NET/LHM-опитування. Ціль нового застосунку: **< 0.2–0.3 % одного ядра** у спокої, **< 30 MB** working set, 1–2 Гц оновлення без перемальовування незмінних елементів.

---

### 4.4. Локальна валідація джерел (non-admin, ця машина)

| Перевірка | Результат |
|---|---|
| PDH `PdhAddEnglishCounterW` + `\Processor Information(_Total)\% Processor Utility` | ✅ працює без admin; 6.37 % (у той самий момент `% Processor Time` = 10.07 %) |
| PDH `\PhysicalDisk(_Total)\% Idle Time` | ✅ 98.74 % |
| PDH `\Memory\Available MBytes` | ✅ 5085 MB |
| PDH `\GPU Engine(*)\Utilization Percentage` | ✅ через `Get-Counter`, non-admin |
| NVML (`nvidia-smi`) | ✅ GTX 1650: 0 %, 42 °C, 750 MiB |
| ADLX (`amdadlx64.dll`) | ✅ Radeon iGPU: 47 °C, 5 %, 6 W |
| ACPI perf-counter `Win32_PerfFormattedData_Counters_ThermalZoneInformation` | ✅ `\_TZ.THRM` = 3202–3212 деци-Kelvin (~47–48 °C), без admin |
| `Get-StorageReliabilityCounter` | ❌ `Access denied` без admin; `Temperature` порожня |
| `CreateFileW(\\.\PhysicalDriveN, GENERIC_READ)` | ❌ `ERROR_ACCESS_DENIED (5)` без admin; з access=0 — OK |
| `IOCTL_STORAGE_QUERY_PROPERTY` `StorageDeviceTemperatureProperty` (52), NVMe | ✅ **41 °C**, 3 sensors (critical 95, warning 90), без admin |
| Те саме, SATA HDD | ❌ `ERROR_INVALID_FUNCTION (1)` |
| `IOCTL_ATA_PASS_THROUGH` / SMART (SATA) | ❌ `ERROR_ACCESS_DENIED (5)` без admin |
| `MSAcpi_ThermalZoneTemperature` | ❌ `Access denied` без admin |

**Висновок:** safe mode без admin реально покриває мережу, CPU %, CPU freq, RAM, GPU %, NVIDIA/AMD GPU temp (NVML/ADLX), **NVMe SSD temp**, disk activity і навіть ACPI-зону (як best-effort). CPU core temp, SATA HDD/SSD temp, mainboard temp — тільки через провайдер/helper.

---

## 5. Інтеграція LibreHardwareMonitor + PawnIO

### 5.1. Міст `OpenHardwareMonitorApi.dll`

Це **C++/CLI mixed-mode** збірка (`mscoree.dll`, `.nep` секція). Вона надає C++-інтерфейс `IOpenHardwareMonitor` і всередині тримає керований `LibreHardwareMonitor.Hardware.Computer`:

- `SetCpuEnable/SetGpuEnable/SetHddEnable/SetMainboardEnable` → `computer.IsCpuEnabled/IsGpuEnabled/IsStorageEnabled/IsMotherboardEnabled`.
- `GetHardwareInfo()` → `computer.Accept(updateVisitor)` (обхід дерева й `Update()`), потім збір:
  - CPU: усі `SensorType.Temperature` → map «назва сенсора → °C»; середнє або конкретне ядро (`AllCpuTemperature`);
  - CPU freq: усі `SensorType.Clock`, крім `Bus Speed`, середнє / 1000;
  - CPU usage: перший `SensorType.Load`, назва не `CPU Total` (особливість LHM);
  - GPU: `GPU Core` temperature та `GPU Core` load; для AMD/NVIDIA/Intel — окремі гілки;
  - Storage: `Total Activity` (load) і температури; імена дисків дедуплікуються суфіксом `#1`.
- `GetGpuTemperature()` має пріоритет: NVIDIA → AMD → Intel.
- `GetHardwareTemperature()` для GPU шукає `GPU Core`, інакше середнє всіх температур; для CPU — `Core Average`, інакше середнє; якщо нічого — рекурсія в `SubHardware`.

### 5.2. Що саме дає PawnIO

У `LibreHardwareMonitorLib.dll` знайдено ресурси:

```
LibreHardwareMonitor.Resources.PawnIo.AMDFamily0F.bin
LibreHardwareMonitor.Resources.PawnIo.AMDFamily10.bin
LibreHardwareMonitor.Resources.PawnIo.AMDFamily17.bin
LibreHardwareMonitor.Resources.PawnIo.RyzenSMU.bin
LibreHardwareMonitor.Resources.PawnIo.IntelMSR.bin
LibreHardwareMonitor.Resources.PawnIo.SmbusI801.bin
LibreHardwareMonitor.Resources.PawnIo.SmbusNCT6793.bin
LibreHardwareMonitor.Resources.PawnIo.SmbusPIIX4.bin
LibreHardwareMonitor.Resources.PawnIo.LpcACPIEC.bin
LibreHardwareMonitor.Resources.PawnIo.LpcCrOSEC.bin
LibreHardwareMonitor.Resources.PawnIo.IsaBridgeEC.bin
LibreHardwareMonitor.Resources.PawnIo.LpcIO.bin
```

Для Ryzen 5 5600H критичні `AMDFamily17.bin` + `RyzenSMU.bin` (доступ до SMN/SMU для температури та частот), для SMBus/DIMM — `Smbus*`/`IsaBridgeEC`. Kernel-драйвер лише виконує ці підписані модулі; сам LHM не робить `WRMSR` з user-mode.

### 5.3. Наслідки для нового проєкту

- **Не інтегрувати LHM in-process у Rust-застосунок.** Варіанти:
  1. окремий .NET-сайдкар (LHM + named pipe/JSON) — мінімум роботи, але +50–100 MB runtime і той самий PawnIO;
  2. окремий Rust-сайдкар із прямим викликом `PawnIOLib.dll` і власними модулями — більше роботи, але повний контроль і малий розмір;
  3. **не постачати драйвер взагалі**, а читати вже запущений HWiNFO/AIDA64/LHM (shared memory/WMI/HTTP) — найбезпечніше для античитів, але залежить від третьої сторони.
- Якщо робити власний сайдкар — він має бути **окремим процесом**, бажано **окремою службою** з мінімальними привілеями, з чітким протоколом «запит → відповідь», таймаутом, версією й валідацією даних. UI не повинен падати, якщо сайдкар зник.
- Варто підтримати **кілька провайдерів** і показувати provenance: `native`, `hwinfo-shm`, `lhm-wmi`, `lhm-http`, `pawnio-helper`. Якщо температура недоступна — показувати `—`, а не вигадувати значення.

---

## 6. Taskbar-вікно: як це зроблено зараз

### 6.1. Win11-шлях (активний на цій машині)

З `TaskBarDlg.cpp` / `Win11TaskbarDlg.cpp`:

1. `FindWindow(L"Shell_TrayWnd", NULL)` — головний taskbar.
2. `InitTaskbarWnd()`:
   - `m_hNotify = FindWindowEx(m_hTaskbar, 0, L"TrayNotifyWnd", NULL)`;
   - `m_hStart = FindWindowEx(m_hTaskbar, nullptr, L"Start", NULL)`;
   - запам'ятовуються прямокутники.
3. Вікно створюється як popup/dialog (`#32770`), потім:
   ```cpp
   m_connot_insert_to_task_bar = !::SetParent(m_hWnd, GetParentHwnd());
   ```
   де `GetParentHwnd()` для Win11 — це сам `Shell_TrayWnd`.
4. Позиція: `notify_x_pos = TrayNotifyWnd.left`; вікно ставиться на `notify_x_pos - width + 2`, вертикально — по центру `Start`/taskbar з урахуванням `window_offset_top`.
5. Якщо `SetParent` не вдався — fallback: вікно залишається top-level, позиціонується абсолютно і за потреби стає `topmost`, коли фокус на taskbar.
6. Win10-шлях інший: `ReBarWindow32` → `MSTaskSwWClass`/`MSTaskListWClass`; TrafficMonitor **зменшує** прямокутник кнопок задач (`MoveWindow(m_hMin, ...)`), щоб звільнити місце, і робить `SetParent` у `ReBarWindow32`. На Win11 цього не роблять — вікно просто накладається на порожню ділянку.

### 6.2. Живі параметри вікна (Win11 26300)

Знайдено через `EnumChildWindows(Shell_TrayWnd)`:

| Параметр | Значення |
|---|---|
| Клас / заголовок | `#32770` / `TrafficMonitorTaskbarWindow` |
| Батько | `Shell_TrayWnd` (0x10256) |
| Розмір | **294 × 35 px** |
| Позиція | **(1246, 1036)**; taskbar: (0, 1027) 1920×53; `TrayNotifyWnd`: (1538, 1027) 382×53 |
| Style | `0x94080044` = `WS_POPUP | WS_VISIBLE | WS_CLIPSIBLINGS | WS_SYSMENU | DS_SETFONT | DS_3DLOOK` |
| ExStyle | `0x90080` = `WS_EX_LAYERED | WS_EX_CONTROLPARENT | WS_EX_TOOLWINDOW` |
| Z-order | перший child `Shell_TrayWnd`, поверх XAML-мосту `Windows.UI.Composition.DesktopWindowContentBridge` |

Тобто це **не `WS_CHILD`**, а popup-вікно, якому `SetParent` змінив батька. Воно отримує координати відносно taskbar, обрізається його межами, але зберігає layered-рендеринг і власний обробник миші. Це працює навіть попри те, що Win11 taskbar — XAML-поверхня (`DesktopWindowContentBridge`).

### 6.3. Рендеринг

- У конфігу `disable_d2d = true`, тому зараз використовується **GDI** (подвійна буферизація, `WS_EX_LAYERED`, `UpdateLayeredWindow`/прозорий колір).
- Код підтримує три режими: `D2D1_WITH_DCOMPOSITION`, `D2D1`, `GDI`; вибір автоматичний залежно від підтримки й налаштувань (`RenderAPISupport.h`, `DrawCommonFactory`).
- Для прозорості на Win11 використовується `WS_EX_LAYERED` + колір-ключ; для D2D — `IDCompositionDevice`/`IDCompositionTarget`.
- PNG-скіни малюються через GDI+, тому складні ефекти (blur, rounded corners) обмежені.
- Шрифт — `Segoe UI`, 8 pt; увімкнено `enable_colorful_emoji`, але GDI не дає кольорових emoji (ще один аргумент за DirectWrite).

### 6.4. Поточний вигляд (2 рядки, 8 елементів)

Скріншот нижньої смуги 1920×60 (лише taskbar):

```
[icons ....]  ↑ 2.7 Mb/s   CPU: 14%   GPU: 0%    GPU: 42 °C   [tray]
              ↓ 0.3 Mb/s   MEM: 85%   CPU: 48 °C  HDD: 61 °C
```

- Два рядки по чотири елементи; стрілки `↑`/`↓`; значення вирівняні праворуч; лейбли — сірувато-блакитні, значення — зелені/помаранчеві/білі залежно від елемента.
- Ширина 294 px при 4 цифрах і `font_size=8`; `item_space=8`, `vertical_margin=0`.
- Вікно стоїть у вільній зоні між іконками застосунків і треєм; при заповненні taskbar іконки можуть перекриватися (відома проблема, див. Help).

### 6.5. Пов'язані функції taskbar-вікна

- Правий клік → власне контекстне меню (показати головне вікно, вибір з'єднання, історія, налаштування, плагіни, вихід).
- Tooltip із повними значеннями (навіть тими, що не показані).
- Подвійний клік — налаштовувана дія (Task Manager за замовчуванням).
- Drag/переміщення, snap до лівого/правого краю, зсуви `window_offset_left/top`, `taskbar_left_space_win11=160`, `taskbar_right_space_win11=280` для обходу widgets/центрованих іконок.
- Мультимонітор: `Shell_SecondaryTrayWnd`, вибір індексу дисплея (`show_taskbar_wnd_in_secondary_display`, `secondary_display_index`).
- Автоприховування при fullscreen (`hide_main_wnd_when_fullscreen` — для головного вікна; для taskbar — логіка в головному вікні).

---

## 7. Карта конфігурації (`config.ini`)

| Ключ | Значення зараз | Що означає / на що впливає |
|---|---:|---|
| `language` | `1049` | Російська локалізація. |
| `cpu_usage_acquire_method` | `1` | PDH `% Processor Utility` (як Task Manager); `0` — `GetSystemTimes`. |
| `monitor_time_span` | `1000` | Період опитування сенсорів, ms. |
| `hardware_monitor_item` | `15` | Біти CPU=1, GPU=2, HDD=4, MBD=8 — увімкнено все. |
| `hard_disk_name` | `Apacer AS2280P4U 1TB` | Який диск показувати для HDD temp/usage. |
| `cpu_core_name` | `Средняя температура` | Який CPU-сенсор брати (середнє). |
| `tbar_display_item` | `255` | 8 елементів: up, down, CPU, MEM, GPU usage, CPU temp, GPU temp, HDD temp. |
| `item_order` | `0..12` | Повний порядок усіх 13 елементів. |
| `task_bar_speed_unit` | `2` | MB/s (0 auto, 1 KB/s, 2 MB/s). |
| `digits_number` | `4` | Ширина числового поля. |
| `item_space` | `8` | Між елементами, px. |
| `vertical_margin` | `0` | Вертикальний відступ. |
| `window_offset_top/left` | `0/0` | Зсув taskbar-вікна. |
| `taskbar_left_space_win11` | `160` | Місце під widgets/ліві елементи. |
| `taskbar_right_space_win11` | `280` | Fallback-відступ від правого краю, якщо `TrayNotifyWnd` не знайдено. |
| `auto_adapt_light_theme` | `true` | Автоперемикання dark/light пресетів. |
| `dark_default_style` / `light_default_style` | `0` / `3` | Який пресет кольорів використовувати. |
| `disable_d2d` | `true` | Зараз GDI; D2D вимкнено (стабільність/сумісність). |
| `enable_colorful_emoji` | `true` | Але GDI не дає кольорових emoji. |
| `cm_graph_type` | `true` | Графік — scrolling, не стовпчиковий. |
| `show_netspeed_figure` | `false` | Окремий графік швидкості вимкнено. |
| `show_status_bar` | `false` | Фонові смужки CPU/MEM вимкнено. |
| `hide_main_window` | `1` | Головне вікно приховане, працює лише taskbar. |
| `show_task_bar_wnd` | `true` | Taskbar-вікно ввімкнене. |
| `show_notify_icon` | `true` | Іконка в треї є. |
| `always_on_top` | `false` | Для головного вікна. |
| `mouse_penetrate` | `false` | Кліки проходять у taskbar-вікно. |
| `double_click_action` | `0` | Відкрити Task Manager. |
| `traffic_tip_enable`, `memory_usage_tip_enable`, `*_temp_tip_enable` | `false` | Сповіщення вимкнені; пороги 80–200. |
| `notify_interval` | `60` | Антидребезгінг сповіщень, s. |
| `portable_mode` | `true` (global_cfg.ini) | Конфіг у каталозі програми, не в AppData. |
| `plugin_disabled` | порожньо | Плагіни не вимкнені, але їх немає. |

**Висновок:** поточний профіль — «максимально компактний дворядковий віджет із температурами, без графіків і сповіщень, у темній темі, з GDI-рендером». Саме цей профіль варто взяти за основу MVP для візуального порівняння.

---

## 8. Функціональний інвентар і пріоритети для нового застосунку

### P0 — без цього застосунок не є заміною

| Функція | Деталі |
|---|---|
| Мережа up/down | Байт/с, авто-вибір активного адаптера, ручний вибір, коректна робота з Wi-Fi/VPN/гібернацією. |
| CPU % | Сумісно з Task Manager (`% Processor Utility`), fallback `GetSystemTimes`. |
| RAM % | `GlobalMemoryStatusEx`; показ «used/total» у tooltip. |
| GPU % | PDH `\GPU Engine(*)\Utilization Percentage`, max за типом engine. |
| Температури | GPU (NVML/ADLX) і NVMe (IOCTL) — native; CPU/MB/SATA — через опціональний провайдер; чіткий стан «недоступно». |
| Дискова активність | PDH `% Idle Time` → `100 - idle`, з корекцією NVMe. |
| Дворядковий компактний віджет | 8 елементів, вирівнювання, кастомні лейбли, 4 цифри. |
| Кольори/теми | Dark/light auto, per-item кольори, прозорість. |
| Позиціонування | Ліворуч від трею, зсуви, snap, multi-monitor. |
| Контекстне меню + tray icon | Показати/сховати, налаштування, вихід, Task Manager. |
| Автозапуск | Unelevated: `HKCU\...\Run` або Task Scheduler без highest privileges. |
| Стійкість | Не падати при зміні адаптера/резюмі/перезапуску Explorer. |
| Мінімальне споживання | < 30 MB WS, < 0.3 % одного ядра. |

### P1 — очікувані «звичні» функції

| Функція | Деталі |
|---|---|
| Графіки | Net speed figure, CPU/MEM/GPU/disk usage graph, dashed box, scrolling vs bars. |
| Скіни | BMP/PNG + layout XML/INI; бажано — новий формат + importer. |
| Кастомізація | Порядок елементів, кількість цифр, одиниці (B/b, KB/MB, auto), hide unit/percent, short mode, spaces. |
| Історія трафіку | Денний/тижневий/місячний/річний вигляд, log scale, Sunday first, експорт. |
| Сповіщення | Пороги трафіку/RAM/температур, інтервал, toast. |
| Деталі мережі | Список з'єднань, IP, швидкості, переініціалізація. |
| Годинник/дата? | У TrafficMonitor немає, але часто просять; можна як плагін. |
| Plugin API | Краще новий (JSON/WASM/named pipe), ніж C++ vtable ABI. |
| Багатомовність | 11 мов; мінімум — EN/UK/RU. |
| Оновлення | Перевірка версій, канали stable/beta. |

### P2 — «nice to have» / сумісність

| Функція | Коментар |
|---|---|
| C++ plugin ABI v8 | Дорого і крихко; можна зробити окремий міст пізніше. |
| Імпорт старих скінів | Читання `skin.ini`/`skin.xml` + PNG/BMP. |
| Імпорт `config.ini` | Мапінг ключів і міграція профілю. |
| Імпорт `history_traffic.dat` | Потрібен реверс бінарного формату. |
| Mouse penetrate / always on top / lock position | Для головного вікна. |
| Оновлення через GitHub/Gitee | Аналогічно TrafficMonitor. |

---

## 9. Спостережувані проблеми та уроки

### 9.1. Стабільність

`error.log` (44 343 B, CP1251) містить **269** рядків із таймстемпами:

| Подія | К-сть | Коментар |
|---|---:|---|
| «Система была выведена из гибернации, соединение повторно инициализировано» | **146** | Ноутбук часто засинає; монітор має коректно переживати resume. |
| «Выбранное соединение не соответствует ожидаемому, соединение восстановлено» | **112** | Wi-Fi/VPN міняють індекс/ім'я адаптера; потрібен стабільний вибір за GUID/LUID, а не за іменем. |
| «Недостаточен размер буфера...» | 2 | Класична проблема `GetIfTable` зі змінною кількістю адаптерів. |
| `Notice: All devices will be recreated` (D2D) | 1 | Перестворення D2D-девайсів. |
| Повний crash dump | **1** (30.10.2026 18:20) | `RaiseException` → `clr.dll`; збережено `.dmp` у `%TEMP%`. |
| Невідомі рядки (китайська, зіпсована кодуванням) | 6 | Ймовірно, повідомлення про зміну мережі. |

**Урок:** 258 подій, пов'язаних із мережею/резюмом, — це не «рідкісний крайній випадок», а основний сценарій для ноутбука. Новий застосунок повинен мати:
- вибір адаптера за **LUID/GUID** із fallback на ім'я;
- обробку `WM_POWERBROADCAST`/resume;
- періодичне перевизначення активного адаптера (але без скидання лічильників у нуль);
- захист від переповнення 32-бітних лічильників `GetIfTable` (краще `GetIfTable2`/`GetIfEntry2` з 64-бітними `InOctets`).

### 9.2. Продуктивність

- 7.18 % одного ядра — це помітно навіть на 6-ядерному CPU (0.6 % загального, але один потік зайнятий). Для порівняння, Task Manager у спокої споживає менше.
- 105 MB private bytes — багато для віджета; значна частина — .NET Framework + LHM + MFC.
- 100 ms таймер для перерахунку layout — надлишковий; краще `SetWinEventHook` + 1 s fallback.
- GDI + layered window + перемальовування всього вікна щосекунди — основна витрата. Direct2D із dirty-rect/кліпуванням дасть кращий результат, але D2D-девайс треба коректно перестворювати після втрати (у лозі є `RecreateAll`).

### 9.3. Сумісність із taskbar

- Win11 26300: `SetParent` у `Shell_TrayWnd` працює, але це **недокументована** поведінка.
- `MSTaskSwWClass`/`MSTaskListWClass` на Win11 26300 існують, але вже не є тим самим «контейнером кнопок», що на Win10; спроба зменшувати `MSTaskSwWClass`, як на Win10, може зламати розкладку.
- `Start` іноді не знаходиться (`FindWindowEx(..., "Start")` → 0 на цій машині), тому код використовує `TrayNotifyWnd` і fallback-відступи.
- `TrayNotifyWnd` може бути відсутнім на secondary taskbar; потрібен fallback (88 px для годинника, як у коді).
- Widgets/центровані іконки вимагають ручних зсувів (`160`/`280`), які залежать від версії Windows і налаштувань користувача. Це крихко.

### 9.4. DPI та теми

- `dpiAware=true` (system) означає, що при зміні DPI/переміщенні вікна між моніторами з різним scaling може бути розмиття або неправильні розміри. У лозі `DPI: 106` — нестандартне значення, яке система масштабує.
- Новий застосунок має бути **Per-Monitor V2** (`SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)`), обробляти `WM_DPICHANGED`, перераховувати шрифти/відступи.
- Тема: `AppsUseLightTheme` + `DwmGetColorizationColor`; на Win11 taskbar прозорий/акрилічний, тому колір-ключ і прозорість треба підбирати обережно (інакше текст «просвічує» або зникає при HDR).

### 9.5. HDR

У Help згадано, що при HDR taskbar-вікно може не відображатися; лікується Direct2D-рендером або вимкненням прозорості. Для нового застосунку:
- не покладатися на `WS_EX_LAYERED` + color key як єдиний режим;
- мати D2D/DComposition шлях із коректним color space (scRGB/sRGB);
- тестувати HDR on/off, SDR white level, Auto HDR.

---

## 10. Що зберегти, що змінити

### Зберегти (функціональний паритет)

1. Дворядковий компактний віджет із 8 елементами і тими самими лейблами.
2. Контекстне меню, tray icon, tooltip, подвійний клік → Task Manager.
3. Авто-вибір адаптера + ручний вибір, історія трафіку за днями.
4. Dark/light auto, per-item кольори, прозорість, зсуви, multi-monitor.
5. Конфіг у стилі INI (або TOML/JSON із міграцією), portable mode.
6. Локалізацію (мінімум EN/UK/RU) і кастомні лейбли.
7. Скіни як окремий шар (новий формат + importer старих).

### Змінити (архітектурно)

| Було | Стане |
|---|---|
| Elevated GUI + in-process LHM | Unelevated UI + окремий sensor helper (опційно) |
| `requireAdministrator` | `asInvoker`; підвищення лише для helper/служби |
| C++/CLI + .NET Framework | Rust + `windows` crate; без .NET у UI |
| GDI + повний перемальовуваний | Direct2D/DirectWrite + dirty rect; GDI fallback |
| `SetParent` як єдиний шлях | Основний — layered overlay; `SetParent` — опційний experimental mode |
| C++ plugin ABI v8 | Новий plugin API (named pipe/JSON/WASM), optional legacy bridge |
| `dpiAware=true` | Per-Monitor V2 |
| Логи CP1251 | UTF-8, структуровані, ротація |
| Конфіг у каталозі програми | `%LOCALAPPDATA%` з atomic write |
| 1 s + 100 ms таймери | 1 s sampling, event-driven layout, 1 Hz redraw |
| PawnIO за замовчуванням | PawnIO/LHM лише за явним opt-in, з попередженням про античити |

---

## 11. Висновки для архітектури

1. **Поточна збірка доводить, що taskbar-віджет на Win11 можна зробити без DeskBand** — через layered popup + `SetParent(Shell_TrayWnd)`. Це найкращий стартовий прототип для Rust.
2. **Більшість метрик не потребує драйвера.** Мережа, CPU load/freq, RAM, GPU load/temp (NVML/ADLX), disk activity, **NVMe temp** — усе це працює через user-mode API. Драйвер потрібен лише для **CPU/MB/SATA** температур (і частково CPU freq/GPU на деяких конфігураціях).
3. **Найбільша архітектурна помилка поточної збірки — elevated UI + in-process .NET/LHM.** Її треба виправити в першу чергу: UI unelevated, сенсори — окремий процес, IPC із таймаутом і версією.
4. **Температури — це продуктова фіча, а не «просто сенсор».** Від неї залежить, чи запускати PawnIO, чи читати HWiNFO shared memory, чи показувати «—». Рішення треба узгодити з користувачем до написання коду.
5. **MVP без температур** можна зробити повністю safe (без драйверів, без admin) і вже замінити 80 % функцій TrafficMonitor. Це найкращий перший реліз; температурний модуль — окремий етап.

---

## Додаток A. Повний список файлів верхнього рівня

| Файл | Розмір, B | Версія | Підпис |
|---|---:|---|---|
| `TrafficMonitor.exe` | 1 934 336 | 1.8.6.0 | NotSigned |
| `OpenHardwareMonitorApi.dll` | 271 872 | 1.0.0.3 | NotSigned |
| `LibreHardwareMonitor.exe` | 4 458 496 | 0.9.6.0 | NotSigned |
| `LibreHardwareMonitorLib.dll` | 1 203 200 | 0.9.6.0 | NotSigned |
| `LibreHardwareMonitorLib.pdb` | 407 664 | — | — |
| `LibreHardwareMonitorLib.xml` | 408 117 | — | — |
| `Aga.Controls.dll` / `.pdb` | 145 920 / 556 544 | 1.7.0.0 | NotSigned |
| `BlackSharp.Core.dll` | 32 768 | 1.0.7 | NotSigned |
| `DiskInfoToolkit.dll` | 903 680 | 1.1.2 | NotSigned |
| `HidSharp.dll` | 262 792 | 2.6.4.0 | Valid (Illusory Studios) |
| `Microsoft.Win32.TaskScheduler.dll` | 333 312 | 2.12.2.0 | NotSigned |
| `OxyPlot.dll` / `OxyPlot.WindowsForms.dll` | 700 928 / 32 768 | 2.2.0.0 | NotSigned |
| `RAMSPDToolkit-NDD.dll` | 233 472 | 1.4.2 | NotSigned |
| `System.*.dll` (17 файлів) | 18–878 KB | 4.6–10.0 | Valid (Microsoft/.NET) |
| `config.ini` | 5 887 | — | — |
| `global_cfg.ini` | 35 | — | — |
| `history_traffic.dat` / `.bak` | 3 477 / 3 146 | — | — |
| `LibreHardwareMonitor.config` | 29 279 | — | — |
| `LibreHardwareMonitor.exe.config` | 544 | — | — |
| `error.log` | 44 343 | — | — |
| `TrafficMonitor.VisualElementsManifest.xml` | 291 | — | — |
| `FAQ.url`, `Home.url` | 156 / 136 | — | — |

## Додаток B. Корисні команди для повторення аудиту

```powershell
# Версії/підписи
Get-ChildItem E:\TrafficMonitor\*.exe, E:\TrafficMonitor\*.dll | ForEach-Object {
  $v=$_.VersionInfo; [PSCustomObject]@{Name=$_.Name; Version=$v.FileVersion;
  Sig=(Get-AuthenticodeSignature $_.FullName).Status} }

# Драйвер PawnIO
Get-CimInstance Win32_SystemDriver | ? Name -eq 'PawnIO'
Get-AuthenticodeSignature 'C:\Windows\System32\DriverStore\FileRepository\pawnio.inf_amd64_a92d2610b0f4c4c8\PawnIO.sys'

# Taskbar-дерево
# (використовуйте EnumChildWindows(FindWindow("Shell_TrayWnd", $null)))
# Очікувано: #32770 'TrafficMonitorTaskbarWindow', батько Shell_TrayWnd,
# style 0x94080044, ex 0x90080, ~294x35, ліворуч від TrayNotifyWnd.

# Маніфест (requireAdministrator)
# python -c "import pefile; ..." або sigcheck -m
```

## Додаток C. Джерела коду TrafficMonitor (для перевірки тверджень)

- `TrafficMonitor/TaskBarDlg.cpp`, `Win11TaskbarDlg.cpp`, `ClassicalTaskbarDlg.cpp` — вбудовування в taskbar, позиціонування, таймери.
- `TrafficMonitor/PdhHardwareQuery/*` — CPU/GPU/disk/CPU-freq через PDH.
- `TrafficMonitor/DisplayItem.h`, `CommonData.h` — 13 елементів, `HardwareItem`, конфіг.
- `OpenHardwareMonitorApi/OpenHardwareMonitorImp.cpp` — мапінг LHM-сенсорів.
- `include/PluginInterface.h` — plugin API v8.
- `TrafficMonitor/TrafficMonitorDlg.cpp` — головний цикл опитування, `IsTemperatureNeeded`, LHM-виклик.
- `TrafficMonitor/TrafficMonitor.cpp` — ініціалізація LHM у фоновому потоці, конфіг.
