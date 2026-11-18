//! Win32 Named Pipe security: current-user DACL and peer executable image verification.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{PCWSTR, PWSTR};

use crate::IpcError;

/// Policy used to validate the peer executable on the other end of a Named Pipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerPolicy {
    /// Peer must reside in the same directory as the current executable and have the
    /// exact filename (case-insensitive), e.g. `oxide-settings.exe` or `oxide-monitor.exe`.
    SiblingExe { expected_name: String },
    /// Peer must be the current process itself (used for deterministic in-process tests).
    SameProcessOnly,
}

impl PeerPolicy {
    pub fn sibling_exe(expected_name: impl Into<String>) -> Self {
        Self::SiblingExe {
            expected_name: expected_name.into(),
        }
    }
}

/// Owned Windows `SECURITY_DESCRIPTOR` granting `GENERIC_ALL` exclusively to the current user SID.
pub struct CurrentUserDacl {
    sd: PSECURITY_DESCRIPTOR,
}

impl CurrentUserDacl {
    pub fn build() -> Result<Self, IpcError> {
        let sid_str = current_user_sid_string()?;
        // `P` (Protected) blocks inherited ACEs; only the current user SID receives GENERIC_ALL.
        let sddl = format!("D:P(A;;GA;;;{sid_str})");
        Self::from_sddl(&sddl)
    }

    /// DACL for a pipe created by an elevated helper that must be reachable by the
    /// same user's medium-integrity monitor. The mandatory label is set to Medium
    /// with no-write-up (`NW`), so low-integrity processes still cannot write while
    /// the normal monitor can use the pipe.
    pub fn build_for_medium_integrity() -> Result<Self, IpcError> {
        let sid_str = current_user_sid_string()?;
        Self::build_for_medium_integrity_for_sid(&sid_str)
    }

    /// Same as [`Self::build_for_medium_integrity`] but for an explicitly supplied
    /// invoking-user SID (needed when UAC elevates a different admin account).
    pub fn build_for_medium_integrity_for_sid(sid: &str) -> Result<Self, IpcError> {
        validate_sid(sid)?;
        // The elevated helper may run as a different admin account (UAC
        // over-the-shoulder). Grant that token access too so the server side can
        // always service its own pipe; the client still needs the invoking SID.
        let helper_sid = current_user_sid_string().ok();
        let sddl = match helper_sid.as_deref() {
            Some(helper) if helper != sid => {
                format!("D:P(A;;GA;;;{sid})(A;;GA;;;{helper})S:(ML;;NW;;;ME)")
            }
            _ => format!("D:P(A;;GA;;;{sid})S:(ML;;NW;;;ME)"),
        };
        Self::from_sddl(&sddl)
    }

    fn from_sddl(sddl: &str) -> Result<Self, IpcError> {
        let sddl_w = to_wide(sddl);
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl_w.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
            .map_err(|err| {
                IpcError::win32("ConvertStringSecurityDescriptorToSecurityDescriptorW", err)
            })?;
        }
        if sd.0.is_null() {
            return Err(IpcError::Security(
                "ConvertStringSecurityDescriptorToSecurityDescriptorW returned null descriptor"
                    .into(),
            ));
        }
        Ok(Self { sd })
    }

    pub fn security_attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.sd.0,
            bInheritHandle: false.into(),
        }
    }
}

impl Drop for CurrentUserDacl {
    fn drop(&mut self) {
        if !self.sd.0.is_null() {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.sd.0)));
            }
            self.sd = PSECURITY_DESCRIPTOR::default();
        }
    }
}

/// Verify the connected Named Pipe client process against `policy`.
pub fn verify_client_peer(pipe: HANDLE, policy: &PeerPolicy) -> Result<u32, IpcError> {
    let mut pid = 0u32;
    unsafe {
        GetNamedPipeClientProcessId(pipe, &mut pid)
            .map_err(|err| IpcError::win32("GetNamedPipeClientProcessId", err))?;
    }
    verify_peer_pid(pid, policy)?;
    Ok(pid)
}

/// Verify the Named Pipe server process against `policy`.
pub fn verify_server_peer(pipe: HANDLE, policy: &PeerPolicy) -> Result<u32, IpcError> {
    let mut pid = 0u32;
    unsafe {
        GetNamedPipeServerProcessId(pipe, &mut pid)
            .map_err(|err| IpcError::win32("GetNamedPipeServerProcessId", err))?;
    }
    verify_peer_pid(pid, policy)?;
    Ok(pid)
}

fn verify_peer_pid(peer_pid: u32, policy: &PeerPolicy) -> Result<(), IpcError> {
    if peer_pid == 0 {
        return Err(IpcError::PeerRejected("peer PID is 0".into()));
    }
    match policy {
        PeerPolicy::SameProcessOnly => {
            let own = std::process::id();
            if peer_pid != own {
                return Err(IpcError::PeerRejected(format!(
                    "expected same-process PID {own}, got {peer_pid}"
                )));
            }
            Ok(())
        }
        PeerPolicy::SiblingExe { expected_name } => {
            let peer_path = query_process_image_path(peer_pid)?;
            let own_path = std::env::current_exe().map_err(|err| {
                IpcError::PeerRejected(format!("failed to resolve current_exe: {err}"))
            })?;
            verify_sibling_paths(&own_path, &peer_path, expected_name)
        }
    }
}

