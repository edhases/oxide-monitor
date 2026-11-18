//! Minimal dynamic binding to the installed PawnIOLib and the pinned official
//! AMD module.
//!
//! Only `ioctl_read_smn` is ever called. The module is loaded from bytes embedded
//! in this executable after a SHA-256 pin check; the official driver additionally
//! verifies the module's RSA signature at load time. No path, offset, or IOCTL name
//! is accepted from the pipe client.

use std::ffi::{CString, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::{FreeLibrary, HANDLE, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{PCSTR, PCWSTR, s};

use crate::HostError;
use crate::crypto;
use crate::protocol::{F17H_M01H_THM_TCON_CUR_TMP, TempReading, decode_temp};

/// Official `AMDFamily17.bin` from `namazso/PawnIO.Modules` release **0.2.3**
/// (the pack contemporary with PawnIO 2.1.0; byte-identical to the blob shipped
/// by LibreHardwareMonitor 0.9.6).
pub const AMDFAMILY17_BIN: &[u8] = include_bytes!("../assets/AMDFamily17.bin");

/// SHA-256 of `AMDFamily17.bin` (0.2.3): 099dc01d6db97ea997fec4a461e191cc64b9d7ce47c9d2153c451c56c2adcf50
pub const AMDFAMILY17_SHA256: [u8; 32] = [
    0x09, 0x9d, 0xc0, 0x1d, 0x6d, 0xb9, 0x7e, 0xa9, 0x97, 0xfe, 0xc4, 0xa4, 0x61, 0xe1, 0x91, 0xcc,
    0x64, 0xb9, 0xd7, 0xce, 0x47, 0xc9, 0xd2, 0x15, 0x3c, 0x45, 0x1c, 0x56, 0xc2, 0xad, 0xcf, 0x50,
];

pub fn module_hash_prefix() -> u32 {
    u32::from_le_bytes(AMDFAMILY17_SHA256[..4].try_into().unwrap())
}

pub fn verify_module() -> Result<(), HostError> {
    let digest = crypto::sha256(AMDFAMILY17_BIN)?;
    if digest == AMDFAMILY17_SHA256 {
        Ok(())
    } else {
        Err(HostError::ModuleRejected(
            "embedded AMDFamily17.bin does not match the pinned SHA-256".into(),
        ))
    }
}

type PawnioVersionFn = unsafe extern "system" fn(*mut u32) -> i32;
type PawnioOpenFn = unsafe extern "system" fn(*mut HANDLE) -> i32;
type PawnioLoadFn = unsafe extern "system" fn(HANDLE, *const u8, usize) -> i32;
type PawnioExecuteFn =
    unsafe extern "system" fn(HANDLE, PCSTR, *const u64, usize, *mut u64, usize, *mut usize) -> i32;
type PawnioCloseFn = unsafe extern "system" fn(HANDLE) -> i32;

struct Lib {
    module: HMODULE,
    version: PawnioVersionFn,
    open: PawnioOpenFn,
    load: PawnioLoadFn,
    execute: PawnioExecuteFn,
    close: PawnioCloseFn,
}

impl Lib {
    fn load(path: &Path) -> Result<Self, HostError> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: full path from the official install directory, null-terminated.
        let module = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }
            .map_err(|err| HostError::PawnIo(format!("LoadLibraryW({}): {err}", path.display())))?;

        unsafe fn symbol(module: HMODULE, name: &str) -> Result<*mut c_void, HostError> {
            let cname = CString::new(name).expect("static export name");
            let proc = unsafe { GetProcAddress(module, PCSTR(cname.as_ptr().cast())) };
            proc.map(|f| f as *mut c_void)
                .ok_or_else(|| HostError::PawnIo(format!("PawnIOLib.dll does not export {name}")))
        }

        // SAFETY: the signatures mirror PawnIOLib.h; the library is the installed,
        // Authenticode-verified official build.
        unsafe {
            Ok(Self {
                module,
                version: std::mem::transmute::<*mut c_void, PawnioVersionFn>(symbol(
                    module,
                    "pawnio_version",
                )?),
                open: std::mem::transmute::<*mut c_void, PawnioOpenFn>(symbol(
                    module,
                    "pawnio_open",
                )?),
                load: std::mem::transmute::<*mut c_void, PawnioLoadFn>(symbol(
                    module,
                    "pawnio_load",
                )?),
                execute: std::mem::transmute::<*mut c_void, PawnioExecuteFn>(symbol(
                    module,
                    "pawnio_execute",
                )?),
                close: std::mem::transmute::<*mut c_void, PawnioCloseFn>(symbol(
                    module,
                    "pawnio_close",
                )?),
            })
        }
    }
}

impl Drop for Lib {
    fn drop(&mut self) {
        if !self.module.is_invalid() {
            // SAFETY: module came from LoadLibraryW and is released once.
            unsafe {
                let _ = FreeLibrary(self.module);
            }
        }
    }
}

/// One open PawnIO device handle with the pinned AMD module loaded.
pub struct Session {
    lib: Lib,
    handle: HANDLE,
    api_version: u32,
}

impl Session {
    pub fn open(lib_path: &Path) -> Result<Self, HostError> {
        verify_module()?;
        crypto::verify_authenticode(lib_path)?;
        let lib = Lib::load(lib_path)?;

        let mut api_version = 0u32;
        let hr = unsafe { (lib.version)(&mut api_version) };
        if hr < 0 {
            return Err(HostError::PawnIo(format!(
                "pawnio_version failed ({hr:#010X})"
            )));
        }

        let mut handle = HANDLE::default();
        let hr = unsafe { (lib.open)(&mut handle) };
        if hr < 0 {
            return Err(HostError::PawnIo(format!(
                "pawnio_open failed ({hr:#010X}); elevation may not have been granted"
            )));
        }

        let hr = unsafe { (lib.load)(handle, AMDFAMILY17_BIN.as_ptr(), AMDFAMILY17_BIN.len()) };
        if hr < 0 {
            // Drop the handle before returning the error.
            let _ = unsafe { (lib.close)(handle) };
            return Err(HostError::ModuleRejected(format!(
                "driver rejected AMDFamily17.bin ({hr:#010X})"
            )));
        }

        Ok(Self {
            lib,
            handle,
            api_version,
        })
    }

    pub fn api_version(&self) -> u32 {
        self.api_version
    }

    pub fn read_smn(&self, offset: u32) -> Result<u32, HostError> {
        let input = [u64::from(offset)];
        let mut output = [0u64; 1];
        let mut written = 0usize;
        let name = s!("ioctl_read_smn");
        let hr = unsafe {
            (self.lib.execute)(
                self.handle,
                name,
                input.as_ptr(),
                input.len(),
                output.as_mut_ptr(),
                output.len(),
                &mut written,
            )
        };
        if hr < 0 {
            return Err(HostError::PawnIo(format!(
                "ioctl_read_smn({offset:#X}) failed ({hr:#010X})"
            )));
        }
        if written != 1 {
            return Err(HostError::PawnIo(format!(
                "ioctl_read_smn returned {written} values, expected 1"
            )));
        }
        Ok(output[0] as u32)
    }

    pub fn read_cpu_temp(&self) -> Result<TempReading, HostError> {
        let raw = self.read_smn(F17H_M01H_THM_TCON_CUR_TMP)?;
        Ok(decode_temp(raw))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.handle.0.is_null() {
            // SAFETY: handle came from pawnio_open and is closed once.
            unsafe {
                let _ = (self.lib.close)(self.handle);
            }
        }
    }
}
