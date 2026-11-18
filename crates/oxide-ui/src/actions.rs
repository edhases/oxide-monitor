//! Execution of configurable mouse click actions (`ClickAction`).

use std::ptr::copy_nonoverlapping;

use oxide_core::{ClickAction, Config, Snapshot, format_speed, format_temp};
use tracing::warn;
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, PWSTR, w};

const CF_UNICODETEXT: u32 = 13;

/// High-level outcome telling `UiState` whether the action needs internal UI handling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionEffect {
    Handled,
    ToggleFlyout,
    ToggleGameMode,
    CyclePreset,
    OpenSettings,
}

pub fn execute_click_action(
    hwnd: HWND,
    action: &ClickAction,
    config: &Config,
    snapshot: &Snapshot,
) -> ActionEffect {
    match action {
        ClickAction::None => ActionEffect::Handled,
        ClickAction::ToggleFlyout => ActionEffect::ToggleFlyout,
        ClickAction::ToggleGameMode => ActionEffect::ToggleGameMode,
        ClickAction::CyclePreset => ActionEffect::CyclePreset,
        ClickAction::OpenSettings => ActionEffect::OpenSettings,
        ClickAction::OpenTaskManager => {
            shell_open(hwnd, "taskmgr.exe", None);
            ActionEffect::Handled
        }
        ClickAction::OpenResourceMonitor => {
            shell_open(hwnd, "perfmon.exe", Some("/res"));
            ActionEffect::Handled
        }
        ClickAction::OpenNetworkStats => {
            shell_open(hwnd, "ms-settings:network-status", None);
            ActionEffect::Handled
        }
        ClickAction::CopySummary => {
            let summary = build_summary_text(config, snapshot);
            copy_text_to_clipboard(hwnd, &summary);
            ActionEffect::Handled
        }
        ClickAction::RunCommand { path, args } => {
            run_direct_process(path, args);
            ActionEffect::Handled
        }
    }
}

pub fn shell_open(hwnd: HWND, file: &str, params: Option<&str>) {
    let file_w = wide(file);
    let params_w = params.map(wide);
    let res = unsafe {
        ShellExecuteW(
            Some(hwnd),
            w!("open"),
            PCWSTR(file_w.as_ptr()),
            params_w
                .as_ref()
                .map(|v| PCWSTR(v.as_ptr()))
                .unwrap_or(PCWSTR::null()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (res.0 as isize) <= 32 {
        warn!(file, "ShellExecuteW failed with code {}", res.0 as isize);
    }
}

/// Launch a user-configured executable directly via `CreateProcessW` (no `cmd.exe` shell).
fn run_direct_process(path: &str, args: &[String]) {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return;
    }
    let mut cmdline = format!("\"{trimmed}\"");
    for arg in args {
        cmdline.push(' ');
        if arg.contains(' ') || arg.contains('\t') {
            cmdline.push('"');
            cmdline.push_str(&arg.replace('"', "\\\""));
            cmdline.push('"');
        } else {
            cmdline.push_str(arg);
        }
    }
    let mut cmd_w = wide(&cmdline);
    let app_w = wide(trimmed);
    let si = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();
    let res = unsafe {
        CreateProcessW(
            PCWSTR(app_w.as_ptr()),
            Some(PWSTR(cmd_w.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        )
    };
    match res {
        Ok(()) => unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(pi.hThread);
            let _ = windows::Win32::Foundation::CloseHandle(pi.hProcess);
        },
        Err(err) => warn!(path = trimmed, %err, "CreateProcessW failed"),
    }
}

fn build_summary_text(config: &Config, snap: &Snapshot) -> String {
    let gpu = snap.primary_gpu();
    let gpu_pct = gpu
        .usage_pct
        .map(|v| format!("{v:.0}%"))
        .unwrap_or_else(|| "—".into());
    format!(
        "Oxide Monitor | ↑ {} ↓ {} | CPU {:.0}% ({}) | RAM {:.0}% | GPU {} ({}) | Disk {}",
        format_speed(snap.network.up_bps, &config.format),
        format_speed(snap.network.down_bps, &config.format),
        snap.cpu.usage_pct.clamp(0.0, 100.0),
        format_temp(snap.cpu.temp_c),
        snap.memory.usage_pct.clamp(0.0, 100.0),
        gpu_pct,
        format_temp(gpu.temp_c),
        format_temp(snap.disk.temp_c),
    )
}

fn copy_text_to_clipboard(hwnd: HWND, text: &str) {
    let utf16 = wide(text);
    let byte_len = utf16.len() * std::mem::size_of::<u16>();
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, byte_len) {
            let dst = GlobalLock(hmem);
            if !dst.is_null() {
                copy_nonoverlapping(utf16.as_ptr().cast::<u8>(), dst.cast::<u8>(), byte_len);
                let _ = GlobalUnlock(hmem);
                let _ = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(hmem.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
