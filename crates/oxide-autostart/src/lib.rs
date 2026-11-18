//! Per-user autostart for Oxide Monitor via Windows Task Scheduler.
//!
//! The backend is abstracted by [`AutostartBackend`] so reconciliation logic (create,
//! update, delete, legacy `HKCU\...\Run` cleanup) is unit-tested without touching the
//! real Task Scheduler. [`TaskSchedulerBackend`] is the production COM implementation;
//! it registers the task with `TASK_LOGON_INTERACTIVE_TOKEN` and `TASK_RUNLEVEL_LUA`,
//! so it never needs administrator rights or UAC.

use std::path::{Path, PathBuf};

use oxide_core::{AUTOSTART_TASK_NAME, AutostartStatus};
use thiserror::Error;

#[cfg(windows)]
mod windows_backend;

#[cfg(windows)]
pub use windows_backend::TaskSchedulerBackend;

/// Argument passed to `oxide-monitor.exe` by the scheduled task.
pub const AUTOSTART_ARGUMENT: &str = "--autostart";
/// Logon delay: lets Explorer/taskbar and the sensor stack settle first.
pub const AUTOSTART_DELAY: &str = "PT15S";
/// Description stored on the scheduled task.
pub const AUTOSTART_DESCRIPTION: &str =
    "Oxide Monitor - taskbar widget autostart (per-user, limited privileges).";

/// Everything needed to create or update the scheduled task.
#[derive(Clone, Copy, Debug)]
pub struct TaskSpec<'a> {
    pub name: &'a str,
    pub exe: &'a Path,
    pub working_dir: &'a Path,
    pub arguments: &'a str,
}

impl<'a> TaskSpec<'a> {
    /// Standard task for `exe`: `--autostart`, working directory = executable folder.
    pub fn for_exe(name: &'a str, exe: &'a Path) -> Self {
        Self {
            name,
            exe,
            working_dir: exe.parent().unwrap_or_else(|| Path::new(".")),
            arguments: AUTOSTART_ARGUMENT,
        }
    }

    /// Standard task for the running executable.
    pub fn current(name: &'a str, exe: &'a Path) -> Self {
        Self::for_exe(name, exe)
    }
}

#[derive(Debug, Error)]
pub enum AutostartError {
    #[error("{context}: {message} (HRESULT {hr:#010x})")]
    Com {
        context: &'static str,
        hr: i32,
        message: String,
    },
    #[error("{context}: Windows error {code}")]
    Win32 { context: &'static str, code: u32 },
    #[error("{0}")]
    Message(String),
}

impl AutostartError {
    pub(crate) fn com(context: &'static str, error: windows::core::Error) -> Self {
        Self::Com {
            context,
            hr: error.code().0,
            message: error.to_string(),
        }
    }

    pub(crate) fn win32(context: &'static str, code: u32) -> Self {
        Self::Win32 { context, code }
    }

    /// HRESULT for COM failures (`None` for Win32/other errors).
    pub fn hresult(&self) -> Option<i32> {
        match self {
            Self::Com { hr, .. } => Some(*hr),
            _ => None,
        }
    }

