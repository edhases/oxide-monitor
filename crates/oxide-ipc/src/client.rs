//! Typed Named Pipe IPC client for `oxide-settings.exe` (and integration tests).

use std::time::Duration;

use oxide_core::Config;

use crate::IpcError;
use crate::pipe::{PipeConnection, connect_with_retry};
use crate::protocol::{
    DEFAULT_PIPE_NAME, IPC_PROTOCOL_VERSION, MonitorToSettings, SettingsToMonitor, decode_payload,
    encode_frame,
};
use crate::security::PeerPolicy;

/// Connected client handle to `oxide-monitor.exe`'s Named Pipe.
pub struct IpcClient {
    conn: PipeConnection,
}

impl IpcClient {
    /// Connect to `\\.\pipe\oxide-ui-v1` (retrying up to `timeout`, e.g. 2 s for direct launch)
    /// and verify that the server executable is a sibling `oxide-monitor.exe`.
    pub fn connect_default(timeout: Duration) -> Result<Self, IpcError> {
        Self::connect(
            DEFAULT_PIPE_NAME,
            timeout,
            &PeerPolicy::sibling_exe("oxide-monitor.exe"),
        )
    }

    /// Connect to a specific pipe with an explicit peer verification policy.
    pub fn connect(
        pipe_name: &str,
        timeout: Duration,
        policy: &PeerPolicy,
    ) -> Result<Self, IpcError> {
        let conn = connect_with_retry(pipe_name, timeout, policy)?;
        Ok(Self { conn })
    }

    pub fn server_pid(&self) -> u32 {
        self.conn.peer_pid()
    }

    pub fn raw_handle(&self) -> windows::Win32::Foundation::HANDLE {
        self.conn.raw()
    }

    /// Send a typed message to `oxide-monitor.exe`.
    pub fn send(&self, msg: &SettingsToMonitor) -> Result<(), IpcError> {
        let frame = encode_frame(msg)?;
        self.conn.write_all(&frame, None)
    }

    /// Send `Hello` with `IPC_PROTOCOL_VERSION` and wait for the server's `HelloAck` response.
    pub fn hello(&self, focus_tab: Option<String>) -> Result<MonitorToSettings, IpcError> {
        self.send(&SettingsToMonitor::Hello {
            protocol_version: IPC_PROTOCOL_VERSION,
            focus_tab,
        })?;
        self.recv_timeout(Some(Duration::from_secs(3)))
    }

    pub fn preview_config(&self, config: Config) -> Result<(), IpcError> {
        self.send(&SettingsToMonitor::PreviewConfig {
            config: Box::new(config),
        })
    }

    pub fn commit_config(
        &self,
        expected_generation: u64,
        expected_hash: u64,
        config: Config,
    ) -> Result<(), IpcError> {
        self.send(&SettingsToMonitor::CommitConfig {
            expected_generation,
            expected_hash,
            config: Box::new(config),
        })
    }

    pub fn revert_config(&self) -> Result<(), IpcError> {
        self.send(&SettingsToMonitor::RevertConfig)
    }

    /// Ask the monitor to launch/stop the optional elevated PawnIO helper.
    /// `start = true` is the only way to trigger the UAC prompt; it is never automatic.
    pub fn pawnio_control(&self, start: bool) -> Result<(), IpcError> {
        self.send(&SettingsToMonitor::PawnioControl { start })
    }

    pub fn ping(&self) -> Result<(), IpcError> {
        self.send(&SettingsToMonitor::Ping)
    }

    /// Receive the next message from `oxide-monitor.exe`, optionally bounded by `timeout`.
    pub fn recv_timeout(&self, timeout: Option<Duration>) -> Result<MonitorToSettings, IpcError> {
        let payload = self.conn.read_frame_timeout(timeout)?;
        decode_payload(&payload)
    }

    /// Send a raw pre-encoded frame (used in protocol boundary / fuzz / oversized frame tests).
    pub fn send_raw_bytes(&self, bytes: &[u8]) -> Result<(), IpcError> {
        self.conn.write_all(bytes, None)
    }
}
