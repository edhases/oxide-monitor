//! Versioned Named Pipe IPC protocol between `oxide-monitor.exe` and `oxide-settings.exe`.
//!
//! Framing: 4-byte little-endian `u32` payload length followed by UTF-8 JSON bytes.

use oxide_core::{Config, HardwareInventory, HistorySliceDto, MonitorDescriptor, SnapshotDto};
use serde::{Deserialize, Serialize};

use crate::IpcError;

/// Current wire protocol version. Client and server reject mismatched versions on `Hello`.
pub const IPC_PROTOCOL_VERSION: u32 = 1;

/// Maximum serialized payload size per frame (2 MiB).
pub const MAX_IPC_FRAME_BYTES: usize = 2 * 1024 * 1024;

/// Fixed Named Pipe path used by `oxide-monitor.exe` (`FILE_FLAG_FIRST_PIPE_INSTANCE`).
pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\oxide-ui-v1";

/// Optimistic concurrency token identifying a committed `config.toml` revision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigRevision {
    pub generation: u64,
    pub config_hash: u64,
}

/// Outcome of a `CommitConfig` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitStatus {
    Ok,
    Conflict,
    Error,
}

/// Messages sent from `oxide-settings.exe` to `oxide-monitor.exe`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SettingsToMonitor {
    Hello {
        protocol_version: u32,
        #[serde(default)]
        focus_tab: Option<String>,
    },
    PreviewConfig {
        config: Box<Config>,
    },
    CommitConfig {
        expected_generation: u64,
        expected_hash: u64,
        config: Box<Config>,
    },
    RevertConfig,
    /// Explicit user action for the optional elevated PawnIO helper. Config alone
    /// must never start it; the helper must not run at monitor startup.
    PawnioControl {
        start: bool,
    },
    Ping,
}

/// Messages sent from `oxide-monitor.exe` to `oxide-settings.exe`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MonitorToSettings {
    HelloAck {
        protocol_version: u32,
        app_version: String,
        arch: String,
        config_path: String,
        portable_mode: bool,
        generation: u64,
        config_hash: u64,
        active_config: Box<Config>,
        monitors: Vec<MonitorDescriptor>,
    },
    TelemetryTick {
        snapshot: SnapshotDto,
        history: HistorySliceDto,
        inventory: HardwareInventory,
    },
    MonitorsChanged {
        monitors: Vec<MonitorDescriptor>,
    },
    CommitResult {
        status: CommitStatus,
        generation: u64,
        config_hash: u64,
        #[serde(default)]
        error: Option<String>,
    },
    FocusTab {
        tab: String,
    },
    ProtocolError {
        code: String,
        message: String,
    },
    Pong,
}

/// Encode a JSON payload with a 4-byte little-endian length prefix.
pub fn encode_frame<T: Serialize>(msg: &T) -> Result<Vec<u8>, IpcError> {
    let payload = serde_json::to_vec(msg)?;
    if payload.is_empty() || payload.len() > MAX_IPC_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge {
            len: payload.len(),
            max: MAX_IPC_FRAME_BYTES,
        });
    }
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Validate a 4-byte length header and return the expected payload byte count.
pub fn decode_frame_len(header: [u8; 4]) -> Result<usize, IpcError> {
    let len = u32::from_le_bytes(header) as usize;
    if len == 0 || len > MAX_IPC_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge {
            len,
            max: MAX_IPC_FRAME_BYTES,
        });
    }
    Ok(len)
}

/// Decode a JSON payload slice into a typed IPC message.
pub fn decode_payload<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> Result<T, IpcError> {
    if payload.is_empty() || payload.len() > MAX_IPC_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge {
            len: payload.len(),
            max: MAX_IPC_FRAME_BYTES,
        });
    }
    Ok(serde_json::from_slice(payload)?)
}
