//! File logging and panic hook for Oxide Monitor.
//!
//! Logs go to `%LOCALAPPDATA%\OxideMonitor\logs\oxide.log`. Debug builds also
//! mirror them to stderr so `cargo run` stays useful.

use std::io::IsTerminal;
use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;

/// Directory that owns the log file: `%LOCALAPPDATA%\OxideMonitor\logs`.
pub fn log_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("OxideMonitor").join("logs")
}

/// Install the global tracing subscriber.
///
/// The returned [`WorkerGuard`] must be held until the process is done with
/// logging; dropping it flushes and stops the appender thread.
pub fn init() -> Option<WorkerGuard> {
    let dir = log_dir();
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "oxide-monitor: failed to create log directory {}: {err}",
            dir.display()
        );
        init_stderr_only();
        return None;
    }

    let appender = tracing_appender::rolling::never(&dir, "oxide.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_writer(writer);

    let subscriber = tracing_subscriber::registry().with(file_layer);

    #[cfg(debug_assertions)]
    let subscriber = subscriber.with(
        tracing_subscriber::fmt::layer()
            .compact()
            .with_ansi(std::io::stderr().is_terminal())
            .with_target(false)
            .with_writer(std::io::stderr),
    );

    if let Err(err) = tracing::subscriber::set_global_default(subscriber) {
        // A subscriber can only already be installed by the embedding process.
        eprintln!("oxide-monitor: failed to install tracing subscriber: {err}");
    }

    Some(guard)
}

/// Last-resort subscriber used when the log directory is unavailable.
fn init_stderr_only() {
    let subscriber = tracing_subscriber::fmt()
        .compact()
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .finish();
    if let Err(err) = tracing::subscriber::set_global_default(subscriber) {
        eprintln!("oxide-monitor: failed to install tracing subscriber: {err}");
    }
}

/// Log panics (thread, location, payload) before the default hook aborts the
/// thread. The process keeps running: the sensor thread restarts on the next
/// poll, and the UI thread is allowed to unwind normally.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let location = info
            .location()
            .map(ToString::to_string)
            .unwrap_or_else(|| "<unknown>".to_string());
        let message = panic_message(info.payload());

        tracing::error!(
            panic = true,
            thread = thread_name,
            location = %location,
            message = %message,
            "thread panicked"
        );
        eprintln!("oxide-monitor: thread '{thread_name}' panicked at {location}: {message}");
    }));
}

/// Extract a printable message from a panic payload.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}
