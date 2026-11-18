# Oxide Monitor — архітектура безпечного аналога TrafficMonitor

**Статус:** draft для узгодження  
**Дата:** 17.11.2026  
**Платформа:** Windows 10 22H2+ / Windows 11 23H2–26H2, x64 (arm64 — пізніше)  
**Мова:** Rust (edition 2024), MSVC toolchain  
**Назва проєкту:** Oxide Monitor

---

## 0. Рішення в одному абзаці

Oxide Monitor — це **невеликий unelevated Rust-застосунок**, який малює віджет у taskbar через **top-level layered overlay-вікно** (без інжекції в Explorer і без `SetParent` за замовчуванням), читає більшість метрик **повністю з user-mode API** (PDH, IP Helper, `GlobalMemoryStatusEx`, NVML/ADLX), **NVMe-температуру — напряму через `IOCTL_STORAGE_QUERY_PROPERTY`**, а температури CPU/плати і SATA-накопичувачів отримує **лише від опціонального зовнішнього провайдера**: HWiNFO Shared Memory, LibreHardwareMonitor HTTP або окремого sensor-helper. У стандартній інсталяції **немає kernel-драйвера, немає admin-прав, немає in-process .NET і немає прослуховування мережі**. Температурний модуль — окремий, явно позначений opt-in рівень із чесним попередженням про античити.

---

## 0.1. Оновлення пріоритетів (2026-11-18)

**Головний пріоритет — універсальна нативна база (Tier 0) на x64 + ARM64 без драйверів і без admin.** Зовнішні провайдери точних температур (HWiNFO/LHM) і Game Mode — **останній етап**, після стабілізації Tier 0.

| Підсистема | Основний native-шлях | Fallback / деградація |
|---|---|---|
| CPU load | PDH `% Processor Utility` | `GetSystemTimes`; ARM64/VM-safe |
| CPU freq | PDH `Processor Frequency` / `% Processor Performance` × max MHz | `CallNtPowerInformation`; `None` при 0/смітті |
| CPU temp | ACPI thermal zone (PDH) | `~` + frozen-value detector; `—` |
| GPU load | PDH GPU Engine per-LUID + DXGI adapters | NVML/ADLX/Level Zero де доступні; `—` |
| GPU temp | NVML (NVIDIA), ADLX (AMD), Level Zero Sysman (Intel) | `—`; PDH/DXGI — лише load/VRAM |
| Диски | усі `PhysicalDriveN`; NVMe `StorageDeviceTemperatureProperty` (52) → log page 0x02 | SATA/USB/RAID `—`; PDH per-disk activity |
| Мережа | `GetIfTable2`/`GetIfEntry2` + `GetAdaptersAddresses`/`GetBestInterfaceEx` | фільтр Hyper-V/WSL/VPN; USB/WWAN; resume-safe |
| Пам'ять | `GlobalMemoryStatusEx` | — |
| ARM64 | `cargo check --target aarch64-pc-windows-msvc` у CI | без x86 asm/залежностей; vendor DLL відсутні → `—` |
| i18n | Fluent `.ftl`, EN/UK/RU, `uk` за замовчуванням | EN fallback; відсутній ключ → id |

**Принципи:**
- Жоден провайдер не є обов'язковим; base minimum (мережа, CPU %, RAM %, disk activity) працює завжди.
- Відсутність `nvml.dll`/`amdadlx64.dll`/`ze_loader.dll` — штатний стан, не помилка і не привід для падіння.
- Кілька GPU/дисків — це норма; UI показує primary, дані для решти зберігаються в `gpus`/`disks`.

---

## 1. Цілі, не-цілі, обмеження

### 1.1. Цілі

1. **Функціональний паритет** із поточним профілем TrafficMonitor: 8 елементів у два рядки (up/down, CPU %, MEM %, GPU %, CPU temp, GPU temp, HDD temp), компактний вигляд, кастомні лейбли, кольори, dark/light, multi-monitor.
2. **Безпека за замовчуванням:** жодного kernel-драйвера, жодного підвищення прав для UI, жодної інжекції в чужі процеси, жодних глобальних хуків.
3. **Стабільність:** падіння sensor-провайдера не повинно валити UI; перезапуск Explorer, resume, зміна адаптера, fullscreen і DPI-зміни — штатні сценарії.
4. **Мінімальне споживання:** < 0.3 % одного ядра у спокої, < 30 MB working set, 1 Hz оновлення, без GPU wake-up у стані спокою.
5. **Прозорість даних:** кожна метрика має джерело, час і якість; якщо температури немає — показується `—`, а не вигадане значення.
6. **Розширюваність:** нові провайдери сенсорів і нові елементи віджета додаються без зміни ядра.

### 1.2. Не-цілі (свідомо поза скоупом)

- Керування вентиляторами, напругами, розгоном, RGB.
- Заміна Task Manager / HWiNFO / AIDA64 / моніторинг процесів.
- FPS-оверлей у грі, інжекція в ігрові процеси, чит-функції.
- Кросплатформеність (Linux/macOS) у першій версії.
- Хмарна синхронізація, акаунти, телеметрія.
- Повна сумісність із C++ plugin ABI TrafficMonitor у v1.

### 1.3. Обмеження

| Обмеження | Наслідок |
|---|---|
| Немає офіційного API для taskbar-віджетів у Win11 | Власний overlay/embed host, а не DeskBand. |
| Немає офіційного user-mode API температури CPU | CPU/MB/SATA — провайдер або opt-in helper; NVMe/GPU — native. |
| Античити блокують kernel-драйвери | Default install без драйверів; PawnIO — лише opt-in. |
| HWiNFO/AIDA64 забороняють bundling | Лише виявлення встановленого провайдера; посилання на офіційне завантаження. |
| LHM 0.9.5+ не має WMI | Актуальний шлях — HTTP `/metrics`; WMI — legacy. |
| Win11 taskbar змінюється між build-ами | Overlay не залежить від XAML; embed — experimental. |

---

## 2. Що показав аудит поточної збірки

Детально — у [`05_current_setup_audit.md`](05_current_setup_audit.md). Головне:

1. `TrafficMonitor.exe` має `requireAdministrator`; UI, рендеринг і сенсори — **один elevated-процес**. Автозапуск — Task Scheduler.
2. LHM 0.9.6 працює **in-process** через C++/CLI міст; температури йдуть через **PawnIO 2.1.0** (WHQL-драйвер + підписані bytecode-модулі). У лозі є один повний crash dump із кадрами `clr.dll`.
3. Більшість метрик **уже читаються без драйвера**: `GetIfTable`, `GetSystemTimes`/PDH, `GlobalMemoryStatusEx`, PDH `\GPU Engine(*)\Utilization Percentage`, PDH `\PhysicalDisk(*)\% Idle Time`, а **NVMe-температура — через `StorageDeviceTemperatureProperty` (52)**. PawnIO/LHM потрібні для CPU/MB/SATA-температур і частини GPU/CPU freq.
4. Taskbar-віджет на Win11 — layered popup `#32770`, який через `SetParent` стає child `Shell_TrayWnd`; позиція — від `TrayNotifyWnd`. На build 26300 це працює, але спирається на приватну ієрархію вікон.
5. Поточний профіль: 8 елементів, два рядки, `disable_d2d=true` (GDI), ~294×35 px, ~7.2 % одного ядра, 40 MB WS / 105 MB private.
6. `E:\TrafficMonitor` writable для `Authenticated Users`, а elevated exe вантажить звідти непідписані DLL → **local privilege escalation**. Це головна причина не повторювати архітектуру «elevated GUI + DLL поруч».
7. 146 resume-подій і 112 переініціалізацій мережі за 5 місяців — ноутбучні сценарії є основними, а не крайніми.
8. **У поточному профілі 7 із 8 елементів доступні без драйвера.** «HDD temp» у конфігу — це `Apacer AS2280P4U 1TB` (NVMe), а не SATA HDD; його температура читається native. Єдина метрика, якої справді бракує, — **CPU temperature**.

**Висновок:** UX варто зберегти, архітектуру — змінити повністю.

---

## 3. Ключові архітектурні рішення (ADR)