    /// True for `E_ACCESSDENIED` (0x80070005) or `ERROR_ACCESS_DENIED` (5).
    pub fn is_access_denied(&self) -> bool {
        match self {
            Self::Com { hr, .. } => *hr == 0x8007_0005u32 as i32,
            Self::Win32 { code, .. } => *code == 5,
            Self::Message(_) => false,
        }
    }
}

/// Pluggable Task Scheduler backend (fake in unit tests).
pub trait AutostartBackend {
    /// Create or update the task for the current user (idempotent).
    fn ensure_task(&self, spec: &TaskSpec<'_>) -> Result<(), AutostartError>;
    /// Delete the task; a missing task is success.
    fn delete_task(&self, name: &str) -> Result<(), AutostartError>;
    /// Whether the task exists and is enabled.
    fn task_registered(&self, name: &str) -> Result<bool, AutostartError>;
    /// Remove the legacy `HKCU\...\Run` value; returns `true` when a value was removed.
    fn remove_legacy_run(&self) -> Result<bool, AutostartError>;
}

/// Reconcile the task with `general.autostart`.
///
/// Always removes the legacy Run value first, then creates/updates or deletes the task.
/// Never falls back to the Run key when Task Scheduler is unavailable.
pub fn reconcile(
    desired: bool,
    spec: &TaskSpec<'_>,
    backend: &dyn AutostartBackend,
) -> AutostartStatus {
    let mut errors: Vec<String> = Vec::new();

    match backend.remove_legacy_run() {
        Ok(true) => tracing::info!("removed legacy HKCU Run autostart value"),
        Ok(false) => {}
        Err(err) => {
            tracing::warn!(%err, "failed to remove legacy HKCU Run autostart value");
            errors.push(format!("legacy Run cleanup: {err}"));
        }
    }

    let mut registered = false;
    if desired {
        match backend.ensure_task(spec) {
            Ok(()) => match backend.task_registered(spec.name) {
                Ok(true) => {
                    registered = true;
                    tracing::info!(
                        task = spec.name,
                        exe = %spec.exe.display(),
                        "autostart scheduled task registered"
                    );
                }
                Ok(false) => {
                    errors.push("scheduled task not found after registration".to_string());
                    tracing::error!(
                        task = spec.name,
                        "task scheduler reported success but the task is missing"
                    );
                }
                Err(err) => {
                    errors.push(format!("task query: {err}"));
                    tracing::warn!(%err, task = spec.name, "failed to query autostart task");
                }
            },
            Err(err) => {
                tracing::error!(
                    task = spec.name,
                    %err,
                    access_denied = err.is_access_denied(),
                    "failed to create/update autostart scheduled task"
                );
                errors.push(format!("create/update: {err}"));
            }
        }
    } else {
        match backend.delete_task(spec.name) {
            Ok(()) => {}
            Err(err) => {
                tracing::warn!(%err, task = spec.name, "failed to delete autostart task");
                errors.push(format!("delete: {err}"));
            }
        }
        match backend.task_registered(spec.name) {
            Ok(found) => {
                registered = found;
                if found {
                    errors.push("scheduled task still present after delete".to_string());
                }
            }
            Err(err) => errors.push(format!("task query: {err}")),
        }
        if !registered {
            tracing::info!(
                task = spec.name,
                "autostart disabled; scheduled task absent"
            );
        }
    }

    AutostartStatus {
        desired_enabled: desired,
        enabled: registered,
        task_name: spec.name.to_string(),
        last_error: (!errors.is_empty()).then(|| errors.join("; ")),
    }
}

/// Standard task spec for the running executable.
pub fn current_exe_task_spec() -> (PathBuf, String) {
    let exe = std::env::current_exe().unwrap_or_default();
    let name = AUTOSTART_TASK_NAME.to_string();
    (exe, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Clone, Debug, PartialEq)]
    struct FakeTask {
        exe: PathBuf,
        working_dir: PathBuf,
        arguments: String,
    }

    #[derive(Default)]
    struct FakeBackend {
        tasks: Mutex<HashMap<String, FakeTask>>,
        legacy_present: Mutex<bool>,
        fail_ensure: Mutex<Option<String>>,
        fail_delete: Mutex<Option<String>>,
        fail_legacy: Mutex<Option<String>>,
    }

    impl FakeBackend {
        fn with_legacy() -> Self {
            let backend = Self::default();
            *backend.legacy_present.lock().unwrap() = true;
            backend
        }
    }

    impl AutostartBackend for FakeBackend {
        fn ensure_task(&self, spec: &TaskSpec<'_>) -> Result<(), AutostartError> {
            if let Some(message) = self.fail_ensure.lock().unwrap().clone() {
                return Err(AutostartError::Message(message));
            }
            self.tasks.lock().unwrap().insert(
                spec.name.to_string(),
                FakeTask {
                    exe: spec.exe.to_path_buf(),
                    working_dir: spec.working_dir.to_path_buf(),
                    arguments: spec.arguments.to_string(),
                },
            );
            Ok(())
        }

        fn delete_task(&self, name: &str) -> Result<(), AutostartError> {
            if let Some(message) = self.fail_delete.lock().unwrap().clone() {
                return Err(AutostartError::Message(message));
            }
            self.tasks.lock().unwrap().remove(name);
            Ok(())
        }

        fn task_registered(&self, name: &str) -> Result<bool, AutostartError> {
            Ok(self.tasks.lock().unwrap().contains_key(name))
        }

        fn remove_legacy_run(&self) -> Result<bool, AutostartError> {
            if let Some(message) = self.fail_legacy.lock().unwrap().clone() {
                return Err(AutostartError::Message(message));
            }
            let mut present = self.legacy_present.lock().unwrap();
            let removed = *present;
            *present = false;
            Ok(removed)
        }
    }

    fn spec<'a>(exe: &'a Path, name: &'a str) -> TaskSpec<'a> {
        TaskSpec::for_exe(name, exe)
    }

