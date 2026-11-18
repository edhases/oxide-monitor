//! Explicit elevated launch of `oxide-pawnio-host.exe` via `ShellExecuteExW("runas")`.
//!
//! Only the monitor calls this, and only after an explicit Start action. A declined
//! UAC prompt maps to `HostError::UacDenied`; the caller must not retry silently.

use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{GetProcessId, WaitForSingleObject};
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{PCWSTR, w};

use crate::{HELPER_EXE, HostError};

const ERROR_CANCELLED: u32 = 1223;
const SE_ERR_ACCESSDENIED: u32 = 5;

/// Resolve the helper beside the running monitor binary.
pub fn helper_path() -> Result<PathBuf, HostError> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| HostError::HelperMissing(exe.display().to_string()))?;
    let helper = dir.join(HELPER_EXE);
    if helper.is_file() {
        Ok(helper)
    } else {
        Err(HostError::HelperMissing(helper.display().to_string()))
    }
}

/// Owned process handle returned by ShellExecuteExW.
pub struct HelperProcess {
    handle: HANDLE,
    pid: u32,
}

impl HelperProcess {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Wait up to `timeout`; returns true if the process exited.
    pub fn wait(&self, timeout: Duration) -> bool {
        let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        unsafe { WaitForSingleObject(self.handle, ms) == WAIT_OBJECT_0 }
    }

    pub fn terminate(&self) {
        unsafe {
            let _ = windows::Win32::System::Threading::TerminateProcess(self.handle, 1);
        }
    }
}

impl Drop for HelperProcess {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

/// Launch the signed-as-elevated helper. This is the only function in the monitor
/// that can trigger a UAC prompt.
pub fn launch_helper() -> Result<HelperProcess, HostError> {
    let path = helper_path()?;
    let sid = oxide_ipc::current_user_sid()?;
    launch_helper_with_sid(&path, &sid)
}

/// Launch the helper for an explicit invoking-user SID. Used by the monitor so a
/// UAC "over-the-shoulder" elevation (helper runs as the admin account) still binds
/// a pipe/DACL for the original user.
pub fn launch_helper_with_sid(
    path: &std::path::Path,
    sid: &str,
) -> Result<HelperProcess, HostError> {
    oxide_ipc::validate_sid(sid).map_err(|err| HostError::Launch(err.to_string()))?;
    let wide_path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let wide_dir: Vec<u16> = path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let params = format!("--sid {sid}");
    let wide_params: Vec<u16> = params.encode_utf16().chain(std::iter::once(0)).collect();

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(wide_path.as_ptr()),
        lpParameters: PCWSTR(wide_params.as_ptr()),
        lpDirectory: PCWSTR(wide_dir.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    // SAFETY: the struct and both strings are valid for the duration of the call.
    if let Err(err) = unsafe { ShellExecuteExW(&mut info) } {
        let last = unsafe { GetLastError() }.0;
        let code = if last != 0 {
            last
        } else {
            (err.code().0 as u32) & 0xFFFF
        };
        return Err(classify_launch_error(code, &err.to_string()));
    }
    if info.hProcess.is_invalid() {
        return Err(HostError::Launch(
            "ShellExecuteExW succeeded but returned no process handle".into(),
        ));
    }
    let pid = unsafe { GetProcessId(info.hProcess) };
    Ok(HelperProcess {
        handle: info.hProcess,
        pid,
    })
}

fn classify_launch_error(code: u32, detail: &str) -> HostError {
    if code == ERROR_CANCELLED || code == SE_ERR_ACCESSDENIED {
        HostError::UacDenied
    } else {
        HostError::Launch(format!("ShellExecuteExW failed ({code}): {detail}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uac_decline_is_classified_as_denied_not_retryable() {
        assert!(matches!(
            classify_launch_error(ERROR_CANCELLED, ""),
            HostError::UacDenied
        ));
        assert!(matches!(
            classify_launch_error(SE_ERR_ACCESSDENIED, ""),
            HostError::UacDenied
        ));
        assert!(matches!(
            classify_launch_error(2, "not found"),
            HostError::Launch(_)
        ));
    }
}
