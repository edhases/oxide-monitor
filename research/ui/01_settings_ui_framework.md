# Дослідження 01: Вибір UI-фреймворку для вікна налаштувань Oxide Monitor

**Дата:** 18.11.2026  
**Контекст:** Rust (edition 2024), Windows 10 22H2+ / Windows 11 23H2–26H2, `x86_64-pc-windows-msvc` + `aarch64-pc-windows-msvc`, ліцензія ядра `MIT OR Apache-2.0`.  
**Пов'язані документи:** [`ARCHITECTURE.md`](../../ARCHITECTURE.md), [`04_taskbar_embedding_rust.md`](../04_taskbar_embedding_rust.md).

---

## 1. Постановка задачі та жорсткі обмеження

Ядро фонового віджета `oxide-monitor.exe` спроєктовано навколо чистого Win32 + Direct2D/DirectWrite (`UpdateLayeredWindowIndirect`, GDI fallback) із суворими обмеженнями:

- **CPU у спокої (1 Hz):** `< 0.3%` одного ядра;
- **RAM (Working Set):** `< 30 MB` (поточний Phase-0 прототип займає ~10–15 MB);
- **GPU wake-up у спокої:** `0` (жодного активного D3D11/D3D12/Vulkan/OpenGL контексту, що тримає дискретну відеокарту ноутбука поза D3cold);
- **Привілеї:** `asInvoker` (Medium IL), без admin;
- **Крос-архітектурність:** повна підтримка **x64** та **ARM64** (`aarch64-pc-windows-msvc`), без x86-асемблера і крихких C++ залежностей.

Вікно налаштувань має кардинально інший профіль використання: користувач відкриває його рідко (кілька хвилин на тиждень), але очікує **сучасний вигляд рівня Windows 11**, плавний скрол, інтерактивний **drag-and-drop конструктор віджета**, **color picker з альфа-каналом**, редактор динамічних порогів, інтерактивні списки заліза та **Live Preview у реальному часі**.

Головна вимога: **після закриття вікна налаштувань фоновий монітор має гарантовано повертатися до `< 30 MB` RAM і `0` GPU wake-up.**

---

## 2. Ключове архітектурне рішення: ізоляція процесу vs in-process вікно

Перш ніж обирати UI-фреймворк, необхідно відповісти на фундаментальне питання: **чи може вікно налаштувань на базі GPU-фреймворку жити всередині `oxide-monitor.exe`?**

### 2.1. Чому in-process підхід порушує бюджет RAM/GPU у Windows

Навіть якщо ретельно викликати `drop()` для `eframe`/`wgpu`/`glow`/Skia/WebView2 при закритті вікна налаштувань, у Windows 10/11 відбуваються незворотні ефекти на рівні процесу:

1. **Невивантаження системних та вендорських DLL:** ініціалізація `wgpu` (DX12/Vulkan) або OpenGL підвантажує `d3d12.dll`, `d3d12core.dll`, `dxgi.dll`, `d3dcompiler_47.dll`, а також user-mode драйвер GPU (`nvwgf2umx.dll` / `amdxc64.dll` / `qcdx12arm64xum.dll`). Після знищення `ID3D12Device` Windows loader здебільшого залишає ці модулі відображеними в адресному просторі процесу, додаючи **25–65 MB** до Working Set / Private Commit.
2. **Кеші драйвера та алокатора:** графічні драйвери NVIDIA/AMD/Qualcomm кешують pipeline states і пули дескрипторів; CRT/Rust heap (`HeapAlloc`) фрагментується після піку алокацій UI (текстури, шрифтові атласи, layout-дерева) і не повертає всі сторінки ОС без `EmptyWorkingSet` (який лише витісняє сторінки у pagefile, а не звільняє Commit).
3. **Ризик dGPU wake-up (Optimus / SmartShift):** ініціалізація DXGI/D3D12 у фоновому процесі на гібридних ноутбуках часто «будить» dGPU під час енумерації адаптерів, а залишкові DXGI-об'єкти можуть заважати переходу в D3cold.
4. **Збільшення розміру `oxide-monitor.exe`:** вбудовування `eframe`+`wgpu` або Slint збільшує головний бінарник, що завантажується при кожному старті ОС, на 4–15 MB.