| ID | Рішення | Обґрунтування | Наслідки |
|---|---|---|---|
| **ADR-01** | **Жодного kernel-драйвера в default install** | WinRing0 має CVE і блокується Defender/античитами; PawnIO — WHQL, але все одно kernel-примітив і може бути заблокований EAC/BattlEye/Vanguard | CPU/MB/SATA-температури відсутні без opt-in провайдера; NVMe temp — є; зате нульовий ризик для античитів у базовому режимі |
| **ADR-02** | **UI — `asInvoker`, без admin** | Elevated GUI + writable DLL path = EoP; elevated UI ламає UIPI/drag-and-drop | Сенсори, що потребують admin, — лише в окремому helper/провайдері |
| **ADR-03** | **Overlay-first, embed — experimental** | Overlay не залежить від XAML, не чіпає Explorer, переживає його крах; embed дає кращий z-order, але cross-process input/DPI ризики | Два backends зі спільним рендерером; embed — за прапорцем і з auto-fallback |
| **ADR-04** | **Direct2D + DirectWrite, GDI fallback** | Якісний дрібний текст, CJK/emoji, точний DPI; GDI — для сумісності та MVP | Один DIB + `UpdateLayeredWindowIndirect`; D2D — основний, GDI — резервний |
| **ADR-05** | **Власний Win32 UI, без egui/wgpu/WebView для віджета** | Віджет 300×35 px не потребує GUI-фреймворку; egui/wgpu дають зайвий CPU/GPU wake-up і проблеми з прозорістю | Менший бінарник, контроль над рендером; більше Win32-коду |
| **ADR-06** | **Плагінні провайдери сенсорів із provenance** | Температури можуть надходити з різних джерел; дані мають бути валідовані й позначені | `SensorProvider` trait, registry, quality/stale, `—` замість вигадки |
| **ADR-07** | **TOML-конфіг + міграція з `config.ini`** | Простий, читабельний, версіонований; INI TrafficMonitor — для імпорту | Окремий `config_version`, atomic write, backup |
| **ADR-08** | **Плагіни — зовнішній процес/JSON, без C++ ABI v8** | C++ vtable ABI крихкий і небезпечний; зовнішній процес ізольований | v1 без плагінів; пізніше — named pipe/JSON або WASM |
| **ADR-09** | **Ніякого in-process .NET/LHM** | .NET + LHM + PawnIO у UI-процесі = падіння всього монітора, 100+ MB, admin | LHM — лише зовнішній HTTP/сайдкар; UI не залежить від .NET |
| **ADR-10** | **Ніякого мережевого listener і телеметрії** | Мінімізація поверхні атаки; LHM-сервер на 8085 — приклад поганого дефолту | Лише вихідні HTTPS-запити для оновлень (opt-in) |
| **ADR-11** | **Fluent для i18n, `uk` за замовчуванням** | Природна мова, множини/роди, fallback; краще за JSON/TOML/YAML | `locales/*.ftl`, `Localizer`, EN fallback; нові мови — без зміни UI-коду |
| **ADR-12** | **Universal Tier 0 (x64 + ARM64) — головний пріоритет** | Працювати на будь-якому залізі без драйверів/admin; vendor DLL — опційні | Multi-GPU/multi-disk, DXGI/PDH fallback, `—` замість падінь; ARM64 check у CI |
| **ADR-13** | **AMD GPU temp: ADLX probe-only у Tier 0** | ADLX 1.x vtable/ABI нестабільні; сирий виклик може впасти в процесі монітора. Load/VRAM уже працюють через DXGI+PDH | AMD temp `—`; закривається безпечним ADLX SDK-біндінгом або HWiNFO/LHM на фінальному етапі |
| **ADR-14** | **Окремий процес налаштувань + виділений Named Pipe `oxide-ipc` + єдиний writer `config.toml`** | In-process `egui`/`wgpu` лишає GPU-драйвери й 30–60 MB RAM у фоновому моніторі. Окремий `crates/oxide-ipc` (`\\.\pipe\oxide-ui-v1`, `FILE_FLAG_FIRST_PIPE_INSTANCE`, current-user DACL, peer exe check, `generation+hash` conflict detection) запускається ліниво й ізолює UI від залежностей налаштувань | Два `.exe` у дистрибутиві (`oxide-monitor.exe` + `oxide-settings.exe`); при відсутності `oxide-settings.exe` — fallback на `notepad.exe` + hot reload `config.toml` (див. [`docs/IPC.md`](docs/IPC.md)) |

---

## 4. Сенсори: підходи, ризики, вибір

### 4.1. Матриця можливостей

| Метрика | Pure user-mode (без драйвера) | HWiNFO SHM | LHM HTTP | LHM/PawnIO helper | WinRing0/InpOut |
|---|---|---|---|---|---|
| Network up/down | ✅ `GetIfTable2` | — | — | — | — |
| CPU % | ✅ PDH / `GetSystemTimes` | ✅ (дубль) | ✅ | ✅ | ✅ |
| CPU freq | ✅ PDH `% Processor Performance` + `CallNtPowerInformation` | ✅ | ✅ | ✅ | ✅ |
| RAM | ✅ `GlobalMemoryStatusEx` | — | — | — | — |
| GPU % | ✅ PDH `\GPU Engine(*)` / NVML | ✅ | ✅ | ✅ | ✅ |
| GPU temp | ⚠️ NVIDIA — NVML; AMD — ADLX/ADL (не всі iGPU) | ✅ | ✅ | ✅ | ✅ |
| CPU temp | ❌ (лише ACPI thermal zone — best-effort, не core temp) | ✅ | ✅ | ✅ | ✅ |
| HDD/SSD temp | ✅ NVMe — `StorageDeviceTemperatureProperty` (52), non-admin; ❌ SATA — потрібен admin/ATA SMART | ✅ | ✅ | ✅ | ✅ |
| Mainboard temp | ❌ | ✅ | ✅ | ✅ | ✅ |
| Disk activity | ✅ PDH `% Idle Time` | ✅ | ✅ | ✅ | ✅ |
| Fan/voltage/power | ❌ | ✅ | ✅ | ✅ | ✅ |

> ⚠️ NVML/ADLX — це vendor user-mode API, не kernel-драйвер; вони не потребують admin. На цій машині NVML дає 42 °C для GTX 1650, ADLX — 47 °C для Radeon iGPU. AMD ADLX у Rust поки alpha, тому для production потрібна власна мінімальна FFI-обгортка.

> ℹ️ **ACPI thermal zone.** На цій машині perf-counter `Win32_PerfFormattedData_Counters_ThermalZoneInformation` віддав `\_TZ.THRM = 3202–3212` деци-Kelvin (**~47–48 °C**) **без admin**, тоді як `MSAcpi_ThermalZoneTemperature` — `Access denied`. Це потенційний «best-effort» показник для ноутбуків, але це **зона платформи**, а не Tctl/Tdie: значення може бути статичним, запізнюватися або показувати температуру корпусу. Показувати його можна лише окремим елементом `CPU (ACPI)` з попередженням, після перевірки на правдоподібність (реагує на навантаження, не `0`, не константа).

### 4.2. Рівні ризику (anti-cheat / Defender)

**Коротка відповідь:** без конфліктів проходять лише механізми, які **не вантажать kernel-драйвер і не інжектять код**: PDH, IP Helper, `GlobalMemoryStatusEx`, NVML/ADLX, читання shared memory. Будь-який драйвер — WinRing0, PawnIO, HWiNFO/AIDA64 — це окремий ризик, який залежить від політики конкретного античиту і може змінитися без попередження.

| Tier | Що це | Admin | Драйвер | Defender | EAC/BattlEye | Vanguard | Висновок |
|---|---|---|---|---|---|---|---|
| **0** | Pure user-mode: PDH, IP Helper, `GlobalMemoryStatusEx`, NVML/ADLX, NVMe temp IOCTL | Ні | Ні | ✅ | ✅ (не вантажить драйвер і не інжектить) | ✅ | **Default** |
| **1** | Читання SHM/HTTP уже запущеного HWiNFO/LHM | Ні | Ні (у нашому процесі) | ✅ | ⚠️ залежить від того, чи дозволений сам HWiNFO/LHM | ⚠️ | **Recommended opt-in** |
| **2** | Окремий LHM-сайдкар або Rust-helper із PawnIO 2.1.0+ (official; device ACL — admin-only) | Так (helper) | **PawnIO** | ✅ (WHQL; не в MS blocklist; HVCI OK) | ⚠️ EAC спирається на MS blocklist, але може мати евристики; FACEIT 2.1.0+ сумісний | ⚠️/❓ | **Opt-in, experimental** |
| **3** | In-process LHM + PawnIO в UI | Так | PawnIO | ✅ | ⚠️ | ⚠️/❌ | **Reject** |
| **4** | WinRing0 / InpOutx64 | Так | WinRing0/InpOut | ❌ (WinRing0 блокується) | ❌ | ❌ | **Reject** |

