//! Minimal CNG SHA-256 and WinVerifyTrust checks for the elevated helper.
//!
//! Keeping these in the helper avoids linking the network/zip provider stack into
//! an elevated process. Only the installed `PawnIOLib.dll` and the embedded module
//! blob are verified; nothing is downloaded or executed.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::{HANDLE, HWND, LUID};
use windows::Win32::Security::Cryptography::{
    BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE, BCRYPT_OBJECT_LENGTH,
    BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM, BCryptCloseAlgorithmProvider,
    BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash, BCryptGetProperty, BCryptHashData,
    BCryptOpenAlgorithmProvider,
};
use windows::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
    WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_SAFER_FLAG, WTD_STATEACTION_CLOSE,
    WTD_STATEACTION_VERIFY, WTD_UI_NONE, WinVerifyTrust,
};
use windows::Win32::Security::{
    AdjustTokenPrivileges, GetTokenInformation, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION, TOKEN_PRIVILEGES, TOKEN_QUERY, TokenElevation,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{PCWSTR, w};

use crate::HostError;

pub const SHA256_LEN: usize = 32;

pub fn sha256(data: &[u8]) -> Result<[u8; SHA256_LEN], HostError> {
    unsafe {
        let mut alg = BCRYPT_ALG_HANDLE::default();
        BCryptOpenAlgorithmProvider(
            &mut alg,
            BCRYPT_SHA256_ALGORITHM,
            PCWSTR::null(),
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        )
        .ok()
        .map_err(|err| HostError::Crypto(format!("BCryptOpenAlgorithmProvider: {err}")))?;

        let result = (|| -> Result<[u8; SHA256_LEN], HostError> {
            let mut object_len = 0u32;
            let mut written = 0u32;
            BCryptGetProperty(
                alg.into(),
                BCRYPT_OBJECT_LENGTH,
                Some(std::slice::from_raw_parts_mut(
                    (&mut object_len as *mut u32).cast::<u8>(),
                    std::mem::size_of::<u32>(),
                )),
                &mut written,
                0,
            )
            .ok()
            .map_err(|err| HostError::Crypto(format!("BCryptGetProperty: {err}")))?;

            let mut object = vec![0u8; object_len as usize];
            let mut hash = BCRYPT_HASH_HANDLE::default();
            BCryptCreateHash(alg, &mut hash, Some(&mut object), None, 0)
                .ok()
                .map_err(|err| HostError::Crypto(format!("BCryptCreateHash: {err}")))?;

            let hashed = (|| -> Result<[u8; SHA256_LEN], HostError> {
                BCryptHashData(hash, data, 0)
                    .ok()
                    .map_err(|err| HostError::Crypto(format!("BCryptHashData: {err}")))?;
                let mut digest = [0u8; SHA256_LEN];
                BCryptFinishHash(hash, &mut digest, 0)
                    .ok()
                    .map_err(|err| HostError::Crypto(format!("BCryptFinishHash: {err}")))?;
                Ok(digest)
            })();

            let _ = BCryptDestroyHash(hash);
            hashed
        })();

        let _ = BCryptCloseAlgorithmProvider(alg, 0);
        result
    }
}

pub fn sha256_file(path: &Path) -> Result<[u8; SHA256_LEN], HostError> {
    let bytes = std::fs::read(path)?;
    sha256(&bytes)
}

/// Verify the file's Authenticode signature and trust chain. Returns an error for
/// unsigned or untrusted files; the official LHM style "unsigned but hash-pinned"
/// policy does not apply here because PawnIOLib is officially signed.
pub fn verify_authenticode(path: &Path) -> Result<(), HostError> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wide.as_ptr()),
        hFile: HANDLE::default(),
        pgKnownSubject: std::ptr::null_mut(),
    };

    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &mut file_info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_SAFER_FLAG,
        ..Default::default()
    };

    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = unsafe {
        WinVerifyTrust(
            HWND::default(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        )
    };

    // Always release WinVerifyTrust's state data, even on failure.
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    let _ = unsafe {
        WinVerifyTrust(
            HWND::default(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        )
    };

    if status == 0 {
        Ok(())
    } else {
        Err(HostError::Crypto(format!(
            "WinVerifyTrust rejected {} ({status:#010X})",
            path.display()
        )))
    }
}

/// Check that the current process token is elevated (the manifest normally
/// guarantees this, but a direct CreateProcess can bypass ShellExecute's prompt).
pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
        .is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// Enable `SeSecurityPrivilege` so `CreateNamedPipeW` can attach the Medium
/// mandatory label SACL. Admin tokens carry the privilege disabled by default.
pub fn enable_security_privilege() -> Result<(), HostError> {
    use windows::Win32::Foundation::{ERROR_NOT_ALL_ASSIGNED, ERROR_SUCCESS};
    use windows::Win32::Security::LookupPrivilegeValueW;

    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .map_err(|err| HostError::Crypto(format!("OpenProcessToken: {err}")))?;

        let result = (|| -> Result<(), HostError> {
            let mut luid = LUID::default();
            LookupPrivilegeValueW(PCWSTR::null(), w!("SeSecurityPrivilege"), &mut luid)
                .map_err(|err| HostError::Crypto(format!("LookupPrivilegeValueW: {err}")))?;
            let state = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            windows::Win32::Foundation::SetLastError(ERROR_SUCCESS);
            AdjustTokenPrivileges(token, false, Some(&state), 0, None, None)
                .map_err(|err| HostError::Crypto(format!("AdjustTokenPrivileges: {err}")))?;
            // AdjustTokenPrivileges succeeds even when the privilege was not held.
            if windows::Win32::Foundation::GetLastError() == ERROR_NOT_ALL_ASSIGNED {
                return Err(HostError::Crypto(
                    "SeSecurityPrivilege is not available in this token".into(),
                ));
            }
            Ok(())
        })();

        let _ = windows::Win32::Foundation::CloseHandle(token);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cng_sha256_matches_the_known_vector() {
        let digest = sha256(b"abc").unwrap();
        assert_eq!(
            digest,
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }

    #[test]
    fn embedded_module_hash_is_pinned() {
        let digest = sha256(crate::pawnio::AMDFAMILY17_BIN).unwrap();
        assert_eq!(digest, crate::pawnio::AMDFAMILY17_SHA256);
    }

    #[test]
    fn unsigned_temp_file_is_rejected_by_authenticode() {
        let path =
            std::env::temp_dir().join(format!("oxide-pawnio-unsigned-{}.exe", std::process::id()));
        std::fs::write(&path, b"not a signed executable").unwrap();
        let result = verify_authenticode(&path);
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err());
    }
}
