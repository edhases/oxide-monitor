//! Non-blocking IPC coordinator, single-writer config transactions, monitor enumeration,
//! and HardwareInventory builder for `oxide-monitor.exe`.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Arc;

use oxide_core::{
    Config, ConfigError, HardwareInventory, HistoryStore, MonitorDescriptor, Snapshot, SnapshotDto,
};
use oxide_ipc::{
    CommitStatus, IPC_PROTOCOL_VERSION, IncomingCommand, IpcServer, IpcServerConfig,
    MonitorToSettings,
};
use tracing::{debug, info, warn};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::BOOL;

use crate::hot_reload::{ConfigWatcher, ReloadEvent};

/// Posted by the IPC server thread to `OxideOverlay` whenever incoming commands are queued.
pub const WM_OXIDE_IPC_CMD: u32 = WM_APP + 27;

const MONITORINFOF_PRIMARY: u32 = 1;

/// Outcome of polling `ConfigWatcher` through `ConfigCoordinator`.
#[derive(Debug)]
pub enum HotReloadOutcome {
    None,
    Apply(Box<Config>),
    LogInvalidOnce(String),
}

/// Single-writer transaction state owning `committed_config`, optimistic concurrency revision
/// (`generation`, `committed_hash`), preview rollback state, and hot-reload watcher.
pub struct ConfigCoordinator {
    committed_config: Config,
    generation: u64,
    committed_hash: u64,
    preview_active: bool,
    watcher: ConfigWatcher,
}

impl ConfigCoordinator {
    pub fn new(mut initial_config: Config, path: PathBuf) -> Self {
        initial_config.normalize();
        let canonical = initial_config.canonical_hash();
        let watcher = ConfigWatcher::new(path, canonical);
        let committed_hash = watcher.current_disk_hash().unwrap_or(canonical);
        Self {
            committed_config: initial_config,
            generation: 1,
            committed_hash,
            preview_active: false,
            watcher,
        }
    }

    pub fn committed_config(&self) -> &Config {
        &self.committed_config
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn committed_hash(&self) -> u64 {
        self.committed_hash
    }

    pub fn is_preview_active(&self) -> bool {
        self.preview_active
    }

    pub fn config_path(&self) -> &std::path::Path {
        self.watcher.path()
    }

    /// Persist a monitor-originated change (tray preset, lock position, Alt+Drag) atomically,
    /// bump generation, and suppress our own write in the hot-reload watcher.
    pub fn commit_local(&mut self, mut config: Config) -> Result<Config, ConfigError> {
        config.normalize();
        let hash = config.save_to_path_returning_hash(self.watcher.path())?;
        self.watcher.record_self_write(hash);
        self.watcher.set_paused(false);
        self.committed_config = config.clone();
        self.committed_hash = hash;
        self.generation = self.generation.wrapping_add(1);
        self.preview_active = false;
        Ok(config)
    }

    /// Enter or update an in-memory IPC live preview without writing `config.toml`.
    /// Pauses hot reload while previewing.
    pub fn begin_preview(&mut self, mut config: Config) -> Config {
        config.normalize();
        self.preview_active = true;
        self.watcher.set_paused(true);
        config
    }

    /// Validate optimistic concurrency (`expected_generation` + `expected_hash` + on-disk hash),
    /// normalize/clamp, write atomically via `ReplaceFileW` + `.bak`, and advance revision.
    pub fn commit_ipc(
        &mut self,
        expected_generation: u64,
        expected_hash: u64,
        mut config: Config,
    ) -> (CommitStatus, Option<Config>, Option<String>) {
        if expected_generation != self.generation || expected_hash != self.committed_hash {
            return (
                CommitStatus::Conflict,
                None,
                Some(format!(
                    "stale revision: expected gen={} hash={:#x}, current gen={} hash={:#x}",
                    expected_generation, expected_hash, self.generation, self.committed_hash
                )),
            );
        }

        // Also detect external file edits made while hot reload was paused during preview.
        if let Some(disk_hash) = self.watcher.current_disk_hash()
            && disk_hash != self.committed_hash
        {
            return (
                CommitStatus::Conflict,
                None,
                Some(format!(
                    "config.toml was modified externally on disk (disk hash={disk_hash:#x} != committed {:#x})",
                    self.committed_hash
                )),
            );
        }

        config.normalize();
        match config.save_to_path_returning_hash(self.watcher.path()) {
            Ok(new_hash) => {
                self.watcher.record_self_write(new_hash);
                self.watcher.set_paused(false);
                self.committed_config = config.clone();
                self.committed_hash = new_hash;
                self.generation = self.generation.wrapping_add(1);
                self.preview_active = false;
                (CommitStatus::Ok, Some(config), None)
            }
            Err(err) => (CommitStatus::Error, None, Some(err.to_string())),
        }
    }

    /// Revert an active IPC preview (or client disconnect without commit) back to `committed_config`
    /// and resume hot reload.
    pub fn revert_preview(&mut self) -> Option<Config> {
        self.watcher.set_paused(false);
        if self.preview_active {
            self.preview_active = false;
            Some(self.committed_config.clone())
        } else {
            None
        }
    }

    /// Poll `config.toml` for external edits (ignored while preview is active).
    pub fn poll_hot_reload(&mut self) -> HotReloadOutcome {
        match self.watcher.poll() {
            ReloadEvent::Unchanged | ReloadEvent::Paused => HotReloadOutcome::None,
            ReloadEvent::Reloaded { config, hash } => {
                self.committed_config = (*config).clone();
                self.committed_hash = hash;
                self.generation = self.generation.wrapping_add(1);
                HotReloadOutcome::Apply(config)
            }
            ReloadEvent::InvalidToml {
                error,
                already_logged,
                ..
            } => {
                if already_logged {
                    HotReloadOutcome::None
                } else {
                    HotReloadOutcome::LogInvalidOnce(error)
                }
            }
        }
    }

    /// Build a `HelloAck` response capturing the current committed revision and config.
    pub fn build_hello_ack(&self, monitors: Vec<MonitorDescriptor>) -> MonitorToSettings {
        let path = self.watcher.path();
        let portable_mode = path == Config::portable_path();
        MonitorToSettings::HelloAck {
            protocol_version: IPC_PROTOCOL_VERSION,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            arch: std::env::consts::ARCH.to_string(),
            config_path: path.to_string_lossy().to_string(),
            portable_mode,
            generation: self.generation,
            config_hash: self.committed_hash,
            active_config: Box::new(self.committed_config.clone()),
            monitors,
        }
    }
}

/// Manages the lazily spawned `IpcServer` lifecycle so idle `oxide-monitor.exe` keeps 0 extra threads.
pub struct IpcHost {
    server: Option<IpcServer>,
}

impl IpcHost {
    pub fn new() -> Self {
        Self { server: None }
    }

