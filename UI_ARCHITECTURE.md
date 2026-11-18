# Oxide Monitor — Архітектура UI/UX налаштувань та рушія кастомізації

**Статус:** погоджено (етап реалізації розпочато з U0)  
**Дата:** 18.11.2026  
**Платформа:** Windows 10 22H2+ / Windows 11 23H2–26H2 (`x86_64-pc-windows-msvc` + `aarch64-pc-windows-msvc`)  
**Мова:** Rust (edition 2024), ліцензія `MIT OR Apache-2.0`  
**Базові дослідження:**
- [`research/ui/01_settings_ui_framework.md`](research/ui/01_settings_ui_framework.md) — вибір UI-фреймворку та ізоляція пам'яті/GPU;
- [`research/ui/02_widget_customization_engine.md`](research/ui/02_widget_customization_engine.md) — рушій кастомізації, типографіка, пороги кольору та Live Preview;
- [`research/ui/03_modern_ux_and_presets.md`](research/ui/03_modern_ux_and_presets.md) — UX налаштувань, каталог заліза, пресети та імпорт із TrafficMonitor;
- [`research/ui/04_widget_interactions_and_polish.md`](research/ui/04_widget_interactions_and_polish.md) — дії миші, Win11 Flyout, Auto-hide та Game Mode;
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — базова архітектура сенсорів, безпеки та taskbar-хосту.

---

## 1. Архітектурне рішення в одному абзаці

Щоб отримати **сучасний інтерактивний UI рівня Windows 11** і **потужний конструктор віджета**, не порушивши бюджет фонового монітора (`< 0.3% CPU`, `< 30 MB RAM`, `0 GPU wake-up`), UI-підсистема розділяється на **три спеціалізовані шари** та **два процеси**:
1. **Taskbar-віджет** (`oxide-monitor.exe`) — чистий Win32 + спільний Direct2D/DirectWrite рендерер (`oxide-render`) у DIB + `UpdateLayeredWindowIndirect` (1 Hz, без GPU-контексту).
2. **Розширений Tooltip / Win11 Flyout** (`oxide-monitor.exe`) — легке нативне popup-вікно на тому ж `oxide-render` + DWM Acrylic/Rounded Corners із лінивою алокацією DIB (відкривається за `< 12 ms`, звільняє буфер через 5 с після закриття).
3. **Вікно налаштувань і конструктор** (**окремий короткоживучий процес `oxide-settings.exe`**) — **`egui 0.37` + `eframe 0.37` (`wgpu` DX12)** + **`egui_dnd 0.18`** + DWM Mica Alt + кастомна Fluent-тема + вбудований **`oxide-render` для піксель-точного Live Preview**. Після закриття вікна процес завершується, і Windows **миттєво звільняє 100% RAM, VRAM, GPU-контекстів і завантажених драйверів**.

---

## 2. Ключові архітектурні рішення (UI-ADR)

| ID | Рішення | Обґрунтування | Наслідки |
|---|---|---|---|
| **UI-ADR-01** | **Окремий процес `oxide-settings.exe`** (Medium IL `asInvoker`) | Windows loader не вивантажує `d3d12.dll`/GPU-драйвери після закриття in-process вікна; лише завершення процесу гарантує 0 B залишкової RAM та 0 GPU wake-up у `oxide-monitor.exe` | Другий `.exe` поруч (~5 MB) + тонкий локальний Named Pipe IPC |
| **UI-ADR-02** | **`egui 0.37` + `eframe` (`wgpu` DX12) + `egui_dnd 0.18`** для `oxide-settings.exe` | 100% Rust, `MIT OR Apache-2.0` (без GPLv3/бейджа Slint), нативний DX12 на x64 + ARM64, найкращі готові інструменти для конструктора (DnD, HSV/Alpha color picker, градієнти, TextureHandle preview), 0 дискового кешу (на відміну від WebView2) | Потрібен модуль `fluent_theme.rs` (~350 LoC) + системний шрифт `Segoe UI Variable` + DWM Mica |
| **UI-ADR-03** | **Спільний крейт `oxide-render` (Direct2D + DirectWrite → BGRA8 DIB)** | Єдине джерело правди для `LayoutEngine` і рендерингу віджета: гарантує 1:1 піксельний збіг між Live Preview у `oxide-settings.exe` і реальним таскбаром | Віджет, Flyout і Live Preview використовують один код; GDI лишається fallback-режимом |
| **UI-ADR-04** | **Win11 Flyout у `oxide-monitor.exe` на чистому D2D + DWM Acrylic** | Flyout має відкриватись за `< 12 ms` без крадіжки фокусу і без запуску важкого `wgpu`-процесу; ліниве звільнення DIB через 5 с тримає 0 B у фоні | Невелика кількість кастомного D2D layout-коду для карток Flyout (read-only + 3–4 кнопки) |
| **UI-ADR-05** | **Декларативний `config.toml` v2 з `MetricSource`, `ColorRule` (OKLab) і `LayoutConfig`** | Дозволяє 1–4 рядки, довільну сітку з `row_span`/`col_span`, кілька GPU/дисків/адаптерів одночасно, 4 візуальні режими (текст/sparkline/прогрес/графік) і плавні теплові градієнти | Автоматична безболісна міграція `version = 1 → 2` при першому запуску |
| **UI-ADR-06** | **Окремий чистий крейт `oxide-import` для TrafficMonitor (`config.ini`, `skin.xml`, `skin.ini`)** | Ізолює парсери legacy-форматів і кодувань від ядра, дає 100% unit-тестованість і прозорий звіт про імпорт без автоматичного ввімкнення драйверів | Безпечна міграція користувачів TrafficMonitor у 3 кліки |
| **UI-ADR-07** | **Подієві `SetWinEventHook` (`OUTOFCONTEXT`) для Auto-hide і Game Mode** | Жодних 100 ms таймерів і сканування процесів щосекунди; `WINEVENT_OUTOFCONTEXT` не інжектить DLL у чужі процеси (100% anti-cheat safe) | Мінімальний CPU (`< 0.01%`), миттєва реакція на Auto-hide та запуск ігор |
| **UI-ADR-08** | **DPI-незалежна геометрія (DIP) + DirectWrite `tabular-nums` (`tnum`)** | Усуває «стрибки» цифр у пропорційних шрифтах і поломку позиції при мішаних моніторах (напр. 150% ноутбук + 100% зовнішній монітор) | Усі розміри й відступи в конфігу — у логічних DIP (96 DPI baseline) |

