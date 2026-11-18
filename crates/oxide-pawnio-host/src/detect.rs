//! Read-only PawnIO installation detection for the helper and the monitor.
//!
//! The official installer writes `InstallLocation` and `DisplayVersion` under
//! `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\PawnIO`. Fall back to
//! `%ProgramFiles%\PawnIO`; never search PATH or the current directory.

use std::path::PathBuf;

use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
};

const UNINSTALL_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\PawnIO";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PawnioInstall {
    /// `DisplayVersion` when the official installer key is present.
    pub version: Option<String>,
    pub install_dir: PathBuf,
    pub lib_path: PathBuf,
}

/// Detect PawnIO without touching the driver, service, or device.
pub fn detect() -> Option<PawnioInstall> {
    let install_dir = read_reg_string(UNINSTALL_KEY, "InstallLocation")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| program_files().join("PawnIO"));
    let lib_path = install_dir.join("PawnIOLib.dll");
    if !lib_path.is_file() {
        return None;
    }
    Some(PawnioInstall {
        version: read_reg_string(UNINSTALL_KEY, "DisplayVersion"),
        install_dir,
        lib_path,
    })
}

fn program_files() -> PathBuf {
    std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
}

fn read_reg_string(key: &str, value: &str) -> Option<String> {
    let key_w: Vec<u16> = key.encode_utf16().chain(std::iter::once(0)).collect();
    let value_w: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buffer = vec![0u16; 512];
    let mut size = (buffer.len() * std::mem::size_of::<u16>()) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            windows::core::PCWSTR(key_w.as_ptr()),
            windows::core::PCWSTR(value_w.as_ptr()),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.0 != 0 {
        return None;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]).trim().to_string())
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_is_read_only_and_consistent_when_present() {
        if let Some(install) = detect() {
            assert!(install.lib_path.is_file());
            assert!(install.lib_path.starts_with(&install.install_dir));
            if let Some(version) = &install.version {
                assert!(!version.trim().is_empty());
            }
        }
    }
}
