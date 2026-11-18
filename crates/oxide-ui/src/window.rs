//! Win32 overlay window: Direct2D/ULW with GDI fallback, 4 anchor modes + Alt+Drag,
//! configurable clicks, lazy Flyout host, event-driven Auto-hide & Game Mode.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use oxide_autostart::{TaskSchedulerBackend, TaskSpec, reconcile as reconcile_autostart_task};
use oxide_core::{
    AUTOSTART_TASK_NAME, AutohideBehavior, AutostartStatus, Config, ExtraTelemetry, FlyoutTrigger,
    HistoryStore, Localizer, PawnioCommand, ProvidersConfig, Snapshot, TaskbarAnchor, WheelAction,
    find_builtin_preset, save_autostart_status,
};
use oxide_ipc::{IncomingCommand, MonitorToSettings};
use oxide_render::{SystemThemeContext, WidgetRenderer, present_layered_window};
use tracing::{debug, error, info, warn};
use windows::Win32::Foundation::{
    COLORREF, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT,
    WPARAM,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, TME_HOVER, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWL_EXSTYLE,
    GWLP_USERDATA, GetCursorPos, GetMessageW, GetWindowLongPtrW, HWND_TOPMOST, IDC_ARROW, IsWindow,
    KillTimer, LWA_ALPHA, LoadCursorW, MA_NOACTIVATE, MSG, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, WM_CAPTURECHANGED, WM_CLOSE, WM_CONTEXTMENU,
    WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ENDSESSION, WM_ERASEBKGND, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_NCDESTROY, WM_PAINT, WM_POWERBROADCAST, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::actions::{self, ActionEffect};
use crate::flyout_window::{
    BTN_OPEN_SETTINGS, BTN_OPEN_TASK_MGR, BTN_TOGGLE_GAME_MODE, FlyoutHost, WM_OXIDE_FLYOUT_BUTTON,
    WM_OXIDE_FLYOUT_LEAVE,
};
use crate::geometry::{
    RectI, TaskbarGeometry, compute_overlay_position, free_drag_offsets_dip,
    is_taskbar_hidden_autohide, query_taskbar_geometry, render_bounds, snap_drag_position,
};
use crate::hooks::{
    WM_OXIDE_FOREGROUND_CHANGED, WM_OXIDE_TASKBAR_MOVED, WinEventHooks,
    should_auto_hide_for_game_or_fullscreen,
};
use crate::ipc_host::{
    ConfigCoordinator, HotReloadOutcome, IpcHost, WM_OXIDE_IPC_CMD, enumerate_monitors,
};
use crate::render::{self, Renderer as GdiRenderer};
use crate::tray::{SettingsLaunchOutcome, TrayRetryState};
use crate::{UiError, layout, tray};

const CLASS_NAME: PCWSTR = w!("OxideOverlay");
const WINDOW_TITLE: PCWSTR = w!("Oxide Monitor");
const TIMER_TICK_ID: usize = 1;
const TIMER_FLYOUT_GRACE_ID: usize = 2;
/// Coalesces a burst of `PreviewConfig` messages into at most one apply per interval.
const TIMER_PREVIEW_ID: usize = 3;
/// Upper bound for live-preview latency while a 25-40 ms preview stream is running.
const PREVIEW_COALESCE_MS: u32 = 50;
/// Missing from the `windows` 0.62 bindings (winuser.h value).
const NIN_KEYSELECT: u32 = 1025;
const WM_MOUSEHOVER: u32 = 0x02A1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const PBT_APMRESUMEAUTOMATIC: u32 = 0x0012;
const PBT_APMRESUMESUSPEND: u32 = 0x0007;
const TRAY_TOGGLE_DEBOUNCE: Duration = Duration::from_millis(400);
const FLYOUT_LEAVE_GRACE_MS: u32 = 250;
const DRAG_THRESHOLD_PX: i32 = 4;

enum OverlayRenderer {
    D2d {
        renderer: WidgetRenderer,
        dpi: u32,
        size_px: (i32, i32),
    },
    Gdi(GdiRenderer),
}

impl OverlayRenderer {
    fn is_d2d(&self) -> bool {
        matches!(self, Self::D2d { .. })
    }

    fn size_px(&self) -> (i32, i32) {
        match self {
            Self::D2d { size_px, .. } => *size_px,
            Self::Gdi(gdi) => gdi.size(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DragSession {
    start_cursor: (i32, i32),
    start_origin: (i32, i32),
    moved: bool,
}

/// One drained IPC batch collapsed into ordered, non-redundant work items.
#[derive(Debug)]
enum CoalescedCommand {
    Hello,
    Preview(Box<Config>),
    Commit {
        expected_generation: u64,
        expected_hash: u64,
        config: Box<Config>,
    },
    Revert,
    PawnioControl {
        start: bool,
    },
    Disconnected,
}

/// Collapse a drained command batch:
/// - keep only the **last** `PreviewConfig` per burst (a live-preview stream must not
///   trigger one heavyweight apply per message);
/// - treat `Hello`, `CommitConfig`, `RevertConfig` and `Disconnected` as barriers that
///   discard a pending preview, so a commit can never be followed by a stale preview.
fn coalesce_commands(commands: Vec<IncomingCommand>) -> Vec<CoalescedCommand> {
    let mut out = Vec::with_capacity(commands.len().min(4));
    let mut pending_preview: Option<Box<Config>> = None;

    for cmd in commands {
        match cmd {
            IncomingCommand::PreviewConfig(config) => pending_preview = Some(config),
            IncomingCommand::Hello { .. } => {
                pending_preview = None;
                out.push(CoalescedCommand::Hello);
            }
            IncomingCommand::CommitConfig {
                expected_generation,
                expected_hash,
                config,
            } => {
                pending_preview = None;
                out.push(CoalescedCommand::Commit {
                    expected_generation,
                    expected_hash,
                    config,
                });
            }
            IncomingCommand::RevertConfig => {
                pending_preview = None;
                out.push(CoalescedCommand::Revert);
            }
            IncomingCommand::PawnioControl { start } => {
                out.push(CoalescedCommand::PawnioControl { start });
            }
            IncomingCommand::Disconnected => {
                pending_preview = None;
                out.push(CoalescedCommand::Disconnected);
            }
        }
    }

    if let Some(config) = pending_preview {
        out.push(CoalescedCommand::Preview(config));
    }
    out
}

/// Which subsystems an old → new config transition must refresh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ApplyScope {
    /// Anything that changes the rendered pixels or the window size.
    visual: bool,
    /// The palette/theme context must be re-read from the registry.
    theme: bool,
    /// Taskbar/monitor geometry must be re-enumerated.
    geometry: bool,
    /// The foreground Game Mode policy must be re-evaluated.
    game_mode: bool,
    /// The tray tooltip text must be refreshed (language-dependent).
    tray: bool,
}

fn apply_scope(old: &Config, new: &Config) -> ApplyScope {
    ApplyScope {
        visual: old.general.theme != new.general.theme
            || old.taskbar != new.taskbar
            || old.surface != new.surface
            || old.layout != new.layout
            || old.typography != new.typography
            || old.format != new.format
            || old.aliases != new.aliases
            || old.items != new.items
            || old.flyout != new.flyout,
        theme: old.general.theme != new.general.theme
            || old.surface.background_mode != new.surface.background_mode
            || old.surface.background_color != new.surface.background_color
            || old.surface.text_shadow != new.surface.text_shadow,
        geometry: old.taskbar.monitor != new.taskbar.monitor
            || old.taskbar.anchor != new.taskbar.anchor
            || old.taskbar.x_offset != new.taskbar.x_offset
            || old.taskbar.y_offset != new.taskbar.y_offset
            || old.taskbar.left_space_win11 != new.taskbar.left_space_win11
            || old.taskbar.right_space_fallback != new.taskbar.right_space_fallback,
        game_mode: old.game_mode != new.game_mode,
        tray: !old
            .general
            .language
            .eq_ignore_ascii_case(&new.general.language),
    }
}

pub fn run(
    config: Config,
    snapshot: Arc<Mutex<Snapshot>>,
    provider_config: Option<Arc<Mutex<ProvidersConfig>>>,
    pawnio_commands: Option<mpsc::Sender<PawnioCommand>>,
    game_mode_flag: Option<Arc<AtomicBool>>,
) -> Result<(), UiError> {
    unsafe {
        if let Err(err) = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
        {
            debug!("SetProcessDpiAwarenessContext: {err}");
        }
    }

    let module = unsafe { GetModuleHandleW(PCWSTR::null()) }
        .map_err(|err| UiError::win32("GetModuleHandleW", err))?;
    let instance = HINSTANCE(module.0);
    register_class(instance)?;

    // Reconciled before the first frame so the settings UI can read the published
    // status as soon as it connects.
    let autostart_status = reconcile_autostart(&config);

    let (geom, taskbar_hwnd) = query_taskbar_geometry(&config.taskbar.monitor);
    let (max_width_px, max_height_px) = render_bounds(&geom);
    let (width, height) = layout::overlay_size_for(
        &config,
        geom.dpi,
        geom.edge.is_vertical(),
        max_width_px,
        max_height_px,
    );
    let (x, y) = compute_overlay_position(&config, &geom, width, height);

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            CLASS_NAME,
            WINDOW_TITLE,
            WS_POPUP,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .map_err(|err| UiError::win32("CreateWindowExW", err))?;

    let initial_dpi = render::dpi_for_window(hwnd).max(geom.dpi);
    let renderer = match WidgetRenderer::new(width.max(1) as u32, height.max(1) as u32) {
        Ok(d2d) => {
            info!(
                dpi = initial_dpi,
                "oxide-ui initialized Direct2D/DirectWrite ULW renderer"
            );
            OverlayRenderer::D2d {
                renderer: d2d,
                dpi: initial_dpi,
                size_px: (width, height),
            }
        }
        Err(err) => {
            warn!(%err, "Direct2D renderer init failed; falling back to GDI");
            configure_gdi_layered(hwnd, config.taskbar.opacity);
            let metrics = layout::render_metrics_for(
                &config,
                initial_dpi,
                geom.edge.is_vertical(),
                max_width_px,
                max_height_px,
            );
            match GdiRenderer::new(
                hwnd,
                &config,
                (metrics.width_px as i32, metrics.height_px as i32),
                metrics.dpi as u32,
            ) {
                Ok(gdi) => OverlayRenderer::Gdi(gdi),
                Err(gdi_err) => {
                    let _ = unsafe { DestroyWindow(hwnd) };
                    return Err(gdi_err);
                }
            }
        }
    };

    let taskbar_message = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    if taskbar_message == 0 {
        warn!("RegisterWindowMessageW(TaskbarCreated) failed");
    }

    let localizer = Localizer::new(&config.general.language);
    let theme_ctx = SystemThemeContext::query_windows();
    let hooks = WinEventHooks::install(hwnd, taskbar_hwnd);
    let auto_game_hidden = should_auto_hide_for_game_or_fullscreen(&config, &geom);
    let coordinator = ConfigCoordinator::new(config.clone(), Config::resolve_path());

    let mut state = Box::new(UiState {
        shown: config.taskbar.show,
        config,
        coordinator,
        ipc_host: IpcHost::new(),
        snapshot,
        history: HistoryStore::new(),
        theme_ctx,
        geom,
        hooks,
        flyout: FlyoutHost::new(),
        renderer,
        localizer,
        last_seq: 0,
        ticks: 0,
        last_pos: None,
        visible: false,
        manual_game_mode: false,
        auto_game_hidden,
        tray_added: false,
        tray_retry: TrayRetryState::default(),
        last_tray_toggle: None,
        last_settings_launch: None,
        hidden_notice_shown: false,
        taskbar_message,
        tracking_mouse: false,
        mouse_inside_overlay: false,
        drag: None,
        pending_preview: None,
        preview_timer_armed: false,
        gdi_fallback_failed: false,
        autostart: autostart_status,
        provider_config,
        pawnio_commands,
        game_mode_flag,
    });

    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &mut *state as *mut UiState as isize);
    }

    match tray::add(hwnd, &state.localizer) {
        Ok(()) => {
            state.tray_added = true;
            state.tray_retry.reset();
        }
        Err(err) => {
            warn!("failed to add tray icon: {err}");
            state.tray_retry.schedule_next();
        }
    }

    if unsafe { SetTimer(Some(hwnd), TIMER_TICK_ID, 1000, None) } == 0 {
        warn!("SetTimer failed");
    }

    state.apply_visibility(hwnd, true);
    state.publish_provider_config();
    state.publish_game_mode();

    info!("oxide-ui overlay running");

    let mut loop_error: Option<UiError> = None;
    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 == -1 {
            error!("GetMessageW failed");
            loop_error = Some(UiError::Other("GetMessageW failed".into()));
            break;
        }
        if result.0 == 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_TICK_ID);
        let _ = KillTimer(Some(hwnd), TIMER_FLYOUT_GRACE_ID);
        let _ = KillTimer(Some(hwnd), TIMER_PREVIEW_ID);
    }
    state.ipc_host.shutdown();
    state.flyout.hide();
    if state.tray_added {
        tray::remove(hwnd);
        state.tray_added = false;
    }
    if unsafe { IsWindow(Some(hwnd)) }.as_bool()
        && let Err(err) = unsafe { DestroyWindow(hwnd) }
    {
        warn!("DestroyWindow failed: {err}");
    }
    // The state box lives on this stack frame; never leave a dangling pointer in the window.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    }

    info!("oxide-ui overlay stopped");
    match loop_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

struct UiState {
    config: Config,
    coordinator: ConfigCoordinator,
    ipc_host: IpcHost,
    snapshot: Arc<Mutex<Snapshot>>,
    history: HistoryStore,
    theme_ctx: SystemThemeContext,
    geom: TaskbarGeometry,
    hooks: WinEventHooks,
    flyout: FlyoutHost,
    renderer: OverlayRenderer,
    localizer: Localizer,
    last_seq: u64,
    ticks: u32,
    last_pos: Option<(i32, i32)>,
    shown: bool,
    visible: bool,
    manual_game_mode: bool,
    auto_game_hidden: bool,
    tray_added: bool,
    tray_retry: TrayRetryState,
    last_tray_toggle: Option<Instant>,
    last_settings_launch: Option<Instant>,
    hidden_notice_shown: bool,
    taskbar_message: u32,
    tracking_mouse: bool,
    mouse_inside_overlay: bool,
    drag: Option<DragSession>,
    /// Latest IPC preview waiting for the coalescing timer; `None` when idle.
    pending_preview: Option<Config>,
    preview_timer_armed: bool,
    /// Set after a failed GDI fallback so a broken D2D target cannot trigger a
    /// `GdiRenderer::new` attempt on every frame.
    gdi_fallback_failed: bool,
    /// Last Windows Task Scheduler autostart reconcile result.
    autostart: AutostartStatus,
    /// Shared live provider config consumed by the sensor thread (optional).
    provider_config: Option<Arc<Mutex<ProvidersConfig>>>,
    /// Explicit Start/Stop actions from Settings (optional).
    pawnio_commands: Option<mpsc::Sender<PawnioCommand>>,
    /// Published `manual_game_mode || auto_game_hidden` for the PawnIO manager (optional).
    game_mode_flag: Option<Arc<AtomicBool>>,
}

impl UiState {
    fn overlay_rect(&self) -> RectI {
        let (w, h) = self.renderer.size_px();
        let (x, y) = self
            .last_pos
            .unwrap_or_else(|| compute_overlay_position(&self.config, &self.geom, w, h));
        RectI::new(x, y, x + w, y + h)
    }

    fn game_mode_active(&self) -> bool {
        self.manual_game_mode || self.auto_game_hidden
    }

    fn publish_game_mode(&self) {
        if let Some(flag) = &self.game_mode_flag {
            flag.store(self.game_mode_active(), Ordering::Relaxed);
        }
    }

    fn refresh_geometry(&mut self) {
        let (geom, tb_hwnd) = query_taskbar_geometry(&self.config.taskbar.monitor);
        self.geom = geom;
        self.hooks.bind_taskbar(tb_hwnd);
    }

    fn should_be_visible(&self) -> bool {
        if !self.shown || self.manual_game_mode || self.auto_game_hidden {
            return false;
        }
        if self.config.taskbar.autohide_behavior == AutohideBehavior::HideWithTaskbar
            && is_taskbar_hidden_autohide(&self.geom)
        {
            return false;
        }
        true
    }

    fn render(&mut self, hwnd: HWND, force: bool) {
        self.publish_game_mode();
        let snapshot = clone_snapshot(&self.snapshot);
        if snapshot.seq != self.last_seq {
            self.history.record_snapshot(&snapshot);
            self.last_seq = snapshot.seq;
        }

        let vertical = self.geom.edge.is_vertical();
        let extra = ExtraTelemetry::default();
        let (max_width_px, max_height_px) = render_bounds(&self.geom);

        match &mut self.renderer {
            OverlayRenderer::D2d {
                renderer,
                dpi,
                size_px,
            } => {
                renderer.set_max_physical_size(max_width_px, max_height_px);
                match renderer.render_if_dirty(
                    &self.config,
                    &snapshot,
                    &self.history,
                    &self.theme_ctx,
                    &extra,
                    *dpi,
                    vertical,
                    force,
                ) {
                    Ok(Some(frame)) => {
                        let new_size = (frame.width as i32, frame.height as i32);
                        let (x, y) = compute_overlay_position(
                            &self.config,
                            &self.geom,
                            new_size.0,
                            new_size.1,
                        );
                        let dib = renderer.dib();
                        match present_layered_window(hwnd, dib, Some((x, y)), 255) {
                            Ok(()) => {
                                *size_px = new_size;
                                self.last_pos = Some((x, y));
                            }
                            Err(err) => {
                                warn!(%err, "UpdateLayeredWindowIndirect failed; falling back to GDI");
                                self.fallback_to_gdi(hwnd, &snapshot);
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        warn!(%err, "Direct2D render failed; falling back to GDI");
                        self.fallback_to_gdi(hwnd, &snapshot);
                    }
                }
            }
            OverlayRenderer::Gdi(gdi) => {
                if let Err(err) = gdi.render(hwnd, &self.config, &snapshot) {
                    error!("GDI render failed: {err}");
                }
            }
        }

        if self.flyout.is_visible() {
            let rect = self.overlay_rect();
            let gm = self.game_mode_active();
            self.flyout.refresh(
                rect,
                &self.geom,
                &self.config,
                &snapshot,
                &self.history,
                &self.theme_ctx,
                &self.localizer,
                gm,
            );
        }
    }

    fn fallback_to_gdi(&mut self, hwnd: HWND, snapshot: &Snapshot) {
        if self.gdi_fallback_failed {
            return;
        }
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let layered = WS_EX_LAYERED.0 as isize;
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !layered);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | layered);
        }
        configure_gdi_layered(hwnd, self.config.taskbar.opacity);
        let dpi = render::dpi_for_window(hwnd).max(self.geom.dpi);
        let (max_width_px, max_height_px) = render_bounds(&self.geom);
        let metrics = layout::render_metrics_for(
            &self.config,
            dpi,
            self.geom.edge.is_vertical(),
            max_width_px,
            max_height_px,
        );
        match GdiRenderer::new(
            hwnd,
            &self.config,
            (metrics.width_px as i32, metrics.height_px as i32),
            metrics.dpi as u32,
        ) {
            Ok(mut gdi) => {
                let _ = gdi.render(hwnd, &self.config, snapshot);
                // Replace only after a successful build so the D2D renderer (and its DIB)
                // is dropped exactly once, after the new one is ready.
                self.renderer = OverlayRenderer::Gdi(gdi);
                self.last_pos = None;
                self.reposition(hwnd, self.visible);
            }
            Err(err) => {
                self.gdi_fallback_failed = true;
                error!("GDI fallback initialization failed; keeping the D2D renderer: {err}");
            }
        }
    }

    fn set_visible(&mut self, hwnd: HWND, visible: bool) {
        if visible {
            self.reposition(hwnd, true);
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            self.visible = true;
            self.render(hwnd, true);
            // Reassert TOPMOST after ULW presentation so returning from fullscreen/Roblox
            // always places the overlay cleanly above the Windows 11 taskbar.
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        } else {
            self.hide_flyout(hwnd);
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            self.visible = false;
        }
    }

    fn show(&mut self, hwnd: HWND) {
        self.shown = true;
        if self.manual_game_mode {
            self.set_manual_game_mode(hwnd, false);
        } else {
            self.apply_visibility(hwnd, true);
        }
    }

    fn hide(&mut self, hwnd: HWND) {
        self.shown = false;
        if !self.hidden_notice_shown {
            tray::notify_hidden(hwnd, &self.localizer);
            self.hidden_notice_shown = true;
        }
        self.apply_visibility(hwnd, true);
    }

    fn toggle_show(&mut self, hwnd: HWND) {
        if self.shown {
            self.hide(hwnd);
        } else {
            self.show(hwnd);
        }
    }

    fn set_manual_game_mode(&mut self, hwnd: HWND, enabled: bool) {
        self.manual_game_mode = enabled;
        self.publish_game_mode();
        if self.tray_added {
            tray::update_tooltip(hwnd, self.manual_game_mode, &self.localizer);
        }
        info!(enabled, "manual Game Mode toggled");
        self.apply_visibility(hwnd, true);
    }

    fn toggle_manual_game_mode(&mut self, hwnd: HWND) {
        self.set_manual_game_mode(hwnd, !self.manual_game_mode);
    }

    fn toggle_lock_position(&mut self, hwnd: HWND) {
        let mut next = self.config.clone();
        next.taskbar.lock_position = !next.taskbar.lock_position;
        self.commit_local_config(hwnd, next);
    }

    /// Unified configuration applier used by IPC preview, IPC commit, hot reload, and runtime presets.
    /// Always respects Game Mode (`manual_game_mode` / `auto_game_hidden`) and auto-hide policy.
    ///
    /// Only the subsystems affected by the transition are refreshed: a live-preview stream
    /// that touches typography/layout must not re-enumerate monitors or re-read the theme
    /// registry on every message.
    fn apply_config(&mut self, hwnd: HWND, mut new_config: Config) {
        new_config.normalize();
        if self.config == new_config {
            self.maybe_reconcile_autostart();
            self.publish_provider_config();
            return;
        }

        let scope = apply_scope(&self.config, &new_config);
        let lang_changed = !self
            .config
            .general
            .language
            .eq_ignore_ascii_case(&new_config.general.language);
        let opacity_changed = self.config.surface.opacity != new_config.surface.opacity;

        self.config = new_config;
        self.shown = self.config.taskbar.show;
        // Settings Apply/hot reload land here after the preview flag is cleared; preview
        // applies skip side effects (Task Scheduler task, live provider config).
        self.maybe_reconcile_autostart();
        self.publish_provider_config();

        if lang_changed {
            self.localizer = Localizer::new(&self.config.general.language);
        }
        if scope.theme {
            self.theme_ctx = SystemThemeContext::query_windows();
        }
        if scope.geometry {
            self.refresh_geometry();
        }
        if scope.game_mode {
            self.auto_game_hidden =
                should_auto_hide_for_game_or_fullscreen(&self.config, &self.geom);
            self.publish_game_mode();
        }

        match &mut self.renderer {
            OverlayRenderer::D2d {
                renderer,
                dpi: current_dpi,
                ..
            } => {
                if scope.geometry {
                    // The target monitor may have a different DPI; `WM_DPICHANGED` is not
                    // guaranteed for a config-only retarget, so adopt the queried DPI now.
                    *current_dpi = self.geom.dpi.max(48);
                }
                if scope.visual || scope.geometry {
                    renderer.invalidate();
                }
            }
            OverlayRenderer::Gdi(gdi) => {
                let dpi = if scope.geometry {
                    self.geom.dpi.max(48)
                } else {
                    render::dpi_for_window(hwnd).max(self.geom.dpi)
                };
                let (max_width_px, max_height_px) = render_bounds(&self.geom);
                let metrics = layout::render_metrics_for(
                    &self.config,
                    dpi,
                    self.geom.edge.is_vertical(),
                    max_width_px,
                    max_height_px,
                );
                if let Err(err) =
                    gdi.ensure_size(hwnd, metrics.width_px as i32, metrics.height_px as i32)
                {
                    warn!("GDI resize failed: {err}");
                }
                if scope.visual
                    && let Err(err) = gdi.update_from_config(&self.config, metrics.dpi as u32)
                {
                    warn!("GDI font update failed: {err}");
                }
                if opacity_changed {
                    configure_gdi_layered(hwnd, self.config.surface.opacity);
                }
            }
        }

        if self.tray_added && (scope.tray || scope.game_mode) {
            tray::update_tooltip(hwnd, self.manual_game_mode, &self.localizer);
        }

        let want = self.should_be_visible();
        if want != self.visible {
            self.set_visible(hwnd, want);
            return;
        }
        if !self.visible || !scope.visual {
            return;
        }

        // ULW repositions/resizes the window from the rendered frame; the GDI path needs
        // an explicit SetWindowPos for every size change.
        if scope.geometry || !self.renderer.is_d2d() {
            self.last_pos = None;
        }
        self.reposition(hwnd, false);
        self.render(hwnd, true);
    }

    /// Reconcile the scheduled task when the committed `general.autostart` differs from
    /// the last successful reconcile. Never runs while a live preview is active.
    fn maybe_reconcile_autostart(&mut self) {
        if self.coordinator.is_preview_active() {
            return;
        }
        if !autostart_needs_reconcile(&self.config, &self.autostart) {
            return;
        }
        self.autostart = reconcile_autostart(&self.config);
    }

    /// Publish the committed provider config to the sensor thread.
    ///
    /// Skipped while a live preview is active: toggling a provider in the Settings draft
    /// must not start network/shared-memory polling until the user presses Apply.
    fn publish_provider_config(&mut self) {
        let Some(handle) = &self.provider_config else {
            return;
        };
        let desired = self.config.providers.clone();
        let mut guard = match handle.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !provider_config_needs_publish(self.coordinator.is_preview_active(), &guard, &desired) {
            return;
        }
        info!(
            hwinfo = desired.hwinfo,
            lhm_http = desired.lhm_http,
            "published live provider configuration"
        );
        *guard = desired;
    }

    /// Commit a monitor-originated change (tray preset, lock position, Alt+Drag) atomically
    /// and apply it cleanly through the unified `apply_config` path.
    fn commit_local_config(&mut self, hwnd: HWND, next: Config) {
        match self.coordinator.commit_local(next.clone()) {
            Ok(committed) => self.apply_config(hwnd, committed),
            Err(err) => {
                warn!(
                    path = %self.coordinator.config_path().display(),
                    %err,
                    "failed to persist config; applying in-memory only"
                );
                self.apply_config(hwnd, next);
            }
        }
    }

    fn apply_preset_by_id(&mut self, hwnd: HWND, preset_id: &str) {
        let Some(preset) = find_builtin_preset(preset_id) else {
            return;
        };
        let mut next = self.coordinator.committed_config().clone();
        preset.apply_to(&mut next);
        self.commit_local_config(hwnd, next);
        info!(preset = %preset_id, "applied and persisted runtime preset");
    }

    fn open_settings(&mut self, hwnd: HWND) {
        self.hide_flyout(hwnd);

        // If oxide-settings.exe is already connected over IPC, focus the existing window
        // instead of spawning a duplicate process.
        if self.ipc_host.is_connected() {
            self.ipc_host.send_control(MonitorToSettings::FocusTab {
                tab: "general".into(),
            });
            info!("focused already-connected oxide-settings.exe over IPC");
            return;
        }

        // Debounce rapid double-clicks while oxide-settings.exe is still initializing.
        let now = Instant::now();
        if let Some(last) = self.last_settings_launch
            && now.duration_since(last) < Duration::from_millis(1500)
        {
            return;
        }
        self.last_settings_launch = Some(now);

        let has_settings_exe = tray::sibling_settings_exe().is_some();
        if has_settings_exe {
            self.ipc_host.ensure_started(hwnd);
        }
        match tray::open_settings_or_config(hwnd) {
            Ok(SettingsLaunchOutcome::SettingsExe) => {
                info!("launched sibling oxide-settings.exe over IPC");
            }
            Ok(SettingsLaunchOutcome::NotepadFallback) => {
                if !self.ipc_host.is_connected() {
                    self.ipc_host.shutdown();
                }
            }
            Err(err) => {
                warn!("opening settings/config failed: {err}");
                if !self.ipc_host.is_connected() {
                    self.ipc_host.shutdown();
                }
            }
        }
    }

    fn on_ipc_commands(&mut self, hwnd: HWND) {
        let commands = self.ipc_host.drain_commands();
        for cmd in coalesce_commands(commands) {
            match cmd {
                CoalescedCommand::Hello => {
                    let ack = self.coordinator.build_hello_ack(enumerate_monitors());
                    self.ipc_host.send_control(ack);
                    let snap = clone_snapshot(&self.snapshot);
                    self.ipc_host.send_telemetry(&snap, &self.history);
                }
                CoalescedCommand::Preview(config) => self.queue_preview(hwnd, *config),
                CoalescedCommand::Commit {
                    expected_generation,
                    expected_hash,
                    config,
                } => {
                    // A commit is authoritative: never apply a preview that was queued
                    // before it.
                    self.cancel_pending_preview(hwnd);
                    let (status, applied, error) =
                        self.coordinator
                            .commit_ipc(expected_generation, expected_hash, *config);
                    if let Some(committed) = applied {
                        self.apply_config(hwnd, committed);
                    }
                    self.ipc_host.send_control(MonitorToSettings::CommitResult {
                        status,
                        generation: self.coordinator.generation(),
                        config_hash: self.coordinator.committed_hash(),
                        error,
                    });
                }
                CoalescedCommand::Revert => {
                    self.cancel_pending_preview(hwnd);
                    if let Some(restored) = self.coordinator.revert_preview() {
                        self.apply_config(hwnd, restored);
                    }
                }
                CoalescedCommand::PawnioControl { start } => {
                    if let Some(sender) = &self.pawnio_commands {
                        let _ = sender.send(if start {
                            PawnioCommand::Start
                        } else {
                            PawnioCommand::Stop
                        });
                    }
                }
                CoalescedCommand::Disconnected => {
                    self.cancel_pending_preview(hwnd);
                    if let Some(restored) = self.coordinator.revert_preview() {
                        info!(
                            "IPC client disconnected during preview; reverted to committed config"
                        );
                        self.apply_config(hwnd, restored);
                    }
                    self.ipc_host.reap_if_finished();
                }
            }
        }
    }

    /// Accept a preview, pause hot reload immediately, and schedule at most one apply
    /// per `PREVIEW_COALESCE_MS` window (leading edge, ~50 ms worst-case latency).
    fn queue_preview(&mut self, hwnd: HWND, config: Config) {
        self.pending_preview = Some(self.coordinator.begin_preview(config));
        if self.preview_timer_armed {
            return;
        }
        if unsafe { SetTimer(Some(hwnd), TIMER_PREVIEW_ID, PREVIEW_COALESCE_MS, None) } != 0 {
            self.preview_timer_armed = true;
        } else {
            // No timer available: degrade to immediate application so preview still works.
            self.flush_pending_preview(hwnd);
        }
    }

    fn flush_pending_preview(&mut self, hwnd: HWND) {
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_PREVIEW_ID);
        }
        self.preview_timer_armed = false;
        if let Some(config) = self.pending_preview.take() {
            self.apply_config(hwnd, config);
        }
    }

    fn cancel_pending_preview(&mut self, hwnd: HWND) {
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_PREVIEW_ID);
        }
        self.preview_timer_armed = false;
        self.pending_preview = None;
    }

    fn hide_flyout(&mut self, hwnd: HWND) {
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_FLYOUT_GRACE_ID);
        }
        self.flyout.hide();
    }

    fn toggle_flyout(&mut self, hwnd: HWND, pinned: bool) {
        if self.flyout.is_visible() {
            self.hide_flyout(hwnd);
        } else {
            self.open_flyout(hwnd, pinned);
        }
    }

    fn open_flyout(&mut self, hwnd: HWND, pinned: bool) {
        if !self.visible {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_FLYOUT_GRACE_ID);
        }
        let snapshot = clone_snapshot(&self.snapshot);
        let rect = self.overlay_rect();
        let gm = self.game_mode_active();
        self.flyout.show(
            hwnd,
            rect,
            &self.geom,
            &self.config,
            &snapshot,
            &self.history,
            &self.theme_ctx,
            &self.localizer,
            gm,
            pinned,
        );
    }

    fn cycle_preset(&mut self, hwnd: HWND) {
        let list = &self.config.interactions.cycle_presets;
        if list.is_empty() {
            return;
        }
        let cur_idx = list
            .iter()
            .position(|id| id == &self.config.general.active_preset)
            .unwrap_or(list.len() - 1);
        let next_id = list[(cur_idx + 1) % list.len()].clone();
        self.apply_preset_by_id(hwnd, &next_id);
    }

    fn apply_visibility(&mut self, hwnd: HWND, force: bool) {
        let want = self.should_be_visible();
        if force || want != self.visible {
            self.set_visible(hwnd, want);
        }
    }

    fn on_tray_activation(&mut self, hwnd: HWND) {
        let now = Instant::now();
        if let Some(last) = self.last_tray_toggle
            && now.duration_since(last) < TRAY_TOGGLE_DEBOUNCE
        {
            return;
        }
        self.last_tray_toggle = Some(now);
        // A left click always reveals the widget; hiding is explicit via the menu.
        self.show(hwnd);
    }

    fn reposition(&mut self, hwnd: HWND, show: bool) {
        if self.drag.is_some() {
            return;
        }
        let (width, height) = self.renderer.size_px();
        let (x, y) = compute_overlay_position(&self.config, &self.geom, width, height);
        if !show && self.last_pos == Some((x, y)) {
            return;
        }
        let result = unsafe {
            if show {
                SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    x,
                    y,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                )
            } else {
                SetWindowPos(
                    hwnd,
                    None,
                    x,
                    y,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                )
            }
        };
        match result {
            Ok(()) => self.last_pos = Some((x, y)),
            Err(err) => error!("SetWindowPos failed: {err}"),
        }
    }

    fn handle_click_effect(&mut self, hwnd: HWND, effect: ActionEffect) {
        match effect {
            ActionEffect::Handled => {}
            ActionEffect::ToggleFlyout => self.toggle_flyout(hwnd, true),
            ActionEffect::ToggleGameMode => self.toggle_manual_game_mode(hwnd),
            ActionEffect::CyclePreset => self.cycle_preset(hwnd),
            ActionEffect::OpenSettings => self.open_settings(hwnd),
        }
    }

    fn on_foreground_changed(&mut self, hwnd: HWND) {
        // Clicking any other window dismisses a pinned Flyout cleanly.
        if self.flyout.is_visible() && self.flyout.is_pinned() {
            self.hide_flyout(hwnd);
        }
        let next = should_auto_hide_for_game_or_fullscreen(&self.config, &self.geom);
        if next != self.auto_game_hidden {
            self.auto_game_hidden = next;
            self.publish_game_mode();
            debug!(
                auto_game_hidden = next,
                "foreground Game Mode state changed"
            );
            self.apply_visibility(hwnd, false);
        }
    }

    fn on_taskbar_moved(&mut self, hwnd: HWND) {
        self.refresh_geometry();
        self.apply_visibility(hwnd, false);
        if self.visible {
            self.last_pos = None;
            self.reposition(hwnd, true);
            // Taskbar thickness/edge may have changed, so the fit bounds and effective
            // DPI must be recomputed immediately instead of waiting for the next tick.
            self.render(hwnd, true);
        }
    }

    fn on_timer(&mut self, hwnd: HWND) {
        self.ticks = self.ticks.wrapping_add(1);
        self.flyout.release_if_idle();
        self.ipc_host.reap_if_finished();

        // Bounded exponential backoff retry for tray icon if initial NIM_ADD failed.
        if !self.tray_added && self.tray_retry.should_retry_now() {
            match tray::add_retry(hwnd, &self.localizer) {
                Ok(()) => {
                    self.tray_added = true;
                    self.tray_retry.reset();
                    if self.manual_game_mode {
                        tray::update_tooltip(hwnd, true, &self.localizer);
                    }
                    info!("tray icon registered on retry");
                }
                Err(err) => {
                    warn!(
                        attempt = self.tray_retry.attempts(),
                        "tray icon retry failed: {err}"
                    );
                    self.tray_retry.schedule_next();
                }
            }
        }

        // Hot reload config.toml every 2s (paused automatically while IPC preview is active).
        if self.ticks.is_multiple_of(2) {
            match self.coordinator.poll_hot_reload() {
                HotReloadOutcome::None => {}
                HotReloadOutcome::Apply(reloaded) => {
                    info!(
                        path = %self.coordinator.config_path().display(),
                        generation = self.coordinator.generation(),
                        "hot-reloaded external config.toml changes"
                    );
                    self.apply_config(hwnd, *reloaded);
                }
                HotReloadOutcome::LogInvalidOnce(err) => {
                    warn!(
                        path = %self.coordinator.config_path().display(),
                        %err,
                        "ignoring invalid config.toml during hot reload; keeping active configuration"
                    );
                }
            }
        }

        // Periodically refresh fullscreen/geometry state as a safety net alongside WinEventHooks.
        self.auto_game_hidden = should_auto_hide_for_game_or_fullscreen(&self.config, &self.geom);
        self.publish_game_mode();
        if self.ticks.is_multiple_of(2) {
            self.refresh_geometry();
        }

        self.apply_visibility(hwnd, false);

        if self.visible && self.drag.is_none() {
            // The taskbar is topmost too, so periodically put the overlay back on top.
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            if self.ticks.is_multiple_of(2) {
                self.reposition(hwnd, false);
            }
        }

        let seq = snapshot_seq(&self.snapshot);
        if seq != self.last_seq {
            if self.visible {
                self.render(hwnd, false);
            } else {
                let snap = clone_snapshot(&self.snapshot);
                self.history.record_snapshot(&snap);
                self.last_seq = snap.seq;
            }
            if self.ipc_host.is_connected() {
                let snap = clone_snapshot(&self.snapshot);
                self.ipc_host.send_telemetry(&snap, &self.history);
            }
        }
    }

    fn on_flyout_grace_timer(&mut self, hwnd: HWND) {
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_FLYOUT_GRACE_ID);
        }
        if self.flyout.is_visible()
            && !self.flyout.is_pinned()
            && !self.mouse_inside_overlay
            && !self.flyout.is_cursor_inside()
        {
            self.flyout.hide();
        }
    }

    fn schedule_flyout_grace(&self, hwnd: HWND) {
        if self.flyout.is_visible() && !self.flyout.is_pinned() {
            unsafe {
                let _ = SetTimer(
                    Some(hwnd),
                    TIMER_FLYOUT_GRACE_ID,
                    FLYOUT_LEAVE_GRACE_MS,
                    None,
                );
            }
        }
    }

    fn on_mouse_move(&mut self, hwnd: HWND) {
        self.mouse_inside_overlay = true;

        if let Some(mut drag) = self.drag {
            let mut pt = POINT::default();
            if unsafe { GetCursorPos(&mut pt) }.is_ok() {
                let dx = pt.x - drag.start_cursor.0;
                let dy = pt.y - drag.start_cursor.1;
                if !drag.moved && (dx.abs() >= DRAG_THRESHOLD_PX || dy.abs() >= DRAG_THRESHOLD_PX) {
                    drag.moved = true;
                    self.hide_flyout(hwnd);
                }
                if drag.moved {
                    let (w, h) = self.renderer.size_px();
                    let (sx, sy) = snap_drag_position(
                        drag.start_origin.0 + dx,
                        drag.start_origin.1 + dy,
                        w,
                        h,
                        &self.geom,
                        &self.config,
                    );
                    let _ = unsafe {
                        SetWindowPos(
                            hwnd,
                            Some(HWND_TOPMOST),
                            sx,
                            sy,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOACTIVATE,
                        )
                    };
                    self.last_pos = Some((sx, sy));
                }
            }
            self.drag = Some(drag);
            return;
        }

        if !self.tracking_mouse {
            let hover_enabled = matches!(
                self.config.flyout.trigger,
                FlyoutTrigger::HoverAndClick | FlyoutTrigger::HoverOnly
            );
            let mut flags = TME_LEAVE;
            if self.config.flyout.enabled && hover_enabled {
                flags |= TME_HOVER;
            }
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: flags,
                hwndTrack: hwnd,
                dwHoverTime: self.config.flyout.hover_delay_ms,
            };
            if unsafe { TrackMouseEvent(&mut tme) }.is_ok() {
                self.tracking_mouse = true;
            }
        }
    }

    fn on_lbutton_down(&mut self, hwnd: HWND) {
        let alt_down = unsafe { GetAsyncKeyState(VK_MENU.0 as i32) } < 0;
        let can_drag = alt_down || !self.config.taskbar.lock_position;
        if !can_drag {
            return;
        }
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt) }.is_ok() {
            let rect = self.overlay_rect();
            self.drag = Some(DragSession {
                start_cursor: (pt.x, pt.y),
                start_origin: (rect.left, rect.top),
                moved: false,
            });
            unsafe {
                SetCapture(hwnd);
            }
        }
    }

    fn finish_drag(&mut self, hwnd: HWND, commit: bool) -> bool {
        let Some(drag) = self.drag.take() else {
            return false;
        };
        unsafe {
            let _ = ReleaseCapture();
        }
        if drag.moved && commit {
            if let Some((x, y)) = self.last_pos {
                let (ox, oy) = free_drag_offsets_dip(x, y, &self.geom);
                let mut next = self.config.clone();
                next.taskbar.anchor = TaskbarAnchor::FreeDrag;
                next.taskbar.x_offset = ox;
                next.taskbar.y_offset = oy;
                self.commit_local_config(hwnd, next);
                info!(
                    x_offset = ox,
                    y_offset = oy,
                    "saved FreeDrag widget position"
                );
            }
        }
        drag.moved
    }

    fn on_display_change(&mut self, hwnd: HWND) {
        self.theme_ctx = SystemThemeContext::query_windows();
        self.refresh_geometry();
        let dpi = render::dpi_for_window(hwnd).max(self.geom.dpi);
        match &mut self.renderer {
            OverlayRenderer::D2d {
                renderer,
                dpi: current_dpi,
                ..
            } => {
                *current_dpi = dpi;
                renderer.invalidate();
            }
            OverlayRenderer::Gdi(gdi) => {
                let (max_width_px, max_height_px) = render_bounds(&self.geom);
                let metrics = layout::render_metrics_for(
                    &self.config,
                    dpi,
                    self.geom.edge.is_vertical(),
                    max_width_px,
                    max_height_px,
                );
                if let Err(err) =
                    gdi.ensure_size(hwnd, metrics.width_px as i32, metrics.height_px as i32)
                {
                    error!("resizing GDI back buffer failed: {err}");
                }
                if let Err(err) = gdi.update_from_config(&self.config, metrics.dpi as u32) {
                    error!("recreating font failed: {err}");
                }
            }
        }
        self.last_pos = None;
        self.reposition(hwnd, self.visible);
        if self.visible {
            self.render(hwnd, true);
        }
        if self.ipc_host.is_connected() {
            self.ipc_host.send_monitors_changed(enumerate_monitors());
        }
    }

    fn on_resume(&mut self, hwnd: HWND) {
        self.theme_ctx = SystemThemeContext::query_windows();
        self.refresh_geometry();
        if let OverlayRenderer::D2d { renderer, .. } = &mut self.renderer {
            renderer.invalidate();
        }
        self.last_pos = None;
        self.reposition(hwnd, self.visible);
        if self.visible {
            self.render(hwnd, true);
        }
        if self.ipc_host.is_connected() {
            self.ipc_host.send_monitors_changed(enumerate_monitors());
        }
    }

    fn on_taskbar_created(&mut self, hwnd: HWND) {
        if self.tray_added {
            tray::remove(hwnd);
            self.tray_added = false;
        }
        self.tray_retry.reset();
        match tray::add_retry(hwnd, &self.localizer) {
            Ok(()) => {
                self.tray_added = true;
                if self.manual_game_mode {
                    tray::update_tooltip(hwnd, true, &self.localizer);
                }
            }
            Err(err) => {
                warn!("failed to re-add tray icon: {err}");
                self.tray_retry.schedule_next();
            }
        }
        self.refresh_geometry();
        if let OverlayRenderer::D2d { renderer, .. } = &mut self.renderer {
            renderer.invalidate();
        }
        self.last_pos = None;
        self.reposition(hwnd, self.visible);
        if self.visible {
            self.render(hwnd, true);
        }
        if self.ipc_host.is_connected() {
            self.ipc_host.send_monitors_changed(enumerate_monitors());
        }
    }
}