    pub fn is_connected(&self) -> bool {
        self.server.as_ref().is_some_and(|s| s.is_connected())
    }

    /// Start the Named Pipe server lazily if it is not already running.
    pub fn ensure_started(&mut self, overlay_hwnd: HWND) -> bool {
        self.reap_if_finished();
        if self.server.is_some() {
            return true;
        }
        let raw_hwnd = overlay_hwnd.0 as isize;
        let on_wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let hwnd = HWND(raw_hwnd as *mut c_void);
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_OXIDE_IPC_CMD, WPARAM(0), LPARAM(0));
            }
        });
        match IpcServer::start(IpcServerConfig::default(), on_wake) {
            Ok(server) => {
                info!(
                    "lazy IPC server started on {}",
                    oxide_ipc::DEFAULT_PIPE_NAME
                );
                self.server = Some(server);
                true
            }
            Err(err) => {
                warn!(%err, "failed to start lazy IPC server");
                false
            }
        }
    }

    pub fn drain_commands(&self) -> Vec<IncomingCommand> {
        self.server
            .as_ref()
            .map(|s| s.drain_commands())
            .unwrap_or_default()
    }

    pub fn send_control(&self, msg: MonitorToSettings) {
        if let Some(srv) = &self.server
            && !srv.send_control(msg)
        {
            debug!("IPC control queue full or closed; dropped message");
        }
    }

    pub fn send_telemetry(&self, snapshot: &Snapshot, history: &HistoryStore) {
        let Some(srv) = &self.server else {
            return;
        };
        if !srv.is_connected() {
            return;
        }
        let tick = MonitorToSettings::TelemetryTick {
            snapshot: SnapshotDto::from(snapshot),
            history: history.to_dto(),
            inventory: HardwareInventory::from_snapshot(snapshot),
        };
        let _ = srv.send_telemetry(tick);
    }

    pub fn send_monitors_changed(&self, monitors: Vec<MonitorDescriptor>) {
        if self.is_connected() {
            self.send_control(MonitorToSettings::MonitorsChanged { monitors });
        }
    }

    /// Join and release the server thread immediately after client disconnect or accept timeout.
    pub fn reap_if_finished(&mut self) {
        if self.server.as_ref().is_some_and(|s| s.is_finished()) {
            if let Some(srv) = self.server.take() {
                srv.stop_and_join();
                debug!("reaped finished IPC server thread (0 idle IPC threads)");
            }
        }
    }

    pub fn shutdown(&mut self) {
        if let Some(srv) = self.server.take() {
            srv.stop_and_join();
        }
    }
}

