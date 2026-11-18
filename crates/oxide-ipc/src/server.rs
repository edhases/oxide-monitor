//! Lazy, non-blocking single-thread Named Pipe IPC server for `oxide-monitor.exe`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use oxide_core::Config;
use tracing::{debug, info, warn};
use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::System::Threading::{INFINITE, WaitForMultipleObjects};

use crate::IpcError;
use crate::pipe::{MultiplexedFrameReader, PipeConnection, PipeListener, WinEvent};
use crate::protocol::{
    DEFAULT_PIPE_NAME, IPC_PROTOCOL_VERSION, MonitorToSettings, SettingsToMonitor, decode_payload,
    encode_frame,
};
use crate::security::PeerPolicy;

/// Configuration for starting a lazy `IpcServer`.
#[derive(Clone, Debug)]
pub struct IpcServerConfig {
    pub pipe_name: String,
    pub peer_policy: PeerPolicy,
    pub accept_timeout: Duration,
}

impl Default for IpcServerConfig {
    fn default() -> Self {
        Self {
            pipe_name: DEFAULT_PIPE_NAME.to_string(),
            peer_policy: PeerPolicy::sibling_exe("oxide-settings.exe"),
            accept_timeout: Duration::from_secs(10),
        }
    }
}

/// Commands delivered asynchronously from the IPC thread to the UI message loop.
#[derive(Clone, Debug)]
pub enum IncomingCommand {
    Hello {
        focus_tab: Option<String>,
    },
    PreviewConfig(Box<Config>),
    CommitConfig {
        expected_generation: u64,
        expected_hash: u64,
        config: Box<Config>,
    },
    RevertConfig,
    PawnioControl {
        start: bool,
    },
    Disconnected,
}

/// Owns the lazily spawned Named Pipe server thread and non-blocking queues.
pub struct IpcServer {
    commands: Arc<Mutex<VecDeque<IncomingCommand>>>,
    control_tx: SyncSender<MonitorToSettings>,
    telemetry_tx: SyncSender<MonitorToSettings>,
    send_event: Arc<WinEvent>,
    stop_event: Arc<WinEvent>,
    connected: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<()>>,
}

impl IpcServer {
    /// Bind the Named Pipe synchronously (failing immediately with `PipeAlreadyInUse` on collision)
    /// and spawn the single overlapped I/O worker thread.
    pub fn start(
        cfg: IpcServerConfig,
        on_wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, IpcError> {
        let listener = PipeListener::bind(&cfg.pipe_name)?;
        let send_event = Arc::new(WinEvent::new_manual_reset()?);
        let stop_event = Arc::new(WinEvent::new_manual_reset()?);
        let commands = Arc::new(Mutex::new(VecDeque::new()));
        let connected = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));

        let (control_tx, control_rx) = mpsc::sync_channel::<MonitorToSettings>(16);
        let (telemetry_tx, telemetry_rx) = mpsc::sync_channel::<MonitorToSettings>(2);

        let worker_commands = Arc::clone(&commands);
        let worker_send = Arc::clone(&send_event);
        let worker_stop = Arc::clone(&stop_event);
        let worker_connected = Arc::clone(&connected);
        let worker_finished = Arc::clone(&finished);

        let join_handle = thread::Builder::new()
            .name("oxide-ipc-server".into())
            .spawn(move || {
                run_server_worker(
                    listener,
                    cfg,
                    worker_commands,
                    control_rx,
                    telemetry_rx,
                    worker_send,
                    worker_stop,
                    worker_connected,
                    on_wake,
                );
                worker_finished.store(true, Ordering::SeqCst);
            })
            .map_err(|err| IpcError::Io(err.to_string()))?;

