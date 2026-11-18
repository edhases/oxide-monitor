# Oxide Monitor: вбудовування віджета в taskbar Windows 10/11 і вибір Rust-стеку

**Дата:** 2026-11-17
**Автор:** research subagent
**Статус:** дослідницький звіт (без коду застосунку)
**Цільові ОС:** Windows 10 21H2/22H2 (19044/19045), Windows 11 22H2 (22621), 23H2 (22631), 24H2 (26100), 25H2 (26200), а також гілка 26300+ (Insider/26H2)

> **Уточнення щодо build-номерів.** 24H2 — це **26100**, 25H2 — **26200**. Build **26300+** (зокрема 26340) — це вже наступна Insider-гілка, де Microsoft **пересунула taskbar в новий контейнер** і зламала XAML-детекцію у TranslucentTB (issue #1382). Це важливо: частина висновків для 26100/26200 не автоматично переноситься на 26300+.

---

## 0. TL;DR

1. **DeskBand для Win11 — мертвий.** Microsoft прямо пише, що desk bands «not supported as of Windows 7» і радить thumbnail toolbars; у Win11 їх немає взагалі, вони працюють лише під ExplorerPatcher/StartAllBack, які тепер малюють власний taskbar.
2. **`SetParent` у `Shell_TrayWnd` технічно працює і сьогодні** (так робить TrafficMonitor), але це cross-process parent/child: спільні input queues, задокументований «forced reset» DPI, залежність від приватної XAML-структури, ризик зламати Explorer і майже гарантована поломка на 26300+.
3. **Найстабільніший шлях — незалежне top-level layered вікно** (`WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST`), позиційоване над taskbar за `GetWindowRect(Shell_TrayWnd)` + `TrayNotifyWnd`, з трекінгом через `SetWinEventHook(EVENT_OBJECT_LOCATIONCHANGE, …, WINEVENT_OUTOFCONTEXT)` і `SHAppBarMessage(ABM_GETTASKBARPOS/ABM_GETSTATE)`. Воно не залежить від XAML, не інжектиться в Explorer і переживає його крах.
4. **Рендеринг:** цільовий — **Direct2D + DirectWrite у DIB + `UpdateLayeredWindowIndirect`** (той самий шлях, що «найкращий» режим TrafficMonitor), запасний — **чистий GDI** з тим самим DIB (на ньому стартує MVP). Жодного `egui`/`wgpu`/WebView: для смужки 200×30 px це зайва вага, зайвий GPU wake-up і проблеми з прозорістю/DPI.
5. **MVP:** overlay-вікно + GDI-рендер + 1 Гц оновлення → далі D2D/DWrite, багатомоніторність, auto-hide, повноекранний режим. **Опційно пізніше** — режим «embed» через `SetParent` з автоматичним fallback на overlay (ніколи не чіпати `MoveWindow` вікон Explorer).

---

## 1. Контекст і вимоги

Потрібен компактний віджет (текст + маленькі графіки) на taskbar, theme-aware, per-monitor DPI, без мерехтіння, <1% CPU, без шкоди для Explorer. Це відразу відсікає три «прості» варіанти: (а) deskband, (б) інжекцію DLL у `explorer.exe`, (в) XAML-халінг. Усі три або вже не працюють, або вимагають підпису/пакетної ідентичності, або ламаються на кожному оновленні taskbar.

Ключове архітектурне рішення: **віджет не повинен бути частиною taskbar**. Він має бути **сусідом**, який знає, де taskbar, і вміє до нього «прилипати».

---

## 2. Методи інтеграції

### 2.1 DeskBand (COM `IDeskBand`)

**Як влаштовано.** DeskBand — це in-proc COM-сервер (`cdylib` DLL), який Explorer завантажує у свій процес і вставляє як band у `ReBarWindow32` (Win10) або в toolbar. Мінімальний контракт:

- `IDeskBand` (успадковує `IDockingWindow` → `IOleWindow`): `GetBandInfo(DWORD dwBandID, DWORD dwViewMode, DESKBANDINFO*)`.
- `IObjectWithSite`: `SetSite(IUnknown* punkSite)` / `GetSite(REFIID, void**)` — через site band отримує `IInputObjectSite`, `IServiceProvider`, `IWebBrowser2`.
- Часто ще `IPersistStream` (стан) і `IDeskBand2` (`CanRenderComposited`, `SetCompositionState`) для composited-режиму.
- `DESKBANDINFO` (а не «DeskBandInfo») повертає `ptMinSize`, `ptMaxSize`, `ptActual`, `wszTitle[256]`, `dwModeFlags` (`DBIMF_NORMAL`, `DBIMF_FIXED`, `DBIMF_VARIABLEHEIGHT`, `DBIMF_USECHEVRON`, …), `crBkgnd`.

**Реєстрація (64-bit view):**

```
HKCR\CLSID\{GUID}\InprocServer32        = C:\Program Files\Oxide\OxideBand.dll
                                          ThreadingModel = Apartment
HKCR\CLSID\{GUID}\Implemented Categories\{00021492-0000-0000-C000-000000000046}  ; CATID_DeskBand
HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Shell Extensions\Approved
                                          {GUID} = "Oxide Monitor Band"
```

Для per-user — `HKCU\Software\Classes\CLSID\...`. На x64 Windows Explorer — 64-bit, тому DLL має бути x64 (на ARM64 — ARM64); 32-бітна версія потрібна лише для старих 32-бітних систем. Після реєстрації band з'являється в контекстному меню taskbar → **Toolbars**.

**Історія та статус:**

- Microsoft у документації `IDeskBand` пише: *«You should use thumbnail toolbars in new development in place of desk bands, which are not supported as of Windows 7»*. `DESKBANDINFO` описаний як структура для **deprecated** `IDeskBand::GetBandInfo`.
- У Windows 11 deskband-API немає: EverythingToolbar прямо пояснює, що «these custom UIs (called deskbands) were removed from Windows 11», і на Win11 пропонує або launcher-режим, або StartAllBack/ExplorerPatcher.
- **24H2:** Microsoft прибрав у 24H2 залишки stock Windows 10 taskbar, тому ExplorerPatcher більше не «повертає» його, а малює власний («Windows 10 (ExplorerPatcher)»). Відповідно deskband під EP працює лише тому, що EP емулює Win10-контейнер.
- **25H2:** усе ще гірше — у трекері EverythingToolbar користувачі повідомляють, що StartAllBack перестав показувати меню Toolbars, а EP — ні.

**Проблеми:** DLL живе **всередині `explorer.exe`** → будь-який crash/AV/утечка = падіння shell; складна COM-реєстрація; 32/64-бітність; після рестарту Explorer band перестворюється, але стан губиться; на Win11 не працює без сторонніх патчерів. **Вердикт: не використовувати.**

### 2.2 `SetParent` / `WS_CHILD` у taskbar

Це те, що робить TrafficMonitor (C++/MFC) і що повторюють численні форки.

**Win10-ієрархія (реальні класи вікон):**

```
Shell_TrayWnd                      (top-level, Explorer, WS_EX_TOPMOST)
├── ReBarWindow32                  (rebar-контейнер)
│   ├── MSTaskSwWClass             (контейнер кнопок задач)
│   │   └── MSTaskListWClass       (toolbar з кнопками)
│   └── ... (address/desktop bands)
├── TrayNotifyWnd
│   ├── SysPager → ToolbarWindow32 (іконки трею)
│   └── TrayClockWClass
└── Start / ...                    (кнопка Пуск у деяких збірках)
```

TrafficMonitor робить так (`ClassicalTaskbarDlg.cpp`):

```cpp
m_hBar = FindWindowEx(m_hTaskbar, 0, L"ReBarWindow32", NULL);
if (!m_hBar) m_hBar = FindWindowEx(m_hTaskbar, nullptr, L"WorkerW", NULL);
m_hMin = FindWindowEx(m_hBar, 0, L"MSTaskSwWClass", NULL);
if (!m_hMin) m_hMin = FindWindowEx(m_hBar, 0, L"MSTaskListWClass", NULL);
```

Далі — найагресивніше: він **зменшує** `MSTaskSwWClass` через `MoveWindow(m_hMin, …)` на ширину віджета, а на виході відновлює прямокутник (`ResetTaskbarPos`). Це cross-process `MoveWindow` у вікно Explorer — саме те, що ламається при зміні версії та при краху.

**Win11-ієрархія (22H2–25H2):**

```
Shell_TrayWnd
├── Windows.UI.Composition.DesktopWindowContentBridge   (HWND-хост XAML-острова)
│   └── Taskbar.TaskbarFrame → Grid#RootGrid → Taskbar.TaskbarBackground
│       ├── Rectangle#BackgroundFill
│       └── Rectangle#BackgroundStroke
├── Windows.UI.Input.InputSite.WindowClass               (input site; у UIA-дереві 26200)
├── TrayNotifyWnd                                        (залишається Win32!)
└── Start (у частині збірок)
```

На 26200 UIA-дерево виглядає як `Shell_TrayWnd` → `Windows.UI.Input.InputSite.WindowClass` → `Taskbar.TaskbarFrameAutomationPeer` → кнопки. Клас `Windows.UI.Composition.DesktopWindowContentBridge` — стабільний маркер «це Win11 taskbar» (його використовують і TrafficMonitor, і Rust-проєкт SPlayer).

**Win11-підхід TrafficMonitor** (`Win11TaskbarDlg.cpp`): `SetParent(m_hWnd, Shell_TrayWnd)` (без `ReBarWindow32`), потім позиціювання **відносно `TrayNotifyWnd`**:

```cpp
m_hNotify = FindWindowEx(m_hTaskbar, 0, L"TrayNotifyWnd", NULL);
m_hStart  = FindWindowEx(m_hTaskbar, nullptr, L"Start", NULL);
...
m_rect.MoveToX(notify_x_pos - m_rect.Width() + 2);
```

Є нюанси, зафіксовані в коді: на secondary-моніторах `TrayNotifyWnd` може не існувати (fallback — 88 px, scaled by DPI), у 24H2 з'явилися проблеми з прозорістю (issue #1953), а з віджет-панеллю — з позицією (issue #1958); при `transparent_color == 0` у темній темі ламається контекстне меню, тому TrafficMonitor підмінює 0 на 1.

**Чому це небезпечно (задокументовано):**

- Raymond Chen: cross-process parent/child **технічно легальний**, але «technically legal to juggle chainsaws» — він **неявно attach'ить input queues** потоків, причому транзитивно; «things will definitely stop working if you change that other window from a top-level window to a child window». Для нас це означає: наш UI-потік ділить чергу введення з GUI-потоком Explorer, і його зависання/крах тягне нас.
- `SetParent` doc: якщо батько й дитина в різних DPI-awareness режимах, для **cross-process** виклику відбувається **«forced reset» (of child window's process)**. Тобто ми можемо втратити Per-Monitor V2 просто через вбудовування.
- Дочірнє вікно **знищується** разом з батьком: крах Explorer → `WM_NCDESTROY` для нашого HWND → треба перестворювати вікно, а не просто переприв'язувати.
- Z-order/clipping: XAML-контент малюється DComp-шаром поверх дочірніх HWND; наше вікно може бути **перекрите** або обрізане межами `Shell_TrayWnd`. Саме тому в Win11 доводиться «підганяти» позицію, а не резервувати місце.
- **26340+:** taskbar переїхав у новий контейнер; `Taskbar.TaskbarFrame` більше не лежить безпосередньо під очікуваним батьком (TranslucentTB #1382). HWND-прив'язка до `Shell_TrayWnd` це переживе (він лишився), але будь-яка логіка на XAML-структурі — ні.

**Вердикт:** допустимо як **опційний режим** з перевіркою успіху й миттєвим fallback, але не як основу.

### 2.3 Top-level layered overlay

Найпростіший і найстабільніший метод: звичайне **top-level** вікно без батька, яке виглядає як частина taskbar.

```rust
// ілюстративно; windows 0.62
let ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST;
let hwnd = CreateWindowExW(ex, w!("OxideOverlay"), w!("Oxide Monitor"),
    WS_POPUP, x, y, w, h, None, None, hinstance, None)?;
```

Ключові стилі:

| Стиль | Навіщо |
|---|---|
| `WS_EX_LAYERED` | per-pixel alpha через `UpdateLayeredWindow` (підтримується і для child з Win8) |
| `WS_EX_TOOLWINDOW` | не показувати в Alt+Tab і на панелі задач |
| `WS_EX_NOACTIVATE` | клік не краде фокус у активного вікна |
| `WS_EX_TOPMOST` | поверх taskbar (він теж topmost) |
| `WS_EX_TRANSPARENT` | click-through, якщо віджет неінтерактивний (не можна постійно, якщо потрібне меню) |

**Позиціювання:**

```rust
// ілюстративно; windows 0.62
let tb = FindWindowW(w!("Shell_TrayWnd"), None)?;            // primary
let tray = FindWindowExW(Some(tb), None, w!("TrayNotifyWnd"), None)?;

let mut rc = RECT::default();
GetWindowRect(tb, &mut rc)?;                                 // екранні координати taskbar
let mut rt = RECT::default();
GetWindowRect(tray, &mut rt)?;                               // ліва межа трею
```

- Taskbar знизу/зверху: `x = rt.left - width - gap`, `y = rc.top + (rc.height - h)/2`.
- Taskbar зліва/справа: орієнтація визначається `rc.width >= rc.height`; позиція — від `TrayNotifyWnd`/краю.
- Win11 centered icons: якір — **ліва межа `TrayNotifyWnd`**; лівіше за нього може бути кнопка Widgets, тому потрібен відступ або перевірка `TaskbarAl` (registry `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced\TaskbarAl`, 1 = center).
- Fallback, якщо `TrayNotifyWnd` немає (secondary): константа 88 px × DPI + user offset (так робить TrafficMonitor).

**Трекінг змін:**

- `SetWinEventHook(EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE, None, Some(cb), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS)` — ловить рух/анімацію taskbar (auto-hide, зміна розміру, поява панелі).
- `EVENT_SYSTEM_FOREGROUND` — перевірка fullscreen і повторне підняття topmost, коли фокус отримав taskbar.
- `EVENT_OBJECT_SHOW` / `EVENT_OBJECT_HIDE` для `Shell_TrayWnd`.
- `WM_SETTINGCHANGE` (`"ImmersiveColorSet"`, `"WindowMetrics"`, `"Display"`), `WM_DISPLAYCHANGE`, `WM_DWMCOLORIZATIONCOLORCHANGED`, `WM_DPICHANGED`.
- `SHAppBarMessage(ABM_GETSTATE)` → `ABS_AUTOHIDE | ABS_ALWAYSONTOP`; `SHAppBarMessage(ABM_GETTASKBARPOS)` дає `APPBARDATA.rc` та `uEdge` (`ABE_LEFT/TOP/RIGHT/BOTTOM`) для primary.

**Auto-hide:** прихований taskbar не `IsWindowVisible(FALSE)`, а «висунутий» за край; `GetWindowRect` показує прямокутник переважно поза монітором. Логіка: якщо `IntersectRect(rc, monitor) < 50%` — ховати overlay або їхати за ним (щоб він «виїжджав» разом з панеллю). Додатково — таймер 250–500 мс на всяк випадок (WinEvent іноді coalesce'ить події).

**Плюси:** нульовий вплив на Explorer, повна свобода рендерингу, працює на всіх збірках, легко тестувати. **Мінуси:** не «справжня» дитина taskbar — при перекритті іншими topmost-вікнами/фулскріном треба ховатися; неможливо отримати «місце» в layout (віджет плаває поверх, а не розсуває іконки); при auto-hide треба синхронізувати анімацію.

### 2.4 Офіційні API розширення taskbar

- **`ITaskbarList3`** (`Explorerframe.dll`): progress, overlay icon, thumbnail toolbars, tab registration. Це API **для кнопки застосунку**, не для довільного віджета. Не підходить.
- **`Windows.UI.Shell.TaskbarManager`** (UWP): `IsSupported`, `RequestPinCurrentAppAsync` — лише пінинг, не UI.
- **Windows Widgets / `IWidgetProvider`** (`Microsoft.Windows.Widgets.Providers`): віджети живуть у **Widgets Board** (Win+W), а не на taskbar. Потрібна **package identity** (MSIX) + `uap3:AppExtension Name="com.microsoft.windows.widgets"` + out-of-process COM (`CreateInstance` з CLSID) або `ActivateApplication`. Контент — Adaptive Card/HTML. Це не taskbar-віджет і не дає inline-тексту в панелі.
- **Офіційного API для taskbar bands у Win11 немає.** Deskband-контракт лишився лише як спадщина Win10.

### 2.5 NotifyIcon (трей)

`Shell_NotifyIcon(NIM_ADD/NIM_MODIFY/NIM_DELETE, &NOTIFYICONDATA)` + `NIM_SETVERSION` з `NOTIFYICON_VERSION_4` (повідомлення `NIN_SELECT`, `NIN_KEYSELECT`, `WM_CONTEXTMENU` з координатами). Дає іконку 16×16 (масштабовану DPI), tooltip до 128 символів, балун/тост.

**Плюси:** офіційно, стабільно, 1 рядок інтеграції, переживає рестарт Explorer через `TaskbarCreated`. **Мінуси:** **немає inline-тексту й графіків** — тільки іконка; tooltip з'являється з затримкою; балуни дрейфують у бік тостів; користувач може сховати іконку в overflow. Для монітора це лише **fallback-режим** («іконка + попап»), не основний.

«Owner-drawn popup» — це не розширення трею, а звичайне наше `WS_POPUP`-вікно, яке показується біля іконки: позицію іконки дає `Shell_NotifyIconGetRect`, показ — `SetForegroundWindow(hwnd)` + `TrackPopupMenu` (інакше меню не закривається по кліку поза ним), закриття — `PostMessage(WM_NULL)`. Це окремий, добре відомий набір пасток, але він не дає віджету бути *на* taskbar.

### 2.6 Порівняння методів

| Метод | Win10 21H2/22H2 | Win11 22H2–25H2 | Win11 26300+ | Складність | Вплив на Explorer | Inline-контент |
|---|---|---|---|---|---|---|
| **DeskBand** (`IDeskBand`) | ✅ (deprecated) | ❌ (лише EP/StartAllBack) | ❌ | 5/5 | **високий**: DLL у `explorer.exe`, crash = shell down | ✅ довільний |
| **`SetParent` → `Shell_TrayWnd`** | ✅ (TrafficMonitor) | ⚠️ працює, але layout/XAML-залежний | ⚠️/❌ (новий контейнер) | 4/5 | **високий**: input-queue attach, DPI reset, знищення HWND | ✅ довільний, але clipping |
| **Layered overlay** | ✅ | ✅ | ✅ | 2/5 | **нульовий** | ✅ довільний |
| **Tray icon** | ✅ | ✅ | ✅ | 1/5 | нульовий | ❌ іконка+tooltip |
| **Widgets Board** | ❌ | ✅ (окрема дошка) | ✅ | 3/5 | нульовий | ✅, але не taskbar |
| **XAML injection** (EP/TTB/Windhawk) | n/a | ✅ | ❌ (26340 зламав) | 5/5 | **критичний**: патч коду Explorer | ✅ |

---

## 3. Рендеринг у Rust

### 3.1 Що саме треба

- Прозорий фон (per-pixel alpha) або точний збіг з темою taskbar.
- Дрібний текст 11–13 px з коректним hinting/AA, CJK та emoji.
- 1–2 маленькі графіки (30×20 px), що скроляться раз на секунду.
- DPI 100–250%, per-monitor.
- Без мерехтіння, без постійного repaint.
- Малий бінарник і старт <100 мс.

### 3.2 Варіанти

**GDI (TextOut/DrawText + `CreateDIBSection` + `UpdateLayeredWindow`).**
Найдешевший і найпростіший. Двохбуферність — вручну (DIB), `WS_EX_COMPOSITED` не потрібен і навіть шкідливий у дочірньому вікні Explorer (він перебудовує перемальовку всього дерева). Текст — `DrawTextW` з `DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX`, шрифт `CreateFontW` з `-MulDiv(9, dpi, 72)`. Плюси: ~0 залежностей, старт <20 мс, бінарник ~1.5–2.5 MB. Мінуси: немає subpixel AA на прозорому фоні (layered → grayscale), немає кольорових emoji, ручне масштабування графіки (але `Polyline`/`LineTo` цілком достатньо). **Це найкращий fallback і найкращий MVP.**

**Direct2D + DirectWrite (`windows` crate, `Win32_Graphics_Direct2D`, `Win32_Graphics_DirectWrite`).**
`ID2D1Factory1` → `ID2D1DCRenderTarget` з `D2D1_RENDER_TARGET_PROPERTIES { pixelFormat: B8G8R8A8_UNORM, alphaMode: PREMULTIPLIED, dpiX/dpiY }` → `BindDC(memdc_of_DIB)` → `DrawText`/`DrawLine` → `EndDraw` → `UpdateLayeredWindowIndirect`. Дає якісний ClearType-подібний AA (на прозорому — grayscale, це фізика), color emoji через `D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT`, нормальний font fallback для CJK, точний DPI. Бінарник +1–2 MB, CPU на 1 Гц — одиниці мікросекунд на кадр. TrafficMonitor використовує саме D2D + `UpdateLayeredWindowIndirect` з fallback на GDI при помилці.

**DirectComposition (+ D3D11/DXGI swapchain).**
`DCompositionCreateDevice` → `CreateTargetForHwnd(hwnd, TRUE)` → `CreateVisual` → `SetContent(swapchain)`; вікно з `WS_EX_NOREDIRECTIONBITMAP`. Документація прямо каже: target можна прив'язати і до **child window**, але вікно **має належати нашому процесу** (інакше `DCOMPOSITION_ERROR_ACCESS_DENIED`) і дочірні вікна можуть клипати visual tree (керується параметром `topmost`). Плюси: GPU-композиція, нульове копіювання, ідеально для анімацій. Мінуси: D3D11 device, складніший resize/DPI, зайвий GPU wake-up, +2–3 MB, і для віджета 1 Гц це overkill.

**Skia (`skia-safe` 0.157).**
Найкраща якість 2D/тексту (власний font manager, color emoji, path rendering), але +8–12 MB до бінарника, довша збірка, C++ toolchain, і для 200×30 px це молоток по цвяху. Виправдано, якщо колись з'являться складні графіки (anti-aliased sparklines, тіні, кастомні шрифти).

**`tiny-skia` + `softbuffer`.**
Pure-Rust, маленький (~1–2 MB), швидкий для простих шейпів. Але **немає тексту** — потрібні `fontdue`/`ab_glyph` + `rustybuzz` для shaping; emoji/CJK — боляче. `softbuffer` презентує в HWND, але не дає per-pixel alpha layered-вікна «з коробки». Не рекомендується.

**`egui`/`eframe` 0.37.**
Дуже швидкий старт розробки, але: (1) eframe володіє `winit`-вікном і message loop — вбудувати як дитину taskbar важко; (2) за замовчуванням **безперервний repaint** → 1–5% CPU і постійні GPU wake-up; (3) прозорість вікна залежить від бекенду (wgpu/glow), на Win32 буває чорний фон; (4) бінарник ~6–9 MB; (5) якість дрібного тексту поступається DirectWrite. Для full-screen dashboard — так, для taskbar-смужки — ні.

**`iced` 0.14 / `slint` 1.19.**
Обидва — повноцінні GUI-фреймворки з власним рендерером (iced — wgpu; slint — software/GL/femtovg). `slint` має Win32-бекенд і непогану якість тексту, але інтеграція як child-window і per-pixel alpha нетривіальні; бінарник 8–12 MB. `iced` ще важчий. Для одного віджета — надлишково.

**`tao`/`winit` + `wgpu` 30.**
Максимальний контроль, але це фактично ручний GUI-фреймворк: самі text layout, самі hit-test, самі DPI. Плюс GPU present кожного кадру. Не для MVP.

### 3.3 Порівняльна таблиця рендерерів

| Варіант | Якість тексту (дрібний/CJK/emoji) | Прозорість | CPU (1 Гц) | Розмір (x64 release)* | DPI | Складність | Висновок |
|---|---|---|---|---|---|---|---|
| **GDI + DIB** | добра / добра / ❌ | per-pixel via ULW | <0.1% | ~1.5–2.5 MB | ручна | 1/5 | **fallback + MVP** |
| **D2D + DWrite** | відмінна / відмінна / ✅ | per-pixel via ULW | <0.1% | ~2.5–4 MB | `SetDpi` | 2/5 | **основний** |
| **DComp + D3D11** | відмінна | per-pixel, GPU | <0.1% + GPU wake | ~3.5–5 MB | `SetDpi` | 4/5 | пізніше, за потреби |
| **skia-safe** | відмінна / відмінна / ✅ | премультипльований bitmap | <0.2% | ~12–20 MB | ручна | 3/5 | якщо складна графіка |
| **tiny-skia + softbuffer** | слабка (немає тексту) | обмежена | <0.2% | ~3–5 MB | ручна | 3/5 | ❌ |
| **egui/eframe** | середня | залежить від бекенду | 1–5% (continuous) | ~6–9 MB | є | 2/5 | ❌ для taskbar |
| **iced** | добра | складна | 1–3% | ~10–15 MB | є | 4/5 | ❌ |
| **slint** | добра | складна | 0.5–2% | ~8–12 MB | є | 3/5 | ❌ |
| **winit+wgpu** | ручна | ручна | 0.5–2% + GPU | ~12–18 MB | ручна | 5/5 | ❌ |

\* Порядок величини для release + LTO; залежить від фіч і CRT. Цифри — оцінки на основі типових збірок цих стеків, не бенчмарк.

**Рекомендація:** **D2D + DWrite (основний) → GDI (fallback)**. Обидва працюють з одним і тим самим DIB і `UpdateLayeredWindow`, тому перемикання — це одна гілка в рендерері (TrafficMonitor робить саме так: при помилці `UpdateLayeredWindowIndirect` вимикає D2D і переходить на GDI).

**Важливо про прозорість і текст:** `UpdateLayeredWindow` з `ULW_ALPHA` вимагає **premultiplied BGRA**; Direct2D з `D2D1_ALPHA_MODE_PREMULTIPLIED` дає саме це. `SetLayeredWindowAttributes(LWA_COLORKEY)` дає «рвані» краї й не підходить для тексту. ClearType/subpixel AA на прозорій поверхні неможливий — тільки grayscale (це обмеження самої технології, не Rust).

---

## 4. Prior art: хто як робить

| Проєкт | Мова/стек | Підхід | Що взяти |
|---|---|---|---|
| **TrafficMonitor** | C++/MFC | Win10: `SetParent` → `ReBarWindow32`, `MoveWindow(MSTaskSwWClass)` щоб **звузити** кнопки; Win11: `SetParent` → `Shell_TrayWnd`, позиція від `TrayNotifyWnd`; рендер GDI / D2D / D2D+DComp; fallback — topmost вікно | Найближчий референс. Але `MoveWindow` у чужі вікна — не повторювати. |
| **SPlayer `taskbar-lyric`** | **Rust** (`windows` crate) | Win11: `SetParent(child, Shell_TrayWnd)`, зняття `WS_CAPTION/WS_THICKFRAME`, `WS_EX_LAYERED \| WS_EX_TOOLWINDOW`; UIA-скан для `start/widgets/content/tray` | Готовий Rust-шаблон attach + UIA-вимірювання |
| **komorebi-bar** | Rust + **eframe/egui** | Окремий top-level бар (не taskbar), `eframe` + `egui` + `sysinfo` | Підтверджує: egui годиться для standalone-бару, не для інтеграції |
| **GlazeWM / Zebar** | Rust (WM) + **Tauri/WebView** (бар) | Zebar — окремий процес, HTML/CSS-віджети | Не для нашого кейсу (WebView2 ~50–100 MB) |
| **Yasb** | **Python + PyQt6** (не Rust) | Top-level frameless Qt-бар, позиціонується як панель | Гарний UX-референс, поганий стек |
| **Seelen UI** | Rust + Tauri + Preact | Повністю **замінює** taskbar своїм `SeelenWeg`, ховає нативний | Підтверджує: працювати «поруч» простіше, ніж «замість» |
| **Rainmeter** | C++/Win32 | `WS_EX_LAYERED \| WS_EX_TOOLWINDOW`, `WS_POPUP`, `UpdateLayeredWindow(ULW_ALPHA)`, topmost; z-order через `EVENT_SYSTEM_FOREGROUND` | Еталон layered-вікна |
| **TaskbarX** | C# | Маніпуляція toolbar'ом `MSTaskSwWClass` (padding/position) для центрування іконок | Підтверджує крихкість такого підходу |
| **TranslucentTB** | C++ + injected ExplorerTAP DLL | XAML-diagnostics (`AdviseVisualTreeChange`), перефарбовує `Rectangle#BackgroundFill` | Потужно, але це інжекція; зламалося на 26340 |
| **Windhawk mods** | C++ DLL, інжекція в `explorer.exe` | Хуки/патчі XAML-дерева | Не для продукту: AV/anti-cheat, крихкість |
| **ExplorerPatcher** | C++ hooks + власний taskbar | Відновлює Win10-подібний taskbar, підтримує deskbands | Доказ, що deskband на Win11 — лише через патчер |
| **StartAllBack** | proprietary | Власна реалізація taskbar | Те саме, закритий код |

---

## 5. Практичні обмеження

### 5.1 DPI

- Manifest: `<dpiAwareness>PerMonitorV2</dpiAwareness>` (або `<dpiAware>true/pm</dpiAware>` + `<dpiAwareness>PerMonitorV2</dpiAwareness>`). `SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)` — **до** створення UI; Microsoft рекомендує manifest, не API.
- Для overlay: `WM_DPICHANGED` → `wParam` містить X/Y DPI, `lParam` — suggested rect; застосувати `SetWindowPos` з цим rect, перерахувати шрифти/відступи через `MulDiv(px, dpi, 96)`.
- Для child-вікна в taskbar: `WM_DPICHANGED` може не приходити (він не top-level) — використовувати `WM_DPICHANGED_AFTERPARENT` (0x02E3) і `GetDpiForWindow(hwnd)`; але пам'ятати про «forced reset» при cross-process `SetParent`.
- `GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, …)` — для вибору монітора; `GetDpiForWindow` — для вікна.
- **Text scaling** (Settings → Accessibility → Text size) — `HKCU\Software\Microsoft\Accessibility\TextScaleFactor` (100–225). Це не DPI: враховувати окремо для розміру шрифту, інакше віджет буде виглядати дрібним у користувачів зі збільшеним текстом.

### 5.2 Тема

- `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize`:
  - `SystemUsesLightTheme` (0/1) — **taskbar** і системні поверхні;
  - `AppsUseLightTheme` (0/1) — застосунки;
  - `EnableTransparency` (0/1) — ефекти прозорості;
  - `ColorPrevalence` (0/1) — акцент на Start/taskbar.
- Сигнал зміни: `WM_SETTINGCHANGE` з `lParam = L"ImmersiveColorSet"` (де-факто стандарт; окремого документованого API немає).
- Акцент: `DwmGetColorizationColor(&color, &opaque)` + `WM_DWMCOLORIZATIONCOLORCHANGED`; додатково `HKCU\Software\Microsoft\Windows\DWM\AccentColor` (ABGR) і `ColorizationColor`.
- Win11 Mica/Acrylic: `DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW/…)` працює для **наших top-level вікон**, але **не дає** кольору taskbar'а; точний збіг з його матеріалом неможливий без читання пікселів. Практично: напівпрозорий фон кольору теми (dark: `#202020` α≈0.85, light: `#F3F3F3` α≈0.85) + accent для графіків. TrafficMonitor за замовчуванням узагалі малює непрозорий фон і дає опцію прозорості.
- **Пастка Win11:** `transparent_color == 0` (чистий чорний як color key) у темній темі ламає контекстне меню — TrafficMonitor підмінює 0 на 1. Для per-pixel alpha це не актуально, але пам'ятати.

### 5.3 Рестарт/крах Explorer

- Зареєструвати `UINT wmTaskbarCreated = RegisterWindowMessageW(w!("TaskbarCreated"));` — Explorer **broadcast**ить це повідомлення всім top-level вікнам після створення taskbar (документовано в «The Taskbar»; на Win10 також надсилається при зміні DPI primary-монітора).
- Після `TaskbarCreated`: почекати 0.5–1 с, у циклі `FindWindowW("Shell_TrayWnd")` (Explorer піднімається 1–5 с), перевірити `IsWindow`, переприв'язати/перестворити вікно.
- Якщо був режим child: HWND знищено → `WM_NCDESTROY`; не використовувати старий handle, створити новий.
- Додатково: таймер-вахта раз на 2–5 с перевіряє `IsWindow(taskbar)` і `GetParent(hwnd) == taskbar`.
- Ніколи не тримати `HWND` taskbar'а «назавжди» — він інвалідний після рестарту.

### 5.4 Elevation / UIPI / bands

- Застосунок має бути **`asInvoker`** (medium IL). Explorer — теж medium, тому `SetParent` і `SetWinEventHook` працюють.
- Якщо застосунок запущено **elevated**: `SetParent` у medium-Explorer формально дозволений (вищий → нижчий IL), але UIPI блокує частину повідомлень/взаємодії; краще взагалі не вбудовуватися, а малювати overlay.
- `SetWindowLongPtr(GWLP_WNDPROC)` для чужого вікна — **заборонено** документацією («should not subclass a window class created by another process»); для свого вікна — можна.
- `SetWindowBand` / `CreateWindowInBand` / `CreateWindowInBandEx` — **приватні** експорти `user32.dll`:
  - `CreateWindowInBand(..., ZBID_*)` працює лише для `ZBID_DEFAULT`/`ZBID_DESKTOP`; для інших потрібен PE-розділ `.imrsiv` + підпис Microsoft.
  - `SetWindowBand` вимагає IAM-доступу (`NtUserEnableIAMAccess` + ключ з `NtUserAcquireIAMKey`), інакше `ERROR_ACCESS_DENIED (0x5)`. Ключ можна отримати лише в процесі, що викликав `SetShellWindow(Ex)` (тобто в Explorer).
  - **Висновок:** для стороннього застосунку недоступно; `ZBID_UIACCESS` — лише з UIAccess-токеном (підписаний бінарник у `Program Files`), і це не «band для віджета».
- `SetWindowDisplayAffinity`/`WDA_EXCLUDEFROMCAPTURE` — не потрібно.

### 5.5 Багатомоніторність, позиція, auto-hide

- Primary: `FindWindowW("Shell_TrayWnd")`. Secondary: `EnumWindows` → клас **`Shell_SecondaryTrayWnd`** (по одному на монітор). TrafficMonitor сортує їх за порядком моніторів через `EnumDisplayMonitors`.
- Позиція taskbar: `ABM_GETTASKBARPOS` (`APPBARDATA.uEdge` = `ABE_LEFT/TOP/RIGHT/BOTTOM`, `rc`) — але це лише primary; для secondary — `GetWindowRect` + порівняння ширини/висоти.
- Auto-hide: `ABM_GETSTATE` → `ABS_AUTOHIDE`; rect taskbar'а виїжджає за межі монітора. Overlay або ховати, або рухати разом (щоб не «висіти» над робочим столом).
- DPI різний на моніторах: позицію й розмір рахувати в **фізичних** пікселях монітора taskbar'а, шрифт — з `GetDpiForWindow(taskbar)`/`GetDpiForMonitor`.
- Win11: на secondary може не бути `TrayNotifyWnd` → fallback-константа (TrafficMonitor: 88 px) + user offset.

### 5.6 Повноекранний режим

- `SHQueryUserNotificationState(&state)`:
  - `QUNS_RUNNING_D3D_FULL_SCREEN` (3) — ексклюзивний fullscreen;
  - `QUNS_BUSY` (2), `QUNS_PRESENTATION_MODE` (4) — теж ховати;
  - `QUNS_ACCEPTS_NOTIFICATIONS` (5) — показувати.
- Функція **не ловить borderless fullscreen** (ігри/відео у вікні без рамок). Додаткова евристика: `GetForegroundWindow()` + `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)`; якщо rect покриває монітор (±2 px) і це не shell-вікно (`Progman`, `WorkerW`, `Shell_TrayWnd`) → ховати overlay.
- Ігнорувати «closaked» UWP-вікна: `DwmGetWindowAttribute(DWMWA_CLOAKED)`.
- Підписатися на `EVENT_SYSTEM_FOREGROUND` і перевіряти стан при кожній зміні фокуса + раз на 2–5 с таймером.

### 5.7 Безпека й anti-cheat

- **Не робити:** DLL-інжекцію (`CreateRemoteThread`, `SetWindowsHookEx` з DLL), `ReadProcessMemory`, патчі `explorer.exe`, глобальні хуки.
- **Можна:** `SetWinEventHook(WINEVENT_OUTOFCONTEXT)` — це документований механізм сповіщень, без інжекції коду; `FindWindow`/`GetWindowRect`/`SetParent` — звичайні Win32-виклики.
- Anti-cheat все одно може реагувати на topmost-overlay; тому: (а) ховати у fullscreen, (б) дати опцію «вимикати віджет для вибраних процесів», (в) не використовувати `WS_EX_TOPMOST` поверх ексклюзивного fullscreen (він і так невидимий), (г) не ставити глобальних клавіатурних хуків.
- Не змінювати чужі вікна (`MoveWindow(MSTaskSwWClass)`, `SetWindowLongPtr` на чужих HWND) — це і є джерело «зламав Explorer».

---

## 6. Рекомендація

### 6.1 Матриця стабільності

| ОС | Build | DeskBand | Child `SetParent` | Layered overlay | Коментар |
|---|---|---|---|---|---|
| Win10 21H2 | 19044 | ✅ | ✅ | ✅ | Deskband deprecated, але працює; overlay — без ризиків |
| Win10 22H2 | 19045 | ✅ | ✅ | ✅ | Те саме |
| Win11 22H2 | 22621 | ❌ | ⚠️ | ✅ | Deskband лише через EP/SAB; child працює, але XAML-layout |
| Win11 23H2 | 22631 | ❌ | ⚠️ | ✅ | Те саме |
| Win11 24H2 | 26100 | ❌ | ⚠️ | ✅ | EP малює власний taskbar; у TM є регресії прозорості/позиції |
| Win11 25H2 | 26200 | ❌ | ⚠️ | ✅ | SAB перестав показувати Toolbars |
| Win11 26H2/Insider | 26300+ | ❌ | ⚠️/❌ | ✅ | 26340: taskbar у новому контейнері, XAML-детекція зламана |

✅ — працює штатно; ⚠️ — працює з обмеженнями/регресіями; ❌ — не працює.

### 6.2 Складність і failure modes

| Підхід | Складність | Типові відмови |
|---|---|---|
| Overlay | 2/5 | не той z-order після кліку по taskbar; розсинхрон при auto-hide; неправильна позиція при centered icons/widgets; fullscreen не приховано |
| Child | 4/5 | `SetParent` fail (AV, Start menu open — відома проблема TM); вікно перекрите XAML; DPI reset; HWND знищено після краху; layout зламано оновленням |
| DeskBand | 5/5 | не з'являється в Toolbars; crash Explorer; 32/64; не працює на Win11; блокується shell-extension policy |

### 6.3 План (фази)

**MVP (2–3 тижні):**
1. Один Rust-бінарник, `windows = "0.62"`, manifest `PerMonitorV2`, `asInvoker`.
2. Overlay-вікно: `WS_POPUP | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST`, `UpdateLayeredWindowIndirect` з DIB.
3. GDI-рендер: час, CPU %, RAM %, up/down, 2 sparkline (кільцевий буфер 60 точок).
4. Позиція: primary taskbar, права частина (ліва межа `TrayNotifyWnd`), vertical/horizontal, DPI-aware.
5. Тема: `SystemUsesLightTheme` + `EnableTransparency` + accent; реакція на `WM_SETTINGCHANGE`.
6. Таймер 1000 мс; `WM_TIMER` → перерахувати метрики → перемалювати DIB → `UpdateLayeredWindowIndirect` (без `InvalidateRect`, без блимання).
7. `TaskbarCreated` + вахтовий таймер; `SHQueryUserNotificationState` → hide.
8. Трей-іконка як **додатковий** режим (fallback, якщо overlay чомусь не показався).

**Phase 2 (1–2 тижні):**
- D2D + DWrite рендерер (перемикання при помилці ULW), color emoji, CJK fallback.
- Multi-monitor: `Shell_SecondaryTrayWnd`, вибір монітора, per-monitor DPI.
- Auto-hide: `ABM_GETSTATE` + `EVENT_OBJECT_LOCATIONCHANGE`, анімація «виїзду».
- Контекстне меню (правий клік), налаштування, автозапуск, локалізація.
- Click-through для неінтерактивних зон; `WM_MOUSEACTIVATE` → `MA_NOACTIVATE`.

**Phase 3 (опційно, за потреби «справжнього» вбудовування):**
- Режим `embed`: `SetParent(hwnd, Shell_TrayWnd)` лише на Win11; перевірка `GetParent(hwnd) == taskbar` + `IsWindowVisible` + rect саніти; при будь-якому сумніві — негайний `SetParent(hwnd, None)` і overlay.
- **Ніколи** не викликати `MoveWindow` для `MSTaskSwWClass`/`MSTaskListWClass` (це те, що робить TrafficMonitor на Win10 і що ламає layout).
- DeskBand — окремий DLL **тільки для Win10**, якщо буде попит; на Win11 не реєструвати взагалі.

### 6.4 Конкретний Rust-стек

```toml
[dependencies]
windows = { version = "0.62", features = [
  "Win32_Foundation",
  "Win32_UI_WindowsAndMessaging",
  "Win32_UI_Shell",
  "Win32_UI_HiDpi",
  "Win32_Graphics_Gdi",
  "Win32_Graphics_Dwm",
  "Win32_Graphics_Direct2D",
  "Win32_Graphics_DirectWrite",
  "Win32_Graphics_Dxgi_Common",
  "Win32_System_Com",
  "Win32_System_LibraryLoader",
  "Win32_System_Performance",        # PDH для CPU/RAM
  "Win32_NetworkManagement_IpHelper" # GetIfTable2 для мережі
] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
parking_lot = "0.12"
```

- **Один UI-потік** з `GetMessageW`/`DispatchMessageW`; метрики — окремий потік, результат через `crossbeam-channel`/atomic.
- Рендер — у DIB (`CreateDIBSection`, 32bpp BGRA, top-down), потім `UpdateLayeredWindowIndirect`. Ніякого `WM_PAINT`-циклу для оновлення даних.
- Шрифти: `Segoe UI` (UI) + `Segoe UI Emoji` (fallback), розмір у DIP × `dpi/96`; для DWrite — `IDWriteFontFallback`.
- Ніяких `winit`/`eframe`/`wgpu`.

### 6.5 Що остаточно вирішити

1. **Overlay vs child** — Recommended: overlay, child — опційний експеримент.
2. **Позиція за замовчуванням** — ліва межа `TrayNotifyWnd` (права частина), з відступом 4–6 px; альтернатива — ліва частина біля Start (але на Win11 centered icons вона перекривається Widgets).
3. **Прозорість** — за замовчуванням напівпрозорий theme-color (α≈0.85), не color key.
4. **Fullscreen** — hide за `SHQueryUserNotificationState` + евристика borderless.

---

## 7. Джерела

**Microsoft Learn / Win32:**

1. `IDeskBand` — «not supported as of Windows 7», thumbnail toolbars: https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-ideskband
2. `DESKBANDINFO` (deprecated `GetBandInfo`): https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/ns-shobjidl_core-deskbandinfo
3. Taskbar Extensions (deskbands section): https://learn.microsoft.com/en-us/windows/win32/shell/taskbar-extensions
4. The Taskbar — `TaskbarCreated` broadcast: https://learn.microsoft.com/en-us/windows/win32/shell/taskbar
5. `SetParent` — cross-process DPI «forced reset»: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setparent
6. `SetWindowLongPtr` — не субкласити чужі класи: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowlongptrw
7. Extended Window Styles (`WS_EX_LAYERED/TOOLWINDOW/NOACTIVATE/TRANSPARENT/NOREDIRECTIONBITMAP`): https://learn.microsoft.com/en-us/windows/win32/winmsg/extended-window-styles
8. `WM_DPICHANGED`: https://learn.microsoft.com/en-us/windows/win32/hidpi/wm-dpichanged
9. `WM_DPICHANGED_AFTERPARENT`: https://learn.microsoft.com/en-us/windows/win32/hidpi/wm-dpichanged-afterparent
10. `SetProcessDpiAwarenessContext`: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setprocessdpiawarenesscontext
11. High DPI Desktop Application Development: https://learn.microsoft.com/en-us/windows/win32/hidpi/high-dpi-desktop-application-development-on-windows
12. `SHAppBarMessage` (`ABM_GETSTATE`, `ABM_GETTASKBARPOS`): https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shappbarmessage
13. `SHQueryUserNotificationState`: https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shqueryusernotificationstate
14. `SetWinEventHook`: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwineventhook
15. `UpdateLayeredWindow`: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow
16. `DwmGetColorizationColor`: https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmgetcolorizationcolor
17. `DWM_SYSTEMBACKDROP_TYPE` (Mica/Acrylic): https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwm_systembackdrop_type
18. DirectComposition `CreateTargetForHwnd` (child window allowed, must belong to caller): https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondesktopdevice-createtargetforhwnd
19. `Shell_NotifyIcon`: https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shell_notifyiconw
20. `NOTIFYICONDATA` / `NOTIFYICON_VERSION_4`: https://learn.microsoft.com/en-us/windows/win32/api/shellapi/ns-shellapi-notifyicondataw
21. Notification area: https://learn.microsoft.com/en-us/windows/win32/shell/notification-area
22. `ITaskbarList3`: https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-itaskbarlist3
23. Widget provider manifest (package identity, `com.microsoft.windows.widgets`): https://learn.microsoft.com/en-us/windows/apps/develop/widgets/widget-provider-manifest
24. Widget providers (`IWidgetProvider`): https://learn.microsoft.com/en-us/windows/apps/develop/widgets/widget-providers

**Reverse engineering / спільнота:**

25. Raymond Chen, cross-process parent/child: https://devblogs.microsoft.com/oldnewthing/?p=4683
26. ADeltaX, «How to call SetWindowBand»: https://blog.adeltax.com/how-to-call-setwindowband/
27. ADeltaX, «Window z-order in Windows 10» (`ZBID`, `CreateWindowInBand`): https://blog.adeltax.com/window-z-order-in-windows-10/
28. TranslucentTB #1382 — taskbar moved into a new container on 26340: https://github.com/TranslucentTB/TranslucentTB/issues/1382
29. EverythingToolbar FAQ — deskbands removed from Win11: https://github.com/srwi/EverythingToolbar/blob/develop/FAQ.md
30. EverythingToolbar #723 — StartAllBack Toolbars menu gone (25H2): https://github.com/srwi/EverythingToolbar/issues/723
31. ExplorerPatcher on 24H2 — stock Win10 taskbar removed: https://explorerpatcher.dev/windows-11-24h2/
32. ExplorerPatcher #3931 — Win10 taskbar style on 24H2: https://github.com/valinet/ExplorerPatcher/discussions/3931

**Prior art (код):**

33. TrafficMonitor `TaskBarDlg.cpp`: https://github.com/zhongyang219/TrafficMonitor/blob/master/TrafficMonitor/TaskBarDlg.cpp
34. TrafficMonitor `ClassicalTaskbarDlg.cpp` (`ReBarWindow32`, `MSTaskSwWClass`, `MoveWindow`): https://github.com/zhongyang219/TrafficMonitor/blob/master/TrafficMonitor/ClassicalTaskbarDlg.cpp
35. TrafficMonitor `Win11TaskbarDlg.cpp` (`TrayNotifyWnd` anchoring): https://github.com/zhongyang219/TrafficMonitor/blob/master/TrafficMonitor/Win11TaskbarDlg.cpp
36. TrafficMonitor wiki — Taskbar Window (обмеження вбудовування): https://github.com/zhongyang219/TrafficMonitor/wiki/Taskbar-Window
37. SPlayer `taskbar-lyric` Win11 Rust strategy: https://github.com/imsyy/SPlayer/blob/38491618/native/taskbar-lyric/src/strategy/win11.rs
38. komorebi-bar (`eframe`): https://github.com/LGUG2Z/komorebi/tree/master/komorebi-bar
39. Seelen UI (`windows` crate + Tauri): https://github.com/eythaann/Seelen-UI
40. Yasb (`pyproject.toml`, PyQt6): https://github.com/amnweb/yasb/blob/main/pyproject.toml
41. Zebar (Tauri/WebView): https://github.com/glzr-io/zebar
42. Rainmeter `Skin.cpp` (`WS_EX_LAYERED|WS_EX_TOOLWINDOW`, `UpdateLayeredWindow`): https://github.com/rainmeter/rainmeter/blob/master/Library/Skin.cpp
43. Windhawk mods (інжекція в Explorer): https://github.com/ramensoftware/windhawk-mods

**Crates (версії на 2026-11-17):**

44. `windows` 0.62.2: https://crates.io/crates/windows
45. `skia-safe` 0.157.0: https://crates.io/crates/skia-safe
46. `eframe`/`egui` 0.37.0: https://crates.io/crates/eframe
47. `slint` 1.19.0: https://crates.io/crates/slint
48. `iced` 0.14.0: https://crates.io/crates/iced
49. `wgpu` 30.0.2: https://crates.io/crates/wgpu
50. `tiny-skia` 0.12.0 / `softbuffer` 0.4.8: https://crates.io/crates/tiny-skia
