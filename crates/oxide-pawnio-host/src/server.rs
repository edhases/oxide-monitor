//! Elevated helper server: bind the per-user pipe, verify the client image,
//! handshake, and serve read-only CPU-temperature requests until Shutdown/disconnect.

use std::time::Duration;

use oxide_ipc::IpcError;
use oxide_ipc::pipe::{PipeListener, WinEvent};
use oxide_ipc::security::{CurrentUserDacl, PeerPolicy};

use crate::protocol::{
    HelloAck, MAX_MESSAGE, Message, PROTOCOL_VERSION, TEMP_READ_FAILED, TEMP_UNAVAILABLE,
    TempReply, read_message, write_message,
};
use crate::{HostError, MONITOR_EXE, detect, log, pawnio};

/// Startup wait for the monitor to connect after the UAC prompt.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-message read timeout; 12 idle periods (~60 s) ends an orphaned helper.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_IDLE_PERIODS: u32 = 12;

pub fn run() -> Result<(), HostError> {
    run_with_sid(None)
}

/// `invoking_sid` is the monitor's user SID. It is passed on the command line by
/// `ShellExecuteEx` because an over-the-shoulder UAC elevation runs the helper as a
/// different admin account; the pipe must still belong to the invoking user.
pub fn run_with_sid(invoking_sid: Option<&str>) -> Result<(), HostError> {
    log::line("helper starting");

    if !crate::crypto::is_elevated() {
        log::line("refusing to run: token is not elevated");
        return Err(HostError::NotElevated);
    }

    let sid = match invoking_sid {
        Some(sid) => {
            oxide_ipc::validate_sid(sid).map_err(|err| HostError::Launch(err.to_string()))?;
            sid.to_string()
        }
        None => oxide_ipc::current_user_sid()?,
    };

    let install = detect().ok_or(HostError::NotInstalled)?;
    log::line(&format!(
        "PawnIO {:?} at {}",
        install.version,
        install.install_dir.display()
    ));

    let session = pawnio::Session::open(&install.lib_path)?;
    log::line(&format!(
        "PawnIO API {:#010X}, module hash prefix {:#010X}",
        session.api_version(),
        pawnio::module_hash_prefix()
    ));

    let pipe = crate::pipe_name_for_sid(&sid);
    crate::crypto::enable_security_privilege()?;
    let dacl = CurrentUserDacl::build_for_medium_integrity_for_sid(&sid)?;
    let listener = PipeListener::bind_with_dacl(&pipe, &dacl)?;
    log::line("pipe bound; waiting for oxide-monitor.exe");

    let stop = WinEvent::new_manual_reset()?;
    let conn = listener.accept(
        CONNECT_TIMEOUT,
        &stop,
        &PeerPolicy::sibling_exe(MONITOR_EXE),
    )?;
    log::line(&format!("client connected (pid {})", conn.peer_pid()));

    match read_message(&conn, Some(READ_TIMEOUT))? {
        Message::Hello { protocol, .. } if protocol == PROTOCOL_VERSION => {}
        other => {
            let _ = write_message(&conn, &Message::Error { code: 1 }, None);
            log::line(&format!("bad handshake: {other:?}"));
            return Err(HostError::Protocol(
                crate::protocol::ProtocolError::BadVersion(PROTOCOL_VERSION),
            ));
        }
    }

    write_message(
        &conn,
        &Message::HelloAck(HelloAck {
            protocol: PROTOCOL_VERSION,
            pawnio_api: session.api_version(),
            module_hash_prefix: pawnio::module_hash_prefix(),
            max_message: MAX_MESSAGE as u16,
        }),
        None,
    )?;

    let mut idle_periods = 0u32;
    loop {
        match read_message(&conn, Some(READ_TIMEOUT)) {
            Ok(Message::ReadCpuTemp) => {
                idle_periods = 0;
                let reply = match session.read_cpu_temp() {
                    Ok(reading) if reading.is_plausible() => TempReply::from_reading(reading),
                    Ok(reading) => {
                        log::line(&format!(
                            "implausible temperature: raw {:#010X}, {:.3} C",
                            reading.raw, reading.temp_c
                        ));
                        TempReply {
                            status: TEMP_UNAVAILABLE,
                            temp_millic: reading.temp_millic(),
                            raw: reading.raw,
                            flags: reading.flags(),
                        }
                    }
                    Err(err) => {
                        log::line(&format!("temperature read failed: {err}"));
                        TempReply::error(TEMP_READ_FAILED)
                    }
                };
                write_message(&conn, &Message::TempReply(reply), None)?;
            }
            Ok(Message::Shutdown) => {
                log::line("shutdown requested by monitor");
                break;
            }
            Ok(other) => {
                log::line(&format!("unexpected message: {other:?}"));
                let _ = write_message(&conn, &Message::Error { code: 2 }, None);
                break;
            }
            Err(HostError::Ipc(IpcError::Timeout)) => {
                idle_periods += 1;
                if idle_periods >= MAX_IDLE_PERIODS {
                    log::line("idle timeout; exiting to avoid an orphaned elevated process");
                    break;
                }
            }
            Err(HostError::Ipc(IpcError::Disconnected)) => {
                log::line("client disconnected");
                break;
            }
            Err(err) => {
                log::line(&format!("pipe read failed: {err}"));
                break;
            }
        }
    }

    log::line("helper stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_refuses_to_run_without_elevation() {
        if crate::crypto::is_elevated() {
            eprintln!("skipped: test process is already elevated");
            return;
        }
        assert!(matches!(run_with_sid(None), Err(HostError::NotElevated)));
        let _ = std::fs::remove_file(crate::log::log_path());
    }
}
