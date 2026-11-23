# Oxide Monitor 0.1.0

Lightweight, driver-free taskbar system monitor for Windows 10/11, written in Rust.

## English

### Highlights

- Classic 294×35 taskbar widget plus dynamic sizing: content-based, fixed DIP, exact
  pixels, 50–300 % scale, Compact/Normal/Large/TV presets and “fit to taskbar”.
- Network, CPU, RAM, GPU, NVMe temperatures, disk activity, multi-GPU/multi-disk
  discovery, per-core Flyout and 60-second sparklines.
- Fluent settings GUI with pixel-accurate Live Preview, Apply/Cancel/Reset, IPC and
  hot-reloaded TOML. Ukrainian (default), English and Russian UI.
- Task Scheduler autostart: per-user, at logon, 15 s delay, limited privileges — no
  admin/UAC. Live status in Settings.
- HWiNFO shared-memory and loopback LibreHardwareMonitor providers with live health
  status and official version checks. LHM can stage a verified portable update.
- Optional PawnIO CPU-temperature helper (opt-in, explicit UAC, CPU Tctl/Tdie only,
  AMD family 17h–1Ah). PawnIO is installed/updated manually from pawnio.eu.

### Install

1. Download `OxideMonitor-0.1.0-Setup.exe` from the release assets.
2. Run it. The installer is per-user, installs to `%LOCALAPPDATA%\OxideMonitor`, and does
   not require administrator rights.
3. Launch **Oxide Monitor** from the Start Menu or the installer.

Portable alternative: `oxide-monitor-0.1.0-win-x64.zip`. Extract it and run
`oxide-monitor.exe`; keep `oxide-settings.exe` and `oxide-pawnio-host.exe` beside it.

Uninstall from **Settings → Apps → Oxide Monitor** or the Start Menu uninstall shortcut.
User configuration in `%APPDATA%\OxideMonitor` is intentionally left in place.

### Security and support

- The installer and binaries are **unsigned**. Verify the SHA-256 in `SHA256SUMS` before
  running. Windows SmartScreen may warn.
- Default mode loads no kernel driver, requires no elevation, opens no network listener
  and sends no telemetry.
- HWiNFO and LibreHardwareMonitor are **not bundled**; they are optional external
  providers. LHM URLs are restricted to loopback.
- PawnIO is a kernel driver and is never installed or updated by Oxide. Enabling the CPU
  helper requires a UAC prompt; some anti-cheat systems may flag kernel drivers. Use only
  if you accept that risk.
- The LHM auto-update only stages a hash-verified portable copy; it never executes it or
  modifies an external LHM installation.

### Known limitations

- ARM64 is compile-checked, not tested on physical ARM64 hardware.
- PawnIO Phase 1 is CPU Tctl/Tdie only; motherboard/SuperIO and SATA-via-PawnIO are not
  included. SATA/mainboard temperatures require an external provider.
- The official LHM executable is not Authenticode-signed; auto-update relies on the
  published SHA-256. PawnIO update is check-only (manual installer).
- The PawnIO helper is unsigned and UAC shows “Unknown publisher” until the project
  obtains a code-signing certificate.

### License

Application code: MIT OR Apache-2.0. The embedded PawnIO module is LGPL-2.1-or-later
(notices and license text are shipped with the binaries). The PawnIO driver/installer is
not redistributed.

## Українська

### Основне

- Класичний віджет 294×35 і динамічний розмір: за вмістом, фіксований DIP, точні
  пікселі, масштаб 50–300 %, пресети Компактний/Звичайний/Великий/ТБ і «fit to taskbar».
- Мережа, CPU, RAM, GPU, температури NVMe, активність дисків, кілька GPU/дисків,
  per-core Flyout і 60-секундні графіки.
- GUI налаштувань з Live Preview, Apply/Cancel/Reset, IPC і hot reload TOML.
  Українська (типова), англійська та російська.
- Автозапуск через Task Scheduler: per-user, при вході, затримка 15 с, limited
  privileges — без admin/UAC. Статус у налаштуваннях.
- HWiNFO shared memory і loopback LibreHardwareMonitor зі станом здоров’я та перевіркою
  офіційних версій. LHM може підготувати перевірену portable-копію оновлення.
- Опційний PawnIO-хелпер для CPU (явний opt-in, UAC, лише Tctl/Tdie, AMD 17h–1Ah).
  PawnIO встановлюється/оновлюється вручну з pawnio.eu.

### Встановлення

1. Завантаж `OxideMonitor-0.1.0-Setup.exe` з assets релізу.
2. Запусти. Інсталятор per-user, ставить у `%LOCALAPPDATA%\OxideMonitor`, без прав
   адміністратора.
3. Запусти **Oxide Monitor** з Start Menu або відразу після встановлення.

Портативний варіант: `oxide-monitor-0.1.0-win-x64.zip`; розпакуй і запусти
`oxide-monitor.exe`, тримаючи поруч `oxide-settings.exe` і `oxide-pawnio-host.exe`.

Видалення: **Settings → Apps → Oxide Monitor** або ярлик Uninstall. Конфігурація в
`%APPDATA%\OxideMonitor` навмисно не видаляється.

### Безпека

- Інсталятор і бінарники **unsigned**; перевір SHA-256 з `SHA256SUMS`. SmartScreen може
  попередити.
- Типовий режим без kernel-драйвера, без elevation, без мережевого listener і телеметрії.
- HWiNFO/LibreHardwareMonitor не бандляться; LHM — лише loopback.
- PawnIO — kernel driver, Oxide його не ставить і не оновлює. Увімкнення CPU-хелпера
  потребує UAC; деякі anti-cheat можуть реагувати на kernel-драйвери.
- Автооновлення LHM лише готує перевірену portable-копію; не запускає її і не змінює
  зовнішній LHM.

### Відомі обмеження

- ARM64 перевірено компіляцією, не на фізичному ARM64-залізі.
- PawnIO Phase 1 — лише CPU Tctl/Tdie; motherboard/SuperIO і SATA через PawnIO не
  входять. SATA/mainboard — через зовнішній провайдер.
- Офіційний LHM.exe не підписаний; автооновлення покладається на SHA-256. PawnIO —
  тільки перевірка версії, оновлення вручну.
- PawnIO-хелпер unsigned, UAC показує «Unknown publisher», доки не буде сертифіката.

### Ліцензія

Код застосунку: MIT OR Apache-2.0. Вбудований PawnIO-модуль — LGPL-2.1-or-later
(повідомлення й текст ліцензії постачаються з бінарниками). Драйвер/інсталятор PawnIO не
редоставляється.
