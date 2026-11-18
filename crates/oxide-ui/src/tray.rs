//! Notification-area icon (with v4 ordering, diagnostics, and bounded backoff retry)
//! and localized context menu with Runtime Presets submenu.

use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use oxide_core::{Config, Localizer, builtin_presets};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, POINT, WPARAM,
};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY, TokenIntegrityLevel,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcess, OpenProcessToken, PROCESS_CREATION_FLAGS,
    PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, FindWindowW, GetCursorPos, HICON, IDI_APPLICATION,
    LoadIconW, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, PostMessageW,
    SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP,
    WM_NULL,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::UiError;

pub const CALLBACK_MESSAGE: u32 = WM_APP + 2;
pub const ICON_ID: u32 = 1;

pub const MENU_TOGGLE: u32 = 1;
pub const MENU_CONFIG: u32 = 2;
pub const MENU_EXIT: u32 = 3;
pub const MENU_FLYOUT: u32 = 4;
pub const MENU_GAME_MODE: u32 = 5;
pub const MENU_LOCK_POS: u32 = 6;
pub const MENU_TASK_MGR: u32 = 7;
pub const MENU_SETTINGS: u32 = 8;
pub const MENU_PRESET_BASE: u32 = 100;

const TOOLTIP_KEY: &str = "tray-tooltip";
const TOOLTIP_GAMEMODE_KEY: &str = "tray-tooltip-gamemode";
const RETRY_DELAYS_SECS: [u64; 5] = [2, 4, 8, 16, 30];

pub struct MenuState<'a> {
    pub shown: bool,
    pub game_mode: bool,
    pub lock_position: bool,
    pub active_preset: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsLaunchOutcome {
    SettingsExe,
    NotepadFallback,
}

/// Bounded exponential retry state for initial `Shell_NotifyIconW(NIM_ADD)` failures
/// (e.g. if Explorer is momentarily restarting or busy).
#[derive(Debug, Default)]
pub struct TrayRetryState {
    attempts: usize,
    next_retry: Option<Instant>,
    exhausted_logged: bool,
}

impl TrayRetryState {
    pub fn reset(&mut self) {
        self.attempts = 0;
        self.next_retry = None;
        self.exhausted_logged = false;
    }

    pub fn schedule_next(&mut self) {
        if self.attempts < RETRY_DELAYS_SECS.len() {
            let delay = Duration::from_secs(RETRY_DELAYS_SECS[self.attempts]);
            self.attempts += 1;
            self.next_retry = Some(Instant::now() + delay);
        } else {
            self.next_retry = None;
        }
    }

    pub fn should_retry_now(&mut self) -> bool {
        let Some(when) = self.next_retry else {
            if self.attempts >= RETRY_DELAYS_SECS.len() && !self.exhausted_logged {
                self.exhausted_logged = true;
                tracing::warn!(
                    attempts = self.attempts,
                    "giving up tray icon retry until TaskbarCreated"
                );
            }
            return false;
        };
        if Instant::now() < when {
            return false;
        }
        // Only spend a retry attempt when Shell_TrayWnd exists.
        let taskbar = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) };
        if taskbar.is_err() {
            return false;
        }
        self.next_retry = None;
        true
    }

    pub fn attempts(&self) -> usize {
        self.attempts
    }
}

/// Register the notification-area icon cleanly:
/// 1. Optional best-effort `NIM_DELETE` on retry to clear stale registrations.
/// 2. `NIM_ADD` with v0-safe flags (`NIF_MESSAGE | NIF_ICON | NIF_TIP`, **no** `NIF_SHOWTIP`).
/// 3. `NIM_SETVERSION(NOTIFYICON_VERSION_4)`.
/// 4. `NIM_MODIFY` with `NIF_TIP | NIF_SHOWTIP` so standard tooltips work under v4.
pub fn add(hwnd: HWND, localizer: &Localizer) -> Result<(), UiError> {
    add_inner(hwnd, localizer, false)
}

pub fn add_retry(hwnd: HWND, localizer: &Localizer) -> Result<(), UiError> {
    add_inner(hwnd, localizer, true)
}