**Ключове формулювання:** «WHQL-підпис» не означає «дозволено античитом». Античити мають власні blocklists і евристики; навіть підписаний драйвер може бути заблокований. Єдиний шлях без конфліктів — **не вантажити драйвер і не інжектити код**, а температури брати з уже запущеного дозволеного провайдера.

**PawnIO — окремо:** official 2.1.0/2.2.0 має два валідні підписи (`namazso.eu` EV + Microsoft WHCP), **не** входить до Microsoft blocklist 10.0.29545.0, сумісний з HVCI (перевірено локально) і FACEIT-сумісний з 2.1.0. Але:
- device за замовчуванням **admin-only** (`SDDL_DEVOBJ_SYS_ALL_ADM_ALL`), тому unelevated UI не може читати його напряму — потрібен elevated helper;
- офіційна збірка приймає лише модулі, підписані ключем namazso, а не будь-які наші;
- це kernel attack surface і bus factor = 1;
- античит може заблокувати його за signer-сертифікатом (прецедент FACEIT) або за device handle, навіть якщо він не в blocklist.

### 4.3. Порівняння провайдерів температур

| Провайдер | Транспорт | Admin для нашого UI | Покриття | Overhead нашого процесу | Ліцензія / обмеження | Складність | Вердикт |
|---|---|---|---|---|---|---|---|
| **HWiNFO 7.0+** | `Global\HWiNFO_SENS_SM2` | Ні | CPU package/Tctl, MB, VRM, fan, power, диски | <0.05 % CPU, <1 MB | Free — 12 h SHM; Pro — без ліміту; bundling заборонено; старі драйвери ≤8.98 у MS blocklist, поточні — ні | Середня (власний парсер) | **Основний зовнішній** |
| **LHM 0.9.5+** | HTTP `127.0.0.1:8085/metrics` | Ні | CPU/GPU/диски/MB | 0.1–0.5 % CPU, 1–2 MB | MPL 2.0; LHM потребує admin; сервер за замовчуванням 0.0.0.0 без auth | Середня (HTTP+парсер) | **Основний безкоштовний зовнішній** |
| **AIDA64** | `AIDA64_SensorValues` | Ні | CPU/GPU/MB/диски | <0.05 % CPU | Платний; без версії/timestamp; bundling заборонено; драйвер має CVE-2024-26507 | Середня | **Уникати** |
| **LHM ≤0.9.4 / OHM** | WMI `root\...\Sensor` | Ні | CPU/GPU/MB | 0.2–1 % CPU | MPL 2.0 / legacy | Низька | Legacy fallback |
| **MSI Afterburner** | `MAHMSharedMemory` | Ні | GPU, CPU temp (якщо увімкнено) | <0.05 % CPU | Freeware, closed; bundling заборонено; старі RTCore64 — CVE | Низька | Gaming fallback |
| **Core Temp** | `CoreTempMappingObjectEx` | Ні | CPU only | <0.02 % CPU | Freeware; session-local | Низька | CPU-only fallback |
| **ACPI thermal zone** | perf-counter `Win32_PerfFormattedData_Counters_ThermalZoneInformation` | Ні | лише зона платформи (не core temp) | <0.01 % CPU | built-in; firmware-dependent | Низька | **Best-effort, окремий елемент `CPU (ACPI)`** |
| **Власний Rust helper + PawnIO** | named pipe | Так (helper) | CPU/GPU/диски/MB | Низький (малий процес) | PawnIO GPLv2 + modules; driver install | Висока | **Довгостроковий opt-in** |
| **LHM-сайдкар (.NET)** | named pipe | Так (helper) | CPU/GPU/диски/MB | Середній (100–300 MB helper) | MPL 2.0; .NET runtime; PawnIO | Середня | **Швидкий місток до parity** |
| **In-process LHM** | FFI | Так (whole app) | Все | Високий (100–200 MB + GC) | MPL 2.0; PawnIO | Середня | **Reject** |
| **WinRing0 / InpOut** | kernel driver | Так | Все | Низький | CVE-2020-14979 / blocklist / anti-cheat | Низька | **Reject** |

**Anti-cheat факти, які треба враховувати:**
- **EAC** публічно заявляє, що використовує Microsoft blocklist; WinRing0/InpOut там є, PawnIO — ні (станом на 17.11.2026).
- **BattlEye** має власний список драйверів із відомими security issues; повідомлення «blocked loading» не є баном.
- **Vanguard** має kernel-античит і публічного списку немає; WinRing0/InpOut блокуються, щодо PawnIO — лише анекдотичні дані.
- **FACEIT** блокує за signer-сертифікатом; PawnIO 2.1.0+ з новим signer вважається сумісним (issue закрито 2026-02-12).
- **HWiNFO** старі драйвери `HWiNFO64A.SYS`/`HWiNFO64I.SYS` ≤8.98 — у blocklist; поточні версії — ні.
- **AIDA64** драйвер має CVE-2024-26507 (arbitrary physical R/W, device без ACL) — не використовувати.
- **CPU-Z** `cpuz.sys` ≤1.0.4.9 — у blocklist; як джерело сенсорів не підходить.
- **MSI Afterburner/RTCore64** — старі версії з CVE-2019-16098; лише GPU, не ядро монітора.

### 4.4. Рекомендована стратегія температур

**Default (Tier 0):** показуємо все, що можна без драйвера, включно з **NVMe-температурою**. CPU/MB/SATA-HDD температури — `—` з tooltip «Немає джерела температур». Це вже замінює 80 % TrafficMonitor і не має жодного anti-cheat ризику.

**Tier 0.5 (опційно):** якщо ACPI thermal zone доступна і правдоподібна — показуємо окремий елемент `CPU (ACPI)` з tooltip «Це зона платформи, не Tctl/Tdie». Вимкнено за замовчуванням; вмикається лише після перевірки, що значення реагує на навантаження і не є константою. На цій машині ACPI-зона дає 47–48 °C, що збігається з поточним CPU temp у TrafficMonitor, але це **не гарантія** для інших ноутбуків.

**Opt-in (Tier 1):**
1. Якщо в системі є HWiNFO з увімкненим Shared Memory — читаємо його (read-only, без admin).
2. Інакше, якщо є LHM із увімкненим HTTP на loopback — читаємо `/metrics`.
3. Інакше — пропонуємо «Налаштувати температури» з покроковою інструкцією та статусом провайдера.

**Opt-in (Tier 2, пізніше):**
- Окремий `oxide-sensors.exe` (Rust + PawnIO) або LHM-сайдкар (.NET) для тих, хто хоче «все з коробки».
- Helper запускається окремо (UAC/Task Scheduler), UI залишається unelevated.
- PawnIO device за замовчуванням admin-only, тому helper **обов'язково** elevated; UI лише читає named pipe.
- У налаштуваннях — явне попередження: «PawnIO — kernel-драйвер; EAC/BattlEye/Vanguard можуть його блокувати. Не використовуйте під час гри із захистом.»
- «Game mode»: зупинити helper, сховати віджет, не вантажити драйвер.

**Ніколи:**
- Не встановлювати драйвер без явної згоди.
- Не вантажити WinRing0/InpOut.
- Не вбудовувати LHM у UI-процес.
- Не обходити 12-годинний ліміт HWiNFO free.

### 4.5. Чому стандартний Windows API не дає температуру CPU

Коротко (деталі — у [`02_native_windows_sensors.md`](02_native_windows_sensors.md)):

- Немає офіційного user-mode API: температура ядер — це vendor-specific MSR/SMN (Intel DTS `IA32_THERM_STATUS`/`IA32_TEMPERATURE_TARGET`, AMD SMN/SMU), доступ до яких можливий лише з ring 0.
- `MSAcpi_ThermalZoneTemperature` — це ACPI thermal zone (часто плата/корпус, не ядра), потребує admin і на багатьох машинах просто не реалізований.
- GPU-вендори дали user-mode API (NVML, ADLX/ADL), тому GPU temp можна отримати без драйвера; CPU-вендори — ні.
- Тому будь-який «чесний» монітор без драйвера або показує `—`, або читає чужий provider.

---

## 5. Архітектура застосунку

### 5.1. Процесна модель

