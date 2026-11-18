# Oxide Monitor IPC Protocol (`oxide-ipc`)

Цей документ описує локальний Named Pipe IPC-міст між фоновим віджетом `oxide-monitor.exe` та окремим короткоживучим процесом налаштувань `oxide-settings.exe` (див. **ADR-14** у [`ARCHITECTURE.md`](../ARCHITECTURE.md)).

## 1. Архітектура та межі залежностей

- Весь протокол, транспорт і перевірки безпеки винесені в окремий крейт [`crates/oxide-ipc`](../crates/oxide-ipc) із **нульовими GUI/D2D/egui залежностями** (`oxide-core`, `serde`, `serde_json`, `windows` лише для Named Pipe / Security / FileSystem).
- `oxide-monitor.exe` (`oxide-app` → `oxide-ui` → `oxide-ipc`) **ніколи** не лінкує `egui`, `eframe`, `wgpu` або інші GPU-фреймворки.
- `oxide-settings.exe` залежатиме лише від `oxide-core`, `oxide-ipc` та `oxide-render` — без `oxide-ui` чи сенсорних крейтів.

## 2. Транспорт, безпека та іменування

| Параметр | Значення |
| --- | --- |
| Назва труби | `\\.\pipe\oxide-ui-v1` (`oxide_ipc::DEFAULT_PIPE_NAME`) |
| Режим створення | `PIPE_ACCESS_DUPLEX \| FILE_FLAG_FIRST_PIPE_INSTANCE \| FILE_FLAG_OVERLAPPED` |
| Тип труби | `PIPE_TYPE_BYTE \| PIPE_READMODE_BYTE \| PIPE_WAIT \| PIPE_REJECT_REMOTE_CLIENTS` |
| Макс. екземплярів | `1` (повторне зв'язування повертає `IpcError::PipeAlreadyInUse`) |
| DACL | Захищений (`D:P(A;;GA;;;<CurrentUserSID>)`) — повний доступ лише для поточного користувача |
| Перевірка peer-процесу | `GetNamedPipeClientProcessId` / `GetNamedPipeServerProcessId` + `QueryFullProcessImageNameW`: очікується сусідній виконуваний файл у тому самому каталозі (`oxide-settings.exe` на сервері, `oxide-monitor.exe` на клієнті) |
| Фреймінг | `u32` LE довжина + UTF-8 JSON payload, ліміт `1..=2 MiB` (`MAX_IPC_FRAME_BYTES`) |
| Версія протоколу | `IPC_PROTOCOL_VERSION = 1` (перевіряється у `Hello`; невідповідність → `ProtocolError { code: "version_mismatch" }` і закриття каналу) |

## 3. Лінивий життєвий цикл та discovery (`WM_OXIDE_OPEN_IPC`)

Щоб `oxide-monitor.exe` у спокої тримав **0 додаткових IPC-потоків і хендлів**:

1. **Запуск із трею / Flyout:** `oxide-monitor.exe` викликає `IpcHost::ensure_started()` (синхронно створює `\\.\pipe\oxide-ui-v1` і запускає 1 overlapped-потік із таймаутом очікування клієнта 10 с), після чого запускає сусідній `oxide-settings.exe --ipc` через `CreateProcessW`. Якщо `oxide-settings.exe` відсутній — сервер одразу зупиняється, а конфіг відкривається в `notepad.exe`.
2. **Прямий запуск `oxide-settings.exe`:** клієнт шукає вікно класу `OxideOverlay`, надсилає йому `PostMessageW(hwnd, WM_OXIDE_OPEN_IPC, 0, 0)` і повторює підключення `IpcClient::connect_default(Duration::from_secs(2))`. Якщо `oxide-monitor.exe` не запущений, `oxide-settings.exe` працює у standalone-режимі з локальним `config.toml`.
3. **Завершення сесії:** при відключенні клієнта потік сервера кладе `IncomingCommand::Disconnected` у чергу, будить UI-потік через `PostMessageW(WM_OXIDE_IPC_CMD)` і завершується. `oxide-ui` негайно робить `join()` потоку (`reap_if_finished()`), повертаючи процес до **0 IPC-потоків**.

## 4. Неблокуюча взаємодія з UI-потоком

- **Вхідні команди (`SettingsToMonitor`):** IPC-потік декодує фрейми, кладе `IncomingCommand` у `Mutex<VecDeque<_>>` і викликає `PostMessageW(overlay_hwnd, WM_OXIDE_IPC_CMD, 0, 0)`. UI-потік ніколи не блокується на pipe I/O.
- **Вихідні повідомлення (`MonitorToSettings`):** дві обмежені неблокуючі черги (`sync_channel(16)` для контрольних відповідей `HelloAck`/`CommitResult`/`MonitorsChanged`/`FocusTab` і `sync_channel(2)` для 1 Hz `TelemetryTick`) зі спільним Win32 `Event`, мультиплексованим із overlapped `ReadFile` через `WaitForMultipleObjects`.

## 5. Єдиний writer `config.toml`, оптимістична конкурентність і Hot Reload

- **Єдиний writer:** поки працює `oxide-monitor.exe`, лише він записує `config.toml` (нормалізація/clamp → `config.toml.tmp` + `sync_all()` → атомарний `ReplaceFileW` із резервною копією `config.toml.bak`).
- **Ревізія (`generation` + `config_hash`):**
  - `HelloAck` повертає поточні `generation: u64` та `config_hash: u64` (FNV-1a 64-bit).
  - `PreviewConfig` застосовує конфіг **лише в пам'яті** без запису на диск і **ставить hot reload на паузу**.
  - `CommitConfig { expected_generation, expected_hash, config }` перевіряє:
    1. збіг `expected_generation` та `expected_hash` із поточною ревізією в пам'яті;
    2. збіг поточного хешу файлу на диску з `committed_hash` (щоб не затерти зовнішні ручні правки, зроблені під час паузи hot reload).
  - При розбіжності повертається `CommitResult { status: Conflict, ... }`, а диск залишається недоторканим.
  - При успіху файл атомарно оновлюється, `generation` збільшується на 1, власний хеш записується у `ConfigWatcher` (щоб не спрацював повторний hot reload), hot reload відновлюється.
- **Відкат (`RevertConfig` / disconnect):** як явний `RevertConfig`, так і аварійне закриття/відключення `oxide-settings.exe` під час активного preview автоматично відкочують віджет до `committed_config` і відновлюють hot reload.
- **Hot reload (`ConfigWatcher`):** опитує `mtime + size` раз на 2 с у існуючому UI-таймері (0 додаткових потоків), читає файл лише при зміні метаданих, порівнює FNV-1a хеш, при битому TOML зберігає робочий конфіг і пише попередження в лог **один раз** на унікальний битий хеш.
