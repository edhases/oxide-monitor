//! Optional Task Scheduler integration test.
//!
//! Creates a uniquely named per-user task, verifies it, updates it, deletes it and
//! cleans up. Skips gracefully when the environment denies Task Scheduler access.
//! Run with `OXIDE_AUTOSTART_IT=1`; it is a no-op otherwise so CI never touches the
//! real scheduler.

#![cfg(windows)]

use oxide_autostart::{AUTOSTART_ARGUMENT, AutostartBackend, TaskSchedulerBackend, TaskSpec};

struct Cleanup<'a> {
    backend: &'a TaskSchedulerBackend,
    name: String,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = self.backend.delete_task(&self.name);
    }
}

#[test]
fn create_query_update_delete_task_without_admin() {
    if std::env::var_os("OXIDE_AUTOSTART_IT").is_none() {
        eprintln!("skipped: set OXIDE_AUTOSTART_IT=1 to run the Task Scheduler integration test");
        return;
    }

    let exe = std::env::current_exe().expect("test executable path");
    let name = format!("Oxide Monitor IT {}", std::process::id());
    let backend = TaskSchedulerBackend::default();
    let _cleanup = Cleanup {
        backend: &backend,
        name: name.clone(),
    };
    let spec = TaskSpec {
        name: &name,
        exe: &exe,
        working_dir: exe.parent().expect("test directory"),
        arguments: AUTOSTART_ARGUMENT,
    };

    match backend.ensure_task(&spec) {
        Ok(()) => {}
        Err(err) if err.is_access_denied() => {
            eprintln!("skipped: Task Scheduler access denied ({err})");
            return;
        }
        Err(err) => panic!("ensure_task failed: {err}"),
    }

    assert!(
        backend.task_registered(&name).expect("query task"),
        "registered task must be visible"
    );

    backend
        .ensure_task(&spec)
        .expect("create-or-update must be idempotent");

    backend.delete_task(&name).expect("delete task");
    assert!(
        !backend.task_registered(&name).expect("query task"),
        "task must be gone after delete"
    );
}