### 2.2. Рішення: окремий короткоживучий процес `oxide-settings.exe`

```
┌─────────────────────────────────────────┐
│ oxide-monitor.exe (постійно у фоні)     │
│ • Pure Win32 + D2D/DWrite + Sensors     │
│ • ~1.8 MB .exe, < 25 MB RAM, 0 GPU ctx  │
└───────────────────┬─────────────────────┘
                    │ CreateProcessW (on demand)
                    │ Named Pipe: \\.\pipe\oxide-ui-v1-<pid>
                    ▼
┌─────────────────────────────────────────┐
│ oxide-settings.exe (живе лише відкритим)│
│ • UI-фреймворк + Конструктор + Preview  │
│ • Завершується при закритті вікна →     │
│   ОС миттєво звільняє 100% RAM/VRAM/DLL │
└─────────────────────────────────────────┘
```

**Переваги процесної ізоляції:**
- **100% гарантоване звільнення ресурсів:** після `ExitProcess(0)` ядро Windows повністю знищує адресний простір, усі DX12/Vulkan/OpenGL/WebView2 контексти, шрифтові атласи та завантажені GPU-драйвери. `oxide-monitor.exe` за весь uptime **ніколи** не завантажує GPU UI-стек.
- **Відмовостійкість (Fault Isolation):** якщо графічний драйвер або UI-бібліотека налаштувань впаде (наприклад, при зміні RDP/GPU-драйвера), сам монітор у таскбарі продовжить працювати безперервно.
- **Чистота ліцензій та збірок:** `oxide-monitor.exe` лишається мінімальним бінарником (~1.5–2.0 MB) із чистими залежностями; за потреби `oxide-settings.exe` можна навіть зібрати або оновити незалежно.
- **Недолік (мінімальний):** поява другого файлу `oxide-settings.exe` у каталозі встановлення / portable ZIP та необхідність тонкого IPC-каналу (Named Pipe, ~250 рядків коду на `serde_json`), який вже гармонійно вписується у дизайн IPC із `ARCHITECTURE.md` (§9.3).

---

## 3. Порівняння UI-фреймворків у Rust на Windows 11 (кінець 2026)

Нижче наведено аналіз 4 груп рішень станом на **листопад 2026 року** саме в контексті `oxide-settings.exe` для Windows 10/11 (x64 + ARM64).

### 3.1. `egui 0.37` + `eframe 0.37`

- **Актуальні версії:** `egui 0.37.0`, `eframe 0.37.0`, `egui_dnd 0.18.x`.
- **Ліцензія:** `MIT OR Apache-2.0` (повний збіг з Oxide Monitor).
- **Рендер-бекенди:**
  - `wgpu` (дефолт у `eframe 0.37`) — нативний **DirectX 12** на Windows 10/11 x64 та **ARM64** (Snapdragon X / Surface Pro). Повна апаратна підтримка без сторонніх драйверів.
  - `glow` (OpenGL) — легший бінарник (~3.5 MB) і менша RAM (~25–35 MB), але **ризикований на ARM64**, де OpenGL працює лише через окремий Microsoft OpenCL/OpenGL Compatibility Pack (який може бути не встановлений). Тому для універсального Tier 0 (x64 + ARM64) слід обирати `wgpu` з DX12 + fallback на WARP software adapter при відсутності GPU.
