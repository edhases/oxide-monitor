# Oxide Monitor

[English version](README.md)

Легкий монітор системи для панелі завдань Windows 10/11, написаний на Rust.

**Без драйвера й без адмін-прав за замовчуванням.** Жодного kernel-драйвера, жодних прав
адміністратора, жодної ін’єкції в Explorer. Джерела температур — опційні: зовнішні
HWiNFO/LHM або явно ввімкнений PawnIO-хелпер для CPU.

## Можливості

- **Віджет у панелі завдань**: класичний дворядковий 294×35 і динамічний розмір —
  за вмістом, фіксований DIP або точні пікселі; масштаб 50–300 %; пресети
  Компактний/Звичайний/Великий/ТБ; «fit to taskbar», щоб нічого не обрізало на низьких
  панелях і телевізорах.
- Мережа up/down, CPU %, RAM %, GPU %, температури CPU/GPU/NVMe, активність дисків
  (SATA/mainboard — через опційні провайдери); кілька GPU і дисків; per-core та історія
  у Flyout.
- **Flyout** з CPU/per-core, пам’яттю, всіма GPU, всіма дисками, мережею, source badges
  і локалізованими діями; tray-іконка з локалізованим меню; `--show` показує вже
  запущений віджет.
- **GUI налаштувань** (`oxide-settings.exe`) для всіх секцій Config v2 з pixel-accurate
  Live Preview, Apply / Cancel / Reset, IPC і hot reload конфігу кожні 2 с. TOML
  залишається advanced/fallback-шляхом.
- **Автозапуск** через per-user завдання Windows Task Scheduler (`Oxide Monitor`, при
  вході, затримка 15 с, limited privileges — без admin/UAC). Статус видно в налаштуваннях.
- **Зовнішні температури**: HWiNFO shared memory і loopback LibreHardwareMonitor HTTP.
  Обидва вимкнені за замовчуванням, вмикаються наживо.
- **Перевірка офіційних версій** LibreHardwareMonitor і PawnIO. LHM може завантажити
  перевірену portable-копію в `%LOCALAPPDATA%\OxideMonitor\providers\LHM`; вона ніколи не
  запускається автоматично, а зовнішня інсталяція LHM не змінюється.
- **Опційний PawnIO-хелпер для CPU**: тільки після явного Enable і UAC. Лише CPU
  Tctl/Tdie; SuperIO/SATA через PawnIO — поза межами Phase 1.
- Fluent-локалізація: **українська (за замовчуванням), англійська, російська**.
- x64 release і `cargo check --target aarch64-pc-windows-msvc` входять у гейт
  (без x86-asm).

## Вимоги

- Windows 10/11 (x64; ARM64 перевіряється компіляцією, фізичне ARM64-залізо ще не
  тестувалося).
- Rust stable і workload **Desktop development with C++** (MSVC linker, Windows SDK).
- Для звичайної роботи адмін-права не потрібні. PawnIO, якщо ввімкнено, вимагає
  одноразового підтвердження admin/UAC для хелпера; сам PawnIO встановлюється вручну.

## Збірка і запуск

```powershell
cargo build -p oxide-app -p oxide-settings
cargo run -p oxide-app
```