fn add_inner(hwnd: HWND, localizer: &Localizer, cleanup_first: bool) -> Result<(), UiError> {
    if cleanup_first {
        remove(hwnd);
    }

    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        // IMPORTANT: Do NOT include NIF_SHOWTIP in initial NIM_ADD before NIM_SETVERSION(v4);
        // Windows 11 Explorer can reject NIM_ADD when v4-only flags are set on a v0 add.
        uFlags: NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: CALLBACK_MESSAGE,
        ..Default::default()
    };

    match app_icon() {
        Ok(icon) if !icon.is_invalid() => {
            data.hIcon = icon;
            data.uFlags |= NIF_ICON;
        }
        Ok(_) => tracing::warn!("application icon is invalid"),
        Err(err) => tracing::warn!("LoadIconW failed: {err}"),
    }

    set_tip(&mut data, &localizer.text(TOOLTIP_KEY));

    if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
        let last_err = unsafe { GetLastError() }.0;
        let session_id = current_session_id();
        let integrity = current_integrity_label();
        let has_tray_wnd = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.is_ok();
        return Err(UiError::Win32(format!(
            "Shell_NotifyIconW(NIM_ADD) failed (last_err={last_err}, session={session_id:?}, integrity={integrity}, has_shell_tray={has_tray_wnd})"
        )));
    }

    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    if !unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) }.as_bool() {
        let last_err = unsafe { GetLastError() }.0;
        tracing::warn!(last_err, "Shell_NotifyIconW(NIM_SETVERSION) failed");
    } else {
        // Now that v4 is active, enable standard tooltip display via NIF_SHOWTIP.
        let mut tip_data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: ICON_ID,
            uFlags: NIF_TIP | NIF_SHOWTIP,
            ..Default::default()
        };
        set_tip(&mut tip_data, &localizer.text(TOOLTIP_KEY));
        if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &tip_data) }.as_bool() {
            let last_err = unsafe { GetLastError() }.0;
            tracing::debug!(last_err, "Shell_NotifyIconW(NIM_MODIFY/NIF_SHOWTIP) failed");
        }
    }

    Ok(())
}

pub fn update_tooltip(hwnd: HWND, game_mode: bool, localizer: &Localizer) {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        uFlags: NIF_TIP | NIF_SHOWTIP,
        ..Default::default()
    };
    let key = if game_mode {
        TOOLTIP_GAMEMODE_KEY
    } else {
        TOOLTIP_KEY
    };
    set_tip(&mut data, &localizer.text(key));
    let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}

pub fn remove(hwnd: HWND) {
    let data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    };
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
}

/// Tell the user where the widget went after an explicit hide.
pub fn notify_hidden(hwnd: HWND, localizer: &Localizer) {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        uFlags: NIF_INFO,
        dwInfoFlags: NIIF_INFO,
        ..Default::default()
    };
    set_chars(
        &mut data.szInfoTitle,
        &localizer.text("hidden-notice-title"),
    );
    set_chars(&mut data.szInfo, &localizer.text("hidden-notice"));
    if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
        tracing::debug!("Shell_NotifyIconW(NIM_MODIFY/NIF_INFO) failed");
    }
}

/// Resolve a menu command id back to a built-in preset ID, if it belongs to the Presets submenu.
pub fn preset_id_from_command(command: u32) -> Option<String> {
    if command < MENU_PRESET_BASE {
        return None;
    }
    let idx = (command - MENU_PRESET_BASE) as usize;
    builtin_presets().get(idx).map(|p| p.preset.id.clone())
}

