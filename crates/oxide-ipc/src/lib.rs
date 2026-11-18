//! Hardened Windows Named Pipe IPC between `oxide-monitor.exe` and `oxide-settings.exe`.
//!
//! Zero GUI / Direct2D / egui dependencies so both binaries stay cleanly decoupled.

pub mod client;
pub mod pipe;
pub mod protocol;
pub mod security;
pub mod server;

use thiserror::Error;

pub use client::IpcClient;
pub use pipe::{MultiplexedFrameReader, WinEvent};
pub use protocol::{
    CommitStatus, ConfigRevision, DEFAULT_PIPE_NAME, IPC_PROTOCOL_VERSION, MAX_IPC_FRAME_BYTES,
    MonitorToSettings, SettingsToMonitor, decode_frame_len, decode_payload, encode_frame,
};
pub use security::{PeerPolicy, current_user_sid, validate_sid};
pub use server::{IncomingCommand, IpcServer, IpcServerConfig};

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("Named Pipe already in use: {0}")]
    PipeAlreadyInUse(String),
    #[error("IPC peer rejected: {0}")]
    PeerRejected(String),
    #[error("IPC security setup failed: {0}")]
    Security(String),
    #[error("IPC protocol version mismatch: got {got}, expected {expected}")]
    VersionMismatch { got: u32, expected: u32 },
    #[error("IPC frame size {len} exceeds valid bounds 1..={max}")]
    FrameTooLarge { len: usize, max: usize },
    #[error("IPC protocol error: {0}")]
    Protocol(String),
    #[error("IPC peer disconnected")]
    Disconnected,
    #[error("IPC operation timed out")]
    Timeout,
    #[error("IPC server stopped")]
    Stopped,
    #[error("Win32 {op} failed: {detail}")]
    Win32 { op: &'static str, detail: String },
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O error: {0}")]
    Io(String),
}

