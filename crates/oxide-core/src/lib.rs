//! Domain types, configuration (v1/v2), color rules, history, inventory,
//! display model, IPC protocol, presets, and formatting for Oxide Monitor.

pub mod autostart;
pub mod color;
pub mod config;
pub mod display_model;
pub mod format;
pub mod history;
pub mod i18n;
pub mod inventory;
pub mod ipc;
pub mod metrics;
pub mod pawnio;
pub mod presets;
pub mod provider_update;
pub mod theme;

pub use autostart::{
    AUTOSTART_STATUS_FILE, AUTOSTART_TASK_NAME, AutostartStatus, AutostartStatusFile,
    autostart_status_path, load_autostart_status, save_autostart_status,
};
pub use color::{ColorRule, ColorSpace, ColorSpec, ColorStop, Rgba, lerp_oklab, lerp_srgb};
pub use config::{
    AdapterTarget, AliasesConfig, AutohideBehavior, BackgroundMode, CONFIG_VERSION, ClickAction,
    ColumnSizing, Config, ConfigError, CpuTempSource, DiskTarget, EdgeInsets, FillOrder,
    FlyoutBackdrop, FlyoutConfig, FlyoutTrigger, FontSpec, GameModeAction, GameModeConfig,
    GeneralConfig, GpuTarget, GridSlot, GroupStyle, InteractionsConfig, ItemConfig, ItemId,
    ItemVisual, LayoutConfig, LayoutGroupConfig, LayoutMode, MemoryDisplayMode, MetricSource,
    MonitorTarget, ProgressStyle, ProvidersConfig, SizeMode, SparklinePlacement, SparklineScale,
    SparklineStyle, SurfaceConfig, TaskbarAnchor, TaskbarBackend, TaskbarConfig, ThroughputDir,
    TypographyConfig, ValueAlign, WheelAction, WidgetRenderSize, WidthMode, config_hash_bytes,
    default_items, thermal_gradient,
};
pub use display_model::{
    DataQuality, DisplayItemModel, DisplayModel, DisplayModelBuilder, ExtraTelemetry, select_disk,
    select_gpu,
};
pub use format::{
    DecimalSeparator, FormatOptions, FormattedParts, SpeedUnit, TempUnit, format_bytes,
    format_freq_parts, format_memory_parts, format_percent, format_percent_parts, format_speed,
    format_speed_parts, format_temp, format_temp_parts,
};
pub use history::{HISTORY_LEN, HistorySliceDto, HistoryStore, RingBuffer, SeriesKey};
pub use i18n::{Language, Localizer};
pub use inventory::{
    AdapterDescriptor, CpuDescriptor, DiskBus, DiskDescriptor, ExternalSensorDescriptor,
    GpuDescriptor, GpuRole, HardwareInventory, ProviderHealth, ProviderStatus, infer_gpu_role,
};
pub use ipc::{
    IPC_PROTOCOL_VERSION, MAX_IPC_FRAME_BYTES, MonitorDescriptor, MonitorToSettings,
    SettingsToMonitor, SnapshotDto, read_frame, settings_pipe_name, write_frame,
};
pub use metrics::{
    CpuSample, DiskSample, GpuSample, GpuVendor, MemorySample, NetworkSample, Snapshot, TempSource,
};
pub use pawnio::{PAWNIO_HELPER_PIPE_PREFIX, PawnioCommand, PawnioState, PawnioStatus};
pub use presets::{PresetDefinition, PresetMeta, builtin_presets, find_builtin_preset};
pub use provider_update::{
    LHM_OFFICIAL_URL, PAWNIO_OFFICIAL_URL, ProviderInstallKind, ProviderUpdateState,
    ProviderUpdateStatus,
};
pub use theme::{Theme, ThemePalette, parse_color};
