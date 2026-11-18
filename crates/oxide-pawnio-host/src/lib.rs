//! Oxide Monitor's optional PawnIO CPU-temperature helper.
//!
//! The library half is used by `oxide-monitor.exe` (pipe client, launch, status
//! controller). The binary half is the elevated `oxide-pawnio-host.exe` server.
//! Settings never links this crate; it only consumes the status DTOs from
//! `oxide-core` over the normal IPC snapshot.

#![cfg(windows)]

pub mod client;
pub mod controller;
pub mod crypto;
pub mod detect;
pub mod launch;
pub mod log;
pub mod protocol;
pub mod server;

mod pawnio;

pub use client::HelperClient;
pub use controller::{Action, Controller, Event};
pub use detect::{PawnioInstall, detect};
pub use launch::{HelperProcess, helper_path, launch_helper};
pub use protocol::{
    HelloAck, MAX_MESSAGE, Message, PROTOCOL_VERSION, TempReading, TempReply, decode_temp,
};

/// Version of the helper crate (not the PawnIO driver version).
pub const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Name checked by the helper before accepting the pipe client.
pub const MONITOR_EXE: &str = "oxide-monitor.exe";
/// Helper executable shipped beside `oxide-monitor.exe`.
pub const HELPER_EXE: &str = "oxide-pawnio-host.exe";

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("PawnIO is not installed")]
    NotInstalled,
    #[error("the PawnIO helper must run elevated")]
    NotElevated,
    #[error("PawnIO module rejected: {0}")]
    ModuleRejected(String),
    #[error("PawnIO library: {0}")]
    PawnIo(String),
    #[error("helper IPC: {0}")]
    Ipc(#[from] oxide_ipc::IpcError),
    #[error("helper protocol: {0}")]
    Protocol(#[from] protocol::ProtocolError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("signature/hash verification: {0}")]
    Crypto(String),
    #[error("UAC elevation was declined")]
    UacDenied,
    #[error("helper launch failed: {0}")]
    Launch(String),
    #[error("helper executable not found beside oxide-monitor.exe: {0}")]
    HelperMissing(String),
}

/// Per-user pipe name. Both the helper and the monitor derive it from the SID;
/// the helper's DACL grants access only to that user (plus SYSTEM).
pub fn pipe_name() -> Result<String, HostError> {
    let sid = oxide_ipc::current_user_sid()?;
    Ok(pipe_name_for_sid(&sid))
}

/// Build the pipe name for an explicit (already validated) invoking-user SID.
pub fn pipe_name_for_sid(sid: &str) -> String {
    format!("{}{sid}", oxide_core::PAWNIO_HELPER_PIPE_PREFIX)
}