---

## 3. Обраний стек і бюджет ресурсів

| Підсистема | Стек / Крейт (версія 11.2026) | Ліцензія | Бюджет RAM | Бюджет CPU | Розмір (release, stripped) |
|---|---|---|---|---|---|
| **Фоновий монітор** (`oxide-monitor.exe`) | `windows 0.62` (Win32, D2D, DWrite, DWM, PDH, IP Helper, Shell), `oxide-render`, `oxide-sensors` | MIT/Apache-2.0 | **`< 25 MB`** (ціль `< 30 MB`) | **`< 0.25%`** (текст)<br>`< 0.6%` (sparklines) | **~1.8–2.4 MB** |
| **Flyout-панель** (всередині `oxide-monitor.exe`) | `oxide-render` (D2D/DWrite DIB) + DWM Acrylic/Round | MIT/Apache-2.0 | `~2 MB` відкритий → **`0 B`** через 5 с | `< 0.1%` лише поки відкритий | входить у `oxide-monitor.exe` |
| **Вікно налаштувань** (`oxide-settings.exe`) | `egui 0.37`, `eframe 0.37` (`wgpu` DX12 only), `egui_dnd 0.18`, `rfd 0.15` (Win32 file dialogs), `oxide-render`, `oxide-import` | MIT/Apache-2.0 | `~35–55 MB` відкритий → **`0 B`** після закриття | Реактивно (repaint-on-demand + 1 Hz preview) | **~4.5–6.0 MB** |
| **Імпорт / Експорт** (`oxide-import`) | `roxmltree 0.21`, `zip 2.x` (deflate only, без C-залежностей), `encoding_rs 0.8` | MIT/Apache-2.0 | `< 1 MB` під час імпорту | разово | входить у `oxide-settings.exe` |
| **Локалізація** (`oxide-core`) | `fluent-bundle 0.16`, `unic-langid 0.9` (`uk` / `en` / `ru`) | MIT/Apache-2.0 | `< 0.2 MB` | 0 | `< 30 KB` |

> **Перевірка x64 + ARM64:** усі перелічені крейти — 100% Pure Rust або офіційні Microsoft `windows` bindings без x86-асемблера та C++ SDK (Skia, V8, Chromium не використовуються). `wgpu` із фічею `dx12` (та WARP fallback) компілюється і працює нативно на `x86_64-pc-windows-msvc` та `aarch64-pc-windows-msvc`.

---

## 4. Процесна модель та протокол IPC (`oxide-monitor` ↔ `oxide-settings`)

