//! Windows implementation of update verification and installed-version detection.

use std::ffi::c_void;
use std::fs::File;
use std::io::Read;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Security::Cryptography::{
    BCRYPT_ALG_HANDLE, BCRYPT_HANDLE, BCRYPT_HASH_HANDLE, BCRYPT_OBJECT_LENGTH,
    BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM, BCryptCloseAlgorithmProvider,
    BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash, BCryptGetProperty, BCryptHashData,
    BCryptOpenAlgorithmProvider,
};
use windows::Win32::Security::WinTrust::{
    WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_DATA_PROVIDER_FLAGS, WINTRUST_FILE_INFO,
    WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    WinVerifyTrust,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows::Win32::System::ProcessStatus::EnumProcesses;
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW, QueryServiceConfigW,
    SC_MANAGER_CONNECT, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{GUID, PCWSTR, PWSTR, w};

use crate::update::{
    ArtifactVerifier, InstalledLhm, InstalledPawnIo, LHM_MANAGED_EXE, SignatureStatus, UpdateError,
    Version,
};

const WINTRUST_ACTION_GENERIC_VERIFY_V2: GUID =
    GUID::from_u128(0x00aa_c56b_cd44_11d0_8cc2_00c0_4fc2_95ee);

/// Production verifier: CNG SHA-256 + Authenticode trust chain.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsVerifier;

impl ArtifactVerifier for WindowsVerifier {
    fn sha256_file(&self, path: &Path) -> Result<[u8; 32], UpdateError> {
        sha256_file(path)
    }

    fn verify_authenticode(&self, path: &Path) -> Result<SignatureStatus, UpdateError> {
        verify_authenticode(path)
    }
}

fn sha256_file(path: &Path) -> Result<[u8; 32], UpdateError> {
    let mut algorithm = BCRYPT_ALG_HANDLE::default();
    let opened = unsafe {
        BCryptOpenAlgorithmProvider(
            &mut algorithm,
            BCRYPT_SHA256_ALGORITHM,
            None,
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        )
    };
    if opened.0 < 0 {
        return Err(UpdateError::Verify(format!(
            "BCryptOpenAlgorithmProvider failed ({:#010x})",
            opened.0
        )));
    }

    let mut hash = BCRYPT_HASH_HANDLE::default();
    let result = (|| -> Result<[u8; 32], UpdateError> {
        let mut object_length_bytes = [0u8; 4];
        let mut written = 0u32;
        let status = unsafe {
            BCryptGetProperty(
                BCRYPT_HANDLE(algorithm.0),
                BCRYPT_OBJECT_LENGTH,
                Some(&mut object_length_bytes),
                &mut written,
                0,
            )
        };
        if status.0 < 0 {
            return Err(UpdateError::Verify("BCryptGetProperty failed".into()));
        }
        let object_length = u32::from_le_bytes(object_length_bytes) as usize;
        let mut object = vec![0u8; object_length];
        let status = unsafe { BCryptCreateHash(algorithm, &mut hash, Some(&mut object), None, 0) };
        if status.0 < 0 {
            return Err(UpdateError::Verify("BCryptCreateHash failed".into()));
        }

        let mut file = File::open(path)?;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let status = unsafe { BCryptHashData(hash, &buffer[..read], 0) };
            if status.0 < 0 {
                return Err(UpdateError::Verify("BCryptHashData failed".into()));
            }
        }

        let mut digest = [0u8; 32];
        let status = unsafe { BCryptFinishHash(hash, &mut digest, 0) };
        if status.0 < 0 {
            return Err(UpdateError::Verify("BCryptFinishHash failed".into()));
        }
        Ok(digest)
    })();

    unsafe {
        if !hash.0.is_null() {
            let _ = BCryptDestroyHash(hash);
        }
        let _ = BCryptCloseAlgorithmProvider(algorithm, 0);
    }
    result
}

fn verify_authenticode(path: &Path) -> Result<SignatureStatus, UpdateError> {
    let wide = wide_path(path);
    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wide.as_ptr()),
        hFile: HANDLE::default(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &mut file_info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        // Do not fetch revocation lists over the network during verification.
        dwProvFlags: WINTRUST_DATA_PROVIDER_FLAGS(
            windows::Win32::Security::WinTrust::WTD_CACHE_ONLY_URL_RETRIEVAL.0,
        ),
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;

    let status = unsafe {
        WinVerifyTrust(
            HWND::default(),
            &mut action,
            &mut data as *mut _ as *mut c_void,
        )
    };

    // Release the state data allocated by the first call.
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    let _ = unsafe {
        WinVerifyTrust(
            HWND::default(),
            &mut action,
            &mut data as *mut _ as *mut c_void,
        )
    };

    match status as u32 {
        0 => Ok(SignatureStatus::Trusted),
        // No signature at all / unknown subject format: the official LHM build is
        // currently unsigned. The staging policy only accepts this with a published
        // SHA-256 match; a present-but-invalid signature stays an error below.
        0x800B_0100 | 0x800B_0001 | 0x800B_0003 => Ok(SignatureStatus::Unsigned),
        _ => Err(UpdateError::Verify(format!(
            "WinVerifyTrust rejected the executable ({status:#010x})"
        ))),
    }
}

