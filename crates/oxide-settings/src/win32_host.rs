//! Win32 host integration for `oxide-settings.exe`:
//! - Single-instance named mutex + bring existing window to foreground
//! - Lazy IPC wakeup (`PostMessageW(OxideOverlay, WM_OXIDE_OPEN_IPC)`)
//! - Windows 11 DWM Mica Alt / Mica / Solid backdrop + Dark Mode + Rounded Corners
//! - Graceful DX12 failure fallback (localized dialog + Notepad `config.toml`)

use std::ffi::c_void;
use std::path::Path;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, FindWindowW, GetWindowTextW, HWND_NOTOPMOST, HWND_TOPMOST,
    IsWindowVisible, MB_ICONWARNING, MB_OK, MessageBoxW, PostMessageW, SW_RESTORE, SW_SHOWNORMAL,
    SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos, ShowWindow, WM_APP,
};
use windows::core::{BOOL, PCWSTR, w};

use crate::i18n::SettingsI18n;

/// Matches `oxide_ui::WM_OXIDE_OPEN_IPC` (`WM_APP + 11`) without depending on `oxide-ui`.
pub const WM_OXIDE_OPEN_IPC: u32 = WM_APP + 11;

const SINGLE_INSTANCE_MUTEX: PCWSTR = w!(r"Local\OxideMonitorSettingsSingleInstance");
const OVERLAY_CLASS: PCWSTR = w!("OxideOverlay");
const DWMSBT_TABBEDWINDOW: u32 = 4; // Mica Alt
const DWMSBT_MAINWINDOW: u32 = 2; // Mica

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackdropKind {
    MicaAlt,
    Mica,
    SolidFallback,
}

impl BackdropKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MicaAlt => "mica-alt",
            Self::Mica => "mica",
            Self::SolidFallback => "solid-fallback",
        }
    }

    pub fn is_translucent(self) -> bool {
        !matches!(self, Self::SolidFallback)
    }
}

/// Holds the named mutex ensuring only one `oxide-settings.exe` instance runs at a time.
pub struct SingleInstanceGuard {
    handle: HANDLE,
}

impl SingleInstanceGuard {
    /// Acquire the single-instance mutex. Returns `None` if another instance is already running
    /// (after attempting to focus its window).
    pub fn acquire_or_focus_existing() -> Option<Self> {
        let handle = unsafe { CreateMutexW(None, false, SINGLE_INSTANCE_MUTEX) }.ok()?;
        let err = unsafe { GetLastError() };
        if err == ERROR_ALREADY_EXISTS {
            unsafe {
                let _ = CloseHandle(handle);
            }
            focus_existing_settings_window();
            return None;
        }
        Some(Self { handle })
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

/// Ask a running `oxide-monitor.exe` (if any) to lazily start `\\.\pipe\oxide-ui-v1`.
pub fn notify_monitor_open_ipc() -> bool {
    let Ok(hwnd) = (unsafe { FindWindowW(OVERLAY_CLASS, PCWSTR::null()) }) else {
        return false;
    };
    if hwnd.is_invalid() {
        return false;
    }
    unsafe { PostMessageW(Some(hwnd), WM_OXIDE_OPEN_IPC, WPARAM(0), LPARAM(0)) }.is_ok()
}

/// Apply Win11 DWM Dark Mode, Rounded Corners, and Mica Alt (with Mica -> Solid fallback).
pub fn apply_win11_backdrop(cc: &eframe::CreationContext<'_>, dark: bool) -> BackdropKind {
    let Ok(wh) = cc.window_handle() else {
        return BackdropKind::SolidFallback;
    };
    let RawWindowHandle::Win32(win32) = wh.as_raw() else {
        return BackdropKind::SolidFallback;
    };
    let hwnd = HWND(win32.hwnd.get() as *mut c_void);
    apply_win11_backdrop_hwnd(hwnd, dark)
}

pub fn apply_win11_backdrop_hwnd(hwnd: HWND, dark: bool) -> BackdropKind {
    let dark_val: u32 = if dark { 1 } else { 0 };
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark_val as *const u32).cast::<c_void>(),
            std::mem::size_of::<u32>() as u32,
        )
    };

    let corner = DWMWCP_ROUND.0 as u32;
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&corner as *const u32).cast::<c_void>(),
            std::mem::size_of::<u32>() as u32,
        )
    };

    let mica_alt = DWMSBT_TABBEDWINDOW;
    if unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&mica_alt as *const u32).cast::<c_void>(),
            std::mem::size_of::<u32>() as u32,
        )
    }
    .is_ok()
    {
        return BackdropKind::MicaAlt;
    }

    let mica = DWMSBT_MAINWINDOW;
    if unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&mica as *const u32).cast::<c_void>(),
            std::mem::size_of::<u32>() as u32,
        )
    }
    .is_ok()
    {
        return BackdropKind::Mica;
    }

    BackdropKind::SolidFallback
}

pub fn update_dwm_dark_mode(hwnd: HWND, dark: bool) {
    let dark_val: u32 = if dark { 1 } else { 0 };
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark_val as *const u32).cast::<c_void>(),
            std::mem::size_of::<u32>() as u32,
        )
    };
}

/// Reliably raise the settings window to the foreground (even when launched from the tray/Flyout).
pub fn bring_window_to_front(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        );
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_NOTOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE,
        );
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Show a localized fallback message if `eframe`/`wgpu`/DX12 fails to initialize,
/// and open `config.toml` in Notepad so the user can always edit settings without blocking monitor.
pub fn handle_dx12_init_fallback(i18n: &SettingsI18n, config_path: &Path, err: &str) {
    let title = wide(&i18n.t("fallback-dx12-title"));
    let body = wide(&format!("{}\n\n({err})", i18n.t("fallback-dx12-body")));
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(body.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONWARNING,
        );
    }
    let _ = open_config_in_notepad(config_path);
}

/// Open `config.toml` in Notepad (used for the advanced "Open TOML..." button and fallback).
pub fn open_config_in_notepad(path: &Path) -> bool {
    let file = wide("notepad.exe");
    let params = wide(&format!("\"{}\"", path.display()));
    let res = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(file.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    (res.0 as isize) > 32
}

fn focus_existing_settings_window() {
    unsafe extern "system" fn enum_proc(hwnd: HWND, _lparam: LPARAM) -> BOOL {
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            return BOOL(1);
        }
        let mut buf = [0u16; 128];
        let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
        if len > 0 {
            let title = String::from_utf16_lossy(&buf[..len as usize]);
            if title.starts_with("Oxide Monitor — ") {
                bring_window_to_front(hwnd);
                return BOOL(0);
            }
        }
        BOOL(1)
    }

    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(0));
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