```
┌────────────────────────────────────────────────────────────────────────────┐
│ oxide-monitor.exe (постійний процес, asInvoker, PMv2)                      │
│                                                                            │
│  [UI Thread: Win32 loop] ──► Overlay (oxide-render)                        │
│        │                 ──► Flyout (oxide-render, lazy DIB)               │
│        │                 ──► WinEventHook (Auto-hide, Game Mode)           │
│        │                                                                   │
│  [Sensor Thread: 1 Hz]   ──► Snapshot + HistoryRing(60) + HardwareInventory│
│        │                                                                   │
│  [IPC Server Thread]     ◄── запускається лише коли відкрито налаштування! │
│        │                     Named Pipe: \\.\pipe\oxide-ui-v1-<pid>        │
└────────┼───────────────────────────────────────────────────────────────────┘
         │  Length-prefixed JSON (DACL: Current User SID only,
         │  PIPE_REJECT_REMOTE_CLIENTS, max_instances = 1)
         ▼
┌────────────────────────────────────────────────────────────────────────────┐
│ oxide-settings.exe (короткоживучий процес, asInvoker, PMv2)                │
│                                                                            │
│  • eframe/egui (wgpu DX12 + DWM Mica Alt + Fluent theme)                   │
│  • Локальний oxide-render (D2D CPU DIB) ──► Live Preview TextureHandle     │
│  • Undo/Redo стек (50 кроків), Пресети, Імпортер TrafficMonitor            │
│  • При закритті вікна: ExitProcess(0) ──► 100% RAM/VRAM повернуто ОС       │
└────────────────────────────────────────────────────────────────────────────┘
```

### Повідомлення IPC (JSON, length-prefixed `u32 LE`, ліміт 2 MB)

**`oxide-monitor.exe` → `oxide-settings.exe`:**
- `HelloAck { app_version, arch, config_path, portable_mode, active_config: Config }`
- `TelemetryTick { snapshot: Snapshot, history: HistorySlice, inventory: HardwareInventory, provider_statuses: Vec<ProviderStatus> }` *(1 Hz, лише поки клієнт підключений)*
- `MonitorsChanged { monitors: Vec<MonitorDescriptor> }`