/// Show the localized context menu and return the selected command id (0 if cancelled).
pub fn show_menu(hwnd: HWND, state: &MenuState<'_>, localizer: &Localizer) -> u32 {
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        tracing::warn!("CreatePopupMenu failed");
        return 0;
    };

    append_item(
        menu,
        MENU_TOGGLE,
        &localizer.text(if state.shown {
            "menu-hide"
        } else {
            "menu-show"
        }),
        Some(state.shown),
    );
    append_item(menu, MENU_FLYOUT, &localizer.text("menu-open-flyout"), None);

    // Localized Runtime Presets submenu (5 built-in presets).
    if let Ok(presets_menu) = unsafe { CreatePopupMenu() } {
        for (idx, preset) in builtin_presets().iter().enumerate() {
            let key = format!("preset-{}", preset.preset.id);
            let translated = localizer.text(&key);
            let label = if translated == key {
                preset.preset.name.clone()
            } else {
                translated
            };
            let is_active = state.active_preset.eq_ignore_ascii_case(&preset.preset.id);
            append_item(
                presets_menu,
                MENU_PRESET_BASE + idx as u32,
                &label,
                Some(is_active),
            );
        }
        let submenu_label = wide(&localizer.text("menu-presets"));
        let _ = unsafe {
            AppendMenuW(
                menu,
                MF_POPUP,
                presets_menu.0 as usize,
                PCWSTR(submenu_label.as_ptr()),
            )
        };
    }

    let _ = unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) };

    append_item(
        menu,
        MENU_GAME_MODE,
        &localizer.text("menu-game-mode"),
        Some(state.game_mode),
    );
    append_item(
        menu,
        MENU_LOCK_POS,
        &localizer.text("menu-lock-position"),
        Some(state.lock_position),
    );
    let _ = unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) };

    append_item(
        menu,
        MENU_TASK_MGR,
        &localizer.text("menu-open-taskmgr"),
        None,
    );
    append_item(menu, MENU_SETTINGS, &localizer.text("menu-settings"), None);
    append_item(menu, MENU_CONFIG, &localizer.text("menu-open-config"), None);
    let _ = unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) };
    append_item(menu, MENU_EXIT, &localizer.text("menu-exit"), None);

    let mut point = POINT::default();
    let _ = unsafe { GetCursorPos(&mut point) };
    let _ = unsafe { SetForegroundWindow(hwnd) };

    let command = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            point.x,
            point.y,
            None,
            hwnd,
            None,
        )
    }
    .0 as u32;

    let _ = unsafe { DestroyMenu(menu) };
    let _ = unsafe { PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0)) };

    command
}

fn append_item(
    menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
    id: u32,
    text: &str,
    checked: Option<bool>,
) {
    let label = wide(text);
    let flags = match checked {
        Some(true) => MF_STRING | MF_CHECKED,
        Some(false) => MF_STRING | MF_UNCHECKED,
        None => MF_STRING,
    };
    let _ = unsafe { AppendMenuW(menu, flags, id as usize, PCWSTR(label.as_ptr())) };
}

/// Locate sibling `oxide-settings.exe` next to the running `oxide-monitor.exe`.
pub fn sibling_settings_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join("oxide-settings.exe");
    candidate.is_file().then_some(candidate)
}

/// Launch sibling `oxide-settings.exe` directly via `CreateProcessW`; fall back to
/// opening `config.toml` in Notepad when `oxide-settings.exe` is not present.
pub fn open_settings_or_config(hwnd: HWND) -> Result<SettingsLaunchOutcome, UiError> {
    if let Some(settings_exe) = sibling_settings_exe() {
        let exe_str = settings_exe.to_string_lossy();
        let mut cmd_w = wide(&format!("\"{exe_str}\" --ipc"));
        let app_w = wide(&exe_str);
        let si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        match unsafe {
            CreateProcessW(
                PCWSTR(app_w.as_ptr()),
                Some(PWSTR(cmd_w.as_mut_ptr())),
                None,
                None,
                false,
                PROCESS_CREATION_FLAGS(0),
                None,
                PCWSTR::null(),
                &si,
                &mut pi,
            )
        } {
            Ok(()) => {
                unsafe {
                    let _ = CloseHandle(pi.hThread);
                    let _ = CloseHandle(pi.hProcess);
                }
                return Ok(SettingsLaunchOutcome::SettingsExe);
            }
            Err(err) => {
                tracing::warn!(%err, path = %exe_str, "CreateProcessW(oxide-settings.exe) failed; falling back to notepad");
            }
        }
    }
    open_config(hwnd)?;
    Ok(SettingsLaunchOutcome::NotepadFallback)
}

