//! Win32 taskbar overlay, Direct2D/ULW renderer with GDI fallback, Flyout,
//! anchor modes + Alt+Drag, event-driven Auto-hide, and Game Mode.

mod actions;
mod flyout_window;
mod geometry;
mod hooks;
mod hot_reload;
mod ipc_host;
mod layout;
mod per_core;
mod render;
mod tray;
mod window;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};

use oxide_core::{Config, PawnioCommand, ProvidersConfig, Snapshot};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum UiError {
    #[error("win32 error: {0}")]
    Win32(String),
    #[error("ui error: {0}")]
    Other(String),
}

/// Custom message used by a second `oxide-monitor.exe` launch to reveal the widget.
pub const SHOW_MESSAGE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 10;

/// Custom message posted by a directly launched `oxide-settings.exe` to lazily start
/// `\\.\pipe\oxide-ui-v1` inside `oxide-monitor.exe` before connecting.
pub const WM_OXIDE_OPEN_IPC: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 11;

impl UiError {
    pub(crate) fn win32(call: &str, error: impl std::fmt::Display) -> Self {
        Self::Win32(format!("{call}: {error}"))
    }
}

pub struct Ui {
    config: Config,
    snapshot: Arc<Mutex<Snapshot>>,
    provider_config: Option<Arc<Mutex<ProvidersConfig>>>,
    pawnio_commands: Option<mpsc::Sender<PawnioCommand>>,
    game_mode_flag: Option<Arc<AtomicBool>>,
}

impl Ui {
    pub fn new(config: Config, snapshot: Arc<Mutex<Snapshot>>) -> Result<Self, UiError> {
        Ok(Self {
            config,
            snapshot,
            provider_config: None,
            pawnio_commands: None,
            game_mode_flag: None,
        })
    }

    /// Share the live provider configuration with the sensor thread. The UI rewrites it
    /// from `apply_config` after Settings Apply / hot reload so provider toggles take
    /// effect without restarting the monitor. Optional: callers that do not provide a
    /// handle keep the startup snapshot behaviour.
    pub fn set_provider_config_handle(&mut self, handle: Arc<Mutex<ProvidersConfig>>) {
        self.provider_config = Some(handle);
    }

    /// Route explicit Start/Stop actions from Settings to the monitor's PawnIO manager.
    pub fn set_pawnio_command_sender(&mut self, sender: mpsc::Sender<PawnioCommand>) {
        self.pawnio_commands = Some(sender);
    }

    /// Publish `manual_game_mode || auto_game_hidden` so the PawnIO manager can stop
    /// the elevated helper while a game is running.
    pub fn set_game_mode_handle(&mut self, handle: Arc<AtomicBool>) {
        self.game_mode_flag = Some(handle);
    }

    pub fn run(self) -> Result<(), UiError> {
        window::run(
            self.config,
            self.snapshot,
            self.provider_config,
            self.pawnio_commands,
            self.game_mode_flag,
        )
    }
}
