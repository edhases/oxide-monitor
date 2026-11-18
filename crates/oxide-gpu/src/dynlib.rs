//! Small `LoadLibraryExW`-based dynamic loader for vendor GPU libraries.
//!
//! Vendor SDKs (NVML, ADLX, Level Zero) are installed with the display driver and
//! are optional at runtime. They are therefore resolved by name from `System32`
//! only — never from the application directory, `PATH` or a configured search
//! path — which prevents a planted DLL next to the executable from being loaded.
//!
//! The loader deliberately does not use `libloading`: `LoadLibraryExW` with
//! `LOAD_LIBRARY_SEARCH_SYSTEM32` is the exact call the security model requires,
//! and it behaves the same on x64 and ARM64.

use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::core::PCSTR;

/// An owned `HMODULE`.
///
/// A module handle is process-wide and may be used from any thread; `GetProcAddress`
/// does not mutate loader state that would make cross-thread use unsound.
pub(crate) struct Library {
    handle: HMODULE,
}

impl Library {
    /// Load a DLL from `System32` only.
    ///
    /// Returns `None` when the library is absent or cannot be loaded (for example
    /// on a machine without the vendor driver, or an x64-only DLL from an ARM64
    /// process). A missing vendor library is logged at debug level here; callers
    /// additionally remember the failure so it is not retried every tick.
    pub(crate) fn load_system32(name: &'static str) -> Option<Self> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

        match unsafe {
            LoadLibraryExW(
                windows::core::PCWSTR::from_raw(wide.as_ptr()),
                None,
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        } {
            Ok(handle) => Some(Self { handle }),
            Err(err) => {
                tracing::debug!(
                    library = name,
                    error = %err,
                    "vendor GPU library not available"
                );
                None
            }
        }
    }

    /// Whether an export exists, without interpreting its signature.
    pub(crate) fn has_symbol(&self, name: &[u8]) -> bool {
        debug_assert_eq!(name.last(), Some(&0), "symbol name must be NUL-terminated");
        let proc = unsafe { GetProcAddress(self.handle, PCSTR::from_raw(name.as_ptr())) };
        proc.is_some()
    }

    /// Resolve an exported function by its NUL-terminated ASCII name.
    ///
    /// # Safety
    ///
    /// `T` must be the exact ABI-compatible function-pointer type of the export.
    /// The caller is responsible for calling an export only on a library version
    /// that provides it; `GetProcAddress` returning a symbol does not validate the
    /// signature.
    pub(crate) unsafe fn symbol<T: Copy>(&self, name: &'static [u8]) -> Option<T> {
        debug_assert_eq!(name.last(), Some(&0), "symbol name must be NUL-terminated");
        debug_assert_eq!(
            std::mem::size_of::<T>(),
            std::mem::size_of::<usize>(),
            "symbol target must be a function pointer"
        );

        let proc = unsafe { GetProcAddress(self.handle, PCSTR::from_raw(name.as_ptr())) }?;
        let raw = proc as usize;
        if raw == 0 {
            return None;
        }
        Some(unsafe { std::mem::transmute_copy::<usize, T>(&raw) })
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            unsafe {
                let _ = FreeLibrary(self.handle);
            }
        }
    }
}

// SAFETY: `HMODULE` is an opaque process-wide handle; `GetProcAddress` and
// `FreeLibrary` are safe to call from any thread as long as the handle is not used
// after `FreeLibrary`. `Library` owns the handle and only exposes it through
// `symbol()` while `self` is alive.
unsafe impl Send for Library {}