```
┌──────────────────────────────────────────────────────────────┐
│ oxide-monitor.exe  (asInvoker, Medium IL, PMv2)              │
│                                                              │
│  UI thread ── Win32 message loop ── overlay/embed/tray       │
│      │            │                                          │
│      │            ├─ D2D/DWrite renderer → DIB → ULW         │
│      │            └─ layout/theme/input/menu                  │
│      │                                                       │
│  Sampler thread ── provider registry ── native providers     │
│      │                 │                                     │
│      │                 ├─ PDH (CPU/GPU/disk)                 │
│      │                 ├─ IP Helper (network)                │
│      │                 ├─ GlobalMemoryStatusEx               │
│      │                 └─ NVML/ADLX (GPU temp)               │
│      │                                                       │
│  Provider workers (optional, isolated threads):              │
│      ├─ HWiNFO SHM reader                                    │
│      ├─ LHM HTTP client                                      │
│      └─ helper named-pipe client                             │
│                                                              │
│  Config/logging/history                                      │
└──────────────────────────────────────────────────────────────┘
             │ named pipe (optional)
┌────────────▼─────────────────────────────────────────────────┐
│ oxide-sensors.exe (optional, elevated)                       │
│  LHM-сайдкар (.NET) або Rust+PawnIO                          │
│  ── read-only sensor snapshot ── JSON lines ──► UI           │
└──────────────────────────────────────────────────────────────┘
```

**Принципи:**
- UI-процес **ніколи** не підвищує права і не вантажить драйвер.
- Кожен зовнішній провайдер ізольований: його падіння/зависання не блокує UI.
- Дані від helper/SHM вважаються **недовіреними**.
- Немає єдиного «бога-об'єкта»: `SensorRegistry` + `Snapshot` + `Renderer`.

### 5.2. Cargo workspace (пропозиція)

```
oxide-monitor/
├─ Cargo.toml
├─ crates/
│  ├─ oxide-core/          # domain types, units, formatting, config schema
│  ├─ oxide-sensors/       # sampler, provider trait, registry, history
│  ├─ oxide-provider-native/# PDH, IP Helper, memory, NVML/ADLX, storage
│  ├─ oxide-provider-hwinfo/# HWiNFO shared-memory parser
│  ├─ oxide-provider-lhm/  # LHM HTTP /metrics parser (+ legacy WMI)
│  ├─ oxide-provider-helper/# named-pipe client
│  ├─ oxide-ui/            # Win32 host, renderer, layout, theme, input
│  └─ oxide-app/           # composition root, tray, menus, settings, logging
├─ src/bin/oxide-monitor.rs
├─ src/bin/oxide-sensors.rs   # optional helper (feature-gated)
└─ xtask/                   # icons, installer, signing, release
```

Для MVP достатньо 4 крейтів: `oxide-core`, `oxide-sensors`, `oxide-ui`, `oxide-app`. Решта — у міру зростання.

### 5.3. Потоки та потік даних

| Потік | Відповідальність | Період |
|---|---|---|
| **UI/message loop** | Вікно, меню, рендер, реакція на події | Подієво + 1 Hz |
| **Sampler** | Оркестрація провайдерів, дельта-обчислення, історія | 1 Hz |
| **Native worker** | PDH/NVML/ADLX (блокуючі виклики) | 1 Hz |
| **External worker(s)** | HWiNFO SHM / LHM HTTP / helper pipe | 1 Hz, timeout 50–500 ms |
| **Config watcher** | Перечитування TOML | Подієво |

Дані:

```
ProviderSnapshot  ──channel──►  Aggregator  ──ArcSwap<MonitorSnapshot>──►  UI
```

- `MonitorSnapshot` — immutable, містить `seq`, `captured_at`, `network`, `cpu`, `memory`, `gpus`, `disks`, `temperatures`, `history` (невеликі ring buffers).
- UI читає останній snapshot без блокування; якщо `seq` не змінився — не перемальовує.
- Повільний провайдер не блокує інших: кожен external worker має власний дедлайн і backoff.

### 5.4. `SensorProvider` (ескіз контракту)

```rust
pub trait SensorProvider: Send {
    fn id(&self) -> ProviderId;
    fn probe(&mut self) -> Availability;          // Ready / Degraded / Unavailable
    fn capabilities(&self) -> Capabilities;
    fn poll(&mut self, budget: Duration) -> Result<ProviderSnapshot, ProviderError>;
    fn close(&mut self);
}
```

- `ProviderSnapshot` містить `Vec<Reading>` + `captured_at` + `provider_version`.
- `Reading { key, kind, label, value: f64, unit, quality }`.
- `Quality = Good | Stale | Suspect | Unavailable`.
- `ProviderError = NotInstalled | NotRunning | Disabled | AccessDenied | VersionUnsupported | MalformedData | Timeout | Io`.
- Registry: пріоритети, backoff 1→2→5→30→60 s, останній good snapshot, stale через 3–5 s, `—` через 10–15 s.
- Нормалізація: °C, f64, скінченні значення, діапазони; CPU temp resolution: `CPU Package` → `Tctl/Tdie` → `Core Max` → max по ядрах; **не** брати `Distance to TjMax`.

### 5.5. Модель даних (спрощено)

```rust
pub struct MonitorSnapshot {
    pub seq: u64,
    pub captured_at: Instant,
    pub network: NetworkSample,     // up_bps, down_bps, adapter
    pub cpu: CpuSample,             // usage_pct, freq_mhz, per_core?
    pub memory: MemorySample,       // used, total, pct
    pub gpus: Vec<GpuSample>,       // luid, name, usage_pct, temp_c
    pub disks: Vec<DiskSample>,     // name, activity_pct, temp_c
    pub temps: BTreeMap<SensorKey, Reading>,
    pub sources: Vec<ProviderStatus>,
}
```

- Історія: `RingBuffer<f32, 60>` на серію; без алокацій у гарячому шляху.
- Форматування: `Unit` (Bytes/Bits, KB/MB/GB, Celsius, MHz, Percent), `FormatOptions` (digits, short mode, space, hide unit/percent).
- Окремі `SensorKey` для однакових назв (кожен диск/ядро).

### 5.6. Деградація та помилки

| Стан | UI |
|---|---|
| Провайдер відсутній | `—`, tooltip «Немає джерела температур» + кнопка налаштування |
| Провайдер stale (>5 s) | Останнє значення сірим, суфікс `~`, tooltip з віком |
| Провайдер unavailable (>15 s) | `—` |
| Часткові дані (тільки CPU) | Показати CPU, решту приховати |
| Helper впав | UI працює; автоматичний reconnect із backoff; лог |
| SHM version mismatch | Відмова від провайдера, лог із версією; не «вгадувати» layout |
| Мережевий адаптер зник | Перевибір за GUID/LUID; не скидати лічильники в нуль |
| Resume | Переініціалізація провайдерів, повторне відкриття mapping/pipe |

---

## 6. Taskbar UI

### 6.1. Host backends

| Backend | Статус | Як працює | Плюси | Мінуси |
|---|---|---|---|---|
| **Overlay** (default) | Основний | Top-level `WS_POPUP` + `WS_EX_LAYERED|TOOLWINDOW|NOACTIVATE|TOPMOST`, позиція над taskbar | Не чіпає Explorer, переживає його крах, працює на всіх build-ах | Не резервує місце в layout; z-order/fullscreen треба контролювати |
| **Embed** (experimental) | Opt-in | `SetParent(hwnd, Shell_TrayWnd)`, позиція від `TrayNotifyWnd` | Ідеальний z-order/clipping, «справжня» частина taskbar | Cross-process input/DPI, HWND гине з Explorer, залежить від приватної ієрархії |
| **Tray** (fallback) | Завжди | `Shell_NotifyIcon` + popup-панель | Офіційно, стабільно | Немає inline-тексту/графіків |
| **DeskBand** | Не реалізуємо | COM `IDeskBand` | — | Deprecated, Win11 не підтримує, DLL у Explorer |

**Правило:** overlay — єдиний backend, який гарантує роботу на Win11 23H2–26H2. Embed — лише якщо користувач явно ввімкнув і overlay не влаштовує; при будь-якій помилці — автоматичний fallback на overlay.

