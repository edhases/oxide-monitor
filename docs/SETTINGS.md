# Oxide Monitor Settings (`oxide-settings.exe`)

`oxide-settings.exe` — окремий короткоживучий Windows 11 Fluent застосунок налаштувань для Oxide Monitor (див. **ADR-14** у [`ARCHITECTURE.md`](../ARCHITECTURE.md) та [`docs/IPC.md`](IPC.md)).

## 1. Архітектура та ізоляція

- **Окремий процес:** `oxide-settings.exe` живе лише поки відкрито вікно налаштувань. Після закриття ОС миттєво звільняє 100% RAM і DirectX 12 ресурсів; фоновий `oxide-monitor.exe` **ніколи** не лінкує `egui`/`eframe`/`wgpu`.
- **Рендерер:** `eframe 0.37` + `wgpu 30.0` у строгому **DX12-only** режимі (`wgpu` features: `dx12`, `parking_lot`, `std`, `wgsl` — без `glow`, `vulkan`, `ash`, `glutin`, `gles`), із `PowerPreference::LowPower` (використовує інтегровану GPU і не будить дискретну відеокарту).
- **Нативний вигляд Windows 11:** `DwmSetWindowAttribute` активує **Mica Alt** (`DWMWA_SYSTEMBACKDROP_TYPE = 4`), заокруглені кути (`DWMWCP_ROUND`) та системну темну/світлу тему (`DWMWA_USE_IMMERSIVE_DARK_MODE`) із автоматичним fallback `Mica → Solid`. Системний шрифт `SegUIVar.ttf` / `segoeui.ttf` підвантажується з `%WINDIR%\Fonts`.
- **Single instance:** іменований м'ютекс `Local\OxideMonitorSettingsSingleInstance` — повторний запуск переводить існуюче вікно на передній план.
- **Аварійний fallback:** якщо DirectX 12 / `wgpu` не ініціалізується (наприклад, пошкоджений відеодрайвер або безграфічна сесія), `oxide-settings.exe` показує локалізоване повідомлення й відкриває `config.toml` у `notepad.exe` без блокування `oxide-monitor.exe`.

## 2. Покриття всіх секцій `Config` v2

GUI є основним шляхом редагування конфігурації (TOML лишається advanced/fallback через кнопку **«Відкрити TOML…»**):

1. **Пресети та загальне (`general`):** 5 вбудованих пресетів (миттєвий preview без запису на диск), мова (`uk` / `en` / `ru` через Fluent), тема (`auto` / `dark` / `light`), автозапуск, інтервали опитування сенсорів і GPU, джерело температури CPU (`Auto` / `Acpi` / `Provider` / `Off`), пріоритетні пристрої (`disk_index`, `adapter`, `default_gpu_luid`), інформація про `config.toml` і ревізію.
2. **Елементи та аліаси (`items` + `aliases`):** видимість, порядок (`↑`/`↓`), дублювання, додавання/видалення, повний інспектор `ItemConfig` (`uid`, `label`, `group`, усі 16 типів `MetricSource` із таргетами, усі 4 візуальні режими `Text`/`TextSparkline`/`TextProgress`/`GraphOnly`, редактор `ColorRule` `Static`/`Steps`/`Gradient` OKLab, `GridSlot`) та аліаси для GPU, дисків і мережевих адаптерів із підказками знайденого заліза.
3. **Макет і поверхня (`layout` + `surface`):** `mode`, `rows`, `fill_order`, `column_sizing`, `width_mode`, швидка кнопка **«Встановити 294×35»** + `exact_physical_size`, `max_width`, `height`, `gap_x`/`gap_y`, `vertical_auto_adapt`, візуальні групи/пігулки; `background_mode`, кольори фону й рамки з альфою, `opacity`, `corner_radius`, `border_width`, `text_shadow`, `padding`, `cell_padding`, `background_image`.
4. **Типографіка і формат (`typography` + `format`):** `tabular_nums` (OpenType `tnum`), `color_emoji`, `value_align`, окремі `FontSpec` для `label`/`value`/`unit`; `unit_byte`, `speed_unit`, `digits`, `hide_unit`, `hide_percent`, `separate_space`, `short_mode`, `decimal_separator`, `temp_unit`.
5. **Таскбар і кліки (`taskbar` + `interactions`):** `show`, `backend`, вибір монітора, 4 режими `anchor`, зміщення `x_offset`/`y_offset`, відступи Win11/fallback, `snap_distance`, `lock_position`, `click_through`, `autohide_behavior`; дії для `left_click`, `double_click`, `middle_click`, `wheel_action`, `cycle_presets` (включно з `RunCommand`).
6. **Flyout та ігровий режим (`flyout` + `game_mode`):** `enabled`, `trigger`, `hover_delay_ms`, `backdrop`, `width`, перемикачі 6 секцій Flyout; `auto_hide_fullscreen`, `auto_hide_borderless`, `action`, `process_rules`, `whitelist_processes` *(перемикачі зовнішніх провайдерів HWiNFO/LHM навмисно відкладені на фінальний етап)*.

## 3. Live Preview, IPC та Standalone-режим

- **Верхній Live Preview** використовує той самий `oxide_render::WidgetRenderer` (Direct2D/DirectWrite), що й віджет на панелі задач, із перемикачем темного/світлого фону таскбара, зумом `100/150/200%` і перемикачем живих даних / симулятора навантаження `0–100%`.
- **IPC-режим (коли запущено `oxide-monitor.exe`):**
  - Будь-яка зміна надсилається як `PreviewConfig` із дебаунсом ~45 ms (на диск нічого не пишеться).
  - **«Застосувати»** надсилає `CommitConfig { expected_generation, expected_hash, config }`; при зовнішній зміні файлу показується банер конфлікту без втрати чернетки.
  - **«Скасувати»** або закриття вікна без збереження відкочує віджет через `RevertConfig` / disconnect rollback.
  - **«Скинути»** завантажує дефолти у попередній перегляд (зберігаючи невідомі top-level ключі `Config.extra`).
- **Standalone-режим (без запущеного `oxide-monitor.exe`):** показує жовтий банер автономного режиму та зберігає зміни атомарно напряму в `config.toml` (`ReplaceFileW` + `.bak`) лише після натискання **«Застосувати»**.

## 4. Відомі обмеження (Known Gaps)

- **ARM64 runtime:** `oxide-settings` проходить компіляційний гейт `cargo check --workspace --target aarch64-pc-windows-msvc` (DX12-only без x86-асемблера та C++ залежностей), але **реальний запуск на фізичному ARM64-пристрої ще не перевірявся**.
- Перемикачі зовнішніх провайдерів (HWiNFO / LibreHardwareMonitor) будуть додані на окремому фінальному етапі.
