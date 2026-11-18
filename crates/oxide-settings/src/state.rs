//! Pure, unit-testable state machine for `oxide-settings.exe`.
//!
//! Manages `draft` vs `committed` `Config` v2, optimistic concurrency (`generation` + `hash`),
//! live preview debouncing (never writes to disk), validation, Conflict handling, Cancel/Revert,
//! Reset-to-defaults preview, and Standalone direct-save mode.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use oxide_core::{
    AutostartStatusFile, Config, DiskSample, GpuSample, GpuVendor, HardwareInventory, HistoryStore,
    MonitorDescriptor, Snapshot, TempSource, find_builtin_preset, load_autostart_status,
};
use oxide_ipc::{CommitStatus, MonitorToSettings};

use crate::i18n::SettingsI18n;

/// Debounce window before sending `PreviewConfig` over IPC while dragging sliders.
pub const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(45);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    GeneralAndPresets,
    ItemsAndAliases,
    Sources,
    LayoutAndSurface,
    TypographyAndFormat,
    TaskbarAndInteractions,
    FlyoutAndGameMode,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 7] = [
        Self::GeneralAndPresets,
        Self::ItemsAndAliases,
        Self::Sources,
        Self::LayoutAndSurface,
        Self::TypographyAndFormat,
        Self::TaskbarAndInteractions,
        Self::FlyoutAndGameMode,
    ];

    pub fn i18n_key(self) -> &'static str {
        match self {
            Self::GeneralAndPresets => "nav-general",
            Self::ItemsAndAliases => "nav-items",
            Self::Sources => "nav-sources",
            Self::LayoutAndSurface => "nav-layout",
            Self::TypographyAndFormat => "nav-typography",
            Self::TaskbarAndInteractions => "nav-taskbar",
            Self::FlyoutAndGameMode => "nav-flyout",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "general" | "presets" => Some(Self::GeneralAndPresets),
            "items" | "aliases" => Some(Self::ItemsAndAliases),
            "sources" | "providers" => Some(Self::Sources),
            "layout" | "surface" => Some(Self::LayoutAndSurface),
            "typography" | "format" => Some(Self::TypographyAndFormat),
            "taskbar" | "interactions" => Some(Self::TaskbarAndInteractions),
            "flyout" | "game_mode" => Some(Self::FlyoutAndGameMode),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionMode {
    Connecting,
    Connected,
    Standalone { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusBanner {
    Info(String),
    Success(String),
    Warning(String),
    Error(String),
}

/// Core UI model holding the live `draft` `Config` alongside `committed` revision metadata.
pub struct SettingsState {
    pub draft: Config,
    pub committed: Config,
    pub generation: u64,
    pub committed_hash: u64,
    pub config_path: PathBuf,
    pub portable_mode: bool,
    pub connection: ConnectionMode,
    pub monitors: Vec<MonitorDescriptor>,
    pub snapshot: Snapshot,
    pub history: HistoryStore,
    pub inventory: HardwareInventory,
    pub has_live_telemetry: bool,
    pub active_tab: SettingsTab,
    pub conflict_detail: Option<String>,
    pub status: Option<StatusBanner>,
    pub preview_pending_since: Option<Instant>,
    pub preview_active_on_monitor: bool,
    pub commit_in_flight: bool,
    pub autostart_status: Option<AutostartStatusFile>,
    /// One-shot explicit UI request for the optional elevated PawnIO helper.
    /// `None` = nothing to send; consumed by the next frame that can reach the monitor.
    pub pending_pawnio_control: Option<bool>,
}

impl SettingsState {
    pub fn load_local(config_path: PathBuf) -> Self {
        let mut initial = Config::load_from_path(&config_path).unwrap_or_default();
        initial.normalize();
        let hash = initial.canonical_hash();
        let portable_mode = config_path == Config::portable_path();
        let (mock_snap, mock_hist) = sample_telemetry(0.35);
        let inventory = HardwareInventory::from_snapshot(&mock_snap);
        Self {
            draft: initial.clone(),
            committed: initial,
            generation: 1,
            committed_hash: hash,
            config_path,
            portable_mode,
            connection: ConnectionMode::Connecting,
            monitors: Vec::new(),
            snapshot: mock_snap,
            history: mock_hist,
            inventory,
            has_live_telemetry: false,
            active_tab: SettingsTab::GeneralAndPresets,
            conflict_detail: None,
            status: None,
            preview_pending_since: None,
            preview_active_on_monitor: false,
            commit_in_flight: false,
            autostart_status: load_autostart_status(),
            pending_pawnio_control: None,
        }
    }

    /// Reload `%APPDATA%\OxideMonitor\autostart.toml` (or portable sidecar) from disk.
    pub fn refresh_autostart_status(&mut self) {
        self.autostart_status = load_autostart_status();
    }

    /// Queue an explicit user action for the optional elevated PawnIO helper.
    ///
    /// Deliberately not driven by `providers.pawnio` alone: the helper must only be
    /// launched after a real click (and the resulting UAC consent).
    pub fn request_pawnio_control(&mut self, start: bool) {
        self.pending_pawnio_control = Some(start);
    }

    /// Take the pending PawnIO request, if any (one-shot: the next call returns `None`).
    pub fn take_pawnio_control(&mut self) -> Option<bool> {
        self.pending_pawnio_control.take()
    }

    pub fn is_dirty(&self) -> bool {
        self.draft != self.committed
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.connection, ConnectionMode::Connected)
    }

    /// Mark that a UI widget mutated `self.draft` — schedules debounced live preview.
    /// Never touches disk.
    pub fn mark_draft_changed(&mut self) {
        self.conflict_detail = None;
        self.status = None;
        if self.is_connected() {
            self.preview_pending_since = Some(Instant::now());
        }
    }

    /// Apply a built-in preset into `self.draft` ONLY (preview until Apply).
    pub fn apply_preset_preview(&mut self, preset_id: &str) -> bool {
        let Some(preset) = find_builtin_preset(preset_id) else {
            return false;
        };
        preset.apply_to(&mut self.draft);
        self.draft.normalize();
        self.mark_draft_changed();
        true
    }

    /// Load factory defaults into `self.draft` ONLY (preview until Apply).
    pub fn reset_to_defaults_preview(&mut self, i18n: &SettingsI18n) {
        let preserved_extra = self.draft.extra.clone();
        self.draft = Config::default();
        self.draft.extra = preserved_extra;
        self.draft.normalize();
        self.conflict_detail = None;
        self.status = Some(StatusBanner::Info(i18n.t("status-reset-preview")));
        if self.is_connected() {
            self.preview_pending_since = Some(Instant::now());
        }
    }

    /// Discard unsaved edits locally and tell caller whether to send `RevertConfig` over IPC.
    pub fn cancel_edits(&mut self, i18n: &SettingsI18n) -> bool {
        self.draft = self.committed.clone();
        self.preview_pending_since = None;
        self.conflict_detail = None;
        self.status = Some(StatusBanner::Info(i18n.t("status-reverted")));
        let should_send_revert = self.is_connected() && self.preview_active_on_monitor;
        self.preview_active_on_monitor = false;
        should_send_revert
    }

    /// Validate `self.draft` before committing. Returns localized validation error strings.
    pub fn validate_draft(&self, i18n: &SettingsI18n) -> Vec<String> {
        validate_config(&self.draft, i18n)
    }

    /// Take a debounced `PreviewConfig` payload if ready to send over IPC.
    pub fn take_due_preview(&mut self, now: Instant) -> Option<Config> {
        if !self.is_connected() {
            self.preview_pending_since = None;
            return None;
        }
        let since = self.preview_pending_since?;
        if now.duration_since(since) < PREVIEW_DEBOUNCE {
            return None;
        }
        self.preview_pending_since = None;
        self.preview_active_on_monitor = true;
        let mut cfg = self.draft.clone();
        cfg.normalize();
        Some(cfg)
    }

    /// Prepare an IPC `CommitConfig` payload (returns `(gen, hash, normalized_cfg)`).
    pub fn prepare_ipc_commit(&mut self) -> (u64, u64, Config) {
        self.preview_pending_since = None;
        self.commit_in_flight = true;
        let mut cfg = self.draft.clone();
        cfg.normalize();
        (self.generation, self.committed_hash, cfg)
    }

    /// Save directly to `config.toml` ONLY in Standalone mode (when `oxide-monitor.exe` is absent).
    pub fn commit_standalone(&mut self, i18n: &SettingsI18n) -> Result<(), String> {
        if self.is_connected() {
            return Err("cannot write directly while connected to monitor".into());
        }
        let errors = self.validate_draft(i18n);
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        self.draft.normalize();
        let new_hash = self
            .draft
            .save_to_path_returning_hash(&self.config_path)
            .map_err(|err| err.to_string())?;
        self.committed = self.draft.clone();
        self.committed_hash = new_hash;
        self.generation = self.generation.wrapping_add(1);
        self.conflict_detail = None;
        self.status = Some(StatusBanner::Success(i18n.t("status-saved")));
        self.refresh_autostart_status();
        Ok(())
    }

    /// Apply an incoming `MonitorToSettings` message to the state machine.
    pub fn apply_ipc_message(&mut self, msg: MonitorToSettings, i18n: &SettingsI18n) {
        match msg {
            MonitorToSettings::HelloAck {
                config_path,
                portable_mode,
                generation,
                config_hash,
                active_config,
                monitors,
                ..
            } => {
                self.connection = ConnectionMode::Connected;
                self.config_path = PathBuf::from(config_path);
                self.portable_mode = portable_mode;
                self.generation = generation;
                self.committed_hash = config_hash;
                self.committed = (*active_config).clone();
                self.draft = *active_config;
                self.monitors = monitors;
                self.conflict_detail = None;
                self.preview_pending_since = None;
                self.preview_active_on_monitor = false;
                self.commit_in_flight = false;
                self.refresh_autostart_status();
            }
            MonitorToSettings::TelemetryTick {
                snapshot,
                history,
                inventory,
            } => {
                self.snapshot = snapshot.to_snapshot();
                self.history = HistoryStore::from_dto(&history);
                self.inventory = inventory;
                self.has_live_telemetry = true;
            }
            MonitorToSettings::MonitorsChanged { monitors } => {
                self.monitors = monitors;
            }
            MonitorToSettings::CommitResult {
                status,
                generation,
                config_hash,
                error,
            } => {
                self.commit_in_flight = false;
                match status {
                    CommitStatus::Ok => {
                        self.generation = generation;
                        self.committed_hash = config_hash;
                        self.draft.normalize();
                        self.committed = self.draft.clone();
                        self.preview_active_on_monitor = false;
                        self.conflict_detail = None;
                        self.status = Some(StatusBanner::Success(i18n.t("status-saved")));
                    }
                    CommitStatus::Conflict => {
                        // Do NOT overwrite user's draft; show Conflict banner and update revision
                        // only when user explicitly reloads.
                        self.conflict_detail =
                            Some(error.unwrap_or_else(|| i18n.t("status-conflict")));
                        self.status = Some(StatusBanner::Warning(i18n.t("status-conflict")));
                    }
                    CommitStatus::Error => {
                        self.status = Some(StatusBanner::Error(
                            error.unwrap_or_else(|| "commit failed".into()),
                        ));
                    }
                }
                self.refresh_autostart_status();
            }
            MonitorToSettings::FocusTab { tab } => {
                if let Some(t) = SettingsTab::from_name(&tab) {
                    self.active_tab = t;
                    if t == SettingsTab::GeneralAndPresets {
                        self.refresh_autostart_status();
                    }
                }
            }
            MonitorToSettings::ProtocolError { code, message } => {
                self.connection = ConnectionMode::Standalone {
                    reason: format!("{code}: {message}"),
                };
            }
            MonitorToSettings::Pong => {}
        }
    }

    pub fn enter_standalone(&mut self, reason: String) {
        self.connection = ConnectionMode::Standalone { reason };
        self.preview_pending_since = None;
        self.preview_active_on_monitor = false;
        self.commit_in_flight = false;
    }
}

/// Validate a `Config` v2 before committing.
pub fn validate_config(cfg: &Config, i18n: &SettingsI18n) -> Vec<String> {
    let mut errors = Vec::new();
    if cfg.items.is_empty() {
        errors.push(i18n.t("validation-no-items"));
    }
    if !(500..=5_000).contains(&cfg.general.poll_interval_ms) {
        errors.push(i18n.t("validation-poll-range"));
    }
    if !(1_000..=60_000).contains(&cfg.general.gpu_poll_ms) {
        errors.push(i18n.t("validation-gpu-poll-range"));
    }
    if cfg.providers.lhm_http
        && crate::tabs::sources::validate_lhm_url(&cfg.providers.lhm_url).is_err()
    {
        errors.push(i18n.t("validation-lhm-url-loopback"));
    }
    errors
}

/// Build realistic mock telemetry for the Live Preview simulator (0.0..=1.0 stress level).
pub fn sample_telemetry(stress: f32) -> (Snapshot, HistoryStore) {
    let s = stress.clamp(0.0, 1.0);
    let mut snap = Snapshot::default();
    snap.seq = 1;
    snap.cpu.usage_pct = 8.0 + s * 88.0;
    snap.cpu.freq_mhz = Some(2100.0 + s * 2400.0);
    snap.cpu.temp_c = Some(38.0 + s * 54.0);
    snap.cpu.temp_source = TempSource::Acpi;

    snap.memory.total_bytes = 16 * 1024 * 1024 * 1024;
    snap.memory.usage_pct = 32.0 + s * 62.0;
    snap.memory.used_bytes =
        ((snap.memory.total_bytes as f64) * (snap.memory.usage_pct as f64 / 100.0)) as u64;

    snap.network.adapter = "Wi-Fi".into();
    snap.network.connected = true;
    snap.network.up_bps = (150_000.0 + (s as f64) * 14_000_000.0) / 8.0;
    snap.network.down_bps = (1_800_000.0 + (s as f64) * 120_000_000.0) / 8.0;

    let dgpu = GpuSample {
        name: "NVIDIA GeForce GTX 1650".into(),
        vendor: GpuVendor::Nvidia,
        luid: Some(0x10DE_0001),
        usage_pct: Some(5.0 + s * 93.0),
        temp_c: Some(40.0 + s * 46.0),
        temp_source: TempSource::Nvml,
        dedicated_vram_bytes: Some(4 * 1024 * 1024 * 1024),
    };
    let igpu = GpuSample {
        name: "AMD Radeon(TM) Graphics".into(),
        vendor: GpuVendor::Amd,
        luid: Some(0x1002_0002),
        usage_pct: Some(3.0 + s * 25.0),
        temp_c: None,
        temp_source: TempSource::None,
        dedicated_vram_bytes: Some(512 * 1024 * 1024),
    };
    snap.gpu = dgpu.clone();
    snap.gpus = vec![dgpu, igpu];

    let nvme = DiskSample {
        name: "Apacer AS2280P4U 1TB".into(),
        index: Some(0),
        is_nvme: true,
        activity_pct: Some(2.0 + s * 78.0),
        temp_c: Some(36.0 + s * 38.0),
        temp_source: TempSource::Nvme,
    };
    snap.disk = nvme.clone();
    snap.disks = vec![nvme];

    let mut hist = HistoryStore::new();
    for i in 0..40 {
        let phase = (i as f32 * 0.25).sin() * 0.14;
        let level = (s + phase).clamp(0.05, 1.0);
        let mut step = snap.clone();
        step.cpu.usage_pct = 8.0 + level * 88.0;
        step.memory.usage_pct = 32.0 + level * 62.0;
        step.gpu.usage_pct = Some(5.0 + level * 93.0);
        step.network.down_bps = (1_800_000.0 + (level as f64) * 120_000_000.0) / 8.0;
        step.network.up_bps = (150_000.0 + (level as f64) * 14_000_000.0) / 8.0;
        hist.record_snapshot(&step);
    }

    (snap, hist)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_cfg_path(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("oxide-settings-state-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    #[test]
    fn config_roundtrip_preserves_all_fields_and_unknown_keys() {
        let path = temp_cfg_path("roundtrip");
        let raw = r#"
version = 2
external_marker = "preserve-me"

[general]
language = "en"
theme = "light"
autostart = true
poll_interval_ms = 1500
gpu_poll_ms = 4000
active_preset = "gamer-sparklines"

[surface]
opacity = 215
corner_radius = 9.0

[layout]
exact_physical_size = false
rows = 2

[custom_plugin]
flag = true
"#;
        fs::write(&path, raw).unwrap();

        let state = SettingsState::load_local(path.clone());
        assert_eq!(state.draft.general.language, "en");
        assert_eq!(state.draft.general.theme, "light");
        assert!(state.draft.general.autostart);
        assert_eq!(state.draft.general.poll_interval_ms, 1500);
        assert_eq!(state.draft.general.gpu_poll_ms, 4000);
        assert_eq!(state.draft.surface.opacity, 215);
        assert!(!state.draft.layout.exact_physical_size);
        assert_eq!(
            state
                .draft
                .extra
                .get("external_marker")
                .and_then(|v| v.as_str()),
            Some("preserve-me")
        );
        assert_eq!(
            state
                .draft
                .extra
                .get("custom_plugin")
                .and_then(|t| t.get("flag"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(!state.is_dirty());

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn preview_never_writes_disk_and_cancel_reverts_cleanly() {
        let path = temp_cfg_path("preview");
        let i18n = SettingsI18n::new("uk");
        let initial = Config::default();
        let initial_hash = initial.save_to_path_returning_hash(&path).unwrap();

        let mut state = SettingsState::load_local(path.clone());
        state.apply_ipc_message(
            MonitorToSettings::HelloAck {
                protocol_version: 1,
                app_version: "0.1.0".into(),
                arch: "x86_64".into(),
                config_path: path.to_string_lossy().into(),
                portable_mode: false,
                generation: 1,
                config_hash: initial_hash,
                active_config: Box::new(initial.clone()),
                monitors: vec![],
            },
            &i18n,
        );

        assert!(state.apply_preset_preview("minimal-1row"));
        assert!(state.is_dirty());
        let due = state.take_due_preview(Instant::now() + PREVIEW_DEBOUNCE);
        assert!(due.is_some());
        assert!(state.preview_active_on_monitor);

        // Disk MUST remain untouched during preview.
        let disk_cfg = Config::load_from_path(&path).unwrap();
        assert_eq!(disk_cfg.general.active_preset, "classic-2row");

        // Cancel reverts draft to committed and requests IPC RevertConfig.
        assert!(state.cancel_edits(&i18n));
        assert!(!state.is_dirty());
        assert_eq!(state.draft.general.active_preset, "classic-2row");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn commit_conflict_preserves_draft_and_sets_banner() {
        let path = temp_cfg_path("conflict");
        let i18n = SettingsI18n::new("uk");
        let initial = Config::default();
        let initial_hash = initial.save_to_path_returning_hash(&path).unwrap();

        let mut state = SettingsState::load_local(path.clone());
        state.apply_ipc_message(
            MonitorToSettings::HelloAck {
                protocol_version: 1,
                app_version: "0.1.0".into(),
                arch: "x86_64".into(),
                config_path: path.to_string_lossy().into(),
                portable_mode: false,
                generation: 2,
                config_hash: initial_hash,
                active_config: Box::new(initial),
                monitors: vec![],
            },
            &i18n,
        );

        state.draft.surface.opacity = 177;
        state.mark_draft_changed();
        let (rev, hash, _) = state.prepare_ipc_commit();
        assert_eq!(rev, 2);
        assert_eq!(hash, initial_hash);

        state.apply_ipc_message(
            MonitorToSettings::CommitResult {
                status: CommitStatus::Conflict,
                generation: 3,
                config_hash: 0xDEAD,
                error: Some("modified externally".into()),
            },
            &i18n,
        );

        assert!(state.conflict_detail.is_some());
        assert_eq!(state.draft.surface.opacity, 177);
        assert!(state.is_dirty());

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn standalone_saves_only_when_disconnected_and_valid() {
        let path = temp_cfg_path("standalone");
        let i18n = SettingsI18n::new("en");
        let initial = Config::default();
        initial.save_to_path(&path).unwrap();

        let mut state = SettingsState::load_local(path.clone());
        state.enter_standalone("monitor not running".into());
        state.draft.general.language = "en".into();
        state.draft.surface.opacity = 205;
        state.commit_standalone(&i18n).unwrap();

        assert!(!state.is_dirty());
        let reloaded = Config::load_from_path(&path).unwrap();
        assert_eq!(reloaded.general.language, "en");
        assert_eq!(reloaded.surface.opacity, 205);

        // Invalid poll interval is rejected before writing.
        state.draft.general.poll_interval_ms = 50;
        assert!(state.commit_standalone(&i18n).is_err());

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn all_config_v2_sections_roundtrip_and_reset_keeps_disk_untouched() {
        use oxide_core::{
            AutohideBehavior, BackgroundMode, ClickAction, ColorRule, ColorSpace, ColorSpec,
            ColorStop, ColumnSizing, CpuTempSource, DecimalSeparator, FillOrder, FlyoutBackdrop,
            FlyoutTrigger, GameModeAction, GroupStyle, ItemVisual, LayoutGroupConfig, LayoutMode,
            MonitorTarget, ProgressStyle, Rgba, SpeedUnit, TaskbarAnchor, TaskbarBackend, TempUnit,
            ValueAlign, WheelAction, WidthMode,
        };

        let path = temp_cfg_path("all_sections");
        let i18n = SettingsI18n::new("uk");
        let mut cfg = Config::default();

        // Mutate every single section of Config v2
        cfg.general.autostart = true;
        cfg.general.language = "en".into();
        cfg.general.theme = "light".into();
        cfg.general.poll_interval_ms = 1200;
        cfg.general.gpu_poll_ms = 4500;
        cfg.general.cpu_temp_source = CpuTempSource::Acpi;
        cfg.general.disk_index = Some(1);
        cfg.general.adapter = Some("Wi-Fi".into());
        cfg.general.default_gpu_luid = Some(0x10DE_0001);

        cfg.aliases
            .gpus
            .insert("NVIDIA GeForce GTX 1650".into(), "GTX".into());
        cfg.aliases
            .disks
            .insert("Apacer NVMe".into(), "NVMe".into());
        cfg.aliases
            .adapters
            .insert("Intel Wi-Fi 6".into(), "Wi-Fi".into());

        cfg.taskbar.show = true;
        cfg.taskbar.backend = TaskbarBackend::Overlay;
        cfg.taskbar.monitor = MonitorTarget::Device {
            device: r"\\.\DISPLAY2".into(),
            name: "Monitor 2".into(),
        };
        cfg.taskbar.anchor = TaskbarAnchor::TaskbarLeft;
        cfg.taskbar.x_offset = 14;
        cfg.taskbar.y_offset = -2;
        cfg.taskbar.left_space_win11 = 180;
        cfg.taskbar.right_space_fallback = 260;
        cfg.taskbar.snap_distance = 10;
        cfg.taskbar.lock_position = false;
        cfg.taskbar.click_through = true;
        cfg.taskbar.autohide_behavior = AutohideBehavior::KeepVisible;

        cfg.surface.background_mode = BackgroundMode::Custom;
        cfg.surface.background_color = ColorSpec::Static(Rgba::new(20, 24, 32, 220));
        cfg.surface.opacity = 215;
        cfg.surface.corner_radius = 8.0;
        cfg.surface.border_width = 1.5;
        cfg.surface.border_color = ColorSpec::ThemeAccent;
        cfg.surface.text_shadow = false;
        cfg.surface.padding.top = 3;
        cfg.surface.cell_padding.x = 5;

        cfg.layout.mode = LayoutMode::FlowRows;
        cfg.layout.rows = 1;
        cfg.layout.fill_order = FillOrder::RowMajor;
        cfg.layout.column_sizing = ColumnSizing::FitContent;
        cfg.layout.width_mode = WidthMode::Auto;
        cfg.layout.max_width = 820;
        cfg.layout.height = 38;
        cfg.layout.gap_x = 10;
        cfg.layout.gap_y = 2;
        cfg.layout.vertical_auto_adapt = false;
        cfg.layout.exact_physical_size = false;
        cfg.layout.groups.push(LayoutGroupConfig {
            id: "compute".into(),
            style: GroupStyle::Pill,
            pill_bg: ColorSpec::Static(Rgba::new(255, 255, 255, 20)),
            pill_radius: 6.0,
            pill_padding_x: 6,
        });

        cfg.typography.tabular_nums = false;
        cfg.typography.color_emoji = false;
        cfg.typography.value_align = ValueAlign::Left;
        cfg.typography.label.size_pt = 8.0;
        cfg.typography.value.weight = 700;
        cfg.typography.unit.italic = true;

        cfg.format.unit_byte = true;
        cfg.format.speed_unit = SpeedUnit::Mbps;
        cfg.format.digits = 5;
        cfg.format.hide_unit = true;
        cfg.format.hide_percent = true;
        cfg.format.separate_space = false;
        cfg.format.short_mode = true;
        cfg.format.decimal_separator = DecimalSeparator::Comma;
        cfg.format.temp_unit = TempUnit::Fahrenheit;

        cfg.items[0].label = "UP!".into();
        cfg.items[0].visual = ItemVisual::TextProgress {
            style: ProgressStyle::BarBottom,
            thickness: 3.0,
            track_alpha: 0.2,
            bar_width: 30.0,
        };
        cfg.items[0].color_rule = Some(ColorRule::Gradient {
            space: ColorSpace::Oklab,
            stops: vec![
                ColorStop {
                    at: 0.0,
                    color: ColorSpec::Static(Rgba::rgb(0x7F, 0xD3, 0x7F)),
                },
                ColorStop {
                    at: 90.0,
                    color: ColorSpec::Static(Rgba::rgb(0xF2, 0x6D, 0x6D)),
                },
            ],
        });

        cfg.interactions.left_click = ClickAction::OpenSettings;
        cfg.interactions.double_click = ClickAction::CopySummary;
        cfg.interactions.middle_click = ClickAction::ToggleGameMode;
        cfg.interactions.wheel_action = WheelAction::CyclePreset;
        cfg.interactions.cycle_presets = vec!["classic-2row".into(), "win11-fluent".into()];

        cfg.flyout.enabled = false;
        cfg.flyout.trigger = FlyoutTrigger::ClickOnly;
        cfg.flyout.hover_delay_ms = 550;
        cfg.flyout.backdrop = FlyoutBackdrop::Solid;
        cfg.flyout.width = 420;
        cfg.flyout.show_cpu_per_core = false;
        cfg.flyout.show_graphs = false;

        cfg.game_mode.auto_hide_fullscreen = false;
        cfg.game_mode.auto_hide_borderless = true;
        cfg.game_mode.action = GameModeAction::HideOnly;
        cfg.game_mode.process_rules = vec!["cs2.exe".into()];
        cfg.game_mode.whitelist_processes = vec!["mstsc.exe".into()];

        cfg.save_to_path(&path).unwrap();
        let with_extra = format!(
            "unknown_keep = \"ok\"\n{}",
            fs::read_to_string(&path).unwrap()
        );
        fs::write(&path, with_extra).unwrap();

        let mut state = SettingsState::load_local(path.clone());
        state.enter_standalone("test".into());
        assert_eq!(state.draft.general.poll_interval_ms, 1200);
        assert_eq!(
            state.draft.aliases.gpu_alias("NVIDIA GeForce GTX 1650"),
            Some("GTX")
        );
        assert_eq!(state.draft.taskbar.anchor, TaskbarAnchor::TaskbarLeft);
        assert_eq!(state.draft.surface.opacity, 215);
        assert_eq!(state.draft.layout.groups.len(), 1);
        assert!(!state.draft.typography.tabular_nums);
        assert_eq!(state.draft.format.temp_unit, TempUnit::Fahrenheit);
        assert_eq!(state.draft.items[0].label, "UP!");
        assert_eq!(
            state.draft.interactions.wheel_action,
            WheelAction::CyclePreset
        );
        assert_eq!(state.draft.flyout.width, 420);
        assert_eq!(
            state.draft.game_mode.process_rules,
            vec!["cs2.exe".to_string()]
        );
        assert_eq!(
            state
                .draft
                .extra
                .get("unknown_keep")
                .and_then(|v| v.as_str()),
            Some("ok")
        );

        // Reset loads defaults into preview ONLY (disk untouched, unknown extra preserved).
        state.reset_to_defaults_preview(&i18n);
        assert!(state.is_dirty());
        assert_eq!(state.draft.general.poll_interval_ms, 1000);
        assert_eq!(
            state
                .draft
                .extra
                .get("unknown_keep")
                .and_then(|v| v.as_str()),
            Some("ok")
        );
        assert_eq!(
            Config::load_from_path(&path)
                .unwrap()
                .general
                .poll_interval_ms,
            1200
        );

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn widget_size_round_trip_preview_commit_cancel_and_preset_preservation() {
        use oxide_core::{SizeMode, WidthMode};

        let path = temp_cfg_path("widget_size_round_trip");
        let i18n = SettingsI18n::new("uk");
        let base_cfg = Config::default();
        let base_hash = base_cfg.canonical_hash();

        let mut state = SettingsState::load_local(path.clone());
        state.apply_ipc_message(
            MonitorToSettings::HelloAck {
                protocol_version: 1,
                app_version: "0.2.0".into(),
                arch: "x86_64".into(),
                config_path: path.to_string_lossy().to_string(),
                portable_mode: false,
                generation: 7,
                config_hash: base_hash,
                active_config: Box::new(base_cfg.clone()),
                monitors: vec![MonitorDescriptor {
                    device: r"\\.\DISPLAY1".into(),
                    friendly_name: "Primary".into(),
                    is_primary: true,
                    dpi: 144,
                    width: 2560,
                    height: 1440,
                }],
            },
            &i18n,
        );
        assert!(state.is_connected());

        // Edit size fields -> Preview payload carries all dynamic size fields
        state.draft.layout.size_mode = Some(SizeMode::FixedDip);
        state.draft.layout.width_mode = WidthMode::Fixed(420);
        state.draft.layout.height = 0;
        state.draft.layout.exact_physical_size = false;
        state.draft.layout.scale_pct = 150;
        state.draft.layout.fit_to_taskbar = false;
        state.mark_draft_changed();

        let due_at = Instant::now() + PREVIEW_DEBOUNCE + Duration::from_millis(5);
        let preview_cfg = state
            .take_due_preview(due_at)
            .expect("preview payload should be ready");
        assert_eq!(preview_cfg.layout.size_mode, Some(SizeMode::FixedDip));
        assert_eq!(preview_cfg.layout.width_mode, WidthMode::Fixed(420));
        assert_eq!(preview_cfg.layout.height, 0);
        assert!(!preview_cfg.layout.exact_physical_size);
        assert_eq!(preview_cfg.layout.scale_pct, 150);
        assert!(!preview_cfg.layout.fit_to_taskbar);

        // Switching presets preserves explicit size_mode, width/height, scale_pct, fit_to_taskbar
        assert!(state.apply_preset_preview("minimal-1row"));
        assert_eq!(state.draft.layout.size_mode, Some(SizeMode::FixedDip));
        assert_eq!(state.draft.layout.width_mode, WidthMode::Fixed(420));
        assert_eq!(state.draft.layout.height, 0);
        assert_eq!(state.draft.layout.scale_pct, 150);
        assert!(!state.draft.layout.fit_to_taskbar);

        // Cancel reverts back to committed defaults and requests IPC Revert
        assert!(state.cancel_edits(&i18n));
        assert_eq!(state.draft.layout.size_mode, None);
        assert_eq!(state.draft.layout.width_mode, WidthMode::Fixed(294));
        assert_eq!(state.draft.layout.height, 35);
        assert_eq!(state.draft.layout.scale_pct, 100);
        assert!(state.draft.layout.fit_to_taskbar);

        // Re-apply edits, prepare IPC commit, and verify TOML persistence round-trip
        state.draft.layout.size_mode = Some(SizeMode::AutoContent);
        state.draft.layout.width_mode = WidthMode::Auto;
        state.draft.layout.height = 48;
        state.draft.layout.scale_pct = 200;
        state.draft.layout.fit_to_taskbar = false;
        let (generation, hash, commit_cfg) = state.prepare_ipc_commit();
        assert_eq!(generation, 7);
        assert_eq!(hash, base_hash);
        assert_eq!(commit_cfg.layout.size_mode, Some(SizeMode::AutoContent));
        assert_eq!(commit_cfg.layout.width_mode, WidthMode::Auto);
        assert_eq!(commit_cfg.layout.height, 48);
        assert_eq!(commit_cfg.layout.scale_pct, 200);
        assert!(!commit_cfg.layout.fit_to_taskbar);

        let saved_hash = commit_cfg.save_to_path_returning_hash(&path).unwrap();
        state.apply_ipc_message(
            MonitorToSettings::CommitResult {
                status: CommitStatus::Ok,
                generation: 8,
                config_hash: saved_hash,
                error: None,
            },
            &i18n,
        );
        assert!(!state.is_dirty());

        let reloaded = SettingsState::load_local(path.clone());
        assert_eq!(reloaded.draft.layout.size_mode, Some(SizeMode::AutoContent));
        assert_eq!(reloaded.draft.layout.width_mode, WidthMode::Auto);
        assert_eq!(reloaded.draft.layout.height, 48);
        assert_eq!(reloaded.draft.layout.scale_pct, 200);
        assert!(!reloaded.draft.layout.fit_to_taskbar);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn pawnio_control_request_is_one_shot() {
        let path = temp_cfg_path("pawnio_control");
        let mut state = SettingsState::load_local(path.clone());
        assert_eq!(state.pending_pawnio_control, None);
        assert_eq!(state.take_pawnio_control(), None);

        // Config changes and previews never queue a helper launch by themselves.
        state.draft.providers.pawnio = true;
        state.mark_draft_changed();
        assert_eq!(state.take_pawnio_control(), None);

        state.request_pawnio_control(true);
        assert_eq!(state.take_pawnio_control(), Some(true));
        assert_eq!(state.take_pawnio_control(), None);

        state.request_pawnio_control(false);
        assert_eq!(state.take_pawnio_control(), Some(false));
        assert_eq!(state.take_pawnio_control(), None);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn lhm_url_validation_blocks_commit_when_invalid() {
        let i18n = SettingsI18n::new("uk");
        let mut cfg = Config::default();

        // When lhm_http is false, non-loopback url is ignored
        cfg.providers.lhm_http = false;
        cfg.providers.lhm_url = "http://remote.host:8085/metrics".into();
        assert!(validate_config(&cfg, &i18n).is_empty());

        // When lhm_http is true and url is valid loopback, no error
        cfg.providers.lhm_http = true;
        cfg.providers.lhm_url = "http://127.0.0.1:8085/metrics".into();
        assert!(validate_config(&cfg, &i18n).is_empty());

        // When lhm_http is true and url is remote or non-http, error is flagged
        cfg.providers.lhm_url = "http://192.168.1.50:8085/metrics".into();
        let errors = validate_config(&cfg, &i18n);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("LibreHardwareMonitor"));

        cfg.providers.lhm_url = "https://127.0.0.1:8085/metrics".into();
        let errors = validate_config(&cfg, &i18n);
        assert_eq!(errors.len(), 1);
    }
}