Release (бінарники поруч у `target\release\`):

```powershell
cargo build --release --bin oxide-monitor --bin oxide-settings --bin oxide-pawnio-host
target\release\oxide-monitor.exe
```

`oxide-pawnio-host.exe` необов’язковий у runtime, але має лежати поруч з
`oxide-monitor.exe`, якщо ввімкнено PawnIO-хелпер.

Корисні режими:

```powershell
oxide-monitor.exe --headless --once   # один знімок сенсорів без UI
oxide-monitor.exe --headless          # три знімки, по одному за секунду
oxide-monitor.exe --show              # показати віджет уже запущеного екземпляра
oxide-settings.exe                    # GUI налаштувань (IPC або standalone)
```

Гейти:

```powershell
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace --target aarch64-pc-windows-msvc
```

`scripts/check-all.ps1` запускає повний локальний гейт (включно з dependency isolation);
`scripts/package.ps1` збирає й копіює release-файли.

## Конфігурація

- **Основний шлях:** GUI налаштувань (`oxide-settings.exe`, з меню трею або картки
  Flyout). Покриває всі секції Config v2 з Live Preview і Apply / Cancel / Reset.
- **Advanced/fallback:** `%APPDATA%\OxideMonitor\config.toml` (portable-режим:
  `config.toml` поруч з exe). Hot reload кожні 2 с; запис через `ReplaceFileW` з `.bak`.
- `resources/config.toml` — приклад.
- `config.general.language = "uk" | "en" | "ru"`.
- `config.general.cpu_temp_source = "auto" | "acpi" | "provider" | "off"`.
- ACPI-температура CPU — це платформна зона, не Tctl/Tdie; UI додає `~`.

### Автозапуск

`general.autostart = true` реєструє per-user завдання Task Scheduler `Oxide Monitor`
(при вході, затримка 15 с, limited privileges, без admin/UAC), яке запускає
`oxide-monitor.exe --autostart`. Стан узгоджується при старті, на Settings → Apply і при
hot reload; старий `HKCU\...\Run` видаляється автоматично. Результат пишеться в
`%APPDATA%\OxideMonitor\autostart.toml` і показується в налаштуваннях.

Видалити завдання:

```powershell
Unregister-ScheduledTask -TaskName 'Oxide Monitor'
```

Деталі: [`docs/AUTOSTART.md`](docs/AUTOSTART.md).

### Зовнішні провайдери температур

Температури CPU/mainboard/SATA можуть надходити з **HWiNFO shared memory** або
**loopback LibreHardwareMonitor** web server. Обидва вимкнені за замовчуванням, не
потребують адмін-прав чи драйвера від Oxide, вмикаються наживо і показують стан
(`connected` / `not_running` / `stale` / `error`) у налаштуваннях.

- HWiNFO: увімкни **Settings → Safety → Shared Memory Support**, запусти HWiNFO, потім
  увімкни джерело HWiNFO в налаштуваннях.
- LHM: запусти LibreHardwareMonitor, увімкни **Options → Remote Web Server → Run**
  (типово `http://127.0.0.1:8085/metrics`), потім увімкни LHM у налаштуваннях. Дозволені
  лише loopback-URL.
- Автооновлення LHM — опційне (`providers.lhm_auto_update`, типово вимкнене). Воно
  завантажує лише офіційні GitHub-релізи, перевіряє опублікований SHA-256, готує portable
  копію і ніколи не запускає її та не змінює зовнішній LHM. Офіційний LHM.exe не має
  Authenticode-підпису; staged-артефакт перевіряється за хешем і не запускається
  автоматично.

Деталі: [`docs/PROVIDERS.md`](docs/PROVIDERS.md) і
[`crates/oxide-pawnio-host/README.md`](crates/oxide-pawnio-host/README.md).

### PawnIO CPU-температура (opt-in)

PawnIO — підписаний kernel-драйвер, який встановлюється й оновлюється **вручну** з
[`pawnio.eu`](https://pawnio.eu). Oxide Monitor ніколи його не встановлює й не оновлює.

Phase 1 підтримує **лише CPU Tctl/Tdie** на AMD family 17h–1Ah. Коли `providers.pawnio`
увімкнено в налаштуваннях, монітор запускає `oxide-pawnio-host.exe` через UAC; хелпер
завантажує один офіційний модуль із pinned hash, читає SMN-температуру і зупиняється на
Stop, Game Mode, виході з програми або втраті pipe. Монітор і налаштування залишаються
без адмін-прав. Хелпер unsigned, тому UAC показує «Unknown publisher»; kernel-драйвери
можуть бути flagged деякими anti-cheat. Motherboard/SuperIO, SATA через PawnIO й
автооновлення драйвера не входять.

## Архітектура та дослідження

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — рішення (ADR-01..ADR-14), порівняння провайдерів,
  схема модулів, roadmap.
- [`docs/SETTINGS.md`](docs/SETTINGS.md) — ізольований GUI налаштувань (DX12-only, Live
  Preview).
- [`docs/IPC.md`](docs/IPC.md) — named-pipe IPC, single-writer config, conflict detection.
- [`docs/AUTOSTART.md`](docs/AUTOSTART.md) — Task Scheduler автозапуск.
- [`docs/PROVIDERS.md`](docs/PROVIDERS.md) — HWiNFO/LHM/PawnIO.
- [`docs/UI_CONTRACT.md`](docs/UI_CONTRACT.md) — публічний контракт UI-крейта.
- [`research/`](research/) — аудит драйверів/anti-cheat, нативні сенсори, shared memory,
  taskbar embedding і локальний аудит TrafficMonitor.
- [`PHASE0.md`](PHASE0.md) — acceptance checklist.
- [`docs/BUILD.md`](docs/BUILD.md), [`docs/ARM64.md`](docs/ARM64.md).

## Безпека

Стандартна збірка не завантажує kernel-драйвер, не потребує elevation, не відкриває
мережевий listener і не надсилає телеметрію. Зовнішні провайдери read-only і вимкнені за
замовчуванням; LHM URL — лише loopback. PawnIO — єдиний опційний kernel-шлях, вимкнений
за замовчуванням, явно opt-in і з попередженням про anti-cheat.

## Ліцензія

Код застосунку — **MIT OR Apache-2.0**. Вбудований PawnIO-модуль —
**LGPL-2.1-or-later**, розповсюджується з notice і посиланням на джерело; драйвер/
інсталятор PawnIO не редоставляється. Див. `LICENSE-MIT`, `LICENSE-APACHE`, `NOTICE.md`
і `crates/oxide-pawnio-host/assets/`.