pub fn open_config(hwnd: HWND) -> Result<(), UiError> {
    let path = Config::resolve_path();
    let file = wide("notepad.exe");
    let parameters = wide(&format!("\"{}\"", path.display()));
    let result = unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            Some(hwnd),
            w!("open"),
            PCWSTR(file.as_ptr()),
            PCWSTR(parameters.as_ptr()),
            PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        )
    };
    if (result.0 as isize) <= 32 {
        return Err(UiError::Other("ShellExecuteW(notepad.exe) failed".into()));
    }
    Ok(())
}

/// Load the icon embedded in the executable (`ID 1`), falling back to the stock icon.
pub fn app_icon() -> windows::core::Result<HICON> {
    let module = unsafe { GetModuleHandleW(PCWSTR::null()) }?;
    let embedded = unsafe { LoadIconW(Some(HINSTANCE(module.0)), PCWSTR(1 as *const u16)) };
    match embedded {
        Ok(icon) if !icon.is_invalid() => Ok(icon),
        _ => unsafe { LoadIconW(None, IDI_APPLICATION) },
    }
}

fn current_session_id() -> Option<u32> {
    unsafe extern "system" {
        fn ProcessIdToSessionId(dw_process_id: u32, p_session_id: *mut u32) -> i32;
    }
    let mut sid = 0u32;
    let ok = unsafe { ProcessIdToSessionId(std::process::id(), &mut sid) };
    (ok != 0).then_some(sid)
}

fn current_integrity_label() -> &'static str {
    let mut token = HANDLE::default();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return "unknown";
    }
    let mut needed = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut needed) };
    if needed == 0 {
        unsafe {
            let _ = CloseHandle(token);
        }
        return "unknown";
    }
    let mut buf = vec![0u8; needed as usize];
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            needed,
            &mut needed,
        )
    };
    unsafe {
        let _ = CloseHandle(token);
    }
    if ok.is_err() {
        return "unknown";
    }
    let label = unsafe { &*(buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()) };
    let sid = label.Label.Sid;
    let count_ptr = unsafe { GetSidSubAuthorityCount(sid) };
    if count_ptr.is_null() {
        return "unknown";
    }
    let count = unsafe { *count_ptr } as u32;
    if count == 0 {
        return "unknown";
    }
    let rid_ptr = unsafe { GetSidSubAuthority(sid, count - 1) };
    if rid_ptr.is_null() {
        return "unknown";
    }
    let rid = unsafe { *rid_ptr };
    match rid {
        0x0000..=0x0FFF => "Untrusted",
        0x1000..=0x1FFF => "Low",
        0x2000..=0x2FFF => "Medium",
        0x3000..=0x3FFF => "High",
        _ => "System",
    }
}

fn set_tip(data: &mut NOTIFYICONDATAW, text: &str) {
    set_chars(&mut data.szTip, text);
}

fn set_chars<const N: usize>(destination: &mut [u16; N], text: &str) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let count = wide.len().min(N.saturating_sub(1));
    destination[..count].copy_from_slice(&wide[..count]);
    destination[count] = 0;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_submenu_command_ids_map_all_five_presets() {
        assert_eq!(preset_id_from_command(0), None);
        assert_eq!(preset_id_from_command(MENU_CONFIG), None);
        let presets = builtin_presets();
        assert_eq!(presets.len(), 5);
        for (i, p) in presets.iter().enumerate() {
            assert_eq!(
                preset_id_from_command(MENU_PRESET_BASE + i as u32).as_deref(),
                Some(p.preset.id.as_str())
            );
        }
        assert_eq!(preset_id_from_command(MENU_PRESET_BASE + 99), None);
    }

    #[test]
    fn tray_retry_state_is_bounded_and_resets() {
        let mut r = TrayRetryState::default();
        for i in 1..=RETRY_DELAYS_SECS.len() {
            r.schedule_next();
            assert_eq!(r.attempts(), i);
        }
        r.schedule_next();
        assert_eq!(r.attempts(), RETRY_DELAYS_SECS.len());
        assert!(!r.should_retry_now());
        r.reset();
        assert_eq!(r.attempts(), 0);
    }
}
