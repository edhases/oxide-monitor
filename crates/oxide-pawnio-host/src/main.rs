#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Elevated, read-only PawnIO CPU-temperature helper.
//!
//! Launched only by `oxide-monitor.exe` via `ShellExecuteExW("runas")` after an
//! explicit user action. The `requireAdministrator` manifest prevents accidental
//! unelevated startup. It binds a per-user Named Pipe, accepts only
//! `oxide-monitor.exe`, and serves a fixed Hello/ReadCpuTemp/Shutdown protocol.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut sid: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--sid" => sid = args.next(),
            other => {
                oxide_pawnio_host::log::line(&format!("unknown argument: {other}"));
                return ExitCode::FAILURE;
            }
        }
    }

    match oxide_pawnio_host::server::run_with_sid(sid.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            oxide_pawnio_host::log::line(&format!("helper failed: {err}"));
            ExitCode::FAILURE
        }
    }
}