impl Drop for IpcHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Enumerate active Windows display monitors for `HelloAck` and `MonitorsChanged`.
pub fn enumerate_monitors() -> Vec<MonitorDescriptor> {
    let mut monitors: Vec<MonitorDescriptor> = Vec::new();
    let ptr = &mut monitors as *mut Vec<MonitorDescriptor>;
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(monitor_enum_proc), LPARAM(ptr as isize));
    }
    monitors
}

unsafe extern "system" fn monitor_enum_proc(
    hmon: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let list = unsafe { &mut *(data.0 as *mut Vec<MonitorDescriptor>) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(hmon, &mut info.monitorInfo as *mut _ as *mut _) }.as_bool() {
        let raw_name = &info.szDevice;
        let nul = raw_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(raw_name.len());
        let device = String::from_utf16_lossy(&raw_name[..nul]);
        let r = info.monitorInfo.rcMonitor;
        let is_primary = (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;
        let mut dpi_x = 96u32;
        let mut dpi_y = 96u32;
        if unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_err() {
            dpi_x = 96;
        }
        let friendly_name = if is_primary {
            format!("{device} (Primary)")
        } else {
            device.clone()
        };
        list.push(MonitorDescriptor {
            device,
            friendly_name,
            is_primary,
            dpi: dpi_x.max(48),
            width: r.right - r.left,
            height: r.bottom - r.top,
        });
    }
    BOOL(1)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use oxide_core::find_builtin_preset;
    use oxide_ipc::{IpcClient, PeerPolicy};

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("oxide-ipc-host-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn unique_pipe(tag: &str) -> String {
        static N: AtomicUsize = AtomicUsize::new(1);
        format!(
            r"\\.\pipe\oxide-ui-host-test-{}-{tag}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn coordinator_preview_conflict_commit_and_disconnect_rollback() {
        let dir = temp_dir("tx");
        let path = dir.join("config.toml");
        let initial = Config::default();
        initial.save_to_path(&path).unwrap();

        let mut coord = ConfigCoordinator::new(initial.clone(), path.clone());
        let gen1 = coord.generation();
        let hash1 = coord.committed_hash();
        assert_eq!(gen1, 1);

        // 1. Preview pauses hot reload and does NOT touch disk.
        let mut preview_cfg = initial.clone();
        preview_cfg.surface.opacity = 160;
        let applied_preview = coord.begin_preview(preview_cfg.clone());
        assert_eq!(applied_preview.surface.opacity, 160);
        assert!(coord.is_preview_active());
        assert!(matches!(coord.poll_hot_reload(), HotReloadOutcome::None));
        assert_eq!(
            Config::load_from_path(&path).unwrap().surface.opacity,
            initial.surface.opacity
        );

        // 2. Stale generation Commit -> Conflict, disk still untouched.
        let (st, applied, err) = coord.commit_ipc(gen1 + 10, hash1, preview_cfg.clone());
        assert_eq!(st, CommitStatus::Conflict);
        assert!(applied.is_none());
        assert!(err.is_some());

        // 3. Valid Commit -> Ok, writes atomically via ReplaceFileW + creates .bak, bumps generation.
        let (st, applied, err) = coord.commit_ipc(gen1, hash1, preview_cfg.clone());
        assert_eq!(st, CommitStatus::Ok, "unexpected commit error: {err:?}");
        assert_eq!(applied.unwrap().surface.opacity, 160);
        assert!(!coord.is_preview_active());
        assert_eq!(coord.generation(), gen1 + 1);
        assert!(path.with_extension("toml.bak").exists());
        // Own write must NOT trigger hot reload.
        assert!(matches!(coord.poll_hot_reload(), HotReloadOutcome::None));

        // 4. Preview + external disk edit -> Commit returns Conflict so external edit is preserved.
        let gen2 = coord.generation();
        let hash2 = coord.committed_hash();
        let mut preview2 = coord.committed_config().clone();
        preview2.surface.opacity = 120;
        coord.begin_preview(preview2.clone());

        let mut external = coord.committed_config().clone();
        external.surface.opacity = 210;
        external.save_to_path(&path).unwrap();

        let (st, _, _) = coord.commit_ipc(gen2, hash2, preview2);
        assert_eq!(st, CommitStatus::Conflict);

        // 5. Disconnect rollback restores committed_config and resumes hot reload (picking up external edit).
        let rolled_back = coord.revert_preview().unwrap();
        assert_eq!(rolled_back.surface.opacity, 160);
        assert!(!coord.is_preview_active());

        match coord.poll_hot_reload() {
            HotReloadOutcome::Apply(reloaded) => assert_eq!(reloaded.surface.opacity, 210),
            other => panic!("expected hot reload after resume, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn end_to_end_named_pipe_disconnect_triggers_preview_rollback() {
        let dir = temp_dir("e2e");
        let path = dir.join("config.toml");
        let initial = Config::default();
        initial.save_to_path(&path).unwrap();

        let mut coord = ConfigCoordinator::new(initial.clone(), path.clone());
        let pipe = unique_pipe("rollback");
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
            .send(&oxide_ipc::SettingsToMonitor::Hello {
                protocol_version: IPC_PROTOCOL_VERSION,
                focus_tab: None,
            })
            .unwrap();

        // Apply preset in preview only.
        let mut preset_cfg = initial.clone();
        find_builtin_preset("minimal-1row")
            .unwrap()
            .apply_to(&mut preset_cfg);
        client.preview_config(preset_cfg).unwrap();

        // Process Hello + Preview on server side.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut saw_preview = false;
        while Instant::now() < deadline && !saw_preview {
            for cmd in srv.drain_commands() {
                match cmd {
                    IncomingCommand::Hello { .. } => {
                        assert!(srv.send_control(coord.build_hello_ack(vec![])));
                    }
                    IncomingCommand::PreviewConfig(cfg) => {
                        let applied = coord.begin_preview(*cfg);
                        assert_eq!(applied.general.active_preset, "minimal-1row");
                        saw_preview = true;
                    }
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_preview);
        assert!(coord.is_preview_active());

        // Client crashes / closes without Commit -> Disconnected command must roll back to classic-2row.
        drop(client);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut rolled_back_preset = None;
        while Instant::now() < deadline && rolled_back_preset.is_none() {
            for cmd in srv.drain_commands() {
                if matches!(cmd, IncomingCommand::Disconnected) {
                    if let Some(cfg) = coord.revert_preview() {
                        rolled_back_preset = Some(cfg.general.active_preset);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(rolled_back_preset.as_deref(), Some("classic-2row"));
        assert!(!coord.is_preview_active());

        srv.stop_and_join();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_connect_disconnect_cycles_report_disconnected_and_finish() {
        let dir = temp_dir("cycles");
        let path = dir.join("config.toml");
        let initial = Config::default();
        initial.save_to_path(&path).unwrap();
        let mut coord = ConfigCoordinator::new(initial, path.clone());

        for cycle in 0..3 {
            let pipe = unique_pipe(&format!("cycle{cycle}"));
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
                .send(&oxide_ipc::SettingsToMonitor::Hello {
                    protocol_version: IPC_PROTOCOL_VERSION,
                    focus_tab: None,
                })
                .unwrap();

            let deadline = Instant::now() + Duration::from_secs(2);
            let mut saw_hello = false;
            while Instant::now() < deadline && !saw_hello {
                for cmd in srv.drain_commands() {
                    if matches!(cmd, IncomingCommand::Hello { .. }) {
                        saw_hello = true;
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(saw_hello, "cycle {cycle}: Hello was not delivered");

            drop(client);

            let deadline = Instant::now() + Duration::from_secs(3);
            let mut disconnected = false;
            while Instant::now() < deadline && !disconnected {
                for cmd in srv.drain_commands() {
                    if matches!(cmd, IncomingCommand::Disconnected) {
                        // No preview was started, so rollback must be a no-op.
                        assert!(coord.revert_preview().is_none());
                        disconnected = true;
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                disconnected,
                "cycle {cycle}: Disconnected command was not delivered"
            );

            let deadline = Instant::now() + Duration::from_secs(2);
            while !srv.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                srv.is_finished(),
                "cycle {cycle}: IPC server thread did not finish after disconnect"
            );
            srv.stop_and_join();
        }

        assert!(!coord.is_preview_active());
        let _ = fs::remove_dir_all(&dir);
    }
}