> **Prior art, який варто оцінити:** [`deskband11lib`](https://github.com/airtaxi/Deskband11Lib) (MIT, листопад 2026) — NativeAOT-бібліотека, яка робить саме це: `SetParent` у taskbar, вимірює кнопки через UI Automation, обробляє Explorer restart/DPI/taskbar edge, має Rust-крейт. Вона могла б стати готовим `embed`-бекендом, але: (а) лише Windows 11 26100+, (б) зовсім нова (2 релізи, десятки завантажень), (в) додає native DLL і .NET NativeAOT-залежність. Рішення: **не робити core-залежністю**, але протестувати як опційний/експериментальний хост і взяти її підхід до UIA-вимірювання як reference.

### 6.2. Геометрія, DPI, multi-monitor

- Primary taskbar: `FindWindowW("Shell_TrayWnd")`; secondary: `EnumWindows` → `Shell_SecondaryTrayWnd`.
- Позиція: `GetWindowRect(taskbar)` + `TrayNotifyWnd` (ліва межа); для Win11 centered icons — UI Automation для кнопок задач (як у Deskband11Lib) або fallback-відступ.
- Орієнтація: `ABM_GETTASKBARPOS` (`ABE_BOTTOM/TOP/LEFT/RIGHT`); для secondary — порівняння ширини/висоти.
- DPI: manifest `PerMonitorV2`; `GetDpiForWindow(taskbar)`/`GetDpiForMonitor`; `WM_DPICHANGED` + `WM_DPICHANGED_AFTERPARENT` (embed); text scale factor окремо.
- Multi-monitor: один overlay на вибраний taskbar; config `monitor = "primary" | index`; per-monitor DPI.
- Auto-hide: `ABM_GETSTATE` → `ABS_AUTOHIDE`; rect taskbar поза монітором → ховати/рухати; `EVENT_OBJECT_LOCATIONCHANGE`.
- Offsets: `window_offset_left/top`, `taskbar_left_space_win11`, `taskbar_right_space_win11` — сумісність із поточним конфігом.

### 6.3. Visibility policy

Ховати overlay, коли:
- fullscreen: `SHQueryUserNotificationState` = `QUNS_RUNNING_D3D_FULL_SCREEN`/`QUNS_BUSY`/`QUNS_PRESENTATION_MODE`, або foreground-вікно покриває монітор (borderless fullscreen) і не є shell-вікном;
- Start menu / Task View / Widgets board відкриті (foreground class/UIA);
- taskbar auto-hide і прихований;
- session locked / display off (`WM_POWERBROADCAST`, `WTS_SESSION_LOCK`);
- user увімкнув «Game mode».

Показувати/повертати topmost:
- `EVENT_SYSTEM_FOREGROUND`, `EVENT_OBJECT_SHOW` для `Shell_TrayWnd`, `TaskbarCreated`;
- після Explorer restart — перестворити вікно, не використовувати старий HWND;
- watchdog раз на 2–5 s перевіряє `IsWindow(taskbar)`.

### 6.4. Rendering pipeline

```
MonitorSnapshot → DisplayModel → LayoutEngine → Canvas (D2D/GDI) → DIB (BGRA8, premultiplied) → UpdateLayeredWindowIndirect
```

- **D2D:** `ID2D1Factory1::CreateDCRenderTarget` (BGRA8, premultiplied), `BindDC` на DIB, `IDWriteTextLayout` для кожного тексту, `DrawTextLayout`; `D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT` для emoji.
- **GDI fallback:** той самий DIB, `SetBkMode(TRANSPARENT)`, `DrawTextW`, `Polyline`; для прозорості — або opaque theme background, або color-key (менш якісно).
- **ULW:** `UpdateLayeredWindowIndirect` з `ULW_ALPHA`, `AC_SRC_ALPHA`, `AC_SRC_OVER`; без `InvalidateRect`, без `WM_PAINT`-мерехтіння.
- **Dirty check:** хеш `DisplayModel`; якщо не змінився — пропустити кадр. Для графіків — оновлювати лише графічну зону.
- **Timer:** `SetCoalescableTimer(1 s)`; рендер лише за новим snapshot.
- **Немає GPU wake-up:** D2D DC render target не створює D3D device; для 1 Hz цього достатньо. DComp/D3D — лише якщо з'являться складні анімації.

### 6.5. Layout і форматування

- Модель: 13 `DisplayItem` (як у TrafficMonitor) + plugin items.
- Два рядки по 4 елементи; `item_order`, `digits_number`, `item_space`, `vertical_margin`, `value_right_align`.
- Лейбли/значення окремими кольорами; per-item colors; `hide_unit`, `hide_percent`, `separate_value_unit_with_space`, `speed_short_mode`.
- Одиниці: мережа — біти/байти, auto/KB/MB; дані — bytes; температури — °C; частота — MHz/GHz; відсотки — %.
- Графіки (Phase 2): ring buffer 60 точок, filled area/line, accent color, dashed box, scrolling vs bars.
- Локалізація: ICU? Ні; власні таблиці EN/UK/RU, формат чисел із locale-aware десятковим розділювачем, але з опцією dot.

### 6.6. Тема

- `HKCU\...\Themes\Personalize`: `SystemUsesLightTheme` (taskbar), `AppsUseLightTheme`, `EnableTransparency`, `ColorPrevalence`.
- `WM_SETTINGCHANGE` з `"ImmersiveColorSet"` / `"WindowMetrics"`; `WM_DWMCOLORIZATIONCOLORCHANGED`.
- Акцент: `DwmGetColorizationColor` + registry `AccentColor` (ABGR).
- Практично: dark `#202020` α≈0.85, light `#F3F3F3` α≈0.85; accent для графіків; опція «непрозорий фон» для HDR/сумісності.
- HDR: per-pixel alpha + sRGB може виглядати інакше; опція «HDR-safe» — opaque background, без color-key.

### 6.7. Input і меню

- `WM_MOUSEACTIVATE` → `MA_NOACTIVATE`; клік не краде фокус.
- Правий клік → `TrackPopupMenu` (тимчасово `SetForegroundWindow`, потім `PostMessage(WM_NULL)`); меню: Show/Hide, Settings, Temperatures, About, Exit.
- Подвійний клік — налаштовувана дія (Task Manager за замовчуванням).
- Tooltip — `TOOLTIPS_CLASS` або власний popup; повні значення + стан провайдера.
- Tray icon — завжди; меню дублює основні дії.
- Click-through для неінтерактивних зон — `WM_NCHITTEST` → `HTTRANSPARENT` (опційно).

### 6.8. Explorer restart, resume, DPI

- `RegisterWindowMessageW("TaskbarCreated")` → перестворити overlay, переприв'язати tray icon.
- `WM_POWERBROADCAST` (`PBT_APMRESUMEAUTOMATIC`) → переініціалізувати PDH/NVML/провайдерів, перевідкрити SHM/pipe.
- `WM_DISPLAYCHANGE`/`WM_DPICHANGED` → перерахувати геометрію, шрифти, DIB.
- Ніколи не тримати HWND taskbar «назавжди»; перевіряти `IsWindow`.

---

## 7. Метрики: джерела, точність, cadence

| Метрика | API | Cadence | Fallback | Особливості |
|---|---|---|---|---|
| Network up/down | `GetIfTable2`/`GetIfEntry2` (`MIB_IF_ROW2`, 64-bit) | 1 Hz | re-enumerate on failure | Вибір за LUID/GUID; дельта за QPC; VPN/Wi-Fi/resume; не скидати в нуль |
| CPU % | PDH `\Processor Information(_Total)\% Processor Utility` | 1 Hz | `GetSystemTimes` | Task Manager parity; clamp 0–100; перший семпл — 0 |
| CPU freq | PDH `% Processor Performance` × max MHz (`CallNtPowerInformation`) | 1 Hz | `CurrentMhz` | AMD може брехати; показувати `~` |
| RAM | `GlobalMemoryStatusEx` | 1 Hz | `GetPerformanceInfo` | `dwMemoryLoad` — ціле; used/total у tooltip |
| GPU % | PDH `\GPU Engine(*)\Utilization Percentage` | 1 Hz | NVML utilization | Агрегація за LUID+engtype, max; мапити LUID через DXGI |
| GPU temp | NVML (NVIDIA), ADLX/ADL (AMD) | 1 Hz | provider | NVML — dGPU; AMD iGPU — не завжди |
| Disk activity | PDH `\PhysicalDisk(*)\% Idle Time` | 1 Hz | `IOCTL_DISK_PERFORMANCE` | `100 - idle`, clamp; NVMe idle може >100 |
| Disk temp (NVMe) | `IOCTL_STORAGE_QUERY_PROPERTY` + `StorageDeviceTemperatureProperty` (52) | 1 Hz | NVMe log page 0x02 | Non-admin з handle access=0; 41 °C на Apacer; 3 sensors; critical 95/warning 90 |
| Disk temp (SATA) | provider / ATA SMART | 1 Hz | — | `StorageDeviceTemperatureProperty` → `ERROR_INVALID_FUNCTION`; ATA PASS-THROUGH → `Access denied` без admin |
| CPU/MB temp | provider only | 1 Hz | — | HWiNFO/LHM/helper |
| CPU temp (ACPI) | `Win32_PerfFormattedData_Counters_ThermalZoneInformation` / PDH `\Thermal Zone Information(*)\High Precision Temperature` | 1 Hz | — | Best-effort зона платформи; окремий елемент, off by default |
| Today/history | власні дельти + файл | 1/min + on change | — | Атомарний запис, backup |

> ℹ️ **CPU Utility — це не «скільки ядер зайнято».** На цій машині під 100 % synthetic load (12 потоків) `% Processor Utility` ≈60 %, бо power limit тримає частоту ~2.0 GHz; Task Manager показує те саме. Не нормалізуйте значення до 100 % і не порівнюйте з `% Processor Time` — це різні метрики.

### 7.1. Локальна валідація (ця машина, non-admin)

| Перевірка | Результат |
|---|---|
| PDH `PdhAddEnglishCounterW` + `% Processor Utility` | ✅ працює без admin; 6.37 % (у той самий момент `% Processor Time` = 10.07 % — різниця важлива) |
| PDH `\PhysicalDisk(_Total)\% Idle Time` | ✅ 98.74 % |
| PDH `\Memory\Available MBytes` | ✅ 5085 MB |
| PDH `\GPU Engine(*)\Utilization Percentage` | ✅ через `Get-Counter`, non-admin |
| NVML (`nvidia-smi`) | ✅ GTX 1650: 0 %, 42 °C, 750 MiB |
| ADLX (`amdadlx64.dll`) | ✅ Radeon iGPU: 47 °C, 5 %, 6 W, 423 MB VRAM |
| `Get-StorageReliabilityCounter` | ❌ `Access denied` без admin |
| `CreateFileW(\\.\PhysicalDriveN, GENERIC_READ)` | ❌ `ERROR_ACCESS_DENIED (5)`; з access=0 — OK |
| `StorageDeviceTemperatureProperty` (52), NVMe | ✅ **41 °C**, 3 sensors, non-admin |
| `StorageDeviceTemperatureProperty` (52), SATA HDD | ❌ `ERROR_INVALID_FUNCTION (1)` |
| `IOCTL_ATA_PASS_THROUGH` / SMART (SATA) | ❌ `ERROR_ACCESS_DENIED (5)` без admin |
| `MSAcpi_ThermalZoneTemperature` | ❌ `Access denied` без admin; на цій платформі не є джерелом core temp |
| Perf-counter `Win32_PerfFormattedData_Counters_ThermalZoneInformation` | ✅ без admin: `\_TZ.THRM = 3212` (48.05 °C) — **ACPI zone**, не Tctl/Tdie |
| ADLX DLL | ✅ `amdadlx64.dll` 1.1.0.56 присутня; Rust-біндінги alpha |
| TrafficMonitor baseline | 7.18 % одного ядра, 40.3 MB WS, 105 MB private |

**Висновок:** safe mode реально покриває network, CPU %, CPU freq, RAM, GPU %, NVIDIA/AMD GPU temp (NVML/ADLX), **NVMe SSD temp**, disk activity — усе без admin. CPU/MB/SATA-HDD temps — тільки provider.

---

## 8. Температури: деталі провайдерів

### 8.1. HWiNFO Shared Memory (основний зовнішній)

- Object: `Global\HWiNFO_SENS_SM2`; mutex `Global\HWiNFO_SM2_MUTEX` (advisory).
- Header 48 B, signature `HWiS` (`0x53695748`), version 1/2, revision 0/1; strides брати з header (`dwSizeOfSensorElement`/`dwSizeOfReadingElement`), не хардкодити.
- Reading: `tReading`, `dwSensorIndex`, `dwReadingID`, labels, `szUnit[16]`, `Value` за offset `0x11C` — **unaligned `f64`**, читати `read_unaligned`.
- `poll_time` — секунди; 0 readings під час перепублікації — штатний стан; mapping перевідкривати при зміні counts.
- Валідація: magic, version, revision, strides 264..4096 / 316..4096, counts ≤ 4096/65536, offsets+size ≤ mapped size, NUL-terminated strings, finite f64, temp −40..150 °C, `poll_time` не старіший за N s.
- Free 12 h: після expiry mapping зникає → `Unavailable`, tooltip «HWiNFO Shared Memory вимкнено (12 h limit)».
- Ліцензія: читати SHM відкритим кодом можна (HWiNFO 7.0+); bundling/автообхід ліміту — ні.

### 8.2. LibreHardwareMonitor HTTP (основний безкоштовний)

- LHM 0.9.5+ **не має WMI**; актуально — `http://127.0.0.1:8085/metrics` (Prometheus) або `/data.json`.
- `/metrics` — кращий вибір: InvariantCulture, базові одиниці, менший payload.
- LHM потребує admin; сервер за замовчуванням слухає `0.0.0.0` без auth; ми підключаємося **тільки на loopback**, перевіряємо, що це localhost, і рекомендуємо `listenerIp=127.0.0.1`.
- Парсер: tolerant до `Value` string/числа, кома/крапка, дублікатів `SensorId`; alias-таблиця назв.
- `/data.json` — fallback, якщо `/metrics` недоступний; парсити дерево, `Value` — форматований рядок.
- WMI — лише legacy для LHM ≤0.9.4/OHM; не пріоритет.

### 8.3. Власний helper (Phase 4)

Два варіанти:

| Варіант | Плюси | Мінуси | Коли |
|---|---|---|---|
| **LHM-сайдкар (.NET)** | Швидко, повне покриття, перевірений LHM | 100–300 MB helper, .NET runtime, PawnIO, MPL | Для швидкого parity |
| **Rust + PawnIO** | Малий, повний контроль, без .NET | Треба писати/ліцензувати Pawn-модулі; GPLv2 helper; driver install | Довгостроково |

- Helper — окремий процес, named pipe, read-only; UI не elevated.
- Запуск: UAC або Task Scheduler (Highest) за явним вибором; не автоматично.
- «Game mode»: stop helper + hide widget; попередження про античити.
- Ліцензія: якщо helper лінкує PawnIO — helper під GPLv2 (з exception для IOCTL-модулів); UI залишається MIT/Apache і спілкується через IPC. Це окремі твори, але юридичну частину треба узгодити.

### 8.4. AIDA64 / Afterburner / Core Temp / vendor WMI

- AIDA64 — платний, без версії/timestamp; підтримувати як альтернативу, не основний.
- Afterburner MAHM — gaming fallback; `dwSrcId=0x80` CPU temp; залежить від GUI-конфігу.
- Core Temp — CPU-only, session-local.
- Vendor WMI (HP/Dell/Lenovo/ASUS) — opportunistic, model-specific; окремі feature-прапорці, не гарантія.

---

## 9. Безпека та приватність

### 9.1. Модель загроз

| Загроза | Вектор | Мітигація |
|---|---|---|
| EoP через writable DLL | Elevated exe + DLL у writable dir | UI `asInvoker`; DLL лише з System32/Program Files; `SetDefaultDllDirectories`; підпис/хеш |
| Підробка SHM | Admin-malware створює `Global\HWiNFO...` | Валідація magic/version/size/offsets/ranges; provenance; не керувати залізом за даними |
| Зловмисний helper | Підміна named pipe/процесу | ACL на pipe (user+SYSTEM), `PIPE_REJECT_REMOTE_CLIENTS`, handshake, PID/шлях, підпис helper |
| HTTP LHM | 0.0.0.0 без auth, POST /Sensor | Лише loopback; перевірка host; не використовувати remote; рекомендувати firewall |
| Supply chain | Залежності/бінарники | `cargo-deny`, `cargo-audit`, lockfile, vendoring, reproducible builds, SBOM |
| Anti-cheat false positive | Overlay/driver | No driver/injection; hide in fullscreen; Game mode; документація |
| Приватність | Мережеві запити | Немає телеметрії; update check — opt-in HTTPS; логи локальні |

### 9.2. Привілеї

- UI: `asInvoker`, Medium IL, PMv2.
- Helper: окремий процес, elevated лише за потреби; мінімальні права; не сервіс за замовчуванням.
- Немає `SeDebugPrivilege`, `SeLoadDriverPrivilege`, `SeCreateGlobalPrivilege` у UI.
- Немає `CreateRemoteThread`, `SetWindowsHookEx` з DLL, `ReadProcessMemory`, патчів Explorer.

### 9.3. IPC (helper)

- Named pipe `\\.\pipe\oxide-sensors-v1`, byte mode, length-prefixed JSON.
- DACL: поточний user + SYSTEM; `PIPE_REJECT_REMOTE_CLIENTS`; max instances 1.
- Handshake: protocol version, helper version, capabilities, nonce.
- Timeout 500 ms; max message 1 MB; sequence numbers; schema validation; numeric ranges.
- Helper не приймає команд керування залізом; тільки `get_snapshot`.
- UI ніколи не показує дані helper без перевірки `quality`/`timestamp`.

### 9.4. Anti-cheat stance (публічна)

> Oxide Monitor не встановлює kernel-драйверів і не інжектить код у гри. Базовий режим читає лише штатні лічильники Windows і vendor GPU API. Температури CPU/HDD можливі лише через опціональний зовнішній провайдер (HWiNFO/LHM) або opt-in helper із PawnIO; такі драйвери можуть блокувати EAC/BattlEye/Vanguard. У «Game mode» helper зупиняється, віджет ховається. Ми не гарантуємо сумісність із будь-яким античитом.

---

## 10. Продуктивність

### 10.1. Бюджет

| Показник | Ціль | Вимір |
|---|---:|---|
| CPU (idle, 1 Hz) | **< 0.3 % одного ядра** | `GetProcessTimes` за 60 s |
| CPU (graphs on) | < 0.8 % одного ядра | те саме |
| Working set | **< 30 MB** | `GetProcessMemoryInfo` |
| Private bytes | < 20 MB | те саме |
| Startup до першого кадру | < 150 ms | `QPC` |
| Пам'ять на історію | < 1 MB | 13 series × 60 × f32 |
| GPU wake-up | 0 у спокої | D2D DC render target, без D3D |
| Мережеві запити | 0 (крім update check) | монітор |

### 10.2. Техніки

- 1 Hz sampling; `SetCoalescableTimer`; без `WM_TIMER`-спаму.
- Immutable snapshots + `ArcSwap`; без locks у рендері.
- Попередньо виділений DIB; reuse text layouts; без алокацій на кадр.
- Dirty check за хешем моделі; графіки — лише зона.
- PDH: один query на метрику, `PdhCollectQueryData` раз на секунду; кешувати counter handles.
- NVML: ініціалізація один раз, `nvmlDeviceGetUtilizationRates`/`GetTemperature` раз на секунду; не тримати зайвих handle.
- External providers: окремі потоки, timeout, backoff; не блокувати sampler.
- Не використовувати `sysinfo` у production-шляху (зайві алокації/оновлення); тільки для прототипу.

### 10.3. Вимір

- `GetProcessTimes`/`QueryThreadCycleTime` + ETW; порівняння з baseline TrafficMonitor (7.18 %).
- CI perf test на фіксованому snapshot-фікстурі; регресія >20 % — fail.
- Ручний тест: 8 год роботи, resume, Explorer restart, 100–200 % DPI.

---

## 11. Конфігурація, історія, локалізація

### 11.1. Конфіг

- `%APPDATA%\OxideMonitor\config.toml` (roaming) + `history/`, `logs/` у `%LOCALAPPDATA%`.
- Portable mode: `config.toml` поруч із exe, якщо є `portable.flag` або `--portable`.
- `config_version` + міграції; atomic write (`ReplaceFileW`/`MoveFileExW`), backup `.bak`.
- Схема (скорочено):

```toml
version = 1

[general]
language = "uk"
autostart = true
theme = "auto"          # auto | dark | light
sampling_ms = 1000

[taskbar]
backend = "overlay"     # overlay | embed | tray
display_items = ["up","down","cpu","memory","gpu","cpu_temp","gpu_temp","hdd_temp"]
rows = 2
digits = 4
item_space = 8
speed_unit = "auto"     # auto | kb | mb
unit_byte = false
...

[providers]
hwinfo = true
lhm_http = true
helper = false
lhm_url = "http://127.0.0.1:8085/metrics"

[colors]
dark_preset = 0
light_preset = 3
```

- Міграція з `config.ini`: мапінг ключів (`tbar_display_item` бітова маска → список, `task_bar_speed_unit`, `font_*`, `*_string`, offsets); best-effort, звіт про невідомі ключі.

### 11.2. Історія трафіку

- Формат: компактний append-only (JSON lines або `postcard`), один запис на день: `{date, up_bytes, down_bytes}`.
- Атомарний запис раз на хвилину/при зміні; backup; відновлення після crash.
- Перегляд: день/тиждень/місяць/квартал/рік; log scale; Sunday first.
- Імпорт `history_traffic.dat` TrafficMonitor — Phase 5 (потрібен реверс формату).

### 11.3. Локалізація

- **Fluent (`.ftl`)**, а не JSON/TOML/YAML: природні множини/роди/пунктуація, плейсхолдери, чіткий fallback. Файли: `crates/oxide-core/locales/{en,uk,ru}.ftl`.
- `oxide_core::Localizer` вантажить вбудовані ресурси (`include_str!`), вибирає мову з `config.general.language`, EN — fallback. Відсутній ключ повертається як id, без паніки.
- EN/UK/RU у v1; `uk` — типова для українського користувача; решта — додається одним `.ftl` + варіантом `Language`.
- Перекладено: tray tooltip, меню, balloon приховування; далі — tooltips/налаштування/помилки.
- Формат чисел: locale-aware, але конфігурований десятковий розділювач; одиниці — `Mb/s`, `MB/s`, `°C`, `GHz`.

---

## 12. Плагіни та сумісність

- **v1:** без плагінів. Внутрішній `PluginHost` trait — пізніше.
- **v2:** зовнішній процес + JSON/named pipe; плагін повертає `ItemDescriptor` + `value/label/unit` раз на секунду. Ізоляція: падіння плагіна не валить UI.
- **Legacy C++ ABI v8:** не в v1. Якщо буде попит — окремий `oxide-legacy-bridge.exe` (C++), який завантажує старі DLL і перекладає їх у JSON; ризик нестабільності ізольований.
- **Скіни:** новий формат (TOML + PNG/WIC); importer старих `skin.ini`/`skin.xml` — Phase 5.
- **Конфіг/історія:** importer — Phase 5.

> **Ліцензійна гігієна.** TrafficMonitor розповсюджується під «Anti 996» license (MIT-подібна, але з labor-клаузами). Ми робимо clean-room реалізацію і не копіюємо його код; імпорт форматів конфігу/скінів — це сумісність, не похідна робота. Якщо колись знадобиться скопіювати код — треба зберегти цю ліцензію. LHM — MPL 2.0 (файловий copyleft), PawnIO — GPLv2 + IOCTL exception; helper із PawnIO має бути окремим GPLv2-твором.

---

## 13. Пакетування, автозапуск, оновлення

| Питання | Рішення |
|---|---|
| Формат | Portable ZIP + per-user MSI/EXE (без admin) |
| Каталог | `%LOCALAPPDATA%\Programs\Oxide Monitor` |
| Автозапуск | `HKCU\...\Run` (UI); helper — Task Scheduler `LeastPrivilege`/`Highest` за вибором |
| Підпис | Code signing certificate (EV або OV) для exe/installer; SmartScreen reputation |
| Оновлення | Opt-in HTTPS check (GitHub Releases), SHA-256 + підпис; без auto-download без згоди |
| Helper | Окремий інсталятор/компонент; UAC; явна згода; можливість видалити |
| Логи | `%LOCALAPPDATA%\OxideMonitor\logs`, UTF-8, ротація, без секретів |
| Crash | Мінідамп + символікація; не надсилати автоматично |

---

## 14. Тестування

### 14.1. Матриця

| Вісь | Значення |
|---|---|
| OS | Win10 22H2 (19045), Win11 23H2 (22631), 24H2 (26100), 25H2 (26200), 26H2/26300+ |
| DPI | 100/125/150/200 %, text scale 100/125/150 |
| Taskbar | bottom/top/left/right, auto-hide, centered/left icons, widgets on/off |
| Monitors | 1, 2 (різний DPI), secondary taskbar |
| Theme | dark/light, transparency on/off, accent, HDR |
| Power | sleep/resume, hibernate, lock/unlock |
| Network | Wi-Fi/Ethernet/VPN, adapter switch, disconnect |
| Explorer | restart, crash, shell reload |
| Providers | none, HWiNFO, LHM, helper down/stale/version mismatch |
| Games | fullscreen/borderless, EAC/BattlEye/Vanguard (manual) |

### 14.2. Автоматизація

- Unit: parsers (HWiNFO, LHM, config), formatting, layout, geometry.
- Fuzz: SHM/JSON parsers (`cargo-fuzz`).
- Integration: mock provider snapshots; golden-image рендер (DIB hash) для layout.
- Perf: CI benchmark + threshold.
- Security: `cargo-audit`, `cargo-deny`, `cargo-geiger` (optional), signed release check.

---

## 15. План розробки MVP

> Оцінки — в інженерних тижнях для одного досвідченого Rust/Win32 розробника; не календарні обіцянки.

| Фаза | Тривалість | Результат | Definition of Done |
|---|---:|---|---|
| **0. Universal native spike** | 1–2 тижні | Overlay + GDI + multi-adapter GPU/disk discovery; mock tests; x64 + ARM64 check | Віджет 294×35, 8 елементів, <1 % CPU, не ламає Explorer; `cargo check --target aarch64-pc-windows-msvc` |
| **1. Universal native MVP** | 3–4 тижні | Усі Tier-0 метрики, multi-GPU/multi-disk, vendor fallbacks, tray, меню, теми, DPI, autostart, логи, інсталятор | Працює на NVIDIA/AMD/Intel/Qualcomm/VM; CPU/MB/SATA `—`; NVMe/GPU temp native; <0.3 % CPU; Win10/11 x64+ARM64 |
| **2. Parity + i18n** | 3 тижні | Графіки, історія, per-item colors, labels, units, notifications, settings UI, Fluent EN/UK/RU | Візуальний паритет; повна українська локалізація |
| **3. External providers + Game Mode** | 2 тижні | HWiNFO SHM + LHM HTTP, перемикачі в треї/config, Game Mode, статус/деградація | Провайдери вимкнені за замовчуванням; `—` без них; Game Mode ховає віджет |
| **4. Temps (extended)** | 3–4 тижні | LHM-сайдкар або Rust+PawnIO helper, named pipe, security review | Opt-in helper; anti-cheat warning; UI unelevated |
| **5. Plugins/legacy** | 3 тижні | Plugin API, config/skin/history importer, optional legacy bridge | Плагін не валить UI; імпорт best-effort |

**MVP = фази 0–1 (universal Tier 0).** Зовнішні провайдери температур — **останній етап** (фаза 3), після стабільної нативної бази.

---

## 16. Ризики та мітигації

| Ризик | Ймовірність | Вплив | Мітигація |
|---|---|---|---|
| Win11 taskbar зміниться (26H2+) | Висока | Середній | Overlay не залежить від XAML; embed experimental; тести на Insider |
| Overlay z-order/fullscreen | Середня | Середній | Visibility policy; topmost reassert; Game mode |
| CPU/MB/SATA temp недоступні без провайдера | Висока | Високий (UX) | Чесний `—`, майстер налаштування, HWiNFO/LHM, helper; NVMe temp — native |
| HWiNFO free 12 h | Висока | Середній | Tooltip/статус; рекомендувати Pro; не обходити |
| LHM HTTP insecure default | Середня | Середній | Лише loopback; перевірка; рекомендації |
| PawnIO блокується античитом | Середня | Високий | Default off; Game mode; попередження; не гарантувати |
| PDH localization | Висока | Низький | `PdhAddEnglishCounterW`; fallback `GetSystemTimes` |
| NVML/ADLX API changes | Середня | Низький | Dynamic load; feature flags; graceful degradation |
| Performance regression | Середня | Середній | Budget + CI perf test |
| Supply chain | Низька | Високий | Lockfile, audit, signing, SBOM |
| Legal (PawnIO GPL, HWiNFO EULA) | Середня | Високий | Окремі helper/бінарники; no bundling; legal review |

---

## 17. Питання для узгодження

1. **Температурна стратегія.** Рекомендація: v1 — safe mode + зовнішні HWiNFO/LHM; v2 — opt-in helper. Варіанти:
   - A) Тільки safe mode (без CPU/SATA HDD temp) — найбезпечніше, але функціональна регресія.
   - A+) Safe + ACPI thermal zone як `CPU (ACPI)` (best-effort, off by default) — компроміс без драйвера.
   - B) Safe + HWiNFO/LHM (рекомендовано) — температури, якщо у користувача є провайдер.
   - C) B + LHM-сайдкар «з коробки» — повний parity, але admin/PawnIO.
   - D) B + власний Rust+PawnIO helper — довгостроково, більше роботи.