- **Можливості для нашого конструктора:**
  - Вбудований повноцінний **Color Picker** (`ui.color_edit_button_srgba`, HSV/RGB/Hex/Alpha) з коробки.
  - **`egui_dnd 0.18`** — зрілий, плавний drag-and-drop для перевпорядкування списків і груп із анімацією зміщення та drag-handle.
  - Проста інтеграція **Live Preview**: `egui` приймає сирий RGBA8-буфер через `ctx.load_texture` / `TextureHandle::set(ColorImage, TextureOptions::NEAREST)`. Ми можемо рендерити віджет у DIB через наш `oxide-render` (Direct2D/DirectWrite) і показувати піксель-у-піксель у вікні налаштувань.
  - **Реактивне оновлення без зайвого CPU:** `eframe` за замовчуванням працює в режимі *repaint-on-demand* (перемальовує лише при подіях миші/клавіатури або за `ctx.request_repaint_after(1s)` для оновлення живих сенсорів).
- **Win11 вигляд (Mica / Fluent):**
  - Через `raw-window-handle` (`frame.window_handle()`) у першому кадрі/creation callback викликається `DwmSetWindowAttribute` (`DWMWA_SYSTEMBACKDROP_TYPE`, `DWMWA_WINDOW_CORNER_PREFERENCE`, `DWMWA_USE_IMMERSIVE_DARK_MODE`).
  - При `ViewportBuilder::with_transparent(true)` та напівпрозорому/прозорому `panel_fill` (`Color32::TRANSPARENT`) системна Mica/Mica Alt просвічує крізь сайдбар і фон вікна.
  - **Слабке місце:** дефолтна тема `egui` виглядає як інструмент розробника (темно-сіра, дрібний вбудований шрифт Ubuntu/Hack). Для вигляду рівня Windows 11 Settings необхідно:
    1. завантажити системний шрифт Windows (`Segoe UI Variable` / `Segoe UI` із `%WINDIR%\Fonts`) через `FontDefinitions`;
    2. налаштувати кастомну `FluentVisuals` тему (~300–400 рядків: м'які картки `rounding = 8.0`, 1 px тонкі обводки `rgba(255,255,255,0.08)`, акцентовані toggle-свічі, навігаційна панель Win11, системний колір акценту з `DwmGetColorizationColor`);
    3. використати гліфи `Segoe Fluent Icons` / `Segoe MDL2 Assets` або векторні SVG-іконки.
- **Бінарник і пам'ять (`oxide-settings.exe`):**
  - Розмір `.exe` (`opt-level = "s"`, `lto = "thin"`, `strip = true`, `wgpu` з вимкненим Vulkan/GL — тільки `dx12`): **~4.5–6.0 MB** (x64 і ARM64).
  - RAM під час роботи: **~35–55 MB**; після закриття — **0 MB**.
  - Холодний старт: **~90–160 ms**.

### 3.2. `Slint 1.19`

- **Актуальна версія:** `slint 1.19.0` (листопад 2026).
- **Вигляд Windows 11:** **найкращий серед нативних Rust-фреймворків із коробки.** Починаючи з v1.16, стиль **Fluent** став дефолтним у Slint, а v1.19 додав `backdrop-blur`, підтримку `reduced-motion` і плавні анімації. Вікно налаштувань виглядає як сучасний Win11-застосунок майже без ручної стилізації.
- **Рендер-бекенди та ARM64:**
  - `renderer-femtovg-wgpu` / `renderer-software` — чистий Rust, компілюється на `aarch64-pc-windows-msvc` без C++ SDK, розмір `.exe` **~3.0–4.5 MB**. Недолік: FemtoVG на Windows історично поступається DirectWrite за якістю хінтингу дрібного тексту.
  - `renderer-skia` — чудова типографіка, але тягне важку C++ Skia-бібліотеку, сильно збільшує час збірки, ускладнює крос-компіляцію під ARM64 і роздуває `.exe` до **~16–22 MB**.
- **Live Preview:** підтримується чисто через `slint::Image::from_rgba8_premultiplied(SharedPixelBuffer)` — так само сумісно з `oxide-render`.
- **Слабкі місця для конструктора Oxide Monitor:**
  1. **Ліцензійне тертя:** Slint має потрійну ліцензію:
     - **GPLv3** — безкоштовна для open-source, але робить бінарник `oxide-settings.exe` copyleft GPLv3 (тоді як решта Oxide Monitor — `MIT OR Apache-2.0`).
     - **Royalty-Free** — безкоштовна для desktop, але вимагає публічної атрибуції (`AboutSlint` бейдж/віджет у вікні «Про програму» та на сторінці завантаження) і накладає обмеження на похідні продукти.
     - **Commercial** — платна.
  2. **Складні віджети конструктора:** на відміну від `egui`, у Slint немає готового production-grade `egui_dnd` або вбудованого HSV/Alpha Color Picker — їх доведеться писати вручну мовою `.slint` + Rust (`TouchArea`, ручний трекінг позицій перетягування).
  3. **Друга мова (`.slint` DSL):** додає окремий компілятор `slint-build` у `build.rs` і двосторонній байндінг стану між `.slint` та Rust-структурами конфіга.
- **RAM під час роботи:** **~25–45 MB** (FemtoVG/software) або **~50–70 MB** (Skia); після закриття окремого процесу — **0 MB**.

### 3.3. `iced 0.14`, `floem 0.2`, `gpui`

| Фреймворк | Версія (11.2026) | Ліцензія | ARM64 Win11 | Оцінка для `oxide-settings.exe` |
|---|---|---|---|---|
| **`iced`** | `0.14.x` | MIT | ✅ Стабільний (`winit`+`wgpu`) | Сильна типографіка (`cosmic-text`), чиста Elm-архітектура. Але: немає вбудованого color picker, drag-and-drop списків із коробки та Fluent-теми; більший бінарник (~7–12 MB); вищий boilerplate для форм із 60+ полями конфігурації (потрібен величезний `enum Message`). |
| **`floem`** | `0.2.0` | MIT/Apache | ⚠️ Експериментальний | Цікава fine-grained реактивність і Taffy layout, але Windows-підтримка (IME, accessibility, DWM Mica, фокус) помітно відстає від macOS/Linux; часті breaking changes і мала екосистема готових віджетів. |
| **`gpui`** | pre-1.0 (git / `gpui-component`) | Apache-2.0 (ядро) | ⚠️ Складна збірка | Створений для редактора Zed (120 FPS IDE). Надмірна складність для вікна налаштувань, нестійкий API (git-залежності), висока вартість підтримки Windows ARM64, великий бінарник (~12–20 MB). |

**Висновок по групі:** `iced` є життєздатним резервним варіантом, але програє `egui` за швидкістю реалізації конструктора (DnD, color picker, градієнт порогів) і розміром бінарника, а `Slint` — за нативним Fluent-виглядом. `floem` і `gpui` на кінець 2026 року не варто брати в основу production-утиліти під Windows x64+ARM64.

### 3.4. Чистий Win32 vs WebView2 (`wry 0.57` / `webview2-com 0.39`)

#### Варіант A: Чистий Win32 (`ComCtl32` + Direct2D)
- **Плюси:** мінімальний розмір (~0.4–0.8 MB), миттєвий старт (< 30 ms), RAM ~8–15 MB, нульові зовнішні залежності, 100% ARM64.
- **Мінуси:** класичні Win32-контроли (`WC_TABCONTROL`, `SysListView32`, `ChooseColorW`) виглядають застаріло (стиль Windows 7/Vista) і погано підтримують темну тему Windows 11 (потрібні недокументовані ординали `uxtheme.dll` `#135`/`#136`). Створення drag-and-drop конструктора, кастомного color picker з альфою, градієнтного редактора порогів і адаптивної Fluent-сітки на голому Direct2D — це фактично написання власного GUI-фреймворку (додає 4–8 тижнів розробки та підтримки).
- **Де чистий Win32 + Direct2D ідеальний:** для **самого таскбар-віджета** та для **розширеного Tooltip / Flyout-панелі** (де інтерфейс переважно read-only + 2–3 кнопки, а критичними є старт `< 15 ms` і `< 2 MB` RAM всередині `oxide-monitor.exe`).

#### Варіант B: WebView2 (`wry 0.57.2` + `webview2-com 0.39.1`)
- **Плюси:**
  - Крихітний власний бінарник `oxide-settings.exe`: **~2.5–4.0 MB** (WebView2 Evergreen Runtime вже вбудований у всі сучасні Windows 11 x64 і ARM64).
  - Бездоганний сучасний Win11 Fluent UI (HTML/CSS + Windows Mica через `DwmSetWindowAttribute` + прозорий фон WebView2), ідеальний рендеринг шрифтів Chromium/DirectWrite, тривіальний drag-and-drop (Pointer Events / Sortable), будь-які color pickers, Live Preview через `<canvas>` + custom protocol `oxide://preview.rgba` (або base64/binary IPC).
  - Повна нативна підтримка ARM64.
- **Мінуси:**
  - **Споживання RAM поки відкрито вікно:** **80–150 MB** (4–6 допоміжних процесів `msedgewebview2.exe`: browser, gpu, renderer, network, storage). Хоча після закриття `oxide-settings.exe` вся пам'ять повертається ОС, для «ультралегкої Rust-утиліти» пік у 130 MB при відкритті налаштувань може дивувати користувачів.
  - **Дисковий слід (`UserDataFolder`):** WebView2 створює кеш `EBWebView` (~15–35 MB) у `%LOCALAPPDATA%\OxideMonitor\`, що особливо небажано для **Portable-режиму** (треба або чистити temp-профіль при виході, що сповільнює старт до 400–600 ms, або засмічувати папку з програмою).
  - **Зовнішня залежність від Runtime:** на зрізаних корпоративних Windows 10 LTSC / Windows 10 без Edge WebView2 може бути відсутній → потрібен fallback або інсталяція bootstrapper.
  - **Два технологічних стеки:** до Rust-репозиторію додається HTML/CSS/JS (або TypeScript + bundler), що ускладнює CI та підтримку.

---

## 4. Підтримка сучасного вигляду Windows 11 (DWM & Mica)

Незалежно від обраного рендерера для `oxide-settings.exe`, справжнє «відчуття Windows 11» забезпечується комбінацією системних атрибутів **Desktop Window Manager (DWM)** через `windows::Win32::Graphics::Dwm::DwmSetWindowAttribute`:

| Атрибут DWM | Константа | Значення | Ефект та сумісність |
|---|---|---|---|
| **Mica / Acrylic backdrop** | `DWMWA_SYSTEMBACKDROP_TYPE` (`38`) | `DWMSBT_MAINWINDOW` (`2`) — Mica<br>`DWMSBT_TABBEDWINDOW` (`4`) — Mica Alt<br>`DWMSBT_TRANSIENTWINDOW` (`3`) — Acrylic | Win11 22H2+ (build 22621+). На Win11 21H2 — legacy `DWMWA_MICA_EFFECT` (`1029`). На Win10 — повертає `E_INVALIDARG`, безпечно ігнорується (fallback на суцільний колір теми). |
| **Закруглені кути** | `DWMWA_WINDOW_CORNER_PREFERENCE` (`33`) | `DWMWCP_ROUND` (`2`) | Win11 21H2+ (build 22000+). Дає нативні 8 px кути та системну тінь без кастомного hit-testing. |
| **Темний заголовок вікна** | `DWMWA_USE_IMMERSIVE_DARK_MODE` (`20`) | `BOOL(1)` / `BOOL(0)` | Win10 2004+ та Win11. Зафарбовує titlebar і системні кнопки (Min/Max/Close) у темний колір. |
| **Колір рамки / заголовка** | `DWMWA_BORDER_COLOR` (`34`) | `COLORREF` або `DWMWA_COLOR_DEFAULT` | Тонка системна рамка у колір акценту або стандартна нейтральна. |

### Технічна умова для Mica в `eframe` / `wgpu`
Щоб матеріал Mica просвічував крізь вікно:
1. У `ViewportBuilder` задається `.with_transparent(true)`.
2. У `DwmExtendFrameIntoClientArea(hwnd, &MARGINS { cxLeftWidth: -1, .. })` рамка розширюється на всю клієнтську область.
3. У `eframe::App::clear_color()` повертається `[0.0, 0.0, 0.0, 0.0]` (на Win11 з увімкненою прозорістю) або непрозорий колір теми (`#202020` / `#F3F3F3`) на Win10 та при `EnableTransparency = 0` у реєстрі (`HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize`).
4. Сайдбар навігації малюється з прозорим фоном (просвічує Mica Alt), а права робоча область — як злегка тонована картка (`rgba(32, 32, 32, 0.65)` у Dark / `rgba(255, 255, 255, 0.72)` у Light) із радіусом `8px`. Це точно відтворює офіційну геометрію **Windows 11 Settings**!

---

## 5. Зведена матриця порівняння

| Критерій | **`egui 0.37` + `eframe` (`wgpu` DX12)** *(окремий процес)* | **`Slint 1.19`** *(Fluent + FemtoVG, окремий процес)* | **`iced 0.14`** *(окремий процес)* | **WebView2 (`wry 0.57`)** *(окремий процес)* | **Чистий Win32 / D2D** *(in-process)* |
|---|---|---|---|---|---|
| **RAM фонового `oxide-monitor.exe` після закриття** | **< 25 MB (0 B overhead)** | **< 25 MB (0 B overhead)** | **< 25 MB (0 B overhead)** | **< 25 MB (0 B overhead)** | ~20–28 MB (залишаються GDI/D2D ресурси) |
| **GPU wake-up фонового процесу після закриття** | **0 (гарантовано)** | **0 (гарантовано)** | **0 (гарантовано)** | **0 (гарантовано)** | **0** |
| **RAM поки налаштування відкриті** | ~35–55 MB | ~25–45 MB | ~45–65 MB | ~80–150 MB (4–6 процесів) | ~10–18 MB |
| **Розмір `oxide-settings.exe` (release, stripped)** | ~4.5–6.0 MB | ~3.0–4.5 MB *(Skia: ~18 MB)* | ~7.0–12.0 MB | ~2.5–4.0 MB *(+ OS Runtime)* | ~0.6 MB |
| **Сумісність з ARM64 (`aarch64-pc-windows-msvc`)** | ⭐⭐⭐⭐⭐ (DX12 нативно) | ⭐⭐⭐⭐ (FemtoVG OK; Skia складно) | ⭐⭐⭐⭐⭐ (DX12 нативно) | ⭐⭐⭐⭐⭐ (вбудовано у Win11 ARM64) | ⭐⭐⭐⭐⭐ |
| **Сучасний вигляд Win11 (Mica, Dark/Light, Fluent)** | ⭐⭐⭐⭐ (Mica + кастомна Fluent-тема ~350 LoC) | ⭐⭐⭐⭐⭐ (Fluent із коробки) | ⭐⭐⭐ (кастомна тема) | ⭐⭐⭐⭐⭐ (HTML/CSS + Mica) | ⭐⭐ (величезні зусилля) |
| **Drag-and-drop списки/сітки** | ⭐⭐⭐⭐⭐ (`egui_dnd 0.18`) | ⭐⭐⭐ (ручна реалізація) | ⭐⭐ (ручна реалізація) | ⭐⭐⭐⭐⭐ (DOM/Pointer API) | ⭐ (вкрай складно) |
| **Color Picker + градієнт порогів** | ⭐⭐⭐⭐⭐ (вбудований + кастомний canvas) | ⭐⭐⭐ (треба писати самому) | ⭐⭐ (треба писати самому) | ⭐⭐⭐⭐⭐ | ⭐ |
| **Live Preview (точна копія D2D-віджета)** | ⭐⭐⭐⭐⭐ (`TextureHandle` з DIB `oxide-render`) | ⭐⭐⭐⭐⭐ (`SharedPixelBuffer`) | ⭐⭐⭐⭐ (`image::Handle`) | ⭐⭐⭐⭐ (`<canvas>` через IPC) | ⭐⭐⭐⭐⭐ (прямий `BitBlt`/D2D) |
| **Ліцензія (сумісність із MIT/Apache-2.0)** | ✅ `MIT OR Apache-2.0` | ⚠️ GPLv3 або Royalty-Free (+ бейдж) | ✅ `MIT` | ✅ `MIT OR Apache-2.0` | ✅ `MIT OR Apache-2.0` |
| **Portable-дружність (0 кешу на диску)** | ✅ Так | ✅ Так | ✅ Так | ❌ `EBWebView` 15–35 MB | ✅ Так |
| **Стек розробки** | 100% Rust | Rust + `.slint` DSL | 100% Rust | Rust + HTML/CSS/TS | 100% Rust (Win32) |
| **Холодний старт вікна** | ~90–160 ms | ~80–140 ms | ~120–200 ms | ~250–500 ms | ~20–40 ms |

---

## 6. Фінальний вибір та рекомендація

### Основне рішення: **`egui 0.37` + `eframe 0.37` (`wgpu` / DX12 only)** у виділеному процесі **`oxide-settings.exe`**

**Чому саме ця комбінація оптимальна для Oxide Monitor:**

1. **Залізне дотримання бюджету фонового процесу:** завдяки окремому процесу `oxide-settings.exe` фоновий `oxide-monitor.exe` не лінкує жодного байта `egui`/`wgpu`, не завантажує DX12-драйверів і гарантовано тримається в межах `< 0.3% CPU`, `< 30 MB RAM`, `0 GPU wake-up`.
2. **Ідеальний збіг під задачу «Конструктор віджета»:** саме в `egui` найпростіше та найнадійніше реалізуються drag-and-drop перевпорядкування (`egui_dnd`), color picker з альфа-каналом, візуальний редактор градієнта порогів, слайдери відступів і піксель-точний Live Preview з `oxide-render` — усе це з реактивним оновленням без шаблонного коду.
3. **100% Pure Rust, MIT/Apache-2.0, ARM64-ready:** жодного copyleft, жодних обов'язкових бейджів, жодного Node.js/WebView2 кешу в portable-папці, нативний DX12 на Windows 10/11 x64 та ARM64.
4. **Сучасний вигляд Windows 11:** поєднання `DwmSetWindowAttribute` (Mica Alt + Rounded Corners + Dark Titlebar), системного шрифту `Segoe UI Variable` і невеликого модуля `fluent_theme.rs` дає охайний нативний вигляд Win11 Settings.

### Важливе розмежування підсистем UI

| Поверхня UI | Де живе | Технологія | Причина |
|---|---|---|---|
| **Taskbar-віджет** | `oxide-monitor.exe` | Win32 + Direct2D/DirectWrite (`oxide-render`) → `UpdateLayeredWindowIndirect` | `< 0.3% CPU`, `0 GPU wake-up`, 1 Hz |
| **Розширений Tooltip / Flyout** | `oxide-monitor.exe` | Win32 + Direct2D/DirectWrite + DWM Acrylic/Round (лінива алокація DIB) | Старт `< 15 ms`, без крадіжки фокусу, `< 2 MB` RAM при відкритті, `0 B` у спокої |
| **Вікно налаштувань і конструктор** | `oxide-settings.exe` | `egui 0.37` + `eframe` (`wgpu` DX12) + `egui_dnd` + DWM Mica + `oxide-render` (Live Preview) | Повна ізоляція пам'яті/GPU, багаті інтерактивні контролі, MIT/Apache-2.0 |

### Коли варто переглянути вибір на користь альтернативи
- **Обрати `Slint 1.19`**, якщо ви готові прийняти бейдж `AboutSlint` (Royalty-Free) або GPLv3 для бінарника `oxide-settings.exe` заради готових Fluent-анімацій із коробки та згодні самостійно реалізувати drag-and-drop і color picker.
- **Обрати `wry 0.57` (WebView2)**, якщо пріоритетом стане веб-дизайн інтерфейсу (HTML/CSS) і ви згодні на ~120 MB RAM під час редагування та відмову від чистого portable без temp-кешу WebView2.
