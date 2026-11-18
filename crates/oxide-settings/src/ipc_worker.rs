//! Zero-polling overlapped background IPC client worker speaking `oxide_ipc::IpcClient`.
//!
//! Uses `WaitForMultipleObjects([pipe_read_event, req_event], INFINITE)` so idle CPU
//! overhead is 0% (no polling timer, no per-tick event/CancelIoEx churn).

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use oxide_core::Config;
use oxide_ipc::{IpcClient, MonitorToSettings, MultiplexedFrameReader, WinEvent, decode_payload};
use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::System::Threading::{INFINITE, WaitForMultipleObjects};

use crate::win32_host::notify_monitor_open_ipc;

/// Commands sent from the UI thread to the IPC worker.
#[derive(Debug)]
pub enum IpcUiRequest {
    Preview(Box<Config>),
    Commit {
        expected_generation: u64,
        expected_hash: u64,
        config: Box<Config>,
    },
    Revert,
    /// Explicit user action for the optional elevated PawnIO helper (UAC on `start`).
    PawnioControl {
        start: bool,
    },
    Reconnect,
    Shutdown,
}

/// Events pushed from the IPC worker back to the UI thread.
#[derive(Debug)]
pub enum IpcUiEvent {
    Connected(Box<MonitorToSettings>),
    Message(Box<MonitorToSettings>),
    Standalone(String),
    Disconnected(String),
}

/// Handle owned by `SettingsApp` for asynchronous zero-polling IPC communication.
pub struct IpcWorker {
    req_tx: Sender<IpcUiRequest>,
    req_event: Arc<WinEvent>,
    evt_rx: Receiver<IpcUiEvent>,
    join: Option<JoinHandle<()>>,
}

impl IpcWorker {
    pub fn spawn(focus_tab: Option<String>, ctx: eframe::egui::Context) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<IpcUiRequest>();
        let (evt_tx, evt_rx) = mpsc::channel::<IpcUiEvent>();
        let req_event =
            Arc::new(WinEvent::new_manual_reset().expect("CreateEventW for IPC worker failed"));
        let worker_event = Arc::clone(&req_event);

        let join = thread::Builder::new()
            .name("oxide-settings-ipc".into())
            .spawn(move || worker_main(focus_tab, req_rx, worker_event, evt_tx, ctx))
            .ok();