        Ok(Self {
            commands,
            control_tx,
            telemetry_tx,
            send_event,
            stop_event,
            connected,
            finished,
            join_handle: Some(join_handle),
        })
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// Drain all queued commands without blocking.
    pub fn drain_commands(&self) -> Vec<IncomingCommand> {
        match self.commands.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(poisoned) => poisoned.into_inner().drain(..).collect(),
        }
    }

    /// Send a control response (`HelloAck`, `CommitResult`, `MonitorsChanged`, `FocusTab`)
    /// without blocking the UI thread.
    pub fn send_control(&self, msg: MonitorToSettings) -> bool {
        if self.finished.load(Ordering::Acquire) {
            return false;
        }
        let ok = self.control_tx.try_send(msg).is_ok();
        if ok {
            self.send_event.signal();
        }
        ok
    }

    /// Push a 1 Hz telemetry tick over a bounded channel; drops gracefully if the client lags.
    pub fn send_telemetry(&self, msg: MonitorToSettings) -> bool {
        if !self.is_connected() || self.finished.load(Ordering::Acquire) {
            return false;
        }
        let ok = self.telemetry_tx.try_send(msg).is_ok();
        if ok {
            self.send_event.signal();
        }
        ok
    }

    /// Signal the server thread to stop immediately and join it so 0 threads/handles remain.
    pub fn stop_and_join(mut self) {
        self.stop_event.signal();
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.stop_event.signal();
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_server_worker(
    listener: PipeListener,
    cfg: IpcServerConfig,
    commands: Arc<Mutex<VecDeque<IncomingCommand>>>,
    control_rx: Receiver<MonitorToSettings>,
    telemetry_rx: Receiver<MonitorToSettings>,
    send_event: Arc<WinEvent>,
    stop_event: Arc<WinEvent>,
    connected: Arc<AtomicBool>,
    on_wake: Arc<dyn Fn() + Send + Sync>,
) {
    debug!(pipe = %listener.pipe_name(), "IPC server listening lazily for settings client");

    let conn = match listener.accept(cfg.accept_timeout, &stop_event, &cfg.peer_policy) {
        Ok(c) => c,
        Err(IpcError::Timeout) => {
            debug!("IPC server accept timed out with no client; shutting down cleanly");
            return;
        }
        Err(IpcError::Stopped) => {
            debug!("IPC server stopped before client connected");
            return;
        }
        Err(err) => {
            warn!(%err, "IPC server rejected or failed to accept client");
            return;
        }
    };

    connected.store(true, Ordering::Release);
    info!(peer_pid = conn.peer_pid(), "IPC settings client connected");

    if let Err(err) = serve_connection(
        &conn,
        &commands,
        &control_rx,
        &telemetry_rx,
        &send_event,
        &stop_event,
        &on_wake,
    ) {
        match err {
            IpcError::Disconnected | IpcError::Stopped => {
                debug!("IPC settings client disconnected cleanly");
            }
            other => {
                warn!(%other, "IPC connection ended with error");
            }
        }
    }

    connected.store(false, Ordering::Release);
    push_command(&commands, IncomingCommand::Disconnected, &on_wake);
}

fn serve_connection(
    conn: &PipeConnection,
    commands: &Arc<Mutex<VecDeque<IncomingCommand>>>,
    control_rx: &Receiver<MonitorToSettings>,
    telemetry_rx: &Receiver<MonitorToSettings>,
    send_event: &WinEvent,
    stop_event: &WinEvent,
    on_wake: &Arc<dyn Fn() + Send + Sync>,
) -> Result<(), IpcError> {
    let mut reader = MultiplexedFrameReader::new(conn.raw())?;
    let mut saw_hello = false;

    loop {
        // 1. Drain any completed incoming frames and re-arm the overlapped ReadFile.
        while let Some(payload) = reader.poll_or_arm()? {
            let msg: SettingsToMonitor = match decode_payload(&payload) {
                Ok(m) => m,
                Err(err) => {
                    let _ = send_frame(
                        conn,
                        &MonitorToSettings::ProtocolError {
                            code: "invalid_payload".into(),
                            message: err.to_string(),
                        },
                        stop_event,
                    );
                    conn.flush();
                    return Err(err);
                }
            };

            match msg {
                SettingsToMonitor::Hello {
                    protocol_version,
                    focus_tab,
                } => {
                    if protocol_version != IPC_PROTOCOL_VERSION {
                        let _ = send_frame(
                            conn,
                            &MonitorToSettings::ProtocolError {
                                code: "version_mismatch".into(),
                                message: format!(
                                    "unsupported protocol version {protocol_version}, expected {IPC_PROTOCOL_VERSION}"
                                ),
                            },
                            stop_event,
                        );
                        conn.flush();
                        return Err(IpcError::VersionMismatch {
                            got: protocol_version,
                            expected: IPC_PROTOCOL_VERSION,
                        });
                    }
                    saw_hello = true;
                    push_command(commands, IncomingCommand::Hello { focus_tab }, on_wake);
                }
                SettingsToMonitor::Ping => {
                    send_frame(conn, &MonitorToSettings::Pong, stop_event)?;
                }
                other if !saw_hello => {
                    let _ = send_frame(
                        conn,
                        &MonitorToSettings::ProtocolError {
                            code: "handshake_required".into(),
                            message: format!("received {other:?} before Hello handshake"),
                        },
                        stop_event,
                    );
                    conn.flush();
                    return Err(IpcError::Protocol("message received before Hello".into()));
                }
                SettingsToMonitor::PreviewConfig { config } => {
                    push_command(commands, IncomingCommand::PreviewConfig(config), on_wake);
                }
                SettingsToMonitor::CommitConfig {
                    expected_generation,
                    expected_hash,
                    config,
                } => {
                    push_command(
                        commands,
                        IncomingCommand::CommitConfig {
                            expected_generation,
                            expected_hash,
                            config,
                        },
                        on_wake,
                    );
                }
                SettingsToMonitor::RevertConfig => {
                    push_command(commands, IncomingCommand::RevertConfig, on_wake);
                }
                SettingsToMonitor::PawnioControl { start } => {
                    push_command(commands, IncomingCommand::PawnioControl { start }, on_wake);
                }
            }
        }

        // 2. Drain pending outgoing control messages first, then telemetry ticks.
        send_event.reset();
        drain_outgoing(conn, control_rx, stop_event)?;
        drain_outgoing(conn, telemetry_rx, stop_event)?;

        // 3. Sleep zero-CPU until either incoming pipe bytes, outgoing queue, or stop signal.
        let handles = [reader.event_handle(), send_event.raw(), stop_event.raw()];
        let wait = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
        if wait == WAIT_OBJECT_0 {
            continue;
        } else if wait.0 == WAIT_OBJECT_0.0 + 1 {
            send_event.reset();
            drain_outgoing(conn, control_rx, stop_event)?;
            drain_outgoing(conn, telemetry_rx, stop_event)?;
        } else {
            return Err(IpcError::Stopped);
        }
    }
}

fn drain_outgoing(
    conn: &PipeConnection,
    rx: &Receiver<MonitorToSettings>,
    stop_event: &WinEvent,
) -> Result<(), IpcError> {
    loop {
        match rx.try_recv() {
            Ok(msg) => send_frame(conn, &msg, stop_event)?,
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => return Err(IpcError::Stopped),
        }
    }
}

fn send_frame(
    conn: &PipeConnection,
    msg: &MonitorToSettings,
    stop_event: &WinEvent,
) -> Result<(), IpcError> {
    let encoded = encode_frame(msg)?;
    conn.write_all(&encoded, Some(stop_event))
}

fn push_command(
    commands: &Arc<Mutex<VecDeque<IncomingCommand>>>,
    cmd: IncomingCommand,
    on_wake: &Arc<dyn Fn() + Send + Sync>,
) {
    match commands.lock() {
        Ok(mut q) => q.push_back(cmd),
        Err(poisoned) => poisoned.into_inner().push_back(cmd),
    }
    on_wake();
}
