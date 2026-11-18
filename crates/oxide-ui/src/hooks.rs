//! Event-driven `SetWinEventHook` (`WINEVENT_OUTOFCONTEXT`) for Auto-hide taskbar
//! tracking and Game Mode (D3D fullscreen / borderless fullscreen / process rules).
//!
//! Zero polling timers and zero DLL injection (anti-cheat safe).

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicIsize, Ordering};

use oxide_core::Config;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND, GWL_STYLE, GetClassNameW,
    GetForegroundWindow, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, OBJID_WINDOW, PostMessageW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
    WM_APP, WS_CAPTION, WS_MAXIMIZE, WS_THICKFRAME,
};
use windows::core::PWSTR;

use crate::geometry::{RectI, TaskbarGeometry};

/// Posted to the overlay HWND when foreground window changes or taskbar moves.
pub const WM_OXIDE_FOREGROUND_CHANGED: u32 = WM_APP + 20;
pub const WM_OXIDE_TASKBAR_MOVED: u32 = WM_APP + 21;

/// Pixel tolerance for fractional DPI rounding (e.g. 106 DPI) on borderless fullscreen checks.
const FULLSCREEN_TOLERANCE_PX: i32 = 2;

static HOOK_TARGET_HWND: AtomicIsize = AtomicIsize::new(0);
static TRACKED_TASKBAR_HWND: AtomicIsize = AtomicIsize::new(0);

/// Shell / desktop window classes that must never trigger borderless-fullscreen hiding.
const SHELL_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "XamlExplorerHostIslandWindow",
    "Windows.UI.Core.CoreWindow",
    "ForegroundStaging",
    "Shell_InputSwitchTopLevelWindow",
    "OxideOverlay",
    "OxideFlyout",
];

/// Normalized shell notification state used by the pure policy evaluator.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FullscreenNotificationState {
    #[default]
    Normal,
    D3dFullScreen,
    PresentationMode,
    Busy,
}

/// Captured foreground window properties for pure, deterministic policy evaluation.
#[derive(Clone, Debug, Default)]
pub struct ForegroundSnapshot {
    pub class_name: String,
    pub exe_path: Option<String>,
    pub frame_rect: Option<RectI>,
    pub style: u32,
    pub is_visible: bool,
    pub is_minimized: bool,
    pub is_cloaked: bool,
}

/// Owns the out-of-context WinEvent hooks and unhooks them cleanly on drop.
pub struct WinEventHooks {
    fg_hook: Option<HWINEVENTHOOK>,
    taskbar_hook: Option<HWINEVENTHOOK>,
    taskbar_pid: u32,
}

impl WinEventHooks {
    pub fn install(target_hwnd: HWND, taskbar_hwnd: Option<HWND>) -> Self {
        HOOK_TARGET_HWND.store(target_hwnd.0 as isize, Ordering::SeqCst);

        let fg_hook = unsafe {
            let h = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(foreground_callback),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            (!h.is_invalid()).then_some(h)
        };

        let mut hooks = Self {
            fg_hook,
            taskbar_hook: None,
            taskbar_pid: 0,
        };
        hooks.bind_taskbar(taskbar_hwnd);
        hooks
    }

    /// Re-bind the `EVENT_OBJECT_LOCATIONCHANGE` hook if Explorer restarted or taskbar HWND changed.
    pub fn bind_taskbar(&mut self, taskbar_hwnd: Option<HWND>) {
        let tb_raw = taskbar_hwnd.map(|h| h.0 as isize).unwrap_or(0);
        TRACKED_TASKBAR_HWND.store(tb_raw, Ordering::Relaxed);
        let pid = taskbar_hwnd.map(window_pid).unwrap_or(0);
        if pid == self.taskbar_pid && self.taskbar_hook.is_some() {
            return;
        }
        if let Some(old) = self.taskbar_hook.take() {
            unsafe {
                let _ = UnhookWinEvent(old);
            }
        }
        self.taskbar_pid = pid;
        if pid != 0 {
            self.taskbar_hook = unsafe {
                let h = SetWinEventHook(
                    EVENT_OBJECT_LOCATIONCHANGE,
                    EVENT_OBJECT_LOCATIONCHANGE,
                    None,
                    Some(taskbar_move_callback),
                    pid,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                );
                (!h.is_invalid()).then_some(h)
            };
        }
    }
}

