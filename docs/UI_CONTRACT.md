# UI contract (for the parallel UI agent)

The native sensor/GPU/storage work is changing data shapes. Keep this contract stable unless you
coordinate with the parent agent.

## Public API

```rust
// crates/oxide-ui
pub struct Ui { ... }
impl Ui {
    pub fn new(config: oxide_core::Config,
               snapshot: std::sync::Arc<std::sync::Mutex<oxide_core::Snapshot>>)
        -> Result<Self, UiError>;
    pub fn run(self) -> Result<(), UiError>;
}
pub const SHOW_MESSAGE: u32 = WM_APP + 10;
pub const WM_OXIDE_OPEN_IPC: u32 = WM_APP + 11; // additive (U3b): lazily start \\.\pipe\oxide-ui-v1
```

`oxide-app` calls `Ui::new` once, then `Ui::run()` (blocking message loop). A second
`oxide-monitor.exe --show` enumerates top-level windows for class `OxideOverlay` and posts
`oxide_ui::SHOW_MESSAGE`; the UI must handle it by setting `shown = true` and showing the overlay.
A directly launched `oxide-settings.exe` may post `oxide_ui::WM_OXIDE_OPEN_IPC` to `OxideOverlay`
to lazily start the single-instance Named Pipe `\\.\pipe\oxide-ui-v1` (protocol in `crates/oxide-ipc`,
documented in `docs/IPC.md`).

## Snapshot fields

`oxide_core::Snapshot` now contains:

```rust
pub network: NetworkSample
pub cpu: CpuSample          // temp_source == Acpi => show "~"
pub memory: MemorySample
pub gpu: GpuSample          // primary GPU for the single-GPU widget
pub gpus: Vec<GpuSample>    // all adapters (multi-GPU)
pub disk: DiskSample        // primary disk for the widget
pub disks: Vec<DiskSample>  // all physical disks
```

`GpuSample` has `name`, `vendor`, `luid`, `usage_pct`, `temp_c`, `temp_source`,
`dedicated_vram_bytes`. `DiskSample` has `name`, `index`, `is_nvme`, `activity_pct`, `temp_c`,
`temp_source`.

The UI should continue to use `snapshot.gpu`/`snapshot.disk` for the 8-item widget and may use
`gpus`/`disks` for tooltips/settings later. Do not assume `gpu` is the first DXGI adapter: sensors
choose the primary (largest discrete NVIDIA/AMD VRAM, then first non-Microsoft).

## Localization

`oxide_core::Localizer` (Fluent) is constructed from `config.general.language` (`uk` default, `en`,
`ru`). Keys currently used by the tray/menu/balloon:

`tray-tooltip`, `menu-show`, `menu-hide`, `menu-open-config`, `menu-exit`,
`hidden-notice-title`, `hidden-notice`.

Do not hard-code English menu/balloon strings; use `Localizer::text`.

## Current behavior (keep or improve, don't regress)

- Window class `OxideOverlay`, title `Oxide Monitor`, `WS_POPUP` +
  `WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST`.
- Position left of `TrayNotifyWnd` on the primary taskbar; fallback bottom-right.
- 294×35 default, two rows, column-major item order (`row + col*rows`): Up/CPU/GPU/GPU temp,
  Down/MEM/CPU temp/HDD temp.
- `WM_TIMER` 1 s reads `Snapshot.seq`; don't render if unchanged. Reposition every 2 s.
- `TaskbarCreated` re-adds tray icon and repositions.
- `SHQueryUserNotificationState` hides in fullscreen; restore on exit.
- Tray left-click shows (does not toggle); menu has Show/Hide, Flyout, localized Runtime Presets
  submenu (5 built-in presets, immediate apply + persist), Game Mode, Lock position, Task Manager,
  Settings (`oxide-settings.exe` if present next to the exe, else Notepad fallback), Open config,
  Exit; hide shows a one-time localized balloon.
- External `config.toml` edits are hot-reloaded every 2 s (`mtime + size + hash`, paused during IPC
  live preview; invalid TOML preserves the active config and logs once). `oxide-monitor.exe` is the
  single atomic writer (`ReplaceFileW` + `.bak`) while running.

If you change this API, update `crates/oxide-app/src/main.rs` and the docs; the parent agent owns
the native crates and will not edit `oxide-ui` while you work.