/// True when the committed config and the last reconcile disagree about autostart.
///
/// `desired != status.desired_enabled` covers a fresh change; `desired != status.enabled`
/// retries after a failed create/delete on the next apply.
fn autostart_needs_reconcile(config: &Config, status: &AutostartStatus) -> bool {
    config.general.autostart != status.desired_enabled || config.general.autostart != status.enabled
}

/// Provider toggles become live only after Apply/hot reload, never during a preview.
fn provider_config_needs_publish(
    preview_active: bool,
    published: &ProvidersConfig,
    desired: &ProvidersConfig,
) -> bool {
    !preview_active && published != desired
}

/// Create/update or delete the per-user Task Scheduler task and publish the status.
fn reconcile_autostart(config: &Config) -> AutostartStatus {
    let exe = std::env::current_exe().unwrap_or_default();
    let name = AUTOSTART_TASK_NAME.to_string();
    let spec = TaskSpec::for_exe(&name, &exe);
    let status = reconcile_autostart_task(
        config.general.autostart,
        &spec,
        &TaskSchedulerBackend::default(),
    );
    if let Err(err) = save_autostart_status(&status) {
        warn!(%err, "failed to publish autostart status file");
    }
    status
}

fn configure_gdi_layered(hwnd: HWND, opacity: u8) {
    if let Err(err) = unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), opacity, LWA_ALPHA) } {
        warn!("SetLayeredWindowAttributes failed: {err}");
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut UiState };
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }

    let taskbar_message = unsafe { (*ptr).taskbar_message };
    if taskbar_message != 0 && message == taskbar_message {
        let state = unsafe { &mut *ptr };
        state.on_taskbar_created(hwnd);
        return LRESULT(0);
    }

    if message == crate::SHOW_MESSAGE {
        let state = unsafe { &mut *ptr };
        state.show(hwnd);
        return LRESULT(0);
    }

    if message == crate::WM_OXIDE_OPEN_IPC {
        let state = unsafe { &mut *ptr };
        state.ipc_host.ensure_started(hwnd);
        return LRESULT(0);
    }

    if message == WM_OXIDE_IPC_CMD {
        let state = unsafe { &mut *ptr };
        state.on_ipc_commands(hwnd);
        return LRESULT(0);
    }

    if message == WM_OXIDE_FOREGROUND_CHANGED {
        let state = unsafe { &mut *ptr };
        state.on_foreground_changed(hwnd);
        return LRESULT(0);
    }

    if message == WM_OXIDE_TASKBAR_MOVED {
        let state = unsafe { &mut *ptr };
        state.on_taskbar_moved(hwnd);
        return LRESULT(0);
    }

    if message == WM_OXIDE_FLYOUT_BUTTON {
        let state = unsafe { &mut *ptr };
        match wparam.0 {
            BTN_TOGGLE_GAME_MODE => state.toggle_manual_game_mode(hwnd),
            BTN_OPEN_SETTINGS => state.open_settings(hwnd),
            BTN_OPEN_TASK_MGR => {
                state.hide_flyout(hwnd);
                actions::shell_open(hwnd, "taskmgr.exe", None);
            }
            _ => {}
        }
        return LRESULT(0);
    }

    if message == WM_OXIDE_FLYOUT_LEAVE {
        let state = unsafe { &mut *ptr };
        state.schedule_flyout_grace(hwnd);
        return LRESULT(0);
    }

    if message == tray::CALLBACK_MESSAGE {
        let event = (lparam.0 as u32) & 0xffff;
        if event == WM_CONTEXTMENU {
            open_context_menu(hwnd);
        } else if event == windows::Win32::UI::Shell::NIN_SELECT
            || event == NIN_KEYSELECT
            || event == WM_LBUTTONDBLCLK
            || event == WM_LBUTTONUP
        {
            let state = unsafe { &mut *ptr };
            state.on_tray_activation(hwnd);
        }
        return LRESULT(0);
    }

    match message {
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
            let state = unsafe { &mut *ptr };
            if !hdc.is_invalid() && !state.renderer.is_d2d() {
                state.render(hwnd, true);
            }
            unsafe {
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let state = unsafe { &mut *ptr };
            state.on_mouse_move(hwnd);
            LRESULT(0)
        }
        WM_MOUSEHOVER => {
            let state = unsafe { &mut *ptr };
            state.tracking_mouse = false;
            if state.drag.is_none() && !state.flyout.is_visible() {
                state.open_flyout(hwnd, false);
            }
            // Re-arm TME_LEAVE so moving the cursor away triggers the leave grace timer
            // even without another intermediate WM_MOUSEMOVE over the overlay.
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            if unsafe { TrackMouseEvent(&mut tme) }.is_ok() {
                state.tracking_mouse = true;
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let state = unsafe { &mut *ptr };
            state.tracking_mouse = false;
            state.mouse_inside_overlay = false;
            state.schedule_flyout_grace(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let state = unsafe { &mut *ptr };
            state.on_lbutton_down(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let state = unsafe { &mut *ptr };
            let was_drag = state.finish_drag(hwnd, true);
            if !was_drag {
                let action = state.config.interactions.left_click.clone();
                let snap = clone_snapshot(&state.snapshot);
                let eff = actions::execute_click_action(hwnd, &action, &state.config, &snap);
                state.handle_click_effect(hwnd, eff);
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            let state = unsafe { &mut *ptr };
            state.finish_drag(hwnd, false);
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let state = unsafe { &mut *ptr };
            state.hide_flyout(hwnd);
            let action = state.config.interactions.double_click.clone();
            let snap = clone_snapshot(&state.snapshot);
            let eff = actions::execute_click_action(hwnd, &action, &state.config, &snap);
            state.handle_click_effect(hwnd, eff);
            LRESULT(0)
        }
        WM_MBUTTONUP => {
            let state = unsafe { &mut *ptr };
            let action = state.config.interactions.middle_click.clone();
            let snap = clone_snapshot(&state.snapshot);
            let eff = actions::execute_click_action(hwnd, &action, &state.config, &snap);
            state.handle_click_effect(hwnd, eff);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let state = unsafe { &mut *ptr };
            if state.config.interactions.wheel_action == WheelAction::CyclePreset {
                state.cycle_preset(hwnd);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let state = unsafe { &mut *ptr };
            state.hide_flyout(hwnd);
            open_context_menu(hwnd);
            LRESULT(0)
        }
        WM_TIMER => {
            let state = unsafe { &mut *ptr };
            if wparam.0 == TIMER_TICK_ID {
                state.on_timer(hwnd);
            } else if wparam.0 == TIMER_FLYOUT_GRACE_ID {
                state.on_flyout_grace_timer(hwnd);
            } else if wparam.0 == TIMER_PREVIEW_ID {
                state.flush_pending_preview(hwnd);
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE | WM_DPICHANGED => {
            let state = unsafe { &mut *ptr };
            state.on_display_change(hwnd);
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            if wparam.0 as u32 == PBT_APMRESUMEAUTOMATIC || wparam.0 as u32 == PBT_APMRESUMESUSPEND
            {
                let state = unsafe { &mut *ptr };
                state.on_resume(hwnd);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                let state = unsafe { &mut *ptr };
                if state.tray_added {
                    tray::remove(hwnd);
                    state.tray_added = false;
                }
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn open_context_menu(hwnd: HWND) {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut UiState };
    if ptr.is_null() {
        return;
    }
    let menu_state = unsafe {
        tray::MenuState {
            shown: (*ptr).shown,
            game_mode: (*ptr).manual_game_mode,
            lock_position: (*ptr).config.taskbar.lock_position,
            active_preset: &(*ptr).config.general.active_preset,
        }
    };
    let localizer = unsafe { &(*ptr).localizer };
    let command = tray::show_menu(hwnd, &menu_state, localizer);
    if command == 0 {
        return;
    }

    // The popup menu runs a nested message loop, so re-read the state pointer.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut UiState };
    if ptr.is_null() {
        return;
    }
    let state = unsafe { &mut *ptr };
    if let Some(preset_id) = tray::preset_id_from_command(command) {
        state.apply_preset_by_id(hwnd, &preset_id);
        return;
    }
    match command {
        tray::MENU_TOGGLE => state.toggle_show(hwnd),
        tray::MENU_FLYOUT => state.toggle_flyout(hwnd, true),
        tray::MENU_GAME_MODE => state.toggle_manual_game_mode(hwnd),
        tray::MENU_LOCK_POS => state.toggle_lock_position(hwnd),
        tray::MENU_TASK_MGR => actions::shell_open(hwnd, "taskmgr.exe", None),
        tray::MENU_SETTINGS => state.open_settings(hwnd),
        tray::MENU_CONFIG => {
            if let Err(err) = tray::open_config(hwnd) {
                error!("opening config failed: {err}");
            }
        }
        tray::MENU_EXIT => unsafe { PostQuitMessage(0) },
        _ => {}
    }
}

fn register_class(instance: HINSTANCE) -> Result<(), UiError> {
    let window_class = WNDCLASSW {
        style: CS_DBLCLKS,
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: tray::app_icon().unwrap_or_default(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: CLASS_NAME,
    };

    if unsafe { RegisterClassW(&window_class) } == 0 {
        let error = unsafe { GetLastError() };
        if error != ERROR_CLASS_ALREADY_EXISTS {
            return Err(UiError::Win32(format!(
                "RegisterClassW failed: {}",
                error.0
            )));
        }
    }
    Ok(())
}

fn clone_snapshot(shared: &Arc<Mutex<Snapshot>>) -> Snapshot {
    match shared.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

fn snapshot_seq(shared: &Arc<Mutex<Snapshot>>) -> u64 {
    match shared.lock() {
        Ok(guard) => guard.seq,
        Err(poisoned) => poisoned.into_inner().seq,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(preset: &str) -> IncomingCommand {
        let mut cfg = Config::default();
        cfg.general.active_preset = preset.into();
        IncomingCommand::PreviewConfig(Box::new(cfg))
    }

    fn commit(generation: u64) -> IncomingCommand {
        IncomingCommand::CommitConfig {
            expected_generation: generation,
            expected_hash: generation.wrapping_mul(7),
            config: Box::new(Config::default()),
        }
    }

    #[test]
    fn coalesce_keeps_only_the_last_preview_per_batch() {
        let out = coalesce_commands(vec![preview("a"), preview("b"), preview("c")]);
        assert_eq!(out.len(), 1);
        match &out[0] {
            CoalescedCommand::Preview(cfg) => assert_eq!(cfg.general.active_preset, "c"),
            other => panic!("expected the last preview, got {other:?}"),
        }
    }

    #[test]
    fn coalesce_drops_preview_before_commit_and_keeps_order() {
        let out = coalesce_commands(vec![preview("stale"), commit(5), preview("fresh")]);
        assert_eq!(out.len(), 2);
        assert!(matches!(
            out[0],
            CoalescedCommand::Commit {
                expected_generation: 5,
                ..
            }
        ));
        match &out[1] {
            CoalescedCommand::Preview(cfg) => assert_eq!(cfg.general.active_preset, "fresh"),
            other => panic!("expected a fresh preview, got {other:?}"),
        }
    }

    #[test]
    fn coalesce_preserves_pawnio_start_stop_commands() {
        let out = coalesce_commands(vec![
            IncomingCommand::PawnioControl { start: true },
            IncomingCommand::PawnioControl { start: false },
        ]);
        assert_eq!(out.len(), 2);
        assert!(matches!(
            out[0],
            CoalescedCommand::PawnioControl { start: true }
        ));
        assert!(matches!(
            out[1],
            CoalescedCommand::PawnioControl { start: false }
        ));
    }

    #[test]
    fn coalesce_prunes_previews_on_revert_and_disconnect() {
        let out = coalesce_commands(vec![
            preview("a"),
            IncomingCommand::RevertConfig,
            preview("b"),
            IncomingCommand::Disconnected,
            preview("c"),
        ]);
        assert_eq!(out.len(), 3);
        assert!(matches!(out[0], CoalescedCommand::Revert));
        assert!(matches!(out[1], CoalescedCommand::Disconnected));
        match &out[2] {
            CoalescedCommand::Preview(cfg) => assert_eq!(cfg.general.active_preset, "c"),
            other => panic!("expected a trailing preview, got {other:?}"),
        }
    }

    #[test]
    fn coalesce_drops_preview_when_a_new_handshake_starts() {
        let out = coalesce_commands(vec![
            preview("stale"),
            IncomingCommand::Hello { focus_tab: None },
        ]);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], CoalescedCommand::Hello));
    }

    #[test]
    fn apply_scope_marks_only_the_affected_subsystems() {
        let base = Config::default();

        let mut opacity = base.clone();
        opacity.surface.opacity = opacity.surface.opacity.saturating_sub(20);
        let scope = apply_scope(&base, &opacity);
        assert!(scope.visual);
        assert!(!scope.theme && !scope.geometry && !scope.game_mode && !scope.tray);

        let mut anchor = base.clone();
        anchor.taskbar.anchor = TaskbarAnchor::TaskbarLeft;
        let scope = apply_scope(&base, &anchor);
        assert!(scope.geometry && scope.visual);
        assert!(!scope.theme && !scope.tray);

        let mut language = base.clone();
        language.general.language = "en".into();
        let scope = apply_scope(&base, &language);
        assert!(scope.tray);
        assert!(!scope.visual && !scope.theme && !scope.geometry);

        let mut theme = base.clone();
        theme.general.theme = "light".into();
        let scope = apply_scope(&base, &theme);
        assert!(scope.theme && scope.visual);
    }

    #[test]
    fn apply_scope_is_silent_for_non_rendering_changes() {
        let base = Config::default();
        let mut next = base.clone();
        next.general.autostart = !base.general.autostart;
        assert_eq!(apply_scope(&base, &next), ApplyScope::default());
    }

    #[test]
    fn autostart_reconcile_is_driven_by_the_committed_flag() {
        let mut config = Config::default();
        let status = AutostartStatus::default();

        // Enabling for the first time needs a create.
        config.general.autostart = true;
        assert!(autostart_needs_reconcile(&config, &status));

        // After a successful enable nothing is retried.
        let enabled = AutostartStatus {
            desired_enabled: true,
            enabled: true,
            ..AutostartStatus::default()
        };
        assert!(!autostart_needs_reconcile(&config, &enabled));

        // A failed create is retried on the next apply.
        let failed = AutostartStatus {
            desired_enabled: true,
            enabled: false,
            last_error: Some("access denied".into()),
            ..AutostartStatus::default()
        };
        assert!(autostart_needs_reconcile(&config, &failed));

        // Disabling deletes the task.
        config.general.autostart = false;
        assert!(autostart_needs_reconcile(&config, &enabled));

        // After a successful delete nothing is retried.
        let disabled = AutostartStatus {
            desired_enabled: false,
            enabled: false,
            ..AutostartStatus::default()
        };
        assert!(!autostart_needs_reconcile(&config, &disabled));
    }

    #[test]
    fn provider_toggles_are_published_only_on_apply() {
        let published = ProvidersConfig::default();
        let mut draft = published.clone();
        draft.hwinfo = true;

        // A live preview never starts shared-memory/network polling.
        assert!(!provider_config_needs_publish(true, &published, &draft));
        // Apply / hot reload publishes the change once.
        assert!(provider_config_needs_publish(false, &published, &draft));
        // Republishing the same value is a no-op.
        assert!(!provider_config_needs_publish(false, &draft, &draft));
        // Disabling is published as well.
        assert!(provider_config_needs_publish(false, &draft, &published));
    }
}