    #[test]
    fn enabling_creates_task_and_removes_legacy_run_value() {
        let backend = FakeBackend::with_legacy();
        let exe = Path::new(r"C:\Apps\Oxide\oxide-monitor.exe");
        let status = reconcile(true, &spec(exe, AUTOSTART_TASK_NAME), &backend);

        assert!(status.desired_enabled);
        assert!(status.enabled);
        assert_eq!(status.task_name, AUTOSTART_TASK_NAME);
        assert!(status.last_error.is_none());

        let tasks = backend.tasks.lock().unwrap();
        let task = tasks.get(AUTOSTART_TASK_NAME).expect("task registered");
        assert_eq!(task.exe, exe);
        assert_eq!(task.working_dir, Path::new(r"C:\Apps\Oxide"));
        assert_eq!(task.arguments, AUTOSTART_ARGUMENT);
        assert!(!*backend.legacy_present.lock().unwrap());
    }

    #[test]
    fn disabling_deletes_task_and_still_cleans_the_legacy_value() {
        let backend = FakeBackend::with_legacy();
        let exe = Path::new(r"C:\Apps\Oxide\oxide-monitor.exe");
        let _ = reconcile(true, &spec(exe, AUTOSTART_TASK_NAME), &backend);
        *backend.legacy_present.lock().unwrap() = true;

        let status = reconcile(false, &spec(exe, AUTOSTART_TASK_NAME), &backend);
        assert!(!status.desired_enabled);
        assert!(!status.enabled);
        assert!(status.last_error.is_none());
        assert!(backend.tasks.lock().unwrap().is_empty());
        assert!(!*backend.legacy_present.lock().unwrap());
    }

    #[test]
    fn reconcile_is_idempotent_and_updates_a_moved_executable() {
        let backend = FakeBackend::default();
        let first = Path::new(r"C:\Old\oxide-monitor.exe");
        let second = Path::new(r"D:\New\oxide-monitor.exe");

        let _ = reconcile(true, &spec(first, AUTOSTART_TASK_NAME), &backend);
        let _ = reconcile(true, &spec(first, AUTOSTART_TASK_NAME), &backend);
        assert_eq!(backend.tasks.lock().unwrap().len(), 1);

        let status = reconcile(true, &spec(second, AUTOSTART_TASK_NAME), &backend);
        assert!(status.enabled);
        let tasks = backend.tasks.lock().unwrap();
        assert_eq!(tasks[AUTOSTART_TASK_NAME].exe, second);
        assert_eq!(tasks[AUTOSTART_TASK_NAME].working_dir, Path::new("D:\\New"));
    }

    #[test]
    fn failure_is_reported_and_never_falls_back_to_the_run_key() {
        let backend = FakeBackend::with_legacy();
        *backend.fail_ensure.lock().unwrap() = Some("access denied (HRESULT 0x80070005)".into());

        let exe = Path::new(r"C:\Apps\Oxide\oxide-monitor.exe");
        let status = reconcile(true, &spec(exe, AUTOSTART_TASK_NAME), &backend);

        assert!(status.desired_enabled);
        assert!(!status.enabled);
        assert!(
            status
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("access denied"))
        );
        // The legacy value is cleaned even when creation fails, and nothing re-creates it.
        assert!(!*backend.legacy_present.lock().unwrap());
        assert!(backend.tasks.lock().unwrap().is_empty());
    }

    #[test]
    fn delete_failure_keeps_the_task_reported_as_registered() {
        let backend = FakeBackend::default();
        let exe = Path::new(r"C:\Apps\Oxide\oxide-monitor.exe");
        let _ = reconcile(true, &spec(exe, AUTOSTART_TASK_NAME), &backend);
        *backend.fail_delete.lock().unwrap() = Some("delete failed".into());

        let status = reconcile(false, &spec(exe, AUTOSTART_TASK_NAME), &backend);
        assert!(!status.desired_enabled);
        assert!(status.enabled);
        assert!(
            status
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("delete failed"))
        );
    }

    #[test]
    fn legacy_cleanup_failure_does_not_block_task_creation() {
        let backend = FakeBackend::with_legacy();
        *backend.fail_legacy.lock().unwrap() = Some("registry denied".into());

        let exe = Path::new(r"C:\Apps\Oxide\oxide-monitor.exe");
        let status = reconcile(true, &spec(exe, AUTOSTART_TASK_NAME), &backend);
        assert!(status.enabled);
        assert!(
            status
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("legacy Run cleanup"))
        );
    }
}
