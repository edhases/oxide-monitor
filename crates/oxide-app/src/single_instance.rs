//! Single-instance guard backed by a named kernel mutex.

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

/// Owns the single-instance mutex handle for the lifetime of the process.
pub struct SingleInstance {
    handle: Option<HANDLE>,
    already_running: bool,
}

impl SingleInstance {
    /// Create or open the named mutex.
    ///
    /// Returns [`None`] when the mutex could not be created at all (for
    /// example, restricted environments); callers should keep starting up and
    /// log the condition. Otherwise inspect [`Self::already_running`].
    pub fn acquire() -> Option<Self> {
        // Name is session-local (`Local\`) so separate users get separate
        // instances.
        // SAFETY: no security attributes are supplied and the name is a static,
        // NUL-terminated UTF-16 literal. On success the guard owns the returned
        // handle and closes it in `Drop`.
        let result = unsafe { CreateMutexW(None, true, w!("Local\\OxideMonitorSingleInstance")) };
        // `GetLastError` must be read immediately after `CreateMutexW`.
        let last_error = unsafe { GetLastError() };

        match result {
            Ok(handle) => Some(Self {
                handle: Some(handle),
                already_running: last_error == ERROR_ALREADY_EXISTS,
            }),
            Err(err) => {
                tracing::warn!(%err, "CreateMutexW failed; single-instance guard disabled");
                None
            }
        }
    }

    /// True when another process already owned the mutex when it was opened.
    pub fn already_running(&self) -> bool {
        self.already_running
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: the handle is owned by this guard and closed exactly once.
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }
}