impl Drop for WinEventHooks {
    fn drop(&mut self) {
        if let Some(h) = self.fg_hook.take() {
            unsafe {
                let _ = UnhookWinEvent(h);
            }
        }
        if let Some(h) = self.taskbar_hook.take() {
            unsafe {
                let _ = UnhookWinEvent(h);
            }
        }
        HOOK_TARGET_HWND.store(0, Ordering::SeqCst);
        TRACKED_TASKBAR_HWND.store(0, Ordering::SeqCst);
    }
}

unsafe extern "system" fn foreground_callback(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    let target = HOOK_TARGET_HWND.load(Ordering::Relaxed);
    if target != 0 {
        let hwnd = HWND(target as *mut c_void);
        unsafe {
            let _ = PostMessageW(
                Some(hwnd),
                WM_OXIDE_FOREGROUND_CHANGED,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

unsafe extern "system" fn taskbar_move_callback(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    if id_object != OBJID_WINDOW.0 || id_child != 0 {
        return;
    }
    // Filter strictly to the tracked taskbar HWND so Explorer tooltips, hover previews,
    // and File Explorer windows never flood our UI message queue.
    let tracked = TRACKED_TASKBAR_HWND.load(Ordering::Relaxed);
    if tracked == 0 || hwnd.0 as isize != tracked {
        return;
    }
    let target = HOOK_TARGET_HWND.load(Ordering::Relaxed);
    if target != 0 {
        let target_hwnd = HWND(target as *mut c_void);
        unsafe {
            let _ = PostMessageW(
                Some(target_hwnd),
                WM_OXIDE_TASKBAR_MOVED,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid
}

/// Evaluate whether auto Game Mode / fullscreen hiding should be active right now.
pub fn should_auto_hide_for_game_or_fullscreen(config: &Config, geom: &TaskbarGeometry) -> bool {
    let fg = capture_foreground_snapshot();
    let quns = query_notification_state();
    evaluate_auto_hide_policy(config, geom, fg.as_ref(), quns)
}

/// Pure, deterministic policy evaluator for auto Game Mode & borderless fullscreen.
pub fn evaluate_auto_hide_policy(
    config: &Config,
    geom: &TaskbarGeometry,
    fg: Option<&ForegroundSnapshot>,
    quns: FullscreenNotificationState,
) -> bool {
    let gm = &config.game_mode;

    if let Some(fg) = fg {
        // 1. Whitelisted apps (e.g. RDP/VM) override all fullscreen/borderless hiding.
        if let Some(exe) = fg.exe_path.as_deref()
            && matches_any_rule(exe, &gm.whitelist_processes)
        {
            return false;
        }

        // 2. Explicit user game/process rules hide even in windowed mode.
        if let Some(exe) = fg.exe_path.as_deref()
            && matches_any_rule(exe, &gm.process_rules)
        {
            return true;
        }
    }

    // 3. Exclusive D3D fullscreen and presentation mode always hide.
    if gm.auto_hide_fullscreen
        && matches!(
            quns,
            FullscreenNotificationState::D3dFullScreen
                | FullscreenNotificationState::PresentationMode
        )
    {
        return true;
    }

    // 4. Borderless fullscreen (and guarded `QUNS_BUSY`) on the widget's target monitor.
    let Some(fg) = fg else {
        return false;
    };
    if !fg.is_visible || fg.is_minimized || fg.is_cloaked || is_shell_class(&fg.class_name) {
        return false;
    }

    let Some(rect) = fg.frame_rect else {
        return false;
    };
    let covers_monitor = rect_covers_monitor(rect, geom.monitor_rect);
    if !covers_monitor {
        return false;
    }

    // Normal maximized desktop apps (with titlebar + resizable frame) should NOT be
    // mistaken for borderless fullscreen games even when the taskbar is auto-hidden.
    if is_normal_maximized_desktop_window(fg.style) {
        return false;
    }

    if gm.auto_hide_borderless {
        return true;
    }

    // Guarded `QUNS_BUSY`: only hide when a real non-shell foreground window covers our monitor.
    gm.auto_hide_fullscreen && quns == FullscreenNotificationState::Busy
}

pub fn is_shell_class(class_name: &str) -> bool {
    SHELL_CLASSES
        .iter()
        .any(|c| c.eq_ignore_ascii_case(class_name))
}

/// True when `fg_rect` covers `monitor_rect` within `FULLSCREEN_TOLERANCE_PX` (handles 106 DPI rounding).
pub fn rect_covers_monitor(fg_rect: RectI, monitor_rect: RectI) -> bool {
    if !fg_rect.is_valid() || !monitor_rect.is_valid() {
        return false;
    }
    fg_rect.left <= monitor_rect.left + FULLSCREEN_TOLERANCE_PX
        && fg_rect.top <= monitor_rect.top + FULLSCREEN_TOLERANCE_PX
        && fg_rect.right >= monitor_rect.right - FULLSCREEN_TOLERANCE_PX
        && fg_rect.bottom >= monitor_rect.bottom - FULLSCREEN_TOLERANCE_PX
}

/// Standard desktop windows maximized on an auto-hide taskbar retain `WS_MAXIMIZE`,
/// `WS_CAPTION`, and `WS_THICKFRAME`. Borderless games strip `WS_CAPTION`/`WS_THICKFRAME`
/// or run unmaximized (`WS_POPUP`) at full monitor bounds.
pub fn is_normal_maximized_desktop_window(style: u32) -> bool {
    let maximized = (style & WS_MAXIMIZE.0) != 0;
    let has_caption = (style & WS_CAPTION.0) == WS_CAPTION.0;
    let has_frame = (style & WS_THICKFRAME.0) != 0;
    maximized && has_caption && has_frame
}

/// Match an executable full path against glob patterns (`*`, `?`), case-insensitively.
/// Patterns without `\` or `/` are matched against the executable filename only.
pub fn matches_any_rule(exe_path: &str, rules: &[String]) -> bool {
    let basename = exe_path.rsplit(['\\', '/']).next().unwrap_or(exe_path);
    for rule in rules {
        let pat = rule.trim();
        if pat.is_empty() {
            continue;
        }
        let target = if pat.contains('\\') || pat.contains('/') {
            exe_path
        } else {
            basename
        };
        if matches_glob(pat, target) {
            return true;
        }
    }
    false
}

/// Case-insensitive ASCII/UTF-8 glob matcher supporting `*` and `?`.
pub fn matches_glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star_idx: Option<usize> = None;
    let mut match_idx = 0usize;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_idx = Some(pi);
            match_idx = ti;
            pi += 1;
        } else if let Some(si) = star_idx {
            pi = si + 1;
            match_idx += 1;
            ti = match_idx;
        } else {
            return false;
        }
    }

    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn capture_foreground_snapshot() -> Option<ForegroundSnapshot> {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return None;
    }
    let is_visible = unsafe { IsWindowVisible(fg) }.as_bool();
    let is_minimized = unsafe { IsIconic(fg) }.as_bool();
    let is_cloaked = query_window_cloaked(fg);
    let style = unsafe { GetWindowLongW(fg, GWL_STYLE) } as u32;
    let class_name = window_class(fg);
    let frame_rect = extended_frame_rect(fg).or_else(|| window_rect(fg));
    let exe_path = process_image_path(window_pid(fg));

    Some(ForegroundSnapshot {
        class_name,
        exe_path,
        frame_rect,
        style,
        is_visible,
        is_minimized,
        is_cloaked,
    })
}

fn query_notification_state() -> FullscreenNotificationState {
    match unsafe { SHQueryUserNotificationState() } {
        Ok(state) if state == QUNS_RUNNING_D3D_FULL_SCREEN => {
            FullscreenNotificationState::D3dFullScreen
        }
        Ok(state) if state == QUNS_PRESENTATION_MODE => {
            FullscreenNotificationState::PresentationMode
        }
        Ok(state) if state == QUNS_BUSY => FullscreenNotificationState::Busy,
        _ => FullscreenNotificationState::Normal,
    }
}

fn extended_frame_rect(hwnd: HWND) -> Option<RectI> {
    let mut r = RECT::default();
    let res = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut r as *mut RECT).cast::<c_void>(),
            size_of::<RECT>() as u32,
        )
    };
    res.ok()?;
    let ri = RectI::from(r);
    ri.is_valid().then_some(ri)
}

fn query_window_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    let res = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast::<c_void>(),
            size_of::<u32>() as u32,
        )
    };
    res.is_ok() && cloaked != 0
}

fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len > 0 {
        String::from_utf16_lossy(&buf[..len as usize])
    } else {
        String::new()
    }
}

fn window_rect(hwnd: HWND) -> Option<RectI> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r) }.ok()?;
    Some(RectI::from(r))
}

fn process_image_path(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    thread_local! {
        static LAST_EXE: std::cell::RefCell<(u32, Option<String>)> = const { std::cell::RefCell::new((0, None)) };
    }
    if let Some(cached) = LAST_EXE.with(|cell| {
        let borrow = cell.borrow();
        (borrow.0 == pid).then(|| borrow.1.clone())
    }) {
        return cached;
    }

    let resolved = query_process_image_path(pid);
    LAST_EXE.with(|cell| {
        *cell.borrow_mut() = (pid, resolved.clone());
    });
    resolved
}

fn query_process_image_path(pid: u32) -> Option<String> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = vec![0u16; 512];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    unsafe {
        let _ = CloseHandle(handle);
    }
    ok.ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::TaskbarEdge;
    use windows::Win32::UI::WindowsAndMessaging::WS_POPUP;

    fn primary_geom() -> TaskbarGeometry {
        TaskbarGeometry {
            monitor_rect: RectI::new(0, 0, 1920, 1080),
            work_rect: RectI::new(0, 0, 1920, 1032),
            taskbar_rect: RectI::new(0, 1032, 1920, 1080),
            tray_rect: Some(RectI::new(1540, 1032, 1920, 1080)),
            edge: TaskbarEdge::Bottom,
            dpi: 106,
            is_autohide: false,
        }
    }

    #[test]
    fn glob_and_process_rule_matching() {
        let rules = vec![
            "cs2.exe".into(),
            "RobloxPlayerBeta.exe".into(),
            "valorant*.exe".into(),
            r"*\steamapps\common\*".into(),
        ];
        assert!(matches_any_rule(r"C:\Games\CS2\cs2.exe", &rules));
        assert!(matches_any_rule(
            r"C:\Users\u\AppData\Local\Roblox\Versions\v1\RobloxPlayerBeta.exe",
            &rules
        ));
        assert!(matches_any_rule(
            r"C:\Riot Games\VALORANT-Win64-Shipping.exe",
            &rules
        ));
        assert!(matches_any_rule(
            r"D:\SteamLibrary\steamapps\common\Hades\Hades.exe",
            &rules
        ));
        assert!(!matches_any_rule(r"C:\Windows\notepad.exe", &rules));
    }

    #[test]
    fn roblox_borderless_hides_even_without_openprocess_and_fractional_dpi() {
        let cfg = Config::default();
        let geom = primary_geom();
        // Roblox with Hyperion/Byfron may return `exe_path = None` and be 1px off at 106 DPI.
        let roblox = ForegroundSnapshot {
            class_name: "WINDOWSCLIENT".into(),
            exe_path: None,
            frame_rect: Some(RectI::new(0, 0, 1920, 1079)),
            style: WS_POPUP.0,
            is_visible: true,
            is_minimized: false,
            is_cloaked: false,
        };
        assert!(evaluate_auto_hide_policy(
            &cfg,
            &geom,
            Some(&roblox),
            FullscreenNotificationState::Normal
        ));

        // Returning to desktop (`WorkerW` / `Progman` / `Shell_TrayWnd`) restores widget even if `QUNS_BUSY` lingers.
        let desktop = ForegroundSnapshot {
            class_name: "WorkerW".into(),
            exe_path: Some(r"C:\Windows\explorer.exe".into()),
            frame_rect: Some(RectI::new(0, 0, 1920, 1080)),
            style: WS_POPUP.0,
            is_visible: true,
            is_minimized: false,
            is_cloaked: false,
        };
        assert!(!evaluate_auto_hide_policy(
            &cfg,
            &geom,
            Some(&desktop),
            FullscreenNotificationState::Busy
        ));
    }

    #[test]
    fn normal_maximized_desktop_window_does_not_falsely_hide_with_autohide() {
        let cfg = Config::default();
        let mut geom = primary_geom();
        geom.is_autohide = true;

        let browser = ForegroundSnapshot {
            class_name: "Chrome_WidgetWin_1".into(),
            exe_path: Some(r"C:\Program Files\Google\Chrome\Application\chrome.exe".into()),
            frame_rect: Some(RectI::new(0, 0, 1920, 1078)),
            style: WS_MAXIMIZE.0 | WS_CAPTION.0 | WS_THICKFRAME.0,
            is_visible: true,
            is_minimized: false,
            is_cloaked: false,
        };
        assert!(!evaluate_auto_hide_policy(
            &cfg,
            &geom,
            Some(&browser),
            FullscreenNotificationState::Normal
        ));

        // Same browser entering F11 fullscreen strips WS_CAPTION/WS_THICKFRAME -> hides.
        let browser_f11 = ForegroundSnapshot {
            style: WS_POPUP.0,
            frame_rect: Some(RectI::new(0, 0, 1920, 1080)),
            ..browser
        };
        assert!(evaluate_auto_hide_policy(
            &cfg,
            &geom,
            Some(&browser_f11),
            FullscreenNotificationState::Normal
        ));
    }

    #[test]
    fn fullscreen_on_second_monitor_and_whitelist_do_not_hide_primary_widget() {
        let mut cfg = Config::default();
        cfg.game_mode.whitelist_processes.push("mstsc.exe".into());
        let geom = primary_geom();

        // Game on monitor 2 (1920..3840) must NOT hide widget on monitor 1 even with QUNS_BUSY.
        let mon2_game = ForegroundSnapshot {
            class_name: "UnrealWindow".into(),
            exe_path: Some(r"D:\Games\game.exe".into()),
            frame_rect: Some(RectI::new(1920, 0, 3840, 1080)),
            style: WS_POPUP.0,
            is_visible: true,
            is_minimized: false,
            is_cloaked: false,
        };
        assert!(!evaluate_auto_hide_policy(
            &cfg,
            &geom,
            Some(&mon2_game),
            FullscreenNotificationState::Busy
        ));

        // Whitelisted RDP session overrides even D3D fullscreen.
        let rdp = ForegroundSnapshot {
            class_name: "TscShellContainerClass".into(),
            exe_path: Some(r"C:\Windows\System32\mstsc.exe".into()),
            frame_rect: Some(RectI::new(0, 0, 1920, 1080)),
            style: WS_POPUP.0,
            is_visible: true,
            is_minimized: false,
            is_cloaked: false,
        };
        assert!(!evaluate_auto_hide_policy(
            &cfg,
            &geom,
            Some(&rdp),
            FullscreenNotificationState::D3dFullScreen
        ));
    }
}