pub(crate) fn verify_sibling_paths(
    own_path: &Path,
    peer_path: &Path,
    expected_name: &str,
) -> Result<(), IpcError> {
    let peer_name = peer_path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| IpcError::PeerRejected("peer path has no filename".into()))?;
    if !peer_name.eq_ignore_ascii_case(expected_name) {
        return Err(IpcError::PeerRejected(format!(
            "unexpected peer executable '{peer_name}', expected '{expected_name}'"
        )));
    }
    let own_dir = normalize_dir(own_path.parent());
    let peer_dir = normalize_dir(peer_path.parent());
    if own_dir.is_empty() || !own_dir.eq_ignore_ascii_case(&peer_dir) {
        return Err(IpcError::PeerRejected(format!(
            "peer directory '{peer_dir}' does not match own directory '{own_dir}'"
        )));
    }
    Ok(())
}

fn normalize_dir(dir: Option<&Path>) -> String {
    let Some(p) = dir else {
        return String::new();
    };
    let s = p.to_string_lossy();
    let stripped = s.strip_prefix(r"\\?\").unwrap_or(&s);
    stripped.trim_end_matches(['\\', '/']).to_string()
}

fn query_process_image_path(pid: u32) -> Result<PathBuf, IpcError> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|err| IpcError::PeerRejected(format!("OpenProcess({pid}) failed: {err}")))?;
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    let res = unsafe {
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
    res.map_err(|err| {
        IpcError::PeerRejected(format!("QueryFullProcessImageNameW({pid}) failed: {err}"))
    })?;
    Ok(PathBuf::from(String::from_utf16_lossy(
        &buf[..len as usize],
    )))
}

/// Current interactive user's SID as `S-1-5-...`, used inter alia to derive
/// per-user Named Pipe names for optional helpers.
pub fn current_user_sid() -> Result<String, IpcError> {
    current_user_sid_string()
}

/// Strict SID validation for values that will be interpolated into an SDDL string.
/// Only `S-` followed by decimal components and hyphens is accepted.
pub fn validate_sid(sid: &str) -> Result<(), IpcError> {
    let valid = sid.len() >= 5
        && sid.len() <= 128
        && sid.starts_with("S-")
        && sid[2..]
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    if valid {
        Ok(())
    } else {
        Err(IpcError::Security(format!("invalid SID: {sid:?}")))
    }
}

fn current_user_sid_string() -> Result<String, IpcError> {
    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|err| IpcError::win32("OpenProcessToken", err))?;
    }

    let mut needed = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
    if needed == 0 {
        unsafe {
            let _ = CloseHandle(token);
        }
        return Err(IpcError::Security(
            "GetTokenInformation(TokenUser) returned 0 size".into(),
        ));
    }

    let mut buffer = vec![0u8; needed as usize];
    let info_res = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            needed,
            &mut needed,
        )
    };
    unsafe {
        let _ = CloseHandle(token);
    }
    info_res.map_err(|err| IpcError::win32("GetTokenInformation(TokenUser)", err))?;

    let token_user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
    let mut sid_pwstr = PWSTR::null();
    unsafe {
        ConvertSidToStringSidW(token_user.User.Sid, &mut sid_pwstr)
            .map_err(|err| IpcError::win32("ConvertSidToStringSidW", err))?;
    }
    if sid_pwstr.is_null() {
        return Err(IpcError::Security(
            "ConvertSidToStringSidW returned null".into(),
        ));
    }

    let sid = unsafe {
        let mut len = 0usize;
        while *sid_pwstr.0.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(sid_pwstr.0, len);
        let text = String::from_utf16_lossy(slice);
        let _ = LocalFree(Some(HLOCAL(sid_pwstr.0.cast::<c_void>())));
        text
    };
    Ok(sid)
}

pub(crate) fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_user_dacl_builds_and_drops_cleanly() {
        let dacl = CurrentUserDacl::build().unwrap();
        let sa = dacl.security_attributes();
        assert!(!sa.lpSecurityDescriptor.is_null());
    }

    #[test]
    fn medium_integrity_dacl_includes_a_label_ace() {
        let dacl = CurrentUserDacl::build_for_medium_integrity().unwrap();
        let sa = dacl.security_attributes();
        assert!(!sa.lpSecurityDescriptor.is_null());
    }

    #[test]
    fn sid_validation_rejects_sddl_injection() {
        assert!(validate_sid("S-1-5-21-1000").is_ok());
        assert!(validate_sid("S-1-5-21-1000)(A;;GA;;;WD").is_err());
        assert!(validate_sid("S-1-5-21-abc").is_err());
        assert!(validate_sid("Everyone").is_err());
        assert!(validate_sid("S-1-").is_err());
    }

    #[test]
    fn sibling_exe_policy_accepts_same_dir_and_rejects_foreign() {
        let monitor = Path::new(r"C:\Apps\OxideMonitor\oxide-monitor.exe");
        let settings = Path::new(r"\\?\C:\Apps\OxideMonitor\Oxide-Settings.exe");
        let foreign = Path::new(r"C:\Temp\oxide-settings.exe");
        let wrong_name = Path::new(r"C:\Apps\OxideMonitor\evil.exe");

        assert!(verify_sibling_paths(monitor, settings, "oxide-settings.exe").is_ok());
        assert!(verify_sibling_paths(monitor, foreign, "oxide-settings.exe").is_err());
        assert!(verify_sibling_paths(monitor, wrong_name, "oxide-settings.exe").is_err());
    }
}