        Self {
            req_tx,
            req_event,
            evt_rx,
            join,
        }
    }

    pub fn send(&self, req: IpcUiRequest) {
        let _ = self.req_tx.send(req);
        self.req_event.signal();
    }

    pub fn drain_events(&self) -> Vec<IpcUiEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.evt_rx.try_recv() {
            out.push(ev);
        }
        out
    }

    pub fn shutdown(mut self) {
        let _ = self.req_tx.send(IpcUiRequest::Shutdown);
        self.req_event.signal();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for IpcWorker {
    fn drop(&mut self) {
        let _ = self.req_tx.send(IpcUiRequest::Shutdown);
        self.req_event.signal();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn worker_main(
    focus_tab: Option<String>,
    req_rx: Receiver<IpcUiRequest>,
    req_event: Arc<WinEvent>,
    evt_tx: Sender<IpcUiEvent>,
    ctx: eframe::egui::Context,
) {
    let mut current_focus = focus_tab;

    loop {
        match connect_and_handshake(current_focus.take()) {
            Ok((client, hello_ack)) => {
                let _ = evt_tx.send(IpcUiEvent::Connected(Box::new(hello_ack)));
                ctx.request_repaint();

                let keep_running = pump_connected(&client, &req_rx, &req_event, &evt_tx, &ctx);
                if !keep_running {
                    return;
                }
            }
            Err(reason) => {
                let _ = evt_tx.send(IpcUiEvent::Standalone(reason));
                ctx.request_repaint();

                if !wait_for_reconnect_or_exit(&req_rx, &req_event) {
                    return;
                }
            }
        }
    }
}

fn connect_and_handshake(
    focus_tab: Option<String>,
) -> Result<(IpcClient, MonitorToSettings), String> {
    let _ = notify_monitor_open_ipc();

    let client = IpcClient::connect_default(Duration::from_secs(2))
        .map_err(|err| format!("IPC unavailable ({err})"))?;

    let ack = client
        .hello(focus_tab)
        .map_err(|err| format!("IPC Hello failed ({err})"))?;

    match ack {
        MonitorToSettings::HelloAck { .. } => Ok((client, ack)),
        MonitorToSettings::ProtocolError { code, message } => {
            Err(format!("protocol error ({code}): {message}"))
        }
        other => Err(format!("unexpected handshake reply: {other:?}")),
    }
}

/// Zero-polling overlapped event loop multiplexing incoming pipe frames and UI requests.
/// Returns `true` to reconnect, `false` to exit cleanly.
fn pump_connected(
    client: &IpcClient,
    req_rx: &Receiver<IpcUiRequest>,
    req_event: &WinEvent,
    evt_tx: &Sender<IpcUiEvent>,
    ctx: &eframe::egui::Context,
) -> bool {
    let mut reader = match MultiplexedFrameReader::new(client.raw_handle()) {
        Ok(r) => r,
        Err(err) => {
            let _ = evt_tx.send(IpcUiEvent::Disconnected(err.to_string()));
            ctx.request_repaint();
            return wait_for_reconnect_or_exit(req_rx, req_event);
        }
    };

    let mut last_telemetry_repaint = Instant::now() - Duration::from_secs(5);

    loop {
        // 1. Drain any ready incoming frames and arm the overlapped ReadFile.
        loop {
            match reader.poll_or_arm() {
                Ok(Some(payload)) => match decode_payload::<MonitorToSettings>(&payload) {
                    Ok(msg) => {
                        let is_tick = matches!(msg, MonitorToSettings::TelemetryTick { .. });
                        let _ = evt_tx.send(IpcUiEvent::Message(Box::new(msg)));
                        if !is_tick {
                            ctx.request_repaint();
                        } else if last_telemetry_repaint.elapsed() >= Duration::from_millis(950) {
                            last_telemetry_repaint = Instant::now();
                            ctx.request_repaint();
                        }
                    }
                    Err(err) => {
                        let _ = evt_tx.send(IpcUiEvent::Disconnected(err.to_string()));
                        ctx.request_repaint();
                        return wait_for_reconnect_or_exit(req_rx, req_event);
                    }
                },
                Ok(None) => break,
                Err(err) => {
                    let _ = evt_tx.send(IpcUiEvent::Disconnected(err.to_string()));
                    ctx.request_repaint();
                    return wait_for_reconnect_or_exit(req_rx, req_event);
                }
            }
        }

        // 2. Drain any pending UI commands (coalescing rapid PreviewConfig messages).
        req_event.reset();
        let mut pending_preview: Option<Box<Config>> = None;
        let mut immediate_cmds: Vec<IpcUiRequest> = Vec::new();
        loop {
            match req_rx.try_recv() {
                Ok(IpcUiRequest::Preview(cfg)) => pending_preview = Some(cfg),
                Ok(IpcUiRequest::Shutdown) => return false,
                Ok(IpcUiRequest::Reconnect) => return true,
                Ok(other) => immediate_cmds.push(other),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return false,
            }
        }

        if let Some(cfg) = pending_preview
            && let Err(err) = client.preview_config(*cfg)
        {
            let _ = evt_tx.send(IpcUiEvent::Disconnected(err.to_string()));
            ctx.request_repaint();
            return wait_for_reconnect_or_exit(req_rx, req_event);
        }

        for cmd in immediate_cmds {
            let res = match cmd {
                IpcUiRequest::Commit {
                    expected_generation,
                    expected_hash,
                    config,
                } => client.commit_config(expected_generation, expected_hash, *config),
                IpcUiRequest::Revert => client.revert_config(),
                IpcUiRequest::PawnioControl { start } => client.pawnio_control(start),
                _ => Ok(()),
            };
            if let Err(err) = res {
                let _ = evt_tx.send(IpcUiEvent::Disconnected(err.to_string()));
                ctx.request_repaint();
                return wait_for_reconnect_or_exit(req_rx, req_event);
            }
        }

        // 3. Sleep at 0% CPU until either incoming pipe bytes arrive or UI posts a command.
        let handles = [reader.event_handle(), req_event.raw()];
        let wait = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
        if wait != WAIT_OBJECT_0 && wait.0 != WAIT_OBJECT_0.0 + 1 {
            return false;
        }
    }
}

fn wait_for_reconnect_or_exit(req_rx: &Receiver<IpcUiRequest>, req_event: &WinEvent) -> bool {
    loop {
        req_event.reset();
        match req_rx.recv() {
            Ok(IpcUiRequest::Reconnect) => return true,
            Ok(IpcUiRequest::Shutdown) | Err(_) => return false,
            Ok(_) => {}
        }
    }
}