**`oxide-settings.exe` → `oxide-monitor.exe`:**
- `Hello { protocol_version: 1, focus_tab: Option<String> }`
- `PreviewConfig { config: Config }` *(debounce 50 ms — оновлює таскбар у пам'яті без запису на диск)*
- `CommitConfig { config: Config }` *(атомарний запис `config.toml` + backup `.bak` + оновлення autostart/провайдерів)*
- `RevertConfig` *(повертає таскбар до збереженого `config.toml`)*
- `ProbeProvider { provider_id }` *(миттєва перевірка HWiNFO/LHM без очікування backoff)*

**Захист від дублів:** якщо користувач двічі натисне «Налаштування», `oxide-monitor.exe` перевіряє, чи живий дочірній процес `oxide-settings.exe` (або його іменований mutex `Local\OxideSettings-<pid>`), і просто активує існуюче вікно (`SetForegroundWindow`) + шле команду переходу на потрібну вкладку.

**Сценарій прямого запуску `oxide-settings.exe` (без аргументів):** якщо користувач відкриває `oxide-settings.exe` подвійним кліком із папки (без переданого `--ipc`), налаштування виконують 3-кроковий graceful fallback:
1. Шукають уже запущений `oxide-monitor.exe` (через вікно класу `OxideOverlay` / well-known pipe) і підключаються до нього;
2. Якщо монітор не запущений, але поруч у тій же директорії є `oxide-monitor.exe` — автоматично запускають його у фоні та підключаються після готовності pipe (таймаут 2 с);
3. Якщо підключення недоступне — відкриваються в **автономному режимі редагування `config.toml` на диску** (з локальним Live Preview на тестових даних та інформаційним банером «Автономний режим»), без помилок і падінь.

---

## 5. Повна схема `config.toml` v2 (кастомізація віджета та поведінки)

> **Зворотна сумісність:** якщо завантажується існуючий `config.toml` із `version = 1`, `oxide_core::Config::load_from_path()` прозоро мігрує його у `version = 2` (зберігаючи резервну копію `config.v1.bak` перед першим перезаписом). Усі поля мають розумні `#[serde(default)]`, тому мінімальний конфіг залишається коротким і читабельним.

```toml
# Oxide Monitor — Повна схема конфігурації v2
version = 2

# ============================================================================
# 1. ЗАГАЛЬНІ НАЛАШТУВАННЯ
# ============================================================================
[general]
language = "uk"                    # "uk" | "en" | "ru"
autostart = false                  # HKCU\...\Run (без admin)
theme = "auto"                     # "auto" | "dark" | "light"
poll_interval_ms = 1000            # 500..5000 (дефолт 1000 ms)
gpu_poll_ms = 3000                 # 1000..60000 (щадний режим для dGPU ноутбуків)
cpu_temp_source = "provider"       # "auto" | "acpi" | "provider" | "off"
active_preset = "classic-2row"     # Ім'я останнього застосованого пресета (довідково)

# Дефолтний вибір пристроїв (використовується елементами з target = "auto")
default_adapter_luid = 0           # 0 = авто-вибір активного маршруту
default_adapter_name = ""          # fallback-назва адаптера
default_gpu_luid = 0               # 0 = авто (пріоритет dGPU)
default_disk_index = 0             # PhysicalDrive0 (якщо None — перший NVMe)

# ============================================================================
# 2. АЛІАСИ ПРИСТРОЇВ (короткі імена користувача)
# ============================================================================
[aliases.disks]
"Apacer AS2280P4U 1TB" = "NVMe"
"HGST HTS721010A9E630" = "HDD"

[aliases.gpus]
"NVIDIA GeForce GTX 1650" = "GTX"
"AMD Radeon(TM) Graphics" = "iGPU"

[aliases.adapters]
"Intel(R) Wi-Fi 6 AX200 160MHz" = "Wi-Fi"

# ============================================================================
# 3. ТАСКБАР І ПОЗИЦІОНУВАННЯ (усі розміри у DIP — 96 DPI baseline)
# ============================================================================
[taskbar]
show = true
backend = "overlay"                # "overlay" (default) | "embed" (experimental) | "tray_only"
monitor = "primary"                # "primary" | { device = "\\\\.\\DISPLAY2", name = "DELL" }
anchor = "tray_left"               # "tray_left" | "taskbar_left" | "taskbar_right" | "free_drag"
x_offset = 0                       # -2000..2000 DIP
y_offset = 0                       # -2000..2000 DIP
left_space_win11 = 160             # Відступ під Widgets при anchor = "taskbar_left"
right_space_fallback = 280         # Fallback-відступ, якщо TrayNotifyWnd відсутній
snap_distance = 8                  # Поріг магнітного прилипання при перетягуванні (DIP)
lock_position = true               # Блокувати випадкове перетягування (Alt+Drag все одно працює)
click_through = false              # WS_EX_TRANSPARENT (Alt тимчасово вимикає)
autohide_behavior = "hide_with_taskbar" # "hide_with_taskbar" | "keep_visible"

# ============================================================================
# 4. ПОВЕРХНЯ ВІДЖЕТА (Фон, прозорість, рамка, відступи)
# ============================================================================
[surface]
background_mode = "transparent"    # "transparent" | "solid_auto" | "custom" | "hdr_safe"
background_color = "#202020D8"     # Використовується для "custom" / "hdr_safe" (#RRGGBBAA)
opacity = 240                      # Глобальна прозорість 30..255
corner_radius = 6.0                # 0.0..16.0 DIP
border_width = 0.0                 # 0.0..2.0 DIP
border_color = "theme.border"      # "#RRGGBBAA" або токен "theme.border" / "theme.accent"
text_shadow = true                 # М'яка 1 px тінь для читабельності на прозорому таскбарі
padding = { top = 2, right = 6, bottom = 2, left = 6 }
cell_padding = { x = 3, y = 1 }
# background_image = "themes/my-skin/background.png" # Опційно для скінів

# ============================================================================
# 5. МАКЕТ (1–4 рядки, довільна сітка, вертикальний таскбар, групи)
# ============================================================================
[layout]
mode = "flow_rows"                 # "flow_rows" | "grid" | "absolute_skin"
rows = 2                           # 1..4 (для flow_rows)
fill_order = "column_major"        # "column_major" (як TrafficMonitor) | "row_major"
column_sizing = "fit_content"      # "fit_content" | "equal"
width_mode = "auto"                # "auto" | { fixed = 294 }
max_width = 700                    # Макс. ширина в auto-режимі (DIP)
height = 36                        # Висота віджета (DIP), або 0 = під висоту таскбара
gap_x = 8                          # Горизонтальний інтервал між колонками (DIP)
gap_y = 1                          # Вертикальний інтервал між рядками (DIP)
vertical_auto_adapt = true         # Авто-стек на вертикальному таскбарі (ABE_LEFT/RIGHT)

# Опційні візуальні групи ("пігулки" або розділювачі між групами)
[[layout.groups]]
id = "net"
style = "none"                     # "none" | "pill" | "separator_after"
pill_bg = "#FFFFFF0D"
pill_radius = 5.0
pill_padding_x = 5

[[layout.groups]]
id = "compute"
style = "none"

# ============================================================================
# 6. ТИПОГРАФІКА (роздільно для label / value / unit + tabular-nums)
# ============================================================================
[typography]
tabular_nums = true                # DirectWrite OpenType 'tnum' (цифри не стрибають!)
color_emoji = true                 # D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT
value_align = "right"              # "left" | "right"

[typography.label]
family = "Segoe UI Variable Small" # Fallback на "Segoe UI" на Win10
size_pt = 7.5
weight = 400                       # 100..900
italic = false
color = "theme.label"              # "#RRGGBBAA" або "theme.label"

[typography.value]
family = "Segoe UI Variable Small"
size_pt = 8.5
weight = 600
italic = false
color = "theme.value"

[typography.unit]
family = "Segoe UI Variable Small"
size_pt = 7.0
weight = 400
italic = false
color = "theme.unit"               # Автоматично ~72% прозорості від value або фіксований

# ============================================================================
# 7. ФОРМАТУВАННЯ ЧИСЕЛ ТА ОДИНИЦЬ
# ============================================================================
[format]
unit_byte = false                  # false = b/s (біти), true = B/s (байти)
speed_unit = "auto"                # "auto" | "kbps" | "mbps"
digits = 4                         # Мінімальна ширина числового слота (символів)
hide_unit = false
hide_percent = false
separate_space = true              # "2.7 Mb/s" vs "2.7Mb/s"
short_mode = false                 # "2.7M"
decimal_separator = "dot"          # "dot" | "comma" | "locale"
temp_unit = "celsius"              # "celsius" | "fahrenheit"

# ============================================================================
# 8. ЕЛЕМЕНТИ ВІДЖЕТА (довільна кількість, Drag-and-Drop порядок)
# ============================================================================
[[items]]
uid = "net_up"
group = "net"
visible = true
label = "↑"
source = { kind = "net_up", adapter = "auto" }
# grid = { row = 0, col = 0, row_span = 1, col_span = 1 }  # Використовується при layout.mode = "grid"
visual = { mode = "text" }
color_rule = { mode = "static", color = "#7FD37F" }

[[items]]
uid = "net_down"
group = "net"
visible = true
label = "↓"
source = { kind = "net_down", adapter = "auto" }
visual = { mode = "text_sparkline", placement = "background", style = "area", window_sec = 60, scale = "auto_decay", fill_alpha = 0.20, stroke_width = 1.25 }
color_rule = { mode = "static", color = "#7FD37F" }

[[items]]
uid = "cpu_load"
group = "compute"
visible = true
label = "CPU:"
source = { kind = "cpu_usage" }
visual = { mode = "text_progress", style = "bar_bottom", thickness = 2.0, track_alpha = 0.16 }
color_rule = { mode = "gradient", space = "oklab", stops = [
  { at = 0.0,  color = "#7FD37F" },
  { at = 65.0, color = "#F0C674" },
  { at = 90.0, color = "#F26D6D" },
] }

[[items]]
uid = "ram_load"
group = "compute"
visible = true
label = "MEM:"
source = { kind = "memory_usage", display_as = "percent" } # "percent" | "used_gb" | "used_total_gb"
visual = { mode = "text_progress", style = "bar_bottom", thickness = 2.0 }
color_rule = { mode = "steps", stops = [
  { at = 0.0,  color = "#F0C674" },
  { at = 85.0, color = "#F28B50" },
  { at = 93.0, color = "#F26D6D" },
] }

[[items]]
uid = "dgpu_load"
group = "compute"
visible = true
label = "GPU:"
source = { kind = "gpu_usage", target = { role = "discrete" } }
visual = { mode = "text" }
color_rule = { mode = "static", color = "#7FBFEF" }

[[items]]
uid = "cpu_temp"
group = "compute"
visible = true
label = "CPU:"
source = { kind = "cpu_temp", source = "auto" }
visual = { mode = "text" }
color_rule = { mode = "gradient", space = "oklab", stops = [
  { at = 40.0, color = "#7FD37F" },
  { at = 70.0, color = "#F0C674" },
  { at = 85.0, color = "#F26D6D" },
] }

[[items]]
uid = "dgpu_temp"
group = "compute"
visible = true
label = "GPU:"
source = { kind = "gpu_temp", target = { role = "discrete" } }
visual = { mode = "text" }
color_rule = { mode = "gradient", space = "oklab", stops = [
  { at = 45.0, color = "#7FBFEF" },
  { at = 72.0, color = "#F0C674" },
  { at = 84.0, color = "#F26D6D" },
] }

[[items]]
uid = "nvme_temp"
group = "compute"
visible = true
label = "NVMe:"
source = { kind = "disk_temp", target = { index = 0 } }
visual = { mode = "text" }
color_rule = { mode = "steps", stops = [
  { at = 0.0,  color = "#7FD37F" },
  { at = 60.0, color = "#F0C674" },
  { at = 75.0, color = "#F26D6D" },
] }

# ============================================================================
# 9. ВЗАЄМОДІЇ ТА РОЗШИРЕНИЙ FLYOUT (Tooltip)
# ============================================================================
[interactions]
left_click = "toggle_flyout"       # "none" | "toggle_flyout" | "open_settings" | "open_task_manager" | "open_resource_monitor" | "open_network_stats" | "toggle_game_mode" | "cycle_preset" | "copy_summary" | { run_command = { path = "...", args = [] } }
double_click = "open_task_manager"
middle_click = "open_settings"
wheel_action = "none"              # "none" | "cycle_preset"
cycle_presets = ["classic-2row", "minimal-1row", "gamer-sparklines"]

[flyout]
enabled = true
trigger = "hover_and_click"        # "hover_and_click" | "click_only" | "hover_only"
hover_delay_ms = 350               # 100..2000 ms
backdrop = "acrylic"               # "acrylic" | "solid"
width = 380                        # DIP
show_cpu_per_core = true
show_graphs = true
show_all_gpus = true
show_all_disks = true
show_network_details = true
show_provider_status = true

# ============================================================================
# 10. GAME MODE ТА ПРАВИЛА АВТОПРИХОВУВАННЯ
# ============================================================================
[game_mode]
auto_hide_fullscreen = true        # SHQueryUserNotificationState (D3D fullscreen / presentation)
auto_hide_borderless = true        # Foreground вікно покриває монітор віджета
action = "hide_and_low_power"      # "hide_only" | "hide_and_low_power" | "strict_anticheat"
process_rules = [                  # Глоб-патерни імені exe або повного шляху
  "cs2.exe",
  "valorant*.exe",
  "*\\steamapps\\common\\*",
]
whitelist_processes = [            # Ігнорувати fullscreen для цих програм
  "mstsc.exe",
]

# ============================================================================
# 11. ЗОВНІШНІ ПРОВАЙДЕРИ СЕНСОРІВ (opt-in)
# ============================================================================
[providers]
hwinfo = false
lhm_http = false
lhm_url = "http://127.0.0.1:8085/metrics"
helper = false
```

---

## 6. Структура Cargo Workspace та граф залежностей

Щоб `oxide-monitor.exe` **ніколи** не лінкував `egui`/`eframe`/`wgpu`/`roxmltree`/`zip`, workspace організується у **9 чітко розмежованих крейтів** (2 бінарних target-и: `oxide-monitor.exe` і `oxide-settings.exe`):

```
oxide-monitor/
├─ Cargo.toml                     # Workspace root (edition 2024)
├─ crates/
│  ├─ oxide-core/                 # [Pure Rust] Config v2 + міграція v1→v2, MetricSource,
│  │                              # Snapshot, HistoryRing, HardwareInventory, ColorRule (OKLab),
│  │                              # DisplayModelBuilder, FormatOptions, Fluent i18n, IPC schema
│  ├─ oxide-render/               # [Win32 D2D/DWrite/GDI] LayoutEngine v2, WidgetRenderer (DIB),
│  │                              # FlyoutRenderer (DIB), tabular-nums, sparklines, progress/arcs
│  ├─ oxide-sensors/              # [Win32 native] PDH, IP Helper (повний список адаптерів),
│  │                              # per-core CPU (NtQuerySystemInformation), Memory, NVMe/Disks
│  ├─ oxide-gpu/                  # [Win32 native] DXGI, PDH GPU, NVML, ADLX, Level Zero
│  ├─ oxide-providers/            # [External] HWiNFO SHM, LHM HTTP, heuristics, ProviderStatus
│  ├─ oxide-ui/                   # [Win32 host] Overlay window, Tray, ClickActions, Flyout host,
│  │                              # Auto-hide WinEventHook, GameMode detector, IPC Server
│  ├─ oxide-import/               # [Pure Rust] Імпорт TrafficMonitor (config.ini, skin.xml/ini),
│  │                              # експорт/імпорт .oxide-preset.toml та .oxide-theme.zip
│  ├─ oxide-app/                  # [bin: oxide-monitor.exe] Composition root, single-instance,
│  │                              # sensor loop, запуск oxide-settings.exe
│  └─ oxide-settings/             # [bin: oxide-settings.exe] egui 0.37 + eframe (wgpu DX12) +
│                                 # egui_dnd + DWM Mica + Fluent theme + Live Preview (oxide-render)
├─ resources/
│  ├─ presets/                    # Вбудовані .oxide-preset.toml (5 пресетів)
│  └─ ...                         # іконка, маніфест (спільний asInvoker + PMv2)
└─ research/
   ├─ 01..05_*.md
   └─ ui/01..04_*.md
```

### Граф залежностей (доказ чистоти `oxide-monitor.exe`)

```
                    ┌──────────────┐
                    │  oxide-core  │  (serde, toml, fluent, OKLab, IPC types)
                    └──────┬───────┘
       ┌───────────────────┼────────────────────┬──────────────────┐
       ▼                   ▼                    ▼                  ▼
┌──────────────┐   ┌──────────────┐    ┌────────────────┐  ┌──────────────┐
│ oxide-render │   │oxide-sensors │    │oxide-providers │  │ oxide-import │
│ (D2D/DWrite) │   │+ oxide-gpu   │    │ (HWiNFO / LHM) │  │ (ini/xml/zip)│
└──────┬───────┘   └──────┬───────┘    └────────┬───────┘  └──────┬───────┘
       │                  │                     │                 │
       ├──────────────┐   │                     │                 │
       ▼              ▼   ▼                     ▼                 │
┌────────────┐   ┌─────────────────────────────────┐              │
│  oxide-ui  │──►│   oxide-app (oxide-monitor.exe) │              │
│ (Win32)    │   │   ❌ НЕ ЗАЛЕЖИТЬ від egui/wgpu! │              │
└────────────┘   └─────────────────────────────────┘              │
       │                                                          │
       └──────────────► ┌─────────────────────────────────────────▼──┐
       (oxide-render)   │   oxide-settings (oxide-settings.exe)      │
                        │   egui 0.37 + eframe(wgpu DX12) + egui_dnd │
                        └────────────────────────────────────────────┘
```

**Ключова властивість графа:** `oxide-monitor.exe` не має жодного шляху залежностей до `egui`, `eframe`, `wgpu`, `egui_dnd`, `rfd`, `roxmltree` чи `zip`. Це гарантує швидку збірку ядра, мінімальний розмір фонового `.exe` та 0 B зайвої пам'яті.

---

## 7. Покроковий план реалізації

> Оцінки подано в інженерних етапах для послідовної, верифікованої розробки. Кожен етап завершується повним прогоном тестів, `cargo check --target aarch64-pc-windows-msvc` і перевіркою бюджету RAM/CPU.

| Етап | Зміст робіт | Критерій готовності (Definition of Done) |
|---|---|---|
| **U0. Config v2 та `oxide-core`** | Розширення `Config` до v2 (`MetricSource`, `LayoutConfig`, `SurfaceConfig`, `TypographyConfig`, `ColorRule` з OKLab, `Interactions`, `Flyout`, `GameMode`, `Aliases`); автоматична міграція `v1 → v2`; `HistoryRing<f32, 60>`; `DisplayModelBuilder`; `HardwareInventory`; схема IPC. | 100% unit-тестів міграції v1→v2, OKLab інтерполяції, форматування та резолвінгу multi-GPU/multi-disk. Старий `config.toml` відкривається без помилок. |
| **U1. `oxide-render` (Direct2D / DirectWrite)** | Виділення рендерера у `oxide-render`; перехід на `ID2D1DCRenderTarget` + premultiplied BGRA8 DIB + `UpdateLayeredWindowIndirect`; окремі `FontSpec` + OpenType `tnum`; 4 візуальні режими (`text`, `text_sparkline`, `text_progress`, `graph_only`); `LayoutEngine v2` (`flow_rows`, `grid`, `vertical_auto`, `groups`); GDI fallback. | Віджет малюється через D2D без стрибків цифр; 5 вбудованих пресетів рендеряться коректно при 100%/150%/200% DPI; CPU `< 0.3%` у текстовому режимі, `0 GPU wake-up`; golden-image тести DIB. |
| **U2. Поведінка, Flyout, Auto-hide та Game Mode (`oxide-ui`)** | Конфігуровані кліки (`left/double/middle/wheel`); Win11 Flyout (`OxideFlyout` на D2D + Acrylic + per-core CPU heatmap + 60s графіки + ліниве звільнення DIB через 5 с); 4 режими `anchor` + `Alt+Drag` зі снапом; `SetWinEventHook` для Auto-hide і borderless fullscreen / `process_rules`; multi-monitor DIP. | Flyout відкривається за `< 12 ms` і повертає 0 B через 5 с; Auto-hide та Game Mode працюють подієво (`0%` додаткового CPU); перевірено на 2 моніторах із різним DPI. |
| **U3. IPC-міст, скелет `oxide-settings.exe`, memory audit та точний `294×35`** | Named Pipe сервер у `oxide-ui` (DACL + `PIPE_REJECT_REMOTE_CLIENTS`); бінарник `oxide-settings.exe` на `egui 0.37` + `eframe` (`wgpu` DX12) + `DwmSetWindowAttribute` (Mica Alt, Rounded Corners, Dark Mode) + `fluent_theme.rs` + `Segoe UI Variable`; запуск із трею/Flyout + прямий запуск без аргументів; синхронізація `PreviewConfig` / `CommitConfig` / `RevertConfig`. **Backlog із U1:** (1) **Memory audit** (`WS ~54 MB / Private ~42 MB / 21 threads` на інтерактивній машині — аудит `nvml.dll`, `d2d1`/`D3D10Warp`, пулів потоків, ціль `< 30 MB` private); (2) **Точний legacy-розмір `294×35`** для `classic-2row` (режим фізичних пікселів або точний підбір padding/typography при нестандартному DPI на кшталт `106`). | Вікно налаштувань відкривається за `< 160 ms`, має нативний вигляд Win11 Mica; після закриття `oxide-settings.exe` фоновий `oxide-monitor.exe` повертається до `< 30 MB` private і `0` GPU контекстів; `classic-2row` підтримує точний еталон `294×35`. |
| **U4. Конструктор віджета та Live Preview** | Панель Live Preview на спільному `oxide-render` (масштаб 100/150/200%, перемикач фону таскбара, симулятор стрес-навантаження 0→100%); Drag-and-Drop редактор (`egui_dnd`) для рядків/груп/сітки; інспектор елемента (визуальний режим, Color Picker, візуальний редактор OKLab-порогів, типографіка); Undo/Redo 50 кроків. | Будь-яка зміна миттєво оновлює і Live Preview, і реальний таскбар (< 50 ms); «Скасувати» повністю відкочує таскбар; `Ctrl+Z/Y` працює для всіх дій конструктора. |
| **U5. Каталог заліза, Провайдери та Пресети** | Вкладка «Сенсори та залізо» з живими значеннями (адаптери, iGPU/dGPU, NVMe/SATA диски, аліаси, додавання в 1 клік); вкладка «Зовнішні провайдери» (статус HWiNFO/LHM, браузер сенсорів, попередження про античити); галерея 5 вбудованих пресетів із живими мініатюрами; експорт/імпорт `.oxide-preset.toml` та `.oxide-theme.zip`. | Користувач може перейменувати диск на `NVMe`, додати обидві GPU у віджет та обмінятися пресетом за 2 кліки; повна локалізація `uk`/`en`/`ru` через Fluent. |
| **U6. Імпортер TrafficMonitor (`oxide-import`) і полірування** | Парсери `config.ini`, `skin.xml`, `skin.ini`; 3-кроковий майстер імпорту зі звітом сумісності та прев'ю; пошук `Ctrl+K` по налаштуваннях; фінальне тестування на Win10 22H2 + Win11 x64 + ARM64, HDR on/off, sleep/resume, Explorer restart. | Існуючий `E:\TrafficMonitor\config.ini` імпортується автоматично у візуально ідентичний віджет; `cargo check --target aarch64-pc-windows-msvc` зелений для всього workspace; упаковка `package.ps1` включає обидва `.exe`. |

---

## 8. Питання для узгодження перед початком реалізації

Перед тим як переходити до написання коду, прошу підтвердити 5 ключових рішень:

1. **Два бінарники (`oxide-monitor.exe` + `oxide-settings.exe`) та стек `egui 0.37` (`wgpu` DX12):**
   - Підтверджуємо `egui` в окремому процесі (100% MIT/Apache-2.0, 0 B RAM/GPU після закриття, найкращий DnD-конструктор), чи ви схиляєтесь до альтернативи `Slint 1.19` (готовий Fluent-вигляд, але Royalty-Free з бейджем `AboutSlint` або GPLv3 для `oxide-settings.exe`)?
2. **Пріоритет черговості етапів (`U0→U1→U2→U3` vs ранній UI):**
   - Чи йдемо рекомендованим порядком: спершу рушій рендерингу `oxide-render` (D2D/DWrite + `config.toml` v2 + Flyout), а потім `oxide-settings.exe`, чи хочеться побачити базове вікно налаштувань якомога раніше (паралельно з переходом на D2D)?
3. **Формат дистрибуції з двома `.exe`:**
   - У portable ZIP лежатимуть `oxide-monitor.exe` та `oxide-settings.exe` поруч. Це прийнятно (чи бажано пакувати їх одним інсталятором, зберігши portable-режим)?
4. **Обсяг Flyout-панелі у першій ітерації:**
   - Чи достатньо у першій версії Flyout показати CPU (+ per-core), RAM, усі GPU, усі диски, мережу (+ 60s графіки) і статус провайдерів, а історію трафіку по днях/місяцях винести окремою вкладкою/вікном у `oxide-settings.exe`?
5. **Імпорт скінів TrafficMonitor:**
   - Чи потрібен повний імпорт растрових фонів `background.png` + абсолютних координат `skin.xml` (`layout_s`/`layout_l`), чи для v1 достатньо імпорту `config.ini` (елементи, порядок, шрифти, кольори, підписи, одиниці) + кольорів/шрифтів із `skin.xml`?