2. **Taskbar backend.** Overlay за замовчуванням (рекомендовано) чи embed, як у TrafficMonitor? Пропоную overlay + experimental embed.
3. **MVP scope.** Текстові 8 елементів без графіків (швидше) чи одразу графіки? Рекомендую text-only MVP, графіки — Phase 2.
4. **Settings UI.** TOML + tray menu для MVP (рекомендовано) чи одразу окреме вікно налаштувань (egui/native)?
5. **Плагіни.** Чи потрібна сумісність зі старими C++ плагінами TrafficMonitor? Рекомендую ні в v1.
6. **Ліцензія.** UI — MIT/Apache-2.0? Helper із PawnIO — GPLv2 окремо. Чи окремо?
7. **Цільові ОС.** Win10 22H2+ чи тільки Win11? ARM64 — зараз чи пізніше?
8. **Дистрибуція.** Portable ZIP + per-user installer? Чи потрібен MSI/Store?
9. **Локалізація.** EN/UK/RU достатньо для v1?
10. **Брендинг.** «Oxide Monitor» — фінальна назва?

---

## Додаток A. Рекомендований стек (станом на 17.11.2026)

| Крейт | Версія | Призначення | Примітка |
|---|---:|---|---|
| `windows` | 0.62.2 | Win32, COM, D2D, DWrite, PDH, IP Helper, Shell | `default-features = false`, лише потрібні features |
| `serde` | 1.0.229 | Конфіг/JSON | derive |
| `toml` | 1.2.1 | Конфіг | — |
| `serde_json` | 1.0.154 | IPC/діагностика | — |
| `tracing` | 0.1.44 | Логи | + `tracing-subscriber`, `tracing-appender` |
| `thiserror` | 2.0.21 | Помилки | — |
| `parking_lot` | 0.13.0 | Locks | — |
| `crossbeam-channel` | 0.5.17 | Канали | — |
| `arc-swap` | 1.x | Останній snapshot | — |
| `nvml-wrapper` | 0.13.0 | NVIDIA GPU | feature `nvidia`, dynamic load |
| `libloading` | 0.9.1 | ADLX/ADL/NVML | optional |
| `ureq` | 3.5.2 | LHM HTTP | blocking, без TLS для loopback |
| `time` | 0.3.55 | Дати/історія | — |
| `windows-registry` | 0.100.0 | Реєстр | або `RegGetValueW` напряму |
| `winreg` | 0.56.0 | Реєстр (альтернатива) | mature, але окрема залежність |
| `tray-icon` | 0.26.1 | Трей | або власний `Shell_NotifyIcon` |
| `egui`/`eframe` | 0.37.0 | Лише вікно налаштувань (опційно) | не для віджета |
| `skia-safe` | 0.157.0 | Не recommended | зайва вага |