impl IpcError {
    pub(crate) fn win32(op: &'static str, err: windows::core::Error) -> Self {
        Self::Win32 {
            op,
            detail: err.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use oxide_core::{Config, HardwareInventory, HistorySliceDto, SnapshotDto};

    use super::*;

    fn unique_pipe(tag: &str) -> String {
        static COUNTER: AtomicUsize = AtomicUsize::new(1);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!(r"\\.\pipe\oxide-ui-test-{}-{tag}-{n}", std::process::id())
    }

    fn wait_for_cmd(server: &IpcServer, timeout: Duration) -> Option<IncomingCommand> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let cmds = server.drain_commands();
            if let Some(first) = cmds.into_iter().next() {
                return Some(first);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn pipe_collision_is_rejected_by_first_pipe_instance() {
        let pipe = unique_pipe("collision");
        let cfg = IpcServerConfig {
            pipe_name: pipe.clone(),
            peer_policy: PeerPolicy::SameProcessOnly,
            accept_timeout: Duration::from_secs(2),
        };
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let srv1 = IpcServer::start(cfg.clone(), Arc::clone(&wake)).unwrap();
        let err = IpcServer::start(cfg, wake).err().unwrap();
        assert!(
            matches!(err, IpcError::PipeAlreadyInUse(_)),
            "expected PipeAlreadyInUse, got {err:?}"
        );
        srv1.stop_and_join();
    }

    #[test]
    fn full_hello_preview_commit_conflict_revert_and_disconnect_flow() {
        let pipe = unique_pipe("full");
        let wakes = Arc::new(AtomicUsize::new(0));
        let wakes_cb = Arc::clone(&wakes);
        let srv = IpcServer::start(
            IpcServerConfig {
                pipe_name: pipe.clone(),
                peer_policy: PeerPolicy::SameProcessOnly,
                accept_timeout: Duration::from_secs(5),
            },
            Arc::new(move || {
                wakes_cb.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap();

        let client =
            IpcClient::connect(&pipe, Duration::from_secs(2), &PeerPolicy::SameProcessOnly)
                .unwrap();

        // Ping/Pong handled directly in server thread.
        client.ping().unwrap();
        assert_eq!(
            client.recv_timeout(Some(Duration::from_secs(2))).unwrap(),
            MonitorToSettings::Pong
        );

        // Hello handshake + HelloAck with generation/hash.
        client
            .send(&SettingsToMonitor::Hello {
                protocol_version: IPC_PROTOCOL_VERSION,
                focus_tab: Some("presets".into()),
            })
            .unwrap();
        let cmd = wait_for_cmd(&srv, Duration::from_secs(2)).unwrap();
        assert!(matches!(
            cmd,
            IncomingCommand::Hello {
                focus_tab: Some(ref t)
            } if t == "presets"
        ));

        let active = Config::default();
        let initial_hash = active.canonical_hash();
        assert!(srv.send_control(MonitorToSettings::HelloAck {
            protocol_version: IPC_PROTOCOL_VERSION,
            app_version: "0.1.0".into(),
            arch: "x86_64".into(),
            config_path: r"C:\test\config.toml".into(),
            portable_mode: false,
            generation: 1,
            config_hash: initial_hash,
            active_config: Box::new(active.clone()),
            monitors: vec![],
        }));

        let ack = client.recv_timeout(Some(Duration::from_secs(2))).unwrap();
        assert!(matches!(
            ack,
            MonitorToSettings::HelloAck {
                generation: 1,
                config_hash,
                ..
            } if config_hash == initial_hash
        ));

        // Telemetry tick pushed over bounded channel.
        assert!(srv.send_telemetry(MonitorToSettings::TelemetryTick {
            snapshot: SnapshotDto::default(),
            history: HistorySliceDto::default(),
            inventory: HardwareInventory::default(),
        }));
        let tick = client.recv_timeout(Some(Duration::from_secs(2))).unwrap();
        assert!(matches!(tick, MonitorToSettings::TelemetryTick { .. }));

        // PreviewConfig.
        let mut preview_cfg = active.clone();
        preview_cfg.surface.opacity = 180;
        client.preview_config(preview_cfg.clone()).unwrap();
        let cmd = wait_for_cmd(&srv, Duration::from_secs(2)).unwrap();
        assert!(matches!(
            cmd,
            IncomingCommand::PreviewConfig(ref c) if c.surface.opacity == 180
        ));

        // CommitConfig (stale generation -> Conflict).
        client.commit_config(999, 0, preview_cfg.clone()).unwrap();
        let cmd = wait_for_cmd(&srv, Duration::from_secs(2)).unwrap();
        assert!(matches!(
            cmd,
            IncomingCommand::CommitConfig {
                expected_generation: 999,
                ..
            }
        ));
        assert!(srv.send_control(MonitorToSettings::CommitResult {
            status: CommitStatus::Conflict,
            generation: 1,
            config_hash: initial_hash,
            error: Some("stale generation".into()),
        }));
        let res = client.recv_timeout(Some(Duration::from_secs(2))).unwrap();
        assert!(matches!(
            res,
            MonitorToSettings::CommitResult {
                status: CommitStatus::Conflict,
                ..
            }
        ));

        // RevertConfig.
        client.revert_config().unwrap();
        let cmd = wait_for_cmd(&srv, Duration::from_secs(2)).unwrap();
        assert!(matches!(cmd, IncomingCommand::RevertConfig));

        // Disconnect triggers Disconnected command + clean server exit.
        drop(client);
        let cmd = wait_for_cmd(&srv, Duration::from_secs(2)).unwrap();
        assert!(matches!(cmd, IncomingCommand::Disconnected));

        let deadline = Instant::now() + Duration::from_secs(2);
        while !srv.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(srv.is_finished());
        assert!(wakes.load(Ordering::SeqCst) >= 5);
        srv.stop_and_join();
    }

    #[test]
    fn pawnio_control_roundtrips_over_ipc() {
        let msg = SettingsToMonitor::PawnioControl { start: true };
        let frame = encode_frame(&msg).unwrap();
        let back: SettingsToMonitor = decode_payload(&frame[4..]).unwrap();
        assert_eq!(back, msg);

        let stop = SettingsToMonitor::PawnioControl { start: false };
        let frame = encode_frame(&stop).unwrap();
        let back: SettingsToMonitor = decode_payload(&frame[4..]).unwrap();
        assert_eq!(back, stop);
    }

    #[test]
    fn version_mismatch_and_oversized_frame_are_rejected() {
        let pipe = unique_pipe("version");
        let srv = IpcServer::start(
            IpcServerConfig {
                pipe_name: pipe.clone(),
                peer_policy: PeerPolicy::SameProcessOnly,
                accept_timeout: Duration::from_secs(3),
            },
            Arc::new(|| {}),
        )
        .unwrap();

        let client =
            IpcClient::connect(&pipe, Duration::from_secs(2), &PeerPolicy::SameProcessOnly)
                .unwrap();
        client
            .send(&SettingsToMonitor::Hello {
                protocol_version: 999,
                focus_tab: None,
            })
            .unwrap();
        let reply = client.recv_timeout(Some(Duration::from_secs(2))).unwrap();
        assert!(matches!(
            reply,
            MonitorToSettings::ProtocolError { ref code, .. } if code == "version_mismatch"
        ));
        srv.stop_and_join();

        // Oversized frame length header (> 2 MiB) is rejected by decoder and server.
        assert!(decode_frame_len(((MAX_IPC_FRAME_BYTES + 1) as u32).to_le_bytes()).is_err());
        assert!(decode_frame_len(0u32.to_le_bytes()).is_err());
    }

    #[test]
    fn lazy_server_cycles_leave_zero_handle_leak() {
        use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

        let run_cycle = |tag: &str| {
            let pipe = unique_pipe(tag);
            let srv = IpcServer::start(
                IpcServerConfig {
                    pipe_name: pipe.clone(),
                    peer_policy: PeerPolicy::SameProcessOnly,
                    accept_timeout: Duration::from_secs(3),
                },
                Arc::new(|| {}),
            )
            .unwrap();
            let client =
                IpcClient::connect(&pipe, Duration::from_secs(2), &PeerPolicy::SameProcessOnly)
                    .unwrap();
            client.ping().unwrap();
            assert_eq!(
                client.recv_timeout(Some(Duration::from_secs(2))).unwrap(),
                MonitorToSettings::Pong
            );
            drop(client);
            let deadline = Instant::now() + Duration::from_secs(2);
            while !srv.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(srv.is_finished());
            srv.stop_and_join();
        };

        // Warm-up cycle for any one-time OS loader handles.
        run_cycle("warmup");

        let mut h_before = 0u32;
        unsafe {
            let _ = GetProcessHandleCount(GetCurrentProcess(), &mut h_before);
        }

        for i in 0..5 {
            run_cycle(&format!("leak{i}"));
        }

        let mut h_after = 0u32;
        unsafe {
            let _ = GetProcessHandleCount(GetCurrentProcess(), &mut h_after);
        }

        // Allow tiny noise from parallel test threads, but 5 full pipe+event+thread cycles
        // would leak 25+ handles if any pipe/event/token handle were dropped improperly.
        assert!(
            h_after <= h_before + 4,
            "handle leak detected across 5 IPC cycles: before={h_before}, after={h_after}"
        );
    }
}
