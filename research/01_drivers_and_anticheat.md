# Драйвери низькорівневого доступу до сенсорів Windows і сумісність з античітами

**Дата:** 2026-11-17  
**Проєкт:** Oxide Monitor — легкий Rust-монітор у таскбарі (мережа, CPU/GPU, RAM, SSD/HDD)  
**Контекст:** поточний кастомний TrafficMonitor (C++) використовує LibreHardwareMonitorLib 0.9.6 + PawnIO 2.1.0  
**Статус:** дослідницький звіт; прикладного коду немає. Усі твердження промарковано як **[ПІДТВЕРДЖЕНО]**, **[ПОВІДОМЛЕНО]** або **[НЕВІДОМО]**.

---

## 0. Головне (TL;DR)

1. **WinRing0/WinRing0x64 — непридатний.** Це старий драйвер (1.2.0/1.2.0.5, 1.3.0/1.3.1b, 2007–2009), який дає будь-якому процесу доступ до фізичної пам'яті, MSR та I/O-портів. Він має CVE-2020-14979 (CVSS 7.8) і десятки похідних CVE, внесений до Microsoft Vulnerable Driver Blocklist (правило за іменем `WinRing0.sys` до версії 2.0.0.0 + 29 правил `Deny` за hash/page-hash), детектується Defender як `VulnerableDriver:WinNT/Winring0`/`HackTool:Win32/Winring0`, і блокуюється EAC, BattlEye, Vanguard та FACEIT. **[ПІДТВЕРДЖЕНО]**
2. **PawnIO 2.1.0+ — найкращий відкритий kernel-варіант**, але це все одно встановлення драйвера. Офіційна збірка має **два підписи**: `namazso.eu` (EV, GLOBALTRUST) і `Microsoft Windows Hardware Compatibility Publisher`; обидва валідні, з timestamp 2026-02-11. Це **не self-signed** і не test-signed. **[ПІДТВЕРДЖЕНО локальною перевіркою]**
3. **Історія блокування PawnIO у FACEIT — це блокування за signer-сертифікатом, а не за «самопідписаним драйвером».** FACEIT заблокував старий сертифікат, який використовувався і для cheat-software; namazso випустив 2.1.0 з новим унікальним підписом, і 2026-02-12 закрив issue словами «This should be fixed as of 2.1.0». **[ПІДТВЕРДЖЕНО]** (деталі — розділ 4.5).
4. **InpOutx64 — непридатний.** Підписаний `Red Fox UK Limited` через старий VeriSign 2004 CA (термін дії минув), доданий до блоклиста хешами, блокуються Vanguard; дає лише port I/O (без MSR/фізичної пам'яті), тому CPU-температуру через нього не отримати. **[ПІДТВЕРДЖЕНО]**
5. **Вендорські драйвери — не панацея.** HWiNFO має власний Microsoft-підписаний драйвер (старі версії ≤8.98 заблоковані), AIDA64 має підписаний, але вразливий драйвер (CVE-2024-26507, arbitrary physical R/W), CPU-Z має блоклист за версією `cpuz.sys` ≤1.0.4.9, MSI Afterburner/RTCore64 — стару CVE-2019-16098. **[ПІДТВЕРДЖЕНО]**
6. **Реалістично безпечна база для Oxide Monitor — pure user-mode без драйвера**: мережа, CPU load, RAM, GPU load/temp через NVAPI/ADL/IGCL, SSD load. CPU package temperature (Tctl/Tdie або Intel DTS) без ring-0 недоступна; ACPI thermal zone — це компроміс, а не заміна. **[ПІДТВЕРДЖЕНО концептуально; локальні виміри нижче]**
7. **Рекомендація:** Tier 0 — user-mode за замовчуванням; Tier 1 — опційно PawnIO 2.1.0+ для CPU/материнки; Tier 2 — опційно HWiNFO shared memory; WinRing0/InpOutx64/AIDA64 — ніколи. Деталі — у матриці (розділ 10).

---

## 1. Як читати докази

| Маркер | Значення |
|---|---|
| **[ПІДТВЕРДЖЕНО]** | Первинне джерело (офіційна документація, код, перевірка підпису, вимір на цій машині) або відтворюваний факт. |
| **[ПОВІДОМЛЕНО]** | Правдоподібне вторинне джерело (форум, GitHub issue, цитата представника), але без незалежного підтвердження. |
| **[НЕВІДОМО]** | Надійних публічних даних немає; висновок неможливий. |

### 1.1. Локальні перевірки на цій машині (2026-11-17)

| Параметр | Значення | Статус |
|---|---|---|
| ОС | Windows 11 Pro 26H2, build 26300, x64 | **[ПІДТВЕРДЖЕНО]** |
| CPU | AMD Ryzen 5 5600H (6C/12T), AuthenticAMD | **[ПІДТВЕРДЖЕНО]** |
| VBS / HVCI | `VirtualizationBasedSecurityStatus=2`, `SecurityServicesRunning={2}` — HVCI увімкнено | **[ПІДТВЕРДЖЕНО]** |
| Vulnerable Driver Blocklist | `VulnerableDriverBlocklistEnable=1` | **[ПІДТВЕРДЖЕНО]** |
| Версія блоклиста | `10.0.29545.0`, PolicyID `{C017F6D6-2021-4204-AA68-F5065969B65E}` | **[ПІДТВЕРДЖЕНО]** |
| PawnIO | 2.1.0, служба `PawnIO` RUNNING, INF `02/02/2026,2.1.0.0`, `Root\PawnIO`, demand-start | **[ПІДТВЕРДЖЕНО]** |
| PawnIO.sys | 136 496 байт, SHA-256 `F92DE04E5A02256E86FFA1B4252FE669282198B7329227F40E51BCA9267D133D` | **[ПІДТВЕРДЖЕНО]** |
| Підписи PawnIO.sys | Sig 0: `namazso.eu` (GLOBALTRUST 2015 CODESIGNING 1), timestamp 2026-02-11 15:41:28; Sig 1: `Microsoft Windows Hardware Compatibility Publisher`, timestamp 2026-02-11 19:06:36; обидва `Valid`; `PawnIO.cat` також Microsoft-підписаний | **[ПІДТВЕРДЖЕНО]** |
| PawnIO у блоклисті | Відсутній (ні за хешем, ні за іменем) | **[ПІДТВЕРДЖЕНО]** |
| ACPI thermal zone | `Win32_PerfFormattedData_Counters_ThermalZoneInformation`: `\_TZ.THRM` = 3212 (деци-Kelvin → 48.05 °C), без admin; `MSAcpi_ThermalZoneTemperature` — `Access denied` без admin | **[ПІДТВЕРДЖЕНО]** |
| Storage WMI | `Get-StorageReliabilityCounter` для Apacer NVMe та HGST SATA повернув порожню `Temperature` без admin (причина може бути і в прошивці/драйвері, не лише в правах) | **[ПІДТВЕРДЖЕНО локально]** |
| Поточний процес | не elevated (`IsInRole(Administrator)=False`) | **[ПІДТВЕРДЖЕНО]** |

---

## 2. Навіщо взагалі драйвер

Windows не дає user-mode-процесу виконувати `RDMSR`/`WRMSR`, `IN`/`OUT` або відображати фізичну пам'ять. А майже всі «справжні» сенсори CPU/плати — саме там:

- **Intel CPU temperature**: `IA32_THERM_STATUS` (MSR 0x19C), `IA32_TEMPERATURE_TARGET` (0x1A2), `IA32_PACKAGE_THERM_STATUS` (0x1B1). Потрібен `RDMSR`.
- **AMD Ryzen (Family 17h/19h)**: Tctl/Tdie читаються через SMN (System Management Network) за фіксованими адресами; потрібен доступ до PCI config/MMIO або спеціальних MSR-подібних інтерфейсів.
- **Материнська плата, VRM, RAM SPD, вентилятори**: SMBus (Intel PCH SMBus або AMD FCH), LPC/EC (Embedded Controller), ISA I/O.
- **SSD/HDD SMART і temperature**: NVMe Admin Command / ATA PASS-THROUGH через `IOCTL_STORAGE_*`; це user-mode API, але зазвичай потрібні права адміністратора на відкриття `\\.\PhysicalDriveN`.

User-mode альтернативи:

- **GPU**: NVAPI (`nvapi64.dll`), AMD ADL (`atiadlxx.dll`), Intel IGCL — це звичайні DLL, без драйвера і без admin. Дають GPU core/hotspot/memory temperature, load, clocks, power. **[ПІДТВЕРДЖЕНО практикою LHM і DeskMon]**
- **CPU load, RAM, network, disk load**: `NtQuerySystemInformation`, PDH, `GetIfTable2`, `GlobalMemoryStatusEx` — без драйвера.
- **ACPI thermal zone**: `MSAcpi_ThermalZoneTemperature` або perf-counter `Win32_PerfFormattedData_Counters_ThermalZoneInformation`. Це **не** Tctl/Tdie і не per-core; значення залежить від firmware, часто статичне або показує зону плати. На цій машині perf-counter віддав 48.05 °C без admin, а `MSAcpi_*` — `Access denied`. **[ПІДТВЕРДЖЕНО локально; обмеження — [ПОВІДОМЛЕНО] численними джерелами]**

Тому архітектурна розвилка проста: або ми взагалі не показуємо CPU package temperature, або приймаємо kernel-драйвер (PawnIO / чужий підписаний драйвер / shared memory).

---

## 3. WinRing0 / WinRing0x64

### 3.1. Що це і які версії

WinRing0 — бібліотека OpenLibSys (автор Noriyuki Miyazaki, він же автор CrystalDiskMark/CrystalCPUID). Останні публічні версії: 1.2.0/1.2.0.5 (2007–2008), 1.3.0 (березень 2009), 1.3.1b (грудень 2009). У 2010 проєкт закрито; WinRing0 2.0.0 так і не вийшов. **[ПІДТВЕРДЖЕНО для версій; [ПОВІДОМЛЕНО] для 2.0.0]** (Crystal Dew Blog, OpenLibSys.org).

Драйвер створює пристрій `\Device\WinRing0_1_2_0` і приймає IOCTL, які дозволяють:

- читати/писати фізичну пам'ять (`MmMapIoSpace`, `\Device\PhysicalMemory`);
- читати/писати MSR (`__rdmsr`/`__wrmsr`, зокрема LSTAR — класичний шлях до ring-0 shellcode);
- виконувати `IN`/`OUT` на довільних портах;
- читати/писати PCI config space.

Ключова проблема — не «баг», а дизайн: драйвер не має жодного ACL і не обмежує діапазони. Будь-який процес, включно з low-integrity, може відкрити пристрій і попросити ядро зробити будь-що. **[ПІДТВЕРДЖЕНО]** (NVD, аналіз Starkeblog для InpOut, SOPHOS для RTCore64 — той самий клас).

### 3.2. CVE

| CVE | Суть | Дата / деталі |
|---|---|---|
| CVE-2020-14979 | `WinRing0.sys`/`WinRing0x64.sys` 1.2.0 (EVGA Precision X1 ≤1.0.6): читання/запис довільної пам'яті, мапінг `\Device\PhysicalMemory`, LPE до SYSTEM | 7.8 High; опубл. 2020-08-11 |
| CVE-2019-7240 | Moo0 System Monitor 1.83: `wrmsr` через IOCTL 0x9C402088 без фільтрації MSR | 2020-03-25 |
| CVE-2017-14311 | NetMechanica NetDecision 5.8.2: LPE через IOCTL 0x9C402088 | 2017-09-19 |
| CVE-2020-13509…13519 | NZXT CAM 4.8.0: серія info-leak/LPE через різні IRP/IOCTL WinRing0x64 | 2020-12-18 |
| CVE-2020-22057 | EVGA Precision XOC 6.2.7: default security descriptor | 2021-12-28 |
| CVE-2021-44899 / 44901 / 44903 | MSI Center/Dragon Center/Center Pro: LPE через WinRing0 та інші драйвери | 2022-02-04 |
| CVE-2023-1047 / 1048 | RealTemp 3.7, Ryzen DRAM Calculator 1.2.0.5: WinRing0x64 | 2023-02-26 |
| CVE-2025-63602 | Awesome Miner ≤11.2.4: WinRing0 1.2.0.5 під іменем `IntelliBreeze.Maintenance.Service.sys`, небезпечний DACL | 2025-11-18 |
| CVE-2026-82628 | Colorful iGameCenter 2.0.0.81: WinRing0x64, неправильні права | 2026-08-31 |
| CVE-2026-129255 | CoolerMaster MASTERPLUS+ 1.9.6: WinRing0x64, default permissions | 2026-11-14 |

Це не повний список; важливо, що нові CVE продовжують з'являтися у 2025–2026, бо драйвер досі масово вбудовують у софт. **[ПІДТВЕРДЖЕНО]** (NVD API, запит `keywordSearch=WinRing0`, 2026-11-17).

### 3.3. Підпис

WinRing0 1.2.0.5 підписаний **сертифікатом автора** (Noriyuki Miyazaki / CrystalMark; у метаданих драйвера фігурує `OpenLibSys.org`), а не Microsoft. Це стара pre-2015 схема підпису, яку Windows досі приймає для сумісності; сертифікат давно минув. LHM-мейнтейнер прямо писав: «Yes, it's signed (by Noriyuki MIYAZAKI from CrystalMark), otherwise it wouldn't have worked… new drivers need to be signed by MS». The Verge: «its author Noriyuki Miyazaki apparently signed it himself». **[ПОВІДОМЛЕНО; узгоджено двома незалежними джерелами]**

Це важливо для розуміння різниці: WinRing0 — **не** WHQL і **не** Microsoft-signed; він виживає лише як legacy-сумісність. PawnIO — навпаки, Microsoft-signed (розділ 4.2).

### 3.4. Блоклист і Defender

У Microsoft Vulnerable Driver Blocklist (версія 10.0.29545.0, завантажена 2026-11-17 через `aka.ms/VulnerableDriverBlockList`) є:

- `<FileAttrib FriendlyName="WinRing0.sys" FileName="WinRing0.sys" MinimumFileVersion="0.0.0.0" MaximumFileVersion="2.0.0.0" />` — **блокування за іменем і версією**, а не лише за хешем;
- 29 правил `Deny` для `WinRing0.sys`, `WinRing0x64.sys`, `WinRing0_1_2_2.sys`, `iFlyWinRing0x64.sys` з SHA-1, SHA-256 і page-hash варіантами.

Це означає: перейменування файлу на `OpenHardwareMonitorLib.sys` допомагає не завжди — ReliaQuest спостерігав саме таке маскування, і LOLDrivers додав хеші. **[ПІДТВЕРДЖЕНО]** (XML блоклиста; LOLDrivers; issue #253).

Defender:

- `VulnerableDriver:WinNT/Winring0` — Microsoft Support прямо пише: «This detection is valid. The WinRing0 driver has been classified as a known vulnerability as documented in CVE-2020-14979». **[ПІДТВЕРДЖЕНО]**
- Варіанти `.D` (2025-05-20) і `.G` (2025-07-21) — у Microsoft Security Intelligence; `.G` описаний як «malicious version of a legitimate kernel-mode driver». **[ПІДТВЕРДЖЕНО]**
- `HackTool:Win32/Winring0` — хвиля березня 2025, коли Defender почав масово карантинити WinRing0 у FanControl, Razer Synapse, SteelSeries Engine, OpenRGB, LHM, MSI Afterburner тощо. **[ПІДТВЕРДЖЕНО]** (The Verge 2025-03-13; Neowin; FanControl #3016).
- `Trojan:Win32/Vigorf.A` — інша хвиля (вересень 2025), generic-детект, який часто ловить саме WinRing0-файли; Microsoft не підтверджувала його як «валідний» для конкретного файлу. **[ПОВІДОМЛЕНО]**

Важливо: це не «false positive» у строгому сенсі. Файл справді є уразливим драйвером; питання лише в тому, чи є конкретний екземпляр malware. Microsoft у статті про `VulnerableDriver:WinNT/Winring0` пропонує exclusion, але попереджає, що це послаблює захист. **[ПІДТВЕРДЖЕНО]**

### 3.5. Чому саме блокують

Тому що WinRing0 — ідеальний BYOVD-примітив:

1. він підписаний і тому завантажується на stock Windows;
2. він дає arbitrary physical memory read/write → можна знайти й перезаписати kernel-структури;
3. він дає arbitrary MSR write → можна перезаписати `LSTAR` і виконати свій код у ring-0;
4. він не має ACL → працює з будь-якого low-integrity процесу;
5. його важко відрізнити від легітимного монітора, бо легітимний монітор робить те саме.

Саме тому Microsoft, EAC, BattlEye, Vanguard і FACEIT ставляться до нього однаково: це не «поганий застосунок», це «поганий примітив». **[ПІДТВЕРДЖЕНО]** (Microsoft, EAC, BE, FACEIT, NVD).

### 3.6. Висновок

WinRing0 не можна використовувати в Oxide Monitor у жодному вигляді: ні як embedded resource, ні як fallback, ні «тимчасово для тестів». Це гарантований конфлікт з Defender, EAC/BE/Vanguard і блоклистом, а також реальна вразливість. **[ПІДТВЕРДЖЕНО]**

---

## 4. PawnIO

### 4.1. Архітектура

PawnIO — це не «WinRing0 з іншим ім'ям». Це маленький kernel-драйвер із вбудованим інтерпретатором Pawn (PawnPP, C++), який виконує **підписані модулі** — аналоги «міні-драйверів» на байткоді. **[ПІДТВЕРДЖЕНО]** (pawnio.eu, GitHub namazso/PawnIO).

Схема:

```
user-mode app
   │  PawnIOLib.dll (C ABI: pawnio_open/load/execute/close)
   ▼
\Device\PawnIO  (kernel driver, root-enumerated software device)
   │  IOCTL_PIO_LOAD_BINARY   — завантажити підписаний модуль у VM
   │  IOCTL_PIO_EXECUTE_FN    — викликати ioctl_* функцію модуля
   │  IOCTL_PIO_VERSION       — версія драйвера
   ▼
Pawn VM → module → natives (msr_read/write, io_in/out, mmio, pci, …)
```

Ключові факти з коду:

- Device type `41394`, device path `\Device\PawnIO`; IOCTL-и `IOCTL_PIO_LOAD_BINARY` (function code 0x821), `IOCTL_PIO_EXECUTE_FN` (0x841), `IOCTL_PIO_VERSION` (0x861), усі `METHOD_BUFFERED`, `FILE_ANY_ACCESS`. **[ПІДТВЕРДЖЕНО]** (`PawnIO/include/pawnio_um.h`).
- Кожен `IRP_MJ_CREATE` відкриває новий VM-контекст; `IOCTL_PIO_LOAD_BINARY` вантажить один модуль на file object; `IOCTL_PIO_EXECUTE_FN` викликає named function. **[ПІДТВЕРДЖЕНО]** (`driver.cpp`).
- Формат модуля: `u32 signatureSize; u8 signature[signatureSize]; u8 pawnModule[];`. Підпис — RSA PKCS#1 v1.5 + SHA-256, перевіряється через `BCryptVerifySignature`. **[ПІДТВЕРДЖЕНО]** (`signature.cpp`, `vm.cpp`).
- Довірені ключі — статичний масив `k_trusted_keys[] = { {k_pubkey_namazso_2023, …}, {} }`; тобто ключ **pinned** у драйвері, а не «будь-який валідний Authenticode». **[ПІДТВЕРДЖЕНО]** (`vm.cpp`).
- У збірці `PAWNIO_UNRESTRICTED` перевірка підпису ігнорується (`DbgPrint` + `status = STATUS_SUCCESS`). Це та сама «Unrestricted edition» з сайту. **[ПІДТВЕРДЖЕНО]** (`vm.cpp`).
- Офіційні модулі — не «довільний доступ»: наприклад, `IntelMSR.p` має whitelist MSR для read і окремий whitelist для write (turbo/power limits, OC mailbox, prefetcher). `LpcIO.p`, за словами мейнтейнера LHM, обмежує адреси LPC. **[ПІДТВЕРДЖЕНО для IntelMSR; [ПОВІДОМЛЕНО] для LpcIO]** (`PawnIO.Modules`).

Тобто модель безпеки така: **драйвер дає потужні примітиви, але офіційна збірка дозволяє лише модулі, підписані ключем namazso, а модулі звужують інтерфейс до конкретних операцій**. Це суттєво краще за WinRing0, але це не «безпечно за замовчуванням»: довіра зміщується на namazso та його ключ. **[ПІДТВЕРДЖЕНО архітектурно; оцінка ризику — аналітична]**

### 4.2. Підпис: факти проти міфу

Поширена теза «PawnIO підписаний самопідписаним сертифікатом» **хибна** для Official edition. Локальна перевірка `signtool verify /pa /all /v` на `PawnIO.sys` 2.1.0:

```
Signature Index: 0 (Primary Signature)
  Issued to: namazso.eu
  Issued by: GLOBALTRUST 2015 CODESIGNING 1
  The signature is timestamped: Wed Feb 11 15:41:28 2026

Signature Index: 1
  Issued to: Microsoft Windows Hardware Compatibility Publisher
  Issued by: Microsoft Windows Third Party Component CA 2014
  The signature is timestamped: Wed Feb 11 19:06:36 2026
```

Обидва підписи `Valid`; файл має page hashes; `PawnIO.cat` також підписаний Microsoft WHCP. **[ПІДТВЕРДЖЕНО локально]**

Що це означає:

- це **Microsoft-signed** драйвер (WHCP), а не self-signed;
- це не test-signing: для завантаження не потрібно вимикати Secure Boot чи вмикати `testsigning`;
- «WHQL» vs «attestation»: обидва шляхи дають сертифікат `Microsoft Windows Hardware Compatibility Publisher`. З файлу неможливо довести, чи це повний HLK/WHQL, чи автоматичний attestation-підпис. Перевірка EKU це підтверджує: і `namazso.eu`, і Microsoft WHCP мають одночасно `1.3.6.1.4.1.311.10.3.5` (WHQL) і `1.3.6.1.4.1.311.10.3.5.1` (attestation), тому за сертифікатом шлях не визначити. Для індивідуального розробника з EV-сертифікатом імовірніший **attestation**. Тому коректне формулювання: «Microsoft-attested/WHCP-signed», а не «100% WHQL-certified». **[ПІДТВЕРДЖЕНО для EKU; HLK vs attestation — [НЕВІДОМО]]**
- self-signed/test-signed є лише **Unrestricted edition** (окрема збірка для розробки, потребує test-signing і несумісна з античітами). **[ПІДТВЕРДЖЕНО]** (pawnio.eu, `vm.cpp`).

Історичний нюанс: до 2.1.0 драйвер використовував інший publisher-сертифікат (не Microsoft), і саме це стало причиною блокування FACEIT (розділ 4.5). Але навіть тоді це був не «self-signed», а сертифікат сторонньої компанії (REAPS s.r.o.), використаний для Microsoft-подання. **[ПОВІДОМЛЕНО]** (коментар namazso 2023-06-13; деталі — нижче).

### 4.3. Що змінилося у 2.x

Публічна історія релізів (GitHub `namazso/PawnIO.Setup`):

| Версія | Дата | Ключові зміни |
|---|---|---|
| 2.0.0 | 2025-05-24 | Перший публічний 2.x: інсталятор, офіційна/Unrestricted збірки, PawnIOLib |
| 2.0.1 | 2025-06-29 | Fix DLL hijack `CRYPTBASE.dll`, ARM64 install, uninstall.exe, zstd замість UPX |
| 2.1.0 | 2026-02-12 | **Новий унікальний signer** (EV `namazso.eu`), installer через Device API; виправлення сумісності з FanControl V259; FACEIT issue закрито 2026-02-12 |
| 2.2.0 | 2026-03-15 | Нові natives для динамічного виклику, PnP-подібне створення device, відновлення старого device path, fix BSOD на Win10 RS5/19H1/19H2, опційне відкриття device для non-admin (не рекомендовано) |

**[ПІДТВЕРДЖЕНО]** (GitHub API/release notes; issue #1; дати публікації).

Що це означає практично:

- 2.1.0 — «вододіл» для античітів: саме в ній з'явився новий signer, який FACEIT більше не блокує.
- 2.2.0 — покращення сумісності; для нашого проєкту краще орієнтуватися на ≥2.2.0 (але 2.1.0 теж прийнятна).
- Модулі LHM оновлені до 2.2 у LHM 0.9.6 (PR #2174), тому зв'язка LHM 0.9.6 + PawnIO 2.2.0 — актуальна. LHM 0.9.6 також містить фікс сумісності з PawnIO 2.1.0 (PR #2222, merged 2026-02-13), що відповідає конфігурації на цій машині. **[ПІДТВЕРДЖЕНО]** (release notes; PR #2222).

### 4.4. Встановлення і модель доступу

- Інсталятор `PawnIO_setup.exe` (підписаний `namazso.eu`, за даними стороннього аналізу — сертифікат до 2027-08-05). **[ПОВІДОМЛЕНО]**; локально перевірено лише `PawnIO.sys` і `.cat`.
- INF: `Class=SoftwareDevice`, `Root\PawnIO`, `CatalogFile=PawnIO.cat`, `DriverVer=02/02/2026,2.1.0.0`, `StartType=3` (demand-start). **[ПІДТВЕРДЖЕНО локально]**
- Служба `PawnIO` — kernel driver, запускається на вимогу (коли застосунок відкриває device). На цій машині вона RUNNING. **[ПІДТВЕРДЖЕНО]**
- Device за замовчуванням **не призначений для non-admin**: у 2.2.0 з'явилася опція відкрити device для non-admin, але release notes кажуть «not recommended». У коментарях до LHM #984 згадується SDDL `SDDL_DEVOBJ_SYS_ALL_ADM_ALL` (SYSTEM/Administrators), але це твердження коментатора, не цитата автора. **[ПІДТВЕРДЖЕНО для 2.2.0 release notes; [ПОВІДОМЛЕНО] для SDDL]**
- Наслідок: **non-elevated taskbar-застосунок не може напряму читати CPU temp через PawnIO**. Потрібен або запуск від адміністратора, або окремий elevation-service (як роблять OpenRGB 1.0 і CaptureEngine), або UI читає дані з привілейованого helper-процесу. **[ПІДТВЕРДЖЕНО архітектурно; приклади — [ПОВІДОМЛЕНО]]**
- Встановлення потребує admin. Автооновлення немає: користувач має запускати новий setup. **[ПІДТВЕРДЖЕНО]** (pawnio.eu; w0a FAQ).

### 4.5. Історія з FACEIT (і чому це важливо)

Хронологія:

1. Вересень 2025: після переходу FanControl V238 на PawnIO користувачі FACEIT почали отримувати «forbidden driver» `PawnIO.sys`. FACEIT AC Team (цитата, передана в issue #1): «PawnIO was signed using a certificate which is also used by cheat software, which is why it is blocked. It will need to be signed with a legit certificate for it to be allowed.» **[ПОВІДОМЛЕНО — цитата FACEIT, передана через GitHub issue]**
2. namazso підтвердив: «It seems that the signer used on PawnIO gets us blocked… I'd rather concentrate on getting a new signer», і додав, що до роботи в античіт-індустрії працював у cheat-індустрії (це публічно видно з його GitHub). **[ПІДТВЕРДЖЕНО]** (issue #1, body + edit).
3. У 2023 namazso пояснював, що підпис тодішньої збірки надала `reaps.gg` — «in-development software licensing (DRM) platform of a friend (hence the driver signing cert)». Тобто йшлося про publisher-сертифікат сторонньої компанії, а не про self-signed. **[ПОВІДОМЛЕНО]** (коментар у LHM #984, 2023-06-13).
4. 2026-02-12: namazso закрив issue: «This should be fixed as of 2.1.0». Локальний 2.1.0 має новий підпис `namazso.eu`. **[ПІДТВЕРДЖЕНО]**
5. FACEIT support-стаття тепер каже: `PawnIO.sys` → «Please update PawnIO to the latest version». **[ПІДТВЕРДЖЕНО]** (оновлено 2026-10-14).
6. Незалежне спостереження: «PawnIO 2.2.0 ran beside FACEIT through played matches on one machine; that is one observation, not a guarantee». **[ПОВІДОМЛЕНО]** (w0a FAQ, 2026-11-06).

Головний висновок: **блокування було за signer-сертифікатом, а не за «самопідписаним драйвером» і не за вразливістю коду**. Це прецедент, який треба враховувати: античіт може заблокувати **будь-який** драйвер за рішенням вендора, навіть якщо технічно він безпечний. **[ПІДТВЕРДЖЕНО]**

### 4.6. Defender, HVCI, Secure Boot

- **Microsoft blocklist:** PawnIO відсутній у версії 10.0.29545.0 — ні за іменем, ні за хешем. **[ПІДТВЕРДЖЕНО локально]**
- **Defender:** документованих детектів `PawnIO.sys` не знайдено (станом на 2026-11-17). Це не доказ вічної безпеки; це лише відсутність доказів. **[НЕВІДОМО/відсутність даних]**
- **HVCI/Memory Integrity:** на цій машині HVCI увімкнено, blocklist увімкнено, PawnIO 2.1.0 працює. Тобто сумісність підтверджена емпірично для цієї конфігурації. **[ПІДТВЕРДЖЕНО локально]**
- **Secure Boot:** official edition не потребує вимкнення Secure Boot; Unrestricted/test-signed — потребує, що автоматично ламає Vanguard/FACEIT. **[ПІДТВЕРДЖЕНО]** (pawnio.eu; FACEIT/w0a).
- **ASR rule** «Block abuse of exploited vulnerable signed drivers» (GUID `56a863a9-875e-4185-98a7-b882c64b5ce5`) блокує **запис** відомого вразливого драйвера на диск, а не завантаження вже наявного; PawnIO під це правило не підпадає. **[ПІДТВЕРДЖЕНО]** (Microsoft ASR docs).

### 4.7. Обмеження і ризики

1. **Один мейнтейнер.** PawnIO — проєкт namazso; bus factor = 1. Якщо він зупиниться, модулі/оновлення/підписання зупиняться. **[ПІДТВЕРДЖЕНО]** (структура проєкту).
2. **Підписання модулів — вузьке місце.** Офіційна збірка приймає лише модулі, підписані ключем namazso. Якщо Oxide Monitor потребує сенсора, якого немає в офіційних модулях, треба або upstream-ити модуль у `PawnIO.Modules` і чекати підпису, або використовувати Unrestricted (test-signing → античіти ламаються). Це суттєве архітектурне обмеження. **[ПІДТВЕРДЖЕНО]** (`vm.cpp`, `PawnIO.Modules`).
3. **Драйвер — це kernel attack surface.** Навіть якщо модулі обмежені, інтерпретатор у ring-0 — це велика довірена база. Дослідники відзначали: «With PawnIo, your threat model is @namazso with native admin execution on your device». **[ПОВІДОМЛЕНО]** (LHM #984, коментарі 2025-09).
4. **Write-можливості модулів.** `IntelMSR` дозволяє записи в turbo/power/OC MSR; `LpcIO` — у LPC. Для read-only монітора це не потрібно, але воно є в підписаних модулях. Використовуйте лише read-функції. **[ПІДТВЕРДЖЕНО]** (`IntelMSR.p`).
5. **Ліцензія.** Драйвер — GPLv2 з винятком для модулів, що спілкуються лише через IOCTL; модулі — LGPL-2.1. Якщо Oxide Monitor лише викликає IOCTL, виняток застосовується; якщо вбудовує/розповсюджує драйвер — треба виконувати GPL-вимоги (джерела, повідомлення). **[ПІДТВЕРДЖЕНО]** (COPYING, README).
6. **Вартість/складність.** Потрібен installer або інструкція, admin, перевірка версії, обробка відсутності драйвера, elevation-service для non-admin UI. Це більше, ніж «просто прочитати MSR». **[ПІДТВЕРДЖЕНО практикою OpenRGB/CaptureEngine/LHM]**

### 4.8. Висновок

PawnIO 2.1.0+ — **найкращий доступний відкритий kernel-варіант** для CPU/материнських сенсорів: Microsoft-підписаний, не в блоклисті, сумісний з HVCI, FACEIT-сумісний з 2.1.0. Але це все одно драйвер, і його безпека/сумісність залежать від політики античітів і від одного мейнтейнера. Для Oxide Monitor це має бути **опційний Tier 1**, а не обов'язкова залежність. **[АНАЛІТИЧНИЙ ВИСНОВОК на основі підтверджених фактів]**

---

## 5. InpOutx64 / InpOut32

### 5.1. Підпис і походження

- InpOut32/InpOutx64 — legacy-проєкт Філа Гіббонса (highrez.co.uk), порт Logix4U. Автор прямо пише: «This project is legacy and no longer actively supported… I am unable to build and sign the driver».
- Драйвер підписаний `Red Fox UK Limited` через `VeriSign Class 3 Code Signing 2004 CA`; це старий, давно минулий сертифікат, не EV і не Microsoft. **[ПІДТВЕРДЖЕНО]** (Starkeblog аналіз; highrez forum).
- Автор підтверджує: сертифікат не відповідає сучасним вимогам; перепідписати він не може. **[ПІДТВЕРДЖЕНО]** (highrez forum, 2018).

### 5.2. Блоклист і античіти

- У Microsoft blocklist є численні `Deny`-правила для `inpoutx` (SHA-1, SHA-256, page hashes) — підтверджено у XML 10.0.29545.0. **[ПІДТВЕРДЖЕНО]**
- Riot Vanguard блокує `inpoutx64.sys`: це прямо згадано як приклад у Riot support-статті «VAN: Incompatible Software»; FanControl issue #1694 (2023) «inpoutx64.sys is getting blocked by Riot Vanguard»; OpenRGB dev: «We used to use InpOut32, but it was conflicting with Riot's Vanguard anti-cheat, so we switched to WinRing0». **[ПІДТВЕРДЖЕНО]** (три незалежні джерела).
- EAC/BattlEye: InpOut є у MS blocklist (за конкретними хешами), тому EAC, який каже «we use the Microsoft blocklist», блокує відповідні збірки; BattlEye блокує драйвери з відомими security issues. **[ПІДТВЕРДЖЕНО для EAC; [ПОВІДОМЛЕНО] для BE]**

### 5.3. Безпека і можливості

- Драйвер створює device з DACL `Everyone`, тому будь-який користувач може викликати `IN`/`OUT`. Це LPE-примітив (Starkeblog показав 8 IOCTL, включно з `out` на довільний порт і `SMI 0xb2`). **[ПІДТВЕРДЖЕНО]**
- Дає **лише port I/O**. Немає MSR, немає фізичної пам'яті, немає PCI config. CPU temperature через MSR — неможливо; теоретично можна читати EC через порти 0x60/0x64, але це board-specific і не дає надійного Tctl/Tdie. **[ПІДТВЕРДЖЕНО технічно]**
- Тому InpOut не підходить для CPU/GPU temp; він годиться хіба що для RGB/SMBus-подібних задач, і то з ризиками.

### 5.4. Висновок

Заборонено. Навіть якщо колись він був «простим способом», сьогодні це блоклист + Vanguard + застарілий підпис + LPE. **[ПІДТВЕРДЖЕНО]**

---

## 6. Вендорські драйвери і shared memory

### 6.1. Порівняльна таблиця

| Продукт | Драйвер | Підпис | MS blocklist | Що дає | Античіт-статус | Висновок |
|---|---|---|---|---|---|---|
| **HWiNFO** | `hwinfo64a.sys` / `HWiNFO_x64_*.sys` (тимчасовий, demand-load) | Microsoft-signed (заява автора); WHQL vs attestation — **[ПОВІДОМЛЕНО]** | старі `HWiNFO32/64A/64I.SYS` ≤ **8.98.0.0** заблоковані за іменем/версією; поточні — ні | повний набір: CPU package/cores, GPU, плата, RAM, SSD | EAC/BE: зазвичай OK (немає в blocklist); Vanguard: **[НЕВІДОМО]** | Найкращий «чужий» драйвер; SHM — опційна інтеграція |
| **AIDA64** | `kerneld.x64` / `aida64x.sys` (`\Device\AIDA64Driver`) | FinalWire (Authenticode); Microsoft attestation — **[НЕВІДОМО]** | принаймні одна відома збірка `aida64.sys` (SHA-256 `273753…`) є в XML під міткою «Firewire kerneld»; інші збірки — не знайдені | повний набір | CVE-2024-26507 (arbitrary phys R/W, device без ACL); EAC/BE — ризик; Vanguard — **[НЕВІДОМО]** | **Уникати** |
| **CPU-Z** | `cpuz.sys` | CPUID S.A.R.L.U. (DigiCert EV) | `cpuz.sys` ≤ **1.0.4.9** заблокований за іменем/версією | інформація про CPU/плату, не моніторинг | EAC/BE — зазвичай OK для нових; Vanguard — **[НЕВІДОМО]** | Не підходить як джерело сенсорів |
| **MSI Afterburner** | `RTCore64.sys` | MSI (Authenticode) | старі версії — hash-deny; CVE-2019-16098 | GPU temp/load/fan, обмежено CPU | FACEIT: «latest fully compatible»; EAC/BE — зазвичай OK; Vanguard — **[НЕВІДОМО]** | Лише GPU; не як ядро монітора |
| **RTSS** | немає kernel-драйвера (user-mode hooks) | — | — | FPS/overlay | overlay можуть блокувати EAC/BE в окремих іграх | Не потрібен для таскбара |
| **LibreHardwareMonitor** | 0.9.4 і раніше — WinRing0; 0.9.5+ — PawnIO | залежить від версії | WinRing0 — так; PawnIO — ні | повний набір | 0.9.6 + PawnIO 2.1/2.2 — як PawnIO | Використовувати лише 0.9.6+ |

**[ПІДТВЕРДЖЕНО]** для blocklist/версій; **[ПОВІДОМЛЕНО]** для WHQL-тверджень, підписів вендорів і «зазвичай OK» з античітами.

### 6.2. Коментарі до таблиці

- **HWiNFO** — за заявою автора, драйвер HWiNFO ніколи не був WinRing0 і підписується Microsoft. При цьому Microsoft Support у статті про `VulnerableDriver:WinNT/Winring0` згадує HWiNFO серед «potentially affected» застосунків; Martin Malik це заперечує. Отже, тут є конфлікт джерел: **[ПОВІДОМЛЕНО]** з обох боків. Старі версії драйвера (`HWiNFO64A.SYS` ≤8.98) справді є в blocklist, що підтверджує: навіть «вендорський» драйвер може потрапити під блокування, якщо версія вразлива. **[ПІДТВЕРДЖЕНО]** (XML; HWiNFO forum).
- **AIDA64** — приклад того, що «підписаний» ≠ «безпечний». Дослідник 2026 року знайшов, що навіть свіжі 8.x збірки `aida64x.sys` мають `\Device\AIDA64Driver` без ACL і IOCTL для `MmMapIoSpace`/`rdmsr`/`wrmsr`; CVE-2024-26507. Принаймні одна відома збірка (`aida64.sys`, SHA-256 `273753…`) уже фігурує в XML blocklist у складі правила «Firewire kerneld», хоча назви «AIDA64» у правилах немає — це ілюструє, чому не можна покладатися на grep за назвою. **[ПІДТВЕРДЖЕНО для збігу хеша; інтерпретація — [ПОВІДОМЛЕНО]]** (CVE; дослідження; LOLDrivers; XML).
- **CPU-Z** — старий `cpuz.sys` ≤1.0.4.9 заблокований; сучасні версії можуть бути OK, але CPU-Z не є монітором і не дає постійного потоку сенсорів.
- **MSI Afterburner/RTSS** — GPU-only; RTCore64 — класичний BYOVD (CVE-2019-16098), старі версії в blocklist. FACEIT каже, що остання версія сумісна, але це не робить драйвер безпечним.
- **LHM** — саме той випадок, коли «бібліотека» визначає ризик: 0.9.4 і раніше тягне WinRing0; 0.9.5+ тягне PawnIO; 0.9.6 — актуальна. LHM також має опцію «disable Ring0 driver installation», що дозволяє працювати без драйвера (але без CPU temp). **[ПІДТВЕРДЖЕНО]** (release notes 0.9.5; PR #1777).

**Хто з них WHQL?** Публічно підтвердженого повного WHQL/HLK-сертифікату немає ні для AIDA64, ні для CPU-Z, ні для Afterburner. Для HWiNFO є заява спільноти про «HLK tested and fully WHQL Certified», але вона не підтверджена документацією Microsoft; автор говорить лише «digitally signed by Microsoft». Для PawnIO підтверджено Microsoft WHCP-підпис, але HLK vs attestation — **[НЕВІДОМО]**. Практично важливий не ярлик «WHQL», а поточний blocklist-статус, версія драйвера і політика конкретного античіту. **[ПОВІДОМЛЕНО / аналітичний висновок]**

---

## 7. Як античіти виявляють драйвери

### 7.1. Easy Anti-Cheat (EAC)

Офіційна стаття EAC (2026-07-17) прямо каже:

> «In order to protect the integrity of online play, EAC will prevent the game from starting if vulnerable drivers are found on the machine.»  
> «We use the Microsoft blocklist.»

Тобто EAC не має (принаймні публічно) власного «чорного списку драйверів»; він спирається на Microsoft blocklist. Це означає:

- WinRing0/InpOut/RTCore, які є в blocklist, блокуються;
- PawnIO, якого немає в blocklist, не блокується за цією логікою;
- але EAC може мати додаткові евристики/deny-листи, про які публічно не повідомляє. **[ПІДТВЕРДЖЕНО для політики; [НЕВІДОМО] для додаткових списків]**

### 7.2. BattlEye

Офіційний FAQ BattlEye:

> «BattlEye is blocking certain software that is using kernel drivers which contain known security issues that can be exploited by cheats… Please ensure that you are using the latest version of such software.»

І окремо, за змістом FAQ: повідомлення «[INFO] Blocked loading of file…» не ведуть до бану. Тобто BE блокує драйвери за власним списком/евристиками, але повідомлення про блокування файлу не є баном. **[ПІДТВЕРДЖЕНО]**

### 7.3. Riot Vanguard

- Vanguard — kernel-античіт, який завантажується на старті системи (`vgk.sys`) і перевіряє драйвери до запуску гри. Офіційні помилки: `VAN: Incompatible Software`, `VAN: Incompatible OEM Driver`; публічного списку драйверів немає. **[ПІДТВЕРДЖЕНО]** (Riot support).
- Vanguard блокує `inpoutx64.sys` (прямо в support-статті) і WinRing0 (issue OpenHardwareMonitor #1268, 2020; EVGA forum 2020). **[ПІДТВЕРДЖЕНО]**
- Щодо PawnIO: офіційних заяв немає. Є поодинокі користувацькі повідомлення, що Vanguard не запускається з PawnIO (Reddit, грудень 2025), і протилежні — що проблема була саме з InpOut32, а не PawnIO (OpenRGB FAQ). **[ПОВІДОМЛЕНО / [НЕВІДОМО]]**

### 7.4. FACEIT

- FACEIT має **ручний deny-list** драйверів і окремо — блокування за signer-сертифікатом. У кейсі PawnIO FACEIT заблокував не конкретний файл, а сертифікат, яким він був підписаний. **[ПІДТВЕРДЖЕНО]** (issue #1; support article).
- FACEIT блокує на старті AC, а не банить акаунт; після рестарту драйвер може бути вивантажений. **[ПІДТВЕРДЖЕНО]** (FACEIT support).

### 7.5. Windows Defender

- **Vulnerable driver blocklist**: hash/page-hash, filename+version, signer rules; увімкнений за замовчуванням з Win11 22H2; також enforced при HVCI, Smart App Control, S mode. **[ПІДТВЕРДЖЕНО]** (Microsoft Learn).
- **ASR** `56a863a9-…`: блокує запис відомого вразливого драйвера, не завантаження вже наявного. **[ПІДТВЕРДЖЕНО]**.
- **Сигнатури**: `VulnerableDriver:WinNT/Winring0*`, `HackTool:Win32/Winring0`, `Trojan:Win32/Vigorf.A`. **[ПІДТВЕРДЖЕНО]** (Microsoft; хвилі 2025).

### 7.6. Зведення механізмів

| Механізм | Хто використовує | Що саме перевіряє | Наслідок для монітора |
|---|---|---|---|
| Hash / authentihash / page hash | MS blocklist, EAC, BE, Vanguard | точний файл | перейменування не допомагає |
| Filename + version | MS blocklist (`WinRing0.sys` ≤2.0.0.0, `HWiNFO64A.SYS` ≤8.98, `cpuz.sys` ≤1.0.4.9) | ім'я + версія | старі версії блокуються навіть з іншим хешем |
| Signer certificate / publisher | FACEIT (PawnIO case), Vanguard (можливо) | видавець | новий/незнайомий signer → блок |
| Loaded module/service enumeration | EAC, BE, Vanguard | `PsLoadedModuleList`, services, DriverStore | драйвер видно навіть без device handle |
| Device object / handle scan | EAC, BE, Vanguard | `\Device\WinRing0_1_2_0`, `\Device\AIDA64Driver`, `\Device\PawnIO` | можуть блокувати за наявністю handle |
| Boot-time load | Vanguard | драйвери, що стартують до гри | блок на старті системи |
| Behavioral / IOCTL heuristics | EAC, BE, Vanguard | підозрілі IOCTL/пам'ять | важко довести для read-only |
| Game-specific deny list | FACEIT, окремі ігри | ручний список | може змінитися без попередження |
| Reputation (signer + history) | FACEIT | репутація видавця | «валідний підпис» не рятує |

**[ПІДТВЕРДЖЕНО]** для публічно заявлених механізмів; частина рядків — **[АНАЛІТИЧНА ІНТЕРПРЕТАЦІЯ]** на основі того, як працюють kernel-античіти.

---

## 8. Що реально проходить

### 8.1. Pure user-mode (без драйвера)

Найбезпечніший варіант: жодного kernel-драйвера, жодного admin, жодного запису в blocklist. Покриває:

- network speed (GetIfTable2/PDH);
- CPU load, per-core load, frequency (PDH/perf counters);
- RAM used/total;
- GPU load, VRAM, clocks, power, **GPU temperature** через NVAPI/ADL/IGCL;
- disk load/latency (PDH);
- SSD/HDD temperature — **частково**: `IOCTL_STORAGE_QUERY_PROPERTY` + `StorageDeviceTemperatureProperty` (52) для NVMe працює **без admin** (локально 41 °C на Apacer); для SATA HDD/SSD цей шлях дає `ERROR_INVALID_FUNCTION`, а ATA PASS-THROUGH/SMART потребує admin. `Get-StorageReliabilityCounter` без admin — `Access denied`.

Не покриває: CPU package/core temperature, motherboard/VRM/RAM temp, fan RPM. ACPI thermal zone — лише як «best effort» і з попередженням. **[ПІДТВЕРДЖЕНО локально + архітектурно]**

### 8.2. Вендорські user-mode API

NVAPI/ADL/IGCL — це DLL, які не є драйверами і не вимагають admin. Вони не створюють kernel surface, тому античіти їх зазвичай не блокують (вони в процесі застосунку, без ін'єкцій у гру). **[ПІДТВЕРДЖЕНО практикою; [ПОВІДОМЛЕНО] щодо відсутності блокувань]**

### 8.3. Shared memory іншого монітора

- **HWiNFO Shared Memory**: публічний інтерфейс з версії 7.0; у безкоштовній версії вимкнений за замовчуванням і має ліміт 12 годин, у Pro — без ліміту. Читання не створює нового драйвера; античіт бачить драйвер HWiNFO, а не наш. **[ПІДТВЕРДЖЕНО]** (HWiNFO licenses; Rainmeter docs).
- **MSI Afterburner MAHM**: GPU-орієнтований; потребує запущеного Afterburner і його драйвера; CPU temp — обмежено/через plugin. **[ПОВІДОМЛЕНО]**
- **AIDA64 Shared Memory**: є, але драйвер AIDA64 — поганий вибір (CVE). **[ПІДТВЕРДЖЕНО]**.

Плюси: нуль нового kernel surface. Мінуси: залежність від стороннього застосунку, ліцензія, 12-годинний ліміт, драйвер хост-застосунку все одно може бути заблокований.

### 8.4. PawnIO

Найкращий kernel-варіант для CPU temp:

- AMD: `AMDFamily17.p` (SMN/Tctl) — підтверджено модулем;
- Intel: `IntelMSR.p` (MSR 0x19C/0x1A2/0x1B1) — підтверджено;
- motherboard/SMBus: `LpcIO.p`, `Smbus*.p`, `IsaBridgeEC.p`.

Але: драйвер, admin, залежність від signer-політики, FACEIT-історія. **[ПІДТВЕРДЖЕНО архітектурно; ризики — аналітичні]**

### 8.5. Що не проходить

- WinRing0 — blocklist + EAC/BE/Vanguard/FACEIT + Defender. **[ПІДТВЕРДЖЕНО]**
- InpOutx64 — blocklist + Vanguard. **[ПІДТВЕРДЖЕНО]**
- AIDA64 — CVE + ризик; не варто. **[ПІДТВЕРДЖЕНО для CVE]**
- Будь-який test-signed/unrestricted драйвер — Secure Boot/античіти. **[ПІДТВЕРДЖЕНО]**

---

## 9. Юридичні та практичні ризики

1. **ToS/EULA античітів.** EAC/BE/Vanguard дозволяють собі блокувати драйвери; це не бан, але це «гра не запускається». BattlEye прямо каже, що повідомлення «Blocked loading of file» не ведуть до бану. FACEIT блокує запуск AC. **[ПІДТВЕРДЖЕНО]**
2. **Бани — рідкісні, але можливі.** Задокументованих банів саме за моніторинговий драйвер немає; є поодинокі непідтверджені скарги (EA Forums, Reddit). Не варто будувати політику на «нас не забанять». **[ПОВІДОМЛЕНО / [НЕВІДОМО]]**
3. **Блокування на старті.** Vanguard завантажується до гри; FACEIT перевіряє перед матчем; EAC — при старті гри. Наявність драйвера в системі може бути достатньою, навіть якщо застосунок не запущений (FACEIT: «достатньо, щоб він був встановлений»). **[ПІДТВЕРДЖЕНО для FACEIT; [ПОВІДОМЛЕНО] для Vanguard]**
4. **Defender ASR / HVCI / Secure Boot.** ASR блокує запис відомих вразливих драйверів; HVCI + blocklist блокують завантаження; Secure Boot забороняє test-signing. PawnIO official сумісний з HVCI (локально підтверджено), WinRing0 — ні. **[ПІДТВЕРДЖЕНО]**
5. **Kernel-mode Hardware-enforced Stack Protection** може ламати навіть драйвери античітів (BattlEye `BEDaisy.sys`, помилка 1275). Це приклад того, що «безпечна» функція Windows і античіт можуть конфліктувати; для монітора це означає: не додавайте ще один драйвер без потреби. **[ПІДТВЕРДЖЕНО]** (Gaijin/BattlEye support).
6. **Ліцензії.** PawnIO — GPLv2 + IOCTL exception; модулі — LGPL-2.1; LHM — MPL-2.0; HWiNFO/AIDA64 — proprietary; HWiNFO SHM безкоштовно лише 12 годин і не для комерційного використання без Pro. Якщо Oxide Monitor комерційний — це окремі витрати/узгодження. **[ПІДТВЕРДЖЕНО]**.
7. **Апаратні ризики.** SMBus/EC/MSR-записи (особливо write) можуть неправильно налаштувати контролер; для read-only монітора ризик низький, але не нульовий. Використовуйте лише read-операції та офіційні модулі. **[ПОВІДОМЛЕНО]** (окремі інциденти з NCT; причинно-наслідковий зв'язок не доведений).
8. **Підтримка.** Драйвер потрібно оновлювати; старі версії можуть потрапити в blocklist (`HWiNFO64A.SYS` ≤8.98 — приклад). Це означає постійну залежність від релізів PawnIO/LHM. **[ПІДТВЕРДЖЕНО]**.

---

## 10. Рекомендаційна матриця

Легенда: ✅ безпечно/прийнятно; ⚠️ умовно (залежить від версії/конфігурації); ❌ блок/небезпечно; ❓ немає даних; «—» не застосовно.

| Механізм | Defender | EAC / BattlEye | Vanguard | Admin? | CPU temp | Вердикт |
|---|---|---|---|---|---|---|
| **Pure user-mode (PDH, NVAPI/ADL/IGCL, GetIfTable2, GlobalMemoryStatusEx)** | ✅ чисто | ✅ немає драйвера | ✅ немає драйвера | Ні | ❌ (лише ACPI zone, ненадійно) | **Базовий Tier 0. За замовчуванням.** |
| **Storage WMI / IOCTL (SSD/HDD temp)** | ✅ чисто | ✅ немає драйвера | ✅ немає драйвера | NVMe: ні; SATA: часто | ⚠️ NVMe — так; SATA — тільки helper | NVMe — native; SATA — опційний helper |
| **HWiNFO Shared Memory** | ✅ (драйвер HWiNFO MS-signed; версії `HWiNFO64A.SYS` ≤8.98 — ❌) | ⚠️ залежить від версії HWiNFO; зазвичай OK | ❓ | Так (для драйвера HWiNFO) | ✅ (з HWiNFO) | **Tier 1: хороша опційна інтеграція** (ліцензія SHM) |
| **AIDA64 Shared Memory** | ⚠️ CVE-2024-26507; принаймні одна збірка в MS blocklist | ⚠️ ризик | ❓ | Так | ✅ | **Уникати** |
| **MSI Afterburner / RTSS** | ⚠️ старі RTCore — ❌ | ⚠️ зазвичай OK, overlay може конфліктувати | ❓ | Так | ⚠️ GPU-only; CPU обмежено | Лише GPU-опція, не ядро |
| **PawnIO 2.1.0+ (official)** | ✅ не в blocklist; HVCI OK (локально) | ✅/❓ немає задокументованих блокувань; EAC — MS blocklist, BE — власний список | ⚠️/❓ поодинокі анекдоти | Так (install + device ACL) | ✅ AMD Tctl/Tdie, Intel package/core, плата | **Tier 1: рекомендований kernel-шлях** |
| **WinRing0 (будь-яка версія)** | ❌ `VulnerableDriver`/`HackTool` | ❌ blocklist | ❌ з 2020 | Так | ✅ | **Заборонено** |
| **InpOutx64** | ❌ hash-deny | ❌ blocklist | ❌ підтверджено | Так | ❌ (лише port I/O) | **Заборонено** |
| **Власний Microsoft-signed драйвер** | ✅ якщо чистий | ⚠️ новий signer — невідомо | ⚠️ невідомо | Так | ✅ | Не виправдано для цього проєкту |

### 10.1. Ризик-драбина

```
Tier 0  Pure user-mode .................. нульовий ризик для античітів
Tier 1  HWiNFO SHM / PawnIO 2.1.0+ ...... низький/середній; залежить від політики
Tier 2  Vendor driver (Afterburner) ...... середній; GPU-only
Tier 3  AIDA64 ........................... високий (CVE)
Tier 4  WinRing0 / InpOutx64 ............. неприйнятний
```

---

## 11. Рекомендована архітектура для Oxide Monitor

1. **Tier 0 — за замовчуванням, без драйвера і без admin.** Мережа, CPU load, RAM, GPU load/temp через вендорські DLL, disk load. Це покриває 80% того, що потрібно таскбару.
2. **CPU temp — опційно, через PawnIO.** Не робити PawnIO обов'язковою залежністю. Визначати:
   - чи встановлений PawnIO;
   - чи версія ≥2.1.0 (краще ≥2.2.0);
   - чи підпис валідний (обидва підписи);
   - чи device доступний (admin/helper).
   Якщо ні — показувати «CPU temp unavailable» з посиланням на офіційний інсталятор, а не «0 °C».
3. **Не розповсюджувати PawnIO мовчки.** Інсталятор — окремий продукт; потрібен explicit consent, admin elevation і GPL-повідомлення. Якщо вбудовуєте — виконуйте GPLv2 + exception (джерела, notices).
4. **Non-admin UI + privileged helper.** Оскільки PawnIO device admin-only, а taskbar-застосунок не має бути elevated, зробіть окремий service/helper, який читає сенсори і віддає їх через IPC. Це той самий підхід, що OpenRGB 1.0 (system service) і CaptureEngine (elevation service). **[ПОВІДОМЛЕНО, але архітектурно обґрунтовано]**
5. **HWiNFO SHM — альтернатива для користувачів, які вже мають HWiNFO.** Читати лише за наявності; поважати 12-годинний ліміт/Pro; не вважати це «безкоштовним джерелом назавжди».
6. **Ніколи не fallback-итися на WinRing0/InpOut.** Навіть якщо користувач має старий TrafficMonitor — не перевикористовуйте його драйвер.
7. **Rust-інтеграція:** прямий IOCTL до `\Device\PawnIO` через C ABI PawnIOLib (без .NET) — технічно можливо; але модулі мають бути офіційними підписаними блобами, а їхні ліцензії (LGPL-2.1) треба дотримувати. Альтернатива — LHM через IPC, але це .NET-залежність. Обидва варіанти — без коду в цьому звіті.
8. **UX:** показувати стан драйвера (версія, підпис, HVCI, blocklist) і попередження про античіти; не намагатися «обійти» блокування; не інжектити overlay у гру.
9. **Тестування:** матриця «чистий user-mode / PawnIO 2.1 / PawnIO 2.2 / HWiNFO SHM / без admin» на AMD та Intel, з HVCI on/off, з EAC/BE/Vanguard/FACEIT.

---

## 12. Поширені міфи

| Міф | Реальність |
|---|---|
| «PawnIO підписаний самопідписаним сертифікатом» | Official edition має два валідні підписи: `namazso.eu` (EV) + Microsoft WHCP. Self-signed/test-signed — лише Unrestricted edition. **[ПІДТВЕРДЖЕНО локально]** |
| «WHQL/WHCP-підпис означає, що античіт пропустить» | FACEIT заблокував PawnIO за **signer-сертифікатом**, а не за валідністю підпису. Політика античітів — це allow/deny за видавцем/хешем, а не «клас підпису». **[ПІДТВЕРДЖЕНО]** |
| «Defender помилково називає WinRing0 вірусом» | Microsoft: детект `VulnerableDriver:WinNT/Winring0` валідний; драйвер справді вразливий. Це не malware, але й не false positive. **[ПІДТВЕРДЖЕНО]** |
| «Підписаний драйвер = безпечний» | AIDA64, RTCore64, WinRing0 — підписані й уразливі. Підпис доводить авторство, не безпеку. **[ПІДТВЕРДЖЕНО]** |
| «EAC/BE банять за моніторинговий драйвер» | Зазвичай це блокування запуску, не бан. BE прямо пише, що «Blocked loading of file» не веде до бану. **[ПІДТВЕРДЖЕНО]** |
| «PawnIO дає необмежений доступ до заліза» | Офіційна збірка приймає лише підписані модулі з whitelist-операціями; але інтерпретатор потужний, і довіра зміщується на namazso. **[ПІДТВЕРДЖЕНО архітектурно]** |
| «InpOut дає CPU temperature» | Лише port I/O; без MSR/фізичної пам'яті. CPU package temp недоступна. **[ПІДТВЕРДЖЕНО]** |

---

## 13. Відкриті питання

1. **Vanguard і PawnIO:** чи є офіційна політика? Публічних заяв немає; лише анекдоти. **[НЕВІДОМО]**
2. **EAC/BE після інцидентів:** чи додадуть PawnIO до списків, якщо якийсь чит використає його модуль? **[НЕВІДОМО]**
3. **HLK/WHQL vs attestation:** для PawnIO, HWiNFO, AIDA64 точний шлях підпису невідомий; сертифікат той самий. **[НЕВІДОМО]**
4. **Стабільність signer:** `namazso.eu` EV-сертифікат до 2027-08-05; що буде після — невідомо. **[НЕВІДОМО]**
5. **Bus factor PawnIO:** один мейнтейнер; довгострокова підтримка під питанням. **[ПІДТВЕРДЖЕНО структурно; наслідки — [НЕВІДОМО]]**
6. **Storage temp без admin:** на цій машині WMI не віддав температуру; чи є універсальний user-mode шлях для NVMe/SATA — залежить від драйвера/прошивки. **[НЕВІДОМО]**
7. **ACPI thermal zone:** наскільки значення відповідає CPU package на різних платформах — firmware-dependent. **[ПОВІДОМЛЕНО]**.

---

## 14. Джерела

### Microsoft

1. Microsoft Learn — *Microsoft recommended driver block rules* (updated 2026-10-08). https://learn.microsoft.com/en-us/windows/security/application-security/application-control/app-control-for-business/design/microsoft-recommended-driver-block-rules
2. Microsoft — *Vulnerable Driver Blocklist* XML, `aka.ms/VulnerableDriverBlockList`, version 10.0.29545.0 (downloaded 2026-11-17; local analysis).
3. Microsoft Support — *Microsoft Defender Antivirus alert — VulnerableDriver:WinNT/Winring0*. https://support.microsoft.com/en-us/windows/security/threat-malware-protection/microsoft-defender-antivirus-alert-vulnerabledriver-winnt-winring0
4. Microsoft Security Intelligence — *VulnerableDriver:WinNT/Winring0.D* (published 2025-05-20). https://www.microsoft.com/en-us/wdsi/threats/malware-encyclopedia-description?Name=VulnerableDriver%3AWinNT%2FWinring0.D
5. Microsoft Security Intelligence — *VulnerableDriver:WinNT/Winring0.G* (published 2025-07-21). https://www.microsoft.com/en-us/wdsi/threats/malware-encyclopedia-description?Name=VulnerableDriver%3AWinNT%2FWinring0.G
6. Microsoft Learn — *Attack surface reduction rules reference* (GUID `56a863a9-875e-4185-98a7-b882c64b5ce5`). https://learn.microsoft.com/en-us/defender-endpoint/attack-surface-reduction-rules-reference
7. Microsoft Learn — *Attestation Sign Windows Drivers* (updated 2025-07-15). https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/code-signing-attestation
8. Microsoft Learn — *WHQL Release Signature*. https://learn.microsoft.com/en-us/windows-hardware/drivers/install/whql-release-signature

### PawnIO

9. PawnIO — офіційний сайт. https://pawnio.eu/
10. GitHub — `namazso/PawnIO` (source: `signature.cpp`, `vm.cpp`, `driver.cpp`, `include/pawnio_um.h`). https://github.com/namazso/PawnIO
11. GitHub — `namazso/PawnIO.Setup` releases: 2.0.0 (2025-05-24), 2.0.1 (2025-06-29), 2.1.0 (2026-02-12), 2.2.0 (2026-03-15). https://github.com/namazso/PawnIO.Setup/releases
12. GitHub — `namazso/PawnIO.Setup` issue #1, *Load block under FACEIT* (2025-09-19 — 2026-05-24; closed 2026-02-12 «fixed as of 2.1.0»). https://github.com/namazso/PawnIO.Setup/issues/1
13. GitHub — `namazso/PawnIO.Modules` (IntelMSR.p, LpcIO.p, AMDFamily17.p). https://github.com/namazso/PawnIO.Modules
14. GitHub — `namazso/PawnIOLib` / `PawnIOLib.h` (C ABI). https://github.com/namazso/PawnIO/blob/master/PawnIOLib/include/PawnIOLib.h

### WinRing0 / InpOut / вендорські драйвери

15. NVD — CVE-2020-14979 (published 2020-08-11). https://nvd.nist.gov/vuln/detail/CVE-2020-14979
16. NVD — CVE-2019-16098 (RTCore64, published 2019-09-11). https://nvd.nist.gov/vuln/detail/CVE-2019-16098
17. NVD API — пошук `WinRing0` (2026-11-17; CVE-2017-14311, CVE-2019-7240, CVE-2020-13509…13519, CVE-2020-22057, CVE-2021-44899/44901/44903, CVE-2023-1047/1048, CVE-2025-63602, CVE-2026-82628, CVE-2026-129255).
18. LOLDrivers — `WinRing0.sys` entry (10 samples, HVCI results, hashes). https://www.loldrivers.io/drivers/7bb5ff05-25f8-410d-ae99-c8e8f082d24f/
19. LOLDrivers — `AIDA64Driver.sys` entry. https://www.loldrivers.io/drivers/deffeb8d-c7d9-46b9-a921-7d8d096460ef/
20. Highrez — InpOut32/InpOutx64 (legacy, signing impossible). https://www.highrez.co.uk/downloads/inpout32/
21. Starkeblog — *Inpoutx64.sys Windows Driver Analysis* (2021-05-15). https://starkeblog.com/windows/kernel/driver/2021/05/15/inpoutx64.sys-windows-driver-analysis.html
22. HWiNFO Forum — *hwinfo64a_163.sys being flagged…* (2021-10-09; Martin: driver signed by Microsoft; HWiNFO never used WinRing0 — 2025-09-08). https://www.hwinfo.com/forum/threads/hwinfo64a_163-sys-being-flagged-by-anti-virus-tools.7615/
23. HWiNFO — *Licenses* (Shared Memory: 12h free / unlimited Pro). https://www.hwinfo.com/licenses/
24. AIDA64 — *Kernel Driver Update in AIDA64 v7.60* (2025-02-17). https://www.aida64.com/news/kernel-driver-update-aida64-v760
25. Ivan Kozlovsky — *Reversing kerneld-x64.sys…* (2025-08-07). https://ivankozlovsky.com/blog/aida64-kerneld-x64-byovd-vuln-report
26. DEV Community / PaulDotSH — *Finding new drivers vulnerable to CVE-2024-26507* (2026-10-10). https://dev.to/nullptr/finding-new-drivers-vulnerable-to-cve-2024-26507-5c1k

### Античіти

27. Epic Games / EAC — *Why are certain drivers blocked by Easy Anti-Cheat?* (2026-07-17). https://eoshelp.epicgames.com/s/article/Why-are-certain-drivers-blocked-by-Easy-Anti-Cheat?language=en_US
28. BattlEye — *FAQ* (blocked drivers; no ban for blocked-file messages). https://www.battleye.ltd/support/faq/
29. Riot Games Support — *VAN: Incompatible Software* (2026-01-27; example `inpoutx64.sys`). https://support-valorant.riotgames.com/hc/de/articles/48441713812755-VAN-Incompatible-Software
30. Riot Games Support — *VAN: Incompatible OEM Driver* (2026-07-17). https://support.riotgames.com/en-us/riot/performance/van-incompatible-oem-driver
31. FACEIT — *«Forbidden driver» error message and blocked drivers* (updated 2026-10-14; PawnIO.sys → update). https://support.faceit.com/hc/en-us/articles/360014237259--Forbidden-driver-error-message-and-blocked-drivers
32. The Verge — *WinRing0: Why Windows is flagging your PC monitoring and fan control apps as a threat* (2025-03-13). https://www.theverge.com/report/629259/winring0-windows-defender-fan-control-pc-monitoring-alert-quarantine
33. Neowin — *Windows 11/10 is flagging «Winring0»…* (2025-03-13). https://www.neowin.net/news/windows-1110-is-flagging-winring0-on-your-pc-monitoring-fan-control-apps-heres-why/
34. Gaijin Support — *BattlEye anti-cheat troubleshooting* (BEDaisy + Hardware-enforced Stack Protection, error 1275). https://support.gaijin.net/hc/en-us/articles/21638218281362-BattlEye-anti-cheat-troubleshooting

### LHM / FanControl / спільнота

35. GitHub — LibreHardwareMonitor PR #1857, *Swap WinRing0 to PawnIO* (merged 2025-09-16). https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/pull/1857
36. GitHub — LibreHardwareMonitor releases v0.9.5 (2026-01-07; «Swap WinRing0 to PawnIO») and v0.9.6 (2026-02-14; «Update PawnIO modules to 2.2»). https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases
37. GitHub — LibreHardwareMonitor issue #984, *WinRing0 gets blocked by AVs* (comments 2023-06-10…2025-09; signing/REAPS/key pinning discussion). https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/issues/984
38. GitHub — LibreHardwareMonitor discussion #2195, *Librehardwaremonitor/pawnio and anti-cheat* (2026-02-04). https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/discussions/2195
39. GitHub — FanControl issue #3516, *FACEIT anticheat will block PawnIO…* (2025-09-19). https://github.com/Rem0o/FanControl.Releases/issues/3516
40. GitHub — FanControl issue #1694, *v153: inpoutx64.sys is getting blocked by Riot Vanguard* (2023-04-04). https://github.com/Rem0o/FanControl.Releases/issues/1694
41. GitHub — FanControl issue #3671 (Rem0o, 2025-11-06: V237 WinRing0 vs V238+ PawnIO/FACEIT). https://github.com/Rem0o/FanControl.Releases/issues/3671
42. OpenHardwareMonitor issue #1268, *Riot's new Vanguard anti cheat blocking WinRing0 driver* (2020-05-01). https://github.com/openhardwaremonitor/openhardwaremonitor/issues/1268
43. w0a — *What is PawnIO, and does FACEIT block it?* (2026-11-06; independent observation of 2.2.0 + FACEIT). https://support.w0a.app/en/what-is-pawnio
44. DeskMon — README/SECURITY (2026; user-mode vs PawnIO measurements, LHM 0.9.6). https://github.com/Kinmanr-ops/DeskMon
45. Crystal Dew Blog — *WinRing0 1.3.0* (2009-03-07) and *WinRing0 公開終了* (2010). https://crystaldew.info/2009/03/07/winring0-130-2/

---

### Примітка про обмеження звіту

- Я не запускав ігри з EAC/BattlEye/Vanguard і не перевіряв блокування в реальному матчі; висновки про античіти базуються на офіційних заявах, blocklist і задокументованих кейсах.
- Локальні перевірки зроблено на одній машині (Windows 11 26H2, Ryzen 5 5600H, HVCI on, PawnIO 2.1.0). Інші конфігурації (Intel, старі Windows, вимкнений HVCI, Secure Boot off) можуть дати інші результати.
- Дати релізів PawnIO взяті з GitHub API; для 2.1.0 є розбіжність між датою публікації релізу (2026-02-12) і датою підпису (2026-02-11), що очікувано.
- «WHQL» для PawnIO/HWiNFO/AIDA64 слід читати як «Microsoft WHCP-signed»; повний HLK-шлях не доведений публічними даними.