**Не використовувати:** `sysinfo` у production (зайві алокації), `egui`/`wgpu` для віджета, `tokio` без потреби, in-process LHM.

### Cargo profile

```toml
[profile.release]
opt-level = "s"
lto = "thin"
codegen-units = 1
strip = true
panic = "unwind"   # UI: catch_unwind навколо провайдерів; helper — abort
```

---

## Додаток B. Джерела

- [`01_drivers_and_anticheat.md`](01_drivers_and_anticheat.md) — драйвери, CVE, блоклисти, EAC/BattlEye/Vanguard.
- [`02_native_windows_sensors.md`](02_native_windows_sensors.md) — PDH/WMI/NVML/ADLX/SMART, Rust-крейти, обмеження.
- [`03_shared_memory_providers.md`](03_shared_memory_providers.md) — HWiNFO/AIDA64/LHM/MAHM/CoreTemp, формати, ліцензії.
- [`04_taskbar_embedding_rust.md`](04_taskbar_embedding_rust.md) — DeskBand/SetParent/overlay, рендеринг, DPI, fullscreen.
- [`05_current_setup_audit.md`](05_current_setup_audit.md) — аудит `E:\TrafficMonitor`, конфіг, taskbar-вікно, безпека.

---

*Цей документ — архітектурне рішення для узгодження. Код застосунку на цьому етапі не створюється.*