/// File version from `VS_FIXEDFILEINFO`, e.g. `0.9.6.0`.
pub fn file_version(path: &Path) -> Option<Version> {
    let wide = wide_path(path);
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None) };
    if size == 0 {
        return None;
    }
    let mut buffer = vec![0u8; size as usize];
    unsafe {
        GetFileVersionInfoW(
            PCWSTR(wide.as_ptr()),
            None,
            size,
            buffer.as_mut_ptr().cast(),
        )
    }
    .ok()?;

    let mut value: *mut c_void = std::ptr::null_mut();
    let mut length = 0u32;
    if !unsafe { VerQueryValueW(buffer.as_ptr().cast(), w!("\\"), &mut value, &mut length) }
        .as_bool()
        || value.is_null()
        || (length as usize) < size_of::<VS_FIXEDFILEINFO>()
    {
        return None;
    }

    let info = unsafe { &*(value as *const VS_FIXEDFILEINFO) };
    if info.dwSignature != 0xFEEF04BD {
        return None;
    }
    Some(Version {
        major: info.dwFileVersionMS >> 16,
        minor: info.dwFileVersionMS & 0xffff,
        patch: info.dwFileVersionLS >> 16,
        build: info.dwFileVersionLS & 0xffff,
    })
}

/// A running LHM from any path (never modified by the monitor).
pub fn find_running_lhm() -> Option<InstalledLhm> {
    let mut pids = vec![0u32; 1024];
    let mut needed = 0u32;
    unsafe { EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut needed) }.ok()?;
    let count = (needed as usize / 4).min(pids.len());

    for &pid in &pids[..count] {
        if pid == 0 || pid == std::process::id() {
            continue;
        }
        let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
        else {
            continue;
        };
        let mut buffer = vec![0u16; 1024];
        let mut length = buffer.len() as u32;
        let queried = unsafe {
            QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut length,
            )
        };
        let _ = unsafe { CloseHandle(process) };
        if queried.is_err() {
            continue;
        }
        let path = PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize]));
        let name_matches = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(LHM_MANAGED_EXE));
        if name_matches {
            return Some(InstalledLhm {
                version: file_version(&path),
                path,
                external: true,
            });
        }
    }
    None
}

/// PawnIO driver detection (read-only): service config, registry fallback, file version.
pub fn detect_pawnio() -> Option<InstalledPawnIo> {
    let path = pawnio_driver_path()?;
    let version = file_version(&path);
    Some(InstalledPawnIo {
        version,
        path: Some(path),
        detail: "PawnIO driver service detected (updates require the official installer)"
            .to_string(),
    })
}

fn pawnio_driver_path() -> Option<PathBuf> {
    if let Some(raw) = service_image_path("PawnIO") {
        return Some(normalize_driver_path(&raw));
    }
    registry_image_path().map(|raw| normalize_driver_path(&raw))
}

fn service_image_path(service_name: &str) -> Option<String> {
    let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }.ok()?;
    let name = wide(service_name);
    let service = unsafe {
        OpenServiceW(
            scm,
            PCWSTR(name.as_ptr()),
            SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS,
        )
    };
    let result = service.ok().and_then(|service| {
        let path = query_service_path(service);
        let _ = unsafe { CloseServiceHandle(service) };
        path
    });
    let _ = unsafe { CloseServiceHandle(scm) };
    result
}

fn query_service_path(service: windows::Win32::System::Services::SC_HANDLE) -> Option<String> {
    let mut needed = 0u32;
    let _ = unsafe { QueryServiceConfigW(service, None, 0, &mut needed) };
    if needed == 0 {
        return None;
    }
    let mut buffer = vec![0u8; needed as usize];
    unsafe {
        QueryServiceConfigW(
            service,
            Some(buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>()),
            needed,
            &mut needed,
        )
    }
    .ok()?;
    let config = unsafe { &*(buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>()) };
    unsafe { config.lpBinaryPathName.to_string() }.ok()
}

fn registry_image_path() -> Option<String> {
    let mut buffer = vec![0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SYSTEM\\CurrentControlSet\\Services\\PawnIO"),
            w!("ImagePath"),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.0 != 0 {
        return None;
    }
    let length = buffer
        .iter()
        .position(|&ch| ch == 0)
        .unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..length]))
}

/// Normalize `\SystemRoot\...`, `\??\C:\...` and quoted service paths.
pub fn normalize_driver_path(raw: &str) -> PathBuf {
    let raw = raw.trim().trim_matches('"');
    let raw = raw.strip_prefix(r"\??\").unwrap_or(raw);
    if let Some(rest) = raw.strip_prefix(r"\SystemRoot\") {
        let root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        return root.join(rest);
    }
    PathBuf::from(raw)
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, contents: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "oxide-verify-{}-{}-{name}",
            std::process::id(),
            crate::update::now_unix_ms()
        ));
        let mut file = File::create(&path).unwrap();
        file.write_all(contents).unwrap();
        path
    }

    #[test]
    fn cng_sha256_matches_the_known_vector() {
        let path = temp_file("abc.bin", b"abc");
        let digest = sha256_file(&path).unwrap();
        assert_eq!(
            crate::update::hex_encode(&digest),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unsigned_files_are_reported_as_unsigned_not_trusted() {
        let path = temp_file("unsigned.exe", b"not a signed executable");
        let status = verify_authenticode(&path).expect("unsigned status");
        assert_eq!(status, SignatureStatus::Unsigned);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn driver_paths_are_normalized() {
        let system_root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        assert_eq!(
            normalize_driver_path(r"\SystemRoot\system32\drivers\PawnIO.sys"),
            system_root.join(r"system32\drivers\PawnIO.sys")
        );
        assert_eq!(
            normalize_driver_path(r#""\??\C:\Windows\system32\drivers\PawnIO.sys""#),
            PathBuf::from(r"C:\Windows\system32\drivers\PawnIO.sys")
        );
    }
}
