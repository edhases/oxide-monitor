//! Configuration schema (v2 with automatic v1 -> v2 migration) and atomic TOML I/O.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::color::{ColorRule, ColorSpec, ColorStop, Rgba};
use crate::format::FormatOptions;

/// Current configuration schema version.
pub const CONFIG_VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CpuTempSource {
    /// Use a provider if available, otherwise ACPI best-effort.
    #[default]
    Auto,
    /// Use ACPI thermal zone only.
    Acpi,
    /// Use HWiNFO/LHM/helper only; show `—` otherwise.
    Provider,
    /// Do not show CPU temperature.
    Off,
}

/// Legacy v1 metric identifier kept for backward compatibility with `config.toml` v1
/// and the Phase-0 GDI renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemId {
    Up,
    Down,
    #[default]
    Cpu,
    Memory,
    Gpu,
    CpuTemp,
    GpuTemp,
    HddTemp,
    HddUsage,
    CpuFreq,
    TotalSpeed,
    TodayTraffic,
}

// ============================================================================
// Metric selector (v2)
// ============================================================================

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterTarget {
    #[default]
    Auto,
    Luid(u64),
    Name(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuTarget {
    #[default]
    Primary,
    Integrated,
    Discrete,
    Luid(u64),
    Index(usize),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskTarget {
    #[default]
    Primary,
    Index(u32),
    Model(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryDisplayMode {
    #[default]
    Percent,
    UsedGb,
    UsedTotalGb,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThroughputDir {
    Read,
    Write,
    #[default]
    Rw,
}

/// Universal metric selector supporting multi-GPU, multi-disk, specific adapters,
/// external providers, and visual separators.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetricSource {
    NetUp {
        #[serde(default)]
        adapter: AdapterTarget,
    },
    NetDown {
        #[serde(default)]
        adapter: AdapterTarget,
    },
    NetCombined {
        #[serde(default)]
        adapter: AdapterTarget,
    },
    NetTotalSpeed {
        #[serde(default)]
        adapter: AdapterTarget,
    },
    NetTodayTotal {
        #[serde(default)]
        adapter: AdapterTarget,
    },
    CpuUsage,
    CpuFreq,
    CpuTemp {
        #[serde(default)]
        source: CpuTempSource,
    },
    MemoryUsage {
        #[serde(default)]
        display_as: MemoryDisplayMode,
    },
    GpuUsage {
        #[serde(default)]
        target: GpuTarget,
    },
    GpuTemp {
        #[serde(default)]
        target: GpuTarget,
    },
    GpuVram {
        #[serde(default)]
        target: GpuTarget,
    },
    DiskTemp {
        #[serde(default)]
        target: DiskTarget,
    },
    DiskActivity {
        #[serde(default)]
        target: DiskTarget,
    },
    DiskThroughput {
        #[serde(default)]
        target: DiskTarget,
        #[serde(default)]
        dir: ThroughputDir,
    },
    ExternalSensor {
        provider: String,
        sensor: String,
    },
    Separator,
}

impl From<ItemId> for MetricSource {
    fn from(id: ItemId) -> Self {
        match id {
            ItemId::Up => Self::NetUp {
                adapter: AdapterTarget::Auto,
            },
            ItemId::Down => Self::NetDown {
                adapter: AdapterTarget::Auto,
            },
            ItemId::Cpu => Self::CpuUsage,
            ItemId::Memory => Self::MemoryUsage {
                display_as: MemoryDisplayMode::Percent,
            },
            ItemId::Gpu => Self::GpuUsage {
                target: GpuTarget::Primary,
            },
            ItemId::CpuTemp => Self::CpuTemp {
                source: CpuTempSource::Auto,
            },
            ItemId::GpuTemp => Self::GpuTemp {
                target: GpuTarget::Primary,
            },
            ItemId::HddTemp => Self::DiskTemp {
                target: DiskTarget::Primary,
            },
            ItemId::HddUsage => Self::DiskActivity {
                target: DiskTarget::Primary,
            },
            ItemId::CpuFreq => Self::CpuFreq,
            ItemId::TotalSpeed => Self::NetTotalSpeed {
                adapter: AdapterTarget::Auto,
            },
            ItemId::TodayTraffic => Self::NetTodayTotal {
                adapter: AdapterTarget::Auto,
            },
        }
    }
}

impl MetricSource {
    /// Best-effort projection back to legacy `ItemId`.
    pub fn legacy_id(&self) -> ItemId {
        match self {
            Self::NetUp { .. } => ItemId::Up,
            Self::NetDown { .. } | Self::NetCombined { .. } => ItemId::Down,
            Self::NetTotalSpeed { .. } => ItemId::TotalSpeed,
            Self::NetTodayTotal { .. } => ItemId::TodayTraffic,
            Self::CpuUsage | Self::ExternalSensor { .. } | Self::Separator => ItemId::Cpu,
            Self::CpuFreq => ItemId::CpuFreq,
            Self::CpuTemp { .. } => ItemId::CpuTemp,
            Self::MemoryUsage { .. } => ItemId::Memory,
            Self::GpuUsage { .. } | Self::GpuVram { .. } => ItemId::Gpu,
            Self::GpuTemp { .. } => ItemId::GpuTemp,
            Self::DiskTemp { .. } => ItemId::HddTemp,
            Self::DiskActivity { .. } | Self::DiskThroughput { .. } => ItemId::HddUsage,
        }
    }
}

// ============================================================================
// Visual modes & grid placement (v2)
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SparklinePlacement {
    #[default]
    Background,
    InlineRight,
    InlineLeft,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SparklineStyle {
    #[default]
    Area,
    Line,
    Bars,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SparklineScale {
    #[default]
    AutoDecay,
    Fixed {
        min: f32,
        max: f32,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStyle {
    #[default]
    BarBottom,
    BarBackground,
    BarInline,
    RadialRing,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ItemVisual {
    #[default]
    Text,
    TextSparkline {
        #[serde(default)]
        placement: SparklinePlacement,
        #[serde(default)]
        style: SparklineStyle,
        #[serde(default = "default_window_sec")]
        window_sec: u16,
        #[serde(default)]
        scale: SparklineScale,
        #[serde(default = "default_fill_alpha")]
        fill_alpha: f32,
        #[serde(default = "default_stroke_width")]
        stroke_width: f32,
        #[serde(default = "default_inline_graph_width")]
        graph_width: f32,
    },
    TextProgress {
        #[serde(default)]
        style: ProgressStyle,
        #[serde(default = "default_bar_thickness")]
        thickness: f32,
        #[serde(default = "default_track_alpha")]
        track_alpha: f32,
        #[serde(default = "default_bar_width")]
        bar_width: f32,
    },
    GraphOnly {
        #[serde(default)]
        style: SparklineStyle,
        #[serde(default = "default_window_sec")]
        window_sec: u16,
        #[serde(default)]
        scale: SparklineScale,
        #[serde(default = "default_fill_alpha")]
        fill_alpha: f32,
        #[serde(default = "default_stroke_width")]
        stroke_width: f32,
        #[serde(default)]
        dashed_border: bool,
        #[serde(default)]
        secondary_color: Option<ColorSpec>,
        #[serde(default = "default_graph_only_width")]
        width: f32,
    },
}

fn default_window_sec() -> u16 {
    60
}
fn default_fill_alpha() -> f32 {
    0.22
}
fn default_stroke_width() -> f32 {
    1.25
}
fn default_inline_graph_width() -> f32 {
    36.0
}
fn default_bar_thickness() -> f32 {
    2.5
}
fn default_track_alpha() -> f32 {
    0.16
}
fn default_bar_width() -> f32 {
    28.0
}
fn default_graph_only_width() -> f32 {
    48.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GridSlot {
    pub row: u16,
    pub col: u16,
    #[serde(default = "default_span")]
    pub row_span: u16,
    #[serde(default = "default_span")]
    pub col_span: u16,
}

fn default_span() -> u16 {
    1
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ItemConfig {
    /// Legacy v1 identifier (still populated for compatibility with Phase-0 GDI).
    #[serde(default)]
    pub id: ItemId,
    /// Stable unique identifier within the layout (e.g. `"cpu_load"`, `"dgpu_temp"`).
    #[serde(default)]
    pub uid: String,
    /// Optional visual group id matching `layout.groups[i].id`.
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub label: String,
    #[serde(default = "default_true")]
    pub visible: bool,
    /// Legacy v1 value color in `#RRGGBB`; superseded by `color_rule` when present.
    #[serde(default)]
    pub color: Option<String>,
    /// Rich v2 metric source; falls back to `MetricSource::from(self.id)`.
    #[serde(default)]
    pub source: Option<MetricSource>,
    /// Grid cell coordinates when `layout.mode == LayoutMode::Grid`.
    #[serde(default)]
    pub grid: Option<GridSlot>,
    /// Visual presentation of this item.
    #[serde(default)]
    pub visual: ItemVisual,
    /// Dynamic coloring rule (static / steps / OKLab gradient).
    #[serde(default)]
    pub color_rule: Option<ColorRule>,
}

impl ItemConfig {
    /// Construct a clean v2 item with `id` kept in sync.
    pub fn new_v2(
        uid: impl Into<String>,
        label: impl Into<String>,
        source: MetricSource,
        visual: ItemVisual,
        color_rule: Option<ColorRule>,
    ) -> Self {
        let legacy_hex = match &color_rule {
            Some(ColorRule::Static {
                color: ColorSpec::Static(rgba),
            }) => Some(rgba.to_hex()),
            _ => None,
        };
        Self {
            id: source.legacy_id(),
            uid: uid.into(),
            group: None,
            label: label.into(),
            visible: true,
            color: legacy_hex,
            source: Some(source),
            grid: None,
            visual,
            color_rule,
        }
    }

    pub fn effective_source(&self) -> MetricSource {
        self.source
            .clone()
            .unwrap_or_else(|| MetricSource::from(self.id))
    }

    pub fn effective_color_rule(&self) -> Option<ColorRule> {
        if let Some(rule) = &self.color_rule {
            return Some(rule.clone());
        }
        self.color.as_deref().and_then(ColorRule::static_hex)
    }

    pub fn normalize(&mut self, index: usize) {
        if let Some(source) = &self.source {
            self.id = source.legacy_id();
        } else {
            self.source = Some(MetricSource::from(self.id));
        }
        if self.uid.trim().is_empty() {
            self.uid = format!("item_{index}");
        }
        if self.color_rule.is_none()
            && let Some(hex) = self.color.as_deref()
        {
            self.color_rule = ColorRule::static_hex(hex);
        }
        if let Some(rule) = &mut self.color_rule {
            rule.normalize();
        }
        if let Some(grid) = &mut self.grid {
            grid.row_span = grid.row_span.clamp(1, 8);
            grid.col_span = grid.col_span.clamp(1, 16);
        }
        match &mut self.visual {
            ItemVisual::Text => {}
            ItemVisual::TextSparkline {
                window_sec,
                fill_alpha,
                stroke_width,
                graph_width,
                ..
            } => {
                *window_sec = (*window_sec).clamp(10, 60);
                *fill_alpha = fill_alpha.clamp(0.0, 1.0);
                *stroke_width = stroke_width.clamp(0.5, 4.0);
                *graph_width = graph_width.clamp(16.0, 120.0);
            }
            ItemVisual::TextProgress {
                thickness,
                track_alpha,
                bar_width,
                ..
            } => {
                *thickness = thickness.clamp(1.0, 8.0);
                *track_alpha = track_alpha.clamp(0.0, 1.0);
                *bar_width = bar_width.clamp(12.0, 80.0);
            }
            ItemVisual::GraphOnly {
                window_sec,
                fill_alpha,
                stroke_width,
                width,
                ..
            } => {
                *window_sec = (*window_sec).clamp(10, 60);
                *fill_alpha = fill_alpha.clamp(0.0, 1.0);
                *stroke_width = stroke_width.clamp(0.5, 4.0);
                *width = width.clamp(20.0, 240.0);
            }
        }
    }
}

fn default_true() -> bool {
    true
}

// ============================================================================
// Surface, Layout, Typography (v2)
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundMode {
    Transparent,
    #[default]
    SolidAuto,
    Custom,
    HdrSafe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EdgeInsets {
    #[serde(default = "default_pad_y")]
    pub top: i32,
    #[serde(default = "default_pad_x")]
    pub right: i32,
    #[serde(default = "default_pad_y")]
    pub bottom: i32,
    #[serde(default = "default_pad_x")]
    pub left: i32,
}

fn default_pad_x() -> i32 {
    3
}
fn default_pad_y() -> i32 {
    1
}

impl Default for EdgeInsets {
    fn default() -> Self {
        Self {
            top: default_pad_y(),
            right: default_pad_x(),
            bottom: default_pad_y(),
            left: default_pad_x(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AxisPadding {
    #[serde(default = "default_cell_pad_x")]
    pub x: i32,
    #[serde(default = "default_cell_pad_y")]
    pub y: i32,
}

fn default_cell_pad_x() -> i32 {
    2
}
fn default_cell_pad_y() -> i32 {
    0
}

impl Default for AxisPadding {
    fn default() -> Self {
        Self {
            x: default_cell_pad_x(),
            y: default_cell_pad_y(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceConfig {
    #[serde(default)]
    pub background_mode: BackgroundMode,
    #[serde(default = "default_bg_color")]
    pub background_color: ColorSpec,
    #[serde(default = "default_opacity")]
    pub opacity: u8,
    #[serde(default = "default_corner_radius")]
    pub corner_radius: f32,
    #[serde(default)]
    pub border_width: f32,
    #[serde(default = "default_border_color")]
    pub border_color: ColorSpec,
    #[serde(default = "default_true")]
    pub text_shadow: bool,
    #[serde(default)]
    pub padding: EdgeInsets,
    #[serde(default)]
    pub cell_padding: AxisPadding,
    #[serde(default)]
    pub background_image: Option<String>,
}

fn default_bg_color() -> ColorSpec {
    ColorSpec::Static(Rgba::new(0x20, 0x20, 0x20, 0xEB))
}
fn default_corner_radius() -> f32 {
    6.0
}
fn default_border_color() -> ColorSpec {
    ColorSpec::ThemeBorder
}

impl Default for SurfaceConfig {
    fn default() -> Self {
        Self {
            background_mode: BackgroundMode::SolidAuto,
            background_color: default_bg_color(),
            opacity: default_opacity(),
            corner_radius: default_corner_radius(),
            border_width: 0.0,
            border_color: default_border_color(),
            text_shadow: true,
            padding: EdgeInsets::default(),
            cell_padding: AxisPadding::default(),
            background_image: None,
        }
    }
}

impl SurfaceConfig {
    pub fn normalize(&mut self) {
        self.opacity = self.opacity.clamp(30, 255);
        self.corner_radius = self.corner_radius.clamp(0.0, 20.0);
        self.border_width = self.border_width.clamp(0.0, 3.0);
        self.padding.top = self.padding.top.clamp(0, 32);
        self.padding.right = self.padding.right.clamp(0, 32);
        self.padding.bottom = self.padding.bottom.clamp(0, 32);
        self.padding.left = self.padding.left.clamp(0, 32);
        self.cell_padding.x = self.cell_padding.x.clamp(0, 24);
        self.cell_padding.y = self.cell_padding.y.clamp(0, 16);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutMode {
    #[default]
    FlowRows,
    Grid,
    AbsoluteSkin,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillOrder {
    #[default]
    ColumnMajor,
    RowMajor,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnSizing {
    #[default]
    FitContent,
    Equal,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidthMode {
    #[default]
    Auto,
    Fixed(i32),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupStyle {
    #[default]
    None,
    Pill,
    SeparatorAfter,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutGroupConfig {
    pub id: String,
    #[serde(default)]
    pub style: GroupStyle,
    #[serde(default = "default_pill_bg")]
    pub pill_bg: ColorSpec,
    #[serde(default = "default_pill_radius")]
    pub pill_radius: f32,
    #[serde(default = "default_pill_pad_x")]
    pub pill_padding_x: i32,
}

fn default_pill_bg() -> ColorSpec {
    ColorSpec::Static(Rgba::new(255, 255, 255, 14))
}
fn default_pill_radius() -> f32 {
    5.0
}
fn default_pill_pad_x() -> i32 {
    5
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeMode {
    /// Width follows the measured content; `layout.height` stays in DIP.
    #[default]
    AutoContent,
    /// `WidthMode::Fixed(_)` and `layout.height` are interpreted as DIP.
    FixedDip,
    /// `WidthMode::Fixed(_)` and `layout.height` are physical pixels at 96 DPI.
    ExactPhysical,
}

/// Physical render size (and the DPI the renderer must draw at) for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WidgetRenderSize {
    pub width_px: u32,
    pub height_px: u32,
    /// Effective DPI after DPI, `scale_pct` and `fit_to_taskbar` correction.
    pub dpi: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutConfig {
    #[serde(default)]
    pub mode: LayoutMode,
    #[serde(default = "default_rows")]
    pub rows: usize,
    #[serde(default)]
    pub fill_order: FillOrder,
    #[serde(default)]
    pub column_sizing: ColumnSizing,
    #[serde(default)]
    pub width_mode: WidthMode,
    #[serde(default = "default_max_width")]
    pub max_width: i32,
    #[serde(default = "default_height")]
    pub height: i32,
    #[serde(default = "default_item_space")]
    pub gap_x: i32,
    #[serde(default = "default_gap_y")]
    pub gap_y: i32,
    #[serde(default = "default_true")]
    pub vertical_auto_adapt: bool,
    /// When true with `WidthMode::Fixed(w)` and `height > 0`, render into exact
    /// physical pixels `(w, height)` regardless of fractional DPI (e.g. 294x35 at 106 DPI).
    #[serde(default = "default_true")]
    pub exact_physical_size: bool,
    /// Explicit user-facing size model. `None` keeps the preset/legacy geometry derived
    /// from `width_mode` + `exact_physical_size`; presets leave it untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_mode: Option<SizeMode>,
    /// Whole-widget zoom in percent (50..=300). 100 keeps the historical pixel size.
    #[serde(
        default = "default_scale_pct",
        skip_serializing_if = "is_default_scale_pct"
    )]
    pub scale_pct: u16,
    /// Shrink uniformly (never crop) so the widget fits the taskbar thickness and work area.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub fit_to_taskbar: bool,
    #[serde(default)]
    pub groups: Vec<LayoutGroupConfig>,
}

fn default_max_width() -> i32 {
    700
}
fn default_gap_y() -> i32 {
    1
}
fn default_scale_pct() -> u16 {
    100
}
fn is_default_scale_pct(value: &u16) -> bool {
    *value == default_scale_pct()
}
fn is_true(value: &bool) -> bool {
    *value
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            mode: LayoutMode::FlowRows,
            rows: default_rows(),
            fill_order: FillOrder::ColumnMajor,
            column_sizing: ColumnSizing::Equal,
            width_mode: WidthMode::Fixed(default_width()),
            max_width: default_max_width(),
            height: default_height(),
            gap_x: default_item_space(),
            gap_y: default_gap_y(),
            vertical_auto_adapt: true,
            exact_physical_size: true,
            size_mode: None,
            scale_pct: default_scale_pct(),
            fit_to_taskbar: true,
            groups: Vec::new(),
        }
    }
}

impl LayoutConfig {
    /// Whole-widget zoom multiplier from `scale_pct` (1.0 == historical size).
    pub fn scale_factor(&self) -> f32 {
        self.scale_pct.clamp(50, 300) as f32 / 100.0
    }

    /// Base DPI before `scale_pct`/fit: 96 for exact-pixel mode, target DPI otherwise.
    pub fn base_dpi(&self, dpi: u32, vertical_taskbar: bool) -> f32 {
        let exact_px = self.exact_physical_size
            && !vertical_taskbar
            && matches!(self.width_mode, WidthMode::Fixed(_))
            && self.height > 0;
        if exact_px { 96.0 } else { dpi.max(48) as f32 }
    }

    /// Compute the physical pixel size and the DPI to render at.
    ///
    /// `width_dip`/`height_dip` come from `LayoutEngine` and are always in 96-DPI DIPs;
    /// `scale_pct` and `fit_to_taskbar` are applied here so every renderer (monitor,
    /// GDI fallback, settings preview) agrees on the geometry.
    pub fn render_size(
        &self,
        width_dip: f32,
        height_dip: f32,
        dpi: u32,
        vertical_taskbar: bool,
        max_width_px: Option<u32>,
        max_height_px: Option<u32>,
    ) -> WidgetRenderSize {
        let scale = self.scale_factor();
        let mut effective_dpi = (self.base_dpi(dpi, vertical_taskbar) * scale).max(48.0);
        let mut width_px = (width_dip * effective_dpi / 96.0).ceil().max(1.0);
        let mut height_px = (height_dip * effective_dpi / 96.0).ceil().max(1.0);

        if self.fit_to_taskbar {
            let mut fit = 1.0_f32;
            if let Some(max_h) = max_height_px.filter(|v| *v > 0) {
                // Keep half a pixel of headroom so the final `ceil` cannot overflow.
                fit = fit.min((max_h as f32 - 0.5) / height_px.max(1.0));
            }
            if let Some(max_w) = max_width_px.filter(|v| *v > 0) {
                fit = fit.min((max_w as f32 - 0.5) / width_px.max(1.0));
            }
            if fit < 1.0 {
                effective_dpi = (effective_dpi * fit).max(48.0);
                width_px = (width_dip * effective_dpi / 96.0).ceil().max(1.0);
                height_px = (height_dip * effective_dpi / 96.0).ceil().max(1.0);
            }
        }

        WidgetRenderSize {
            width_px: width_px.round().max(1.0) as u32,
            height_px: height_px.round().max(1.0) as u32,
            dpi: effective_dpi,
        }
    }

    pub fn normalize(&mut self) {
        self.rows = self.rows.clamp(1, 4);
        self.max_width = self.max_width.clamp(120, 2400);
        if let WidthMode::Fixed(w) = &mut self.width_mode {
            *w = (*w).clamp(80, 2000);
        }
        if self.height != 0 {
            self.height = self.height.clamp(18, 400);
        }
        self.gap_x = self.gap_x.clamp(0, 32);
        self.gap_y = self.gap_y.clamp(0, 16);
        self.scale_pct = self.scale_pct.clamp(50, 300);
        match self.size_mode {
            None => {}
            Some(SizeMode::AutoContent) => {
                self.width_mode = WidthMode::Auto;
                self.exact_physical_size = false;
            }
            Some(SizeMode::FixedDip) => {
                if matches!(self.width_mode, WidthMode::Auto) {
                    self.width_mode = WidthMode::Fixed(default_width());
                }
                self.exact_physical_size = false;
            }
            Some(SizeMode::ExactPhysical) => {
                if matches!(self.width_mode, WidthMode::Auto) {
                    self.width_mode = WidthMode::Fixed(default_width());
                }
                self.exact_physical_size = true;
            }
        }
        for g in &mut self.groups {
            g.pill_radius = g.pill_radius.clamp(0.0, 16.0);
            g.pill_padding_x = g.pill_padding_x.clamp(0, 24);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueAlign {
    Left,
    #[default]
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FontSpec {
    #[serde(default = "default_font")]
    pub family: String,
    #[serde(default = "default_font_size_f32")]
    pub size_pt: f32,
    #[serde(default = "default_weight_normal")]
    pub weight: u16,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub color: ColorSpec,
}

fn default_font_size_f32() -> f32 {
    8.5
}
fn default_weight_normal() -> u16 {
    400
}

impl FontSpec {
    pub fn normalize(&mut self) {
        if self.family.trim().is_empty() {
            self.family = default_font();
        }
        self.size_pt = self.size_pt.clamp(6.0, 28.0);
        self.weight = self.weight.clamp(100, 900);
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TypographyConfig {
    #[serde(default = "default_true")]
    pub tabular_nums: bool,
    #[serde(default = "default_true")]
    pub color_emoji: bool,
    #[serde(default)]
    pub value_align: ValueAlign,
    #[serde(default = "default_label_font")]
    pub label: FontSpec,
    #[serde(default = "default_value_font")]
    pub value: FontSpec,
    #[serde(default = "default_unit_font")]
    pub unit: FontSpec,
}

fn default_label_font() -> FontSpec {
    FontSpec {
        family: default_font(),
        size_pt: 7.5,
        weight: 400,
        italic: false,
        color: ColorSpec::ThemeLabel,
    }
}

fn default_value_font() -> FontSpec {
    FontSpec {
        family: default_font(),
        size_pt: 8.5,
        weight: 600,
        italic: false,
        color: ColorSpec::ThemeValue,
    }
}

fn default_unit_font() -> FontSpec {
    FontSpec {
        family: default_font(),
        size_pt: 7.0,
        weight: 400,
        italic: false,
        color: ColorSpec::ThemeUnit,
    }
}

impl Default for TypographyConfig {
    fn default() -> Self {
        Self {
            tabular_nums: true,
            color_emoji: true,
            value_align: ValueAlign::Right,
            label: default_label_font(),
            value: default_value_font(),
            unit: default_unit_font(),
        }
    }
}

impl TypographyConfig {
    pub fn normalize(&mut self) {
        self.label.normalize();
        self.value.normalize();
        self.unit.normalize();
    }
}

// ============================================================================
// Taskbar, Interactions, Flyout, Game Mode, Aliases
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskbarBackend {
    #[default]
    Overlay,
    Embed,
    TrayOnly,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskbarAnchor {
    #[default]
    TrayLeft,
    TaskbarLeft,
    TaskbarRight,
    FreeDrag,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutohideBehavior {
    #[default]
    HideWithTaskbar,
    KeepVisible,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorTarget {
    #[default]
    Primary,
    Device {
        device: String,
        #[serde(default)]
        name: String,
    },
    AllTaskbars,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskbarConfig {
    #[serde(default = "default_true")]
    pub show: bool,
    #[serde(default)]
    pub backend: TaskbarBackend,
    #[serde(default)]
    pub monitor: MonitorTarget,
    #[serde(default)]
    pub anchor: TaskbarAnchor,
    /// Legacy v1 width (kept synced with `layout.width_mode` for Phase-0 GDI).
    #[serde(default = "default_width")]
    pub width: i32,
    /// Legacy v1 height (kept synced with `layout.height`).
    #[serde(default = "default_height")]
    pub height: i32,
    #[serde(default)]
    pub x_offset: i32,
    #[serde(default)]
    pub y_offset: i32,
    #[serde(default = "default_left_space_win11")]
    pub left_space_win11: i32,
    #[serde(default = "default_right_space_fallback")]
    pub right_space_fallback: i32,
    #[serde(default = "default_snap_distance")]
    pub snap_distance: i32,
    #[serde(default = "default_true")]
    pub lock_position: bool,
    #[serde(default)]
    pub click_through: bool,
    #[serde(default)]
    pub autohide_behavior: AutohideBehavior,
    /// Legacy v1 opacity (kept synced with `surface.opacity`).
    #[serde(default = "default_opacity")]
    pub opacity: u8,
    /// Legacy v1 font family (kept synced with `typography.value.family`).
    #[serde(default = "default_font")]
    pub font_name: String,
    /// Legacy v1 font size (kept synced with `typography.value.size_pt`).
    #[serde(default = "default_font_size")]
    pub font_size: i32,
    /// Legacy v1 rows (kept synced with `layout.rows`).
    #[serde(default = "default_rows")]
    pub rows: usize,
    /// Legacy v1 horizontal spacing (kept synced with `layout.gap_x`).
    #[serde(default = "default_item_space")]
    pub item_space: i32,
}

fn default_width() -> i32 {
    294
}
fn default_height() -> i32 {
    35
}
fn default_opacity() -> u8 {
    235
}
fn default_font() -> String {
    "Segoe UI".to_string()
}
fn default_font_size() -> i32 {
    8
}
fn default_rows() -> usize {
    2
}
fn default_item_space() -> i32 {
    6
}
fn default_left_space_win11() -> i32 {
    160
}
fn default_right_space_fallback() -> i32 {
    280
}
fn default_snap_distance() -> i32 {
    8
}

impl Default for TaskbarConfig {
    fn default() -> Self {
        Self {
            show: true,
            backend: TaskbarBackend::Overlay,
            monitor: MonitorTarget::Primary,
            anchor: TaskbarAnchor::TrayLeft,
            width: default_width(),
            height: default_height(),
            x_offset: 0,
            y_offset: 0,
            left_space_win11: default_left_space_win11(),
            right_space_fallback: default_right_space_fallback(),
            snap_distance: default_snap_distance(),
            lock_position: true,
            click_through: false,
            autohide_behavior: AutohideBehavior::HideWithTaskbar,
            opacity: default_opacity(),
            font_name: default_font(),
            font_size: default_font_size(),
            rows: default_rows(),
            item_space: default_item_space(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickAction {
    #[default]
    None,
    ToggleFlyout,
    OpenSettings,
    OpenTaskManager,
    OpenResourceMonitor,
    OpenNetworkStats,
    ToggleGameMode,
    CyclePreset,
    CopySummary,
    RunCommand {
        path: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WheelAction {
    #[default]
    None,
    CyclePreset,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionsConfig {
    #[serde(default = "default_left_click")]
    pub left_click: ClickAction,
    #[serde(default = "default_double_click")]
    pub double_click: ClickAction,
    #[serde(default = "default_middle_click")]
    pub middle_click: ClickAction,
    #[serde(default)]
    pub wheel_action: WheelAction,
    #[serde(default)]
    pub cycle_presets: Vec<String>,
}

fn default_left_click() -> ClickAction {
    ClickAction::ToggleFlyout
}
fn default_double_click() -> ClickAction {
    ClickAction::OpenTaskManager
}
fn default_middle_click() -> ClickAction {
    ClickAction::OpenSettings
}

impl Default for InteractionsConfig {
    fn default() -> Self {
        Self {
            left_click: default_left_click(),
            double_click: default_double_click(),
            middle_click: default_middle_click(),
            wheel_action: WheelAction::None,
            cycle_presets: vec![
                "classic-2row".into(),
                "minimal-1row".into(),
                "gamer-sparklines".into(),
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlyoutTrigger {
    #[default]
    HoverAndClick,
    ClickOnly,
    HoverOnly,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlyoutBackdrop {
    #[default]
    Acrylic,
    Solid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlyoutConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub trigger: FlyoutTrigger,
    #[serde(default = "default_hover_delay_ms")]
    pub hover_delay_ms: u32,
    #[serde(default)]
    pub backdrop: FlyoutBackdrop,
    #[serde(default = "default_flyout_width")]
    pub width: i32,
    #[serde(default = "default_true")]
    pub show_cpu_per_core: bool,
    #[serde(default = "default_true")]
    pub show_graphs: bool,
    #[serde(default = "default_true")]
    pub show_all_gpus: bool,
    #[serde(default = "default_true")]
    pub show_all_disks: bool,
    #[serde(default = "default_true")]
    pub show_network_details: bool,
    #[serde(default = "default_true")]
    pub show_provider_status: bool,
}

fn default_hover_delay_ms() -> u32 {
    350
}
fn default_flyout_width() -> i32 {
    380
}

impl Default for FlyoutConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            trigger: FlyoutTrigger::HoverAndClick,
            hover_delay_ms: default_hover_delay_ms(),
            backdrop: FlyoutBackdrop::Acrylic,
            width: default_flyout_width(),
            show_cpu_per_core: true,
            show_graphs: true,
            show_all_gpus: true,
            show_all_disks: true,
            show_network_details: true,
            show_provider_status: true,
        }
    }
}

impl FlyoutConfig {
    pub fn normalize(&mut self) {
        self.hover_delay_ms = self.hover_delay_ms.clamp(100, 2000);
        self.width = self.width.clamp(280, 640);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameModeAction {
    HideOnly,
    #[default]
    HideAndLowPower,
    StrictAnticheat,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameModeConfig {
    #[serde(default = "default_true")]
    pub auto_hide_fullscreen: bool,
    #[serde(default = "default_true")]
    pub auto_hide_borderless: bool,
    #[serde(default)]
    pub action: GameModeAction,
    #[serde(default)]
    pub process_rules: Vec<String>,
    #[serde(default)]
    pub whitelist_processes: Vec<String>,
}

impl Default for GameModeConfig {
    fn default() -> Self {
        Self {
            auto_hide_fullscreen: true,
            auto_hide_borderless: true,
            action: GameModeAction::HideAndLowPower,
            process_rules: Vec::new(),
            whitelist_processes: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasesConfig {
    #[serde(default)]
    pub disks: BTreeMap<String, String>,
    #[serde(default)]
    pub gpus: BTreeMap<String, String>,
    #[serde(default)]
    pub adapters: BTreeMap<String, String>,
}

impl AliasesConfig {
    pub fn disk_alias<'a>(&'a self, device_name: &'a str) -> Option<&'a str> {
        self.disks.get(device_name).map(String::as_str)
    }

    pub fn gpu_alias<'a>(&'a self, device_name: &'a str) -> Option<&'a str> {
        self.gpus.get(device_name).map(String::as_str)
    }

    pub fn adapter_alias<'a>(&'a self, device_name: &'a str) -> Option<&'a str> {
        self.adapters.get(device_name).map(String::as_str)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,
    #[serde(default)]
    pub cpu_temp_source: CpuTempSource,
    /// How often NVML may be polled, even when the dGPU is idle.
    #[serde(default = "default_gpu_poll_ms")]
    pub gpu_poll_ms: u64,
    /// Optional physical drive index for the primary disk item.
    #[serde(default)]
    pub disk_index: Option<u32>,
    /// Optional adapter name; empty/None means auto-select.
    #[serde(default)]
    pub adapter: Option<String>,
    /// Optional stable adapter LUID (0 / None means auto-select).
    #[serde(default)]
    pub adapter_luid: Option<u64>,
    /// Optional preferred primary GPU LUID.
    #[serde(default)]
    pub default_gpu_luid: Option<u64>,
    /// Name of the last applied preset (informational).
    #[serde(default = "default_active_preset")]
    pub active_preset: String,
}

fn default_language() -> String {
    "uk".to_string()
}
fn default_theme() -> String {
    "dark".to_string()
}
fn default_poll_interval_ms() -> u64 {
    1000
}
fn default_gpu_poll_ms() -> u64 {
    3000
}
fn default_active_preset() -> String {
    "classic-2row".to_string()
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            autostart: false,
            language: default_language(),
            theme: default_theme(),
            poll_interval_ms: default_poll_interval_ms(),
            cpu_temp_source: CpuTempSource::Auto,
            gpu_poll_ms: default_gpu_poll_ms(),
            disk_index: None,
            adapter: None,
            adapter_luid: None,
            default_gpu_luid: None,
            active_preset: default_active_preset(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvidersConfig {
    #[serde(default)]
    pub hwinfo: bool,
    #[serde(default)]
    pub lhm_http: bool,
    #[serde(default = "default_lhm_url")]
    pub lhm_url: String,
    /// Download and stage the latest official portable LHM under the per-user provider
    /// folder when the LHM provider is enabled. Off by default; never installs/runs it.
    #[serde(default)]
    pub lhm_auto_update: bool,
    /// Opt in to the optional elevated PawnIO helper for CPU Tctl/Tdie.
    ///
    /// Off by default. Even when true, the helper is launched only after an explicit
    /// Enable action/UAC consent and never at monitor startup. The monitor itself
    /// stays unelevated.
    #[serde(default)]
    pub pawnio: bool,
    /// Reserved for a future bundled helper process. Unsupported and intentionally not
    /// exposed in the Settings UI; it has no effect on provider polling.
    #[serde(default)]
    pub helper: bool,
}

fn default_lhm_url() -> String {
    "http://127.0.0.1:8085/metrics".to_string()
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            hwinfo: false,
            lhm_http: false,
            lhm_url: default_lhm_url(),
            lhm_auto_update: false,
            pawnio: false,
            helper: false,
        }
    }
}

// ============================================================================
// Root Config (v2, backward-compatible with v1)
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub aliases: AliasesConfig,
    #[serde(default)]
    pub taskbar: TaskbarConfig,
    #[serde(default)]
    pub surface: SurfaceConfig,
    #[serde(default)]
    pub layout: LayoutConfig,
    #[serde(default)]
    pub typography: TypographyConfig,
    #[serde(default)]
    pub format: FormatOptions,
    #[serde(default)]
    pub items: Vec<ItemConfig>,
    #[serde(default)]
    pub interactions: InteractionsConfig,
    #[serde(default)]
    pub flyout: FlyoutConfig,
    #[serde(default)]
    pub game_mode: GameModeConfig,
    #[serde(default)]
    pub providers: ProvidersConfig,
    /// Preserved unknown top-level keys/tables so external or future config keys are never lost
    /// during GUI editing or IPC round-trips.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, toml::Value>,
}

fn default_version() -> u32 {
    CONFIG_VERSION
}

impl Default for Config {
    fn default() -> Self {
        let mut config = Self {
            version: CONFIG_VERSION,
            general: GeneralConfig::default(),
            aliases: AliasesConfig::default(),
            taskbar: TaskbarConfig::default(),
            surface: SurfaceConfig::default(),
            layout: LayoutConfig::default(),
            typography: TypographyConfig::default(),
            format: FormatOptions::default(),
            items: default_items(),
            interactions: InteractionsConfig::default(),
            flyout: FlyoutConfig::default(),
            game_mode: GameModeConfig::default(),
            providers: ProvidersConfig::default(),
            extra: BTreeMap::new(),
        };
        config.normalize();
        config
    }
}

pub fn default_items() -> Vec<ItemConfig> {
    vec![
        ItemConfig::new_v2(
            "net_up",
            "↑",
            MetricSource::NetUp {
                adapter: AdapterTarget::Auto,
            },
            ItemVisual::Text,
            ColorRule::static_hex("#7FD37F"),
        ),
        ItemConfig::new_v2(
            "net_down",
            "↓",
            MetricSource::NetDown {
                adapter: AdapterTarget::Auto,
            },
            ItemVisual::Text,
            ColorRule::static_hex("#7FD37F"),
        ),
        ItemConfig::new_v2(
            "cpu_load",
            "CPU:",
            MetricSource::CpuUsage,
            ItemVisual::Text,
            ColorRule::static_hex("#F0C674"),
        ),
        ItemConfig::new_v2(
            "ram_load",
            "MEM:",
            MetricSource::MemoryUsage {
                display_as: MemoryDisplayMode::Percent,
            },
            ItemVisual::Text,
            ColorRule::static_hex("#F0C674"),
        ),
        ItemConfig::new_v2(
            "gpu_load",
            "GPU:",
            MetricSource::GpuUsage {
                target: GpuTarget::Primary,
            },
            ItemVisual::Text,
            ColorRule::static_hex("#7FBFEF"),
        ),
        ItemConfig::new_v2(
            "cpu_temp",
            "CPU:",
            MetricSource::CpuTemp {
                source: CpuTempSource::Auto,
            },
            ItemVisual::Text,
            ColorRule::static_hex("#F0A050"),
        ),
        ItemConfig::new_v2(
            "gpu_temp",
            "GPU:",
            MetricSource::GpuTemp {
                target: GpuTarget::Primary,
            },
            ItemVisual::Text,
            ColorRule::static_hex("#7FBFEF"),
        ),
        ItemConfig::new_v2(
            "hdd_temp",
            "HDD:",
            MetricSource::DiskTemp {
                target: DiskTarget::Primary,
            },
            ItemVisual::Text,
            ColorRule::static_hex("#E08080"),
        ),
    ]
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("failed to serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("failed to write config {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl Config {
    /// Load config from `path`, migrating v1 -> v2 if needed, or return defaults if missing.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_toml_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Parse a TOML config string, automatically migrating v1 schemas to v2.
    pub fn from_toml_str(text: &str) -> Result<Self, toml::de::Error> {
        let raw_table: toml::Value = toml::from_str(text)?;
        let raw_version = raw_table
            .get("version")
            .and_then(toml::Value::as_integer)
            .unwrap_or(1) as u32;
        let has_v2_layout = raw_table.get("layout").is_some();
        let has_v2_surface = raw_table.get("surface").is_some();
        let has_v2_typography = raw_table.get("typography").is_some();

        let mut config: Self = toml::from_str(text)?;
        if raw_version < 2 {
            config.migrate_v1_to_v2(!has_v2_layout, !has_v2_surface, !has_v2_typography);
        }
        config.normalize();
        Ok(config)
    }

    /// Project legacy v1 `[taskbar]` and `[[items]]` fields into the v2 sections
    /// when those v2 sections were absent in the loaded file.
    pub fn migrate_v1_to_v2(
        &mut self,
        inherit_layout: bool,
        inherit_surface: bool,
        inherit_typography: bool,
    ) {
        if inherit_layout {
            self.layout.rows = self.taskbar.rows;
            self.layout.width_mode = WidthMode::Fixed(self.taskbar.width);
            self.layout.height = self.taskbar.height;
            self.layout.gap_x = self.taskbar.item_space;
            self.layout.column_sizing = ColumnSizing::Equal;
            self.layout.fill_order = FillOrder::ColumnMajor;
            self.layout.exact_physical_size = true;
        }
        if inherit_surface {
            self.surface.opacity = self.taskbar.opacity;
            self.surface.background_mode = BackgroundMode::SolidAuto;
            self.surface.padding = EdgeInsets {
                top: 1,
                right: 3,
                bottom: 1,
                left: 3,
            };
            self.surface.cell_padding = AxisPadding { x: 2, y: 0 };
        }
        if inherit_typography {
            let family = self.taskbar.font_name.clone();
            let size = self.taskbar.font_size as f32;
            self.typography.label.family = family.clone();
            self.typography.label.size_pt = size;
            self.typography.value.family = family.clone();
            self.typography.value.size_pt = size;
            self.typography.unit.family = family;
            self.typography.unit.size_pt = (size - 0.5).max(6.0);
        }

        for (index, item) in self.items.iter_mut().enumerate() {
            if item.source.is_none() {
                item.source = Some(MetricSource::from(item.id));
            }
            if item.uid.is_empty() {
                item.uid = format!("{}_{index}", legacy_uid_prefix(item.id));
            }
            if item.color_rule.is_none()
                && let Some(hex) = item.color.as_deref()
            {
                item.color_rule = ColorRule::static_hex(hex);
            }
        }
        self.version = CONFIG_VERSION;
    }

    /// Clamp invalid values and keep legacy `[taskbar]` mirror fields synchronized
    /// so Phase-0/GDI callers and v2 D2D callers observe consistent dimensions.
    pub fn normalize(&mut self) {
        if self.version < CONFIG_VERSION {
            self.version = CONFIG_VERSION;
        }
        if self.items.is_empty() {
            self.items = default_items();
        }

        self.general.poll_interval_ms = self.general.poll_interval_ms.clamp(500, 5_000);
        self.general.gpu_poll_ms = self.general.gpu_poll_ms.clamp(1_000, 60_000);

        self.surface.normalize();
        self.layout.normalize();
        self.typography.normalize();
        self.format.normalize();
        self.flyout.normalize();

        for (index, item) in self.items.iter_mut().enumerate() {
            item.normalize(index);
        }

        // Clamp taskbar positioning fields.
        self.taskbar.x_offset = self.taskbar.x_offset.clamp(-2000, 2000);
        self.taskbar.y_offset = self.taskbar.y_offset.clamp(-2000, 2000);
        self.taskbar.left_space_win11 = self.taskbar.left_space_win11.clamp(0, 800);
        self.taskbar.right_space_fallback = self.taskbar.right_space_fallback.clamp(0, 800);
        self.taskbar.snap_distance = self.taskbar.snap_distance.clamp(0, 32);

        // Keep legacy v1 fields synced from v2 canonical sections.
        self.taskbar.rows = self.layout.rows;
        self.taskbar.item_space = self.layout.gap_x;
        self.taskbar.opacity = self.surface.opacity;
        self.taskbar.font_name = self.typography.value.family.clone();
        self.taskbar.font_size = self.typography.value.size_pt.round().clamp(6.0, 24.0) as i32;
        if let WidthMode::Fixed(w) = self.layout.width_mode {
            self.taskbar.width = w;
        } else {
            self.taskbar.width = self.taskbar.width.clamp(120, 2000);
        }
        if self.layout.height > 0 {
            self.taskbar.height = self.layout.height.clamp(20, 200);
        } else {
            self.taskbar.height = self.taskbar.height.clamp(20, 200);
        }
    }

    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from_path(Self::default_path())
    }

    /// Compute deterministic FNV-1a 64-bit hash of the normalized canonical TOML representation.
    pub fn canonical_hash(&self) -> u64 {
        let mut normalized = self.clone();
        normalized.normalize();
        match toml::to_string_pretty(&normalized) {
            Ok(text) => config_hash_bytes(text.as_bytes()),
            Err(_) => 0,
        }
    }

    /// Save atomically: normalize/clamp, write temp file + `sync_all`, then replace via
    /// Win32 `ReplaceFileW` (creating `.bak` atomically) with a safe rename fallback.
    /// Returns the FNV-1a 64-bit hash of the exact bytes written to disk.
    pub fn save_to_path_returning_hash(&self, path: impl AsRef<Path>) -> Result<u64, ConfigError> {
        use std::io::Write;

        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut normalized = self.clone();
        normalized.normalize();
        let text = toml::to_string_pretty(&normalized)?;
        let bytes = text.as_bytes();
        let hash = config_hash_bytes(bytes);

        let tmp = path.with_extension("toml.tmp");
        {
            let mut f = fs::File::create(&tmp).map_err(|source| ConfigError::Write {
                path: tmp.clone(),
                source,
            })?;
            f.write_all(bytes).map_err(|source| ConfigError::Write {
                path: tmp.clone(),
                source,
            })?;
            let _ = f.sync_all();
        }

        if path.exists() {
            let bak = path.with_extension("toml.bak");
            if !replace_file_atomic(path, &tmp, &bak) {
                let _ = fs::copy(path, &bak);
                let _ = fs::remove_file(path);
                fs::rename(&tmp, path).map_err(|source| ConfigError::Write {
                    path: path.to_path_buf(),
                    source,
                })?;
            }
        } else {
            fs::rename(&tmp, path).map_err(|source| ConfigError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        }

        Ok(hash)
    }

    /// Save atomically: write temp file, keep `.bak`, and replace via `ReplaceFileW`.
    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        self.save_to_path_returning_hash(path).map(|_| ())
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to_path(Self::default_path())
    }

    /// `%APPDATA%\OxideMonitor\config.toml`
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        base.join("OxideMonitor").join("config.toml")
    }

    /// Portable mode: `config.toml` next to the executable.
    pub fn portable_path() -> PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("config.toml")
    }

    /// Use portable config when explicitly requested or when a portable file exists.
    pub fn resolve_path() -> PathBuf {
        if std::env::var_os("OXIDE_PORTABLE").is_some() {
            return Self::portable_path();
        }
        let portable = Self::portable_path();
        if portable.exists() {
            return portable;
        }
        Self::default_path()
    }
}

/// Deterministic FNV-1a 64-bit hash for config file contents and IPC optimistic concurrency.
pub fn config_hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(windows)]
fn replace_file_atomic(target: &Path, replacement: &Path, backup: &Path) -> bool {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    const REPLACEFILE_IGNORE_MERGE_ERRORS: u32 = 0x0000_0002;

    unsafe extern "system" {
        fn ReplaceFileW(
            lp_replaced_file_name: *const u16,
            lp_replacement_file_name: *const u16,
            lp_backup_file_name: *const u16,
            dw_replace_flags: u32,
            lp_exclude: *mut c_void,
            lp_reserved: *mut c_void,
        ) -> i32;
    }

    let to_wide = |p: &Path| -> Vec<u16> {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };

    let target_w = to_wide(target);
    let replacement_w = to_wide(replacement);
    let backup_w = to_wide(backup);

    unsafe {
        ReplaceFileW(
            target_w.as_ptr(),
            replacement_w.as_ptr(),
            backup_w.as_ptr(),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) != 0
    }
}

#[cfg(not(windows))]
fn replace_file_atomic(_target: &Path, _replacement: &Path, _backup: &Path) -> bool {
    false
}

fn legacy_uid_prefix(id: ItemId) -> &'static str {
    match id {
        ItemId::Up => "net_up",
        ItemId::Down => "net_down",
        ItemId::Cpu => "cpu",
        ItemId::Memory => "mem",
        ItemId::Gpu => "gpu",
        ItemId::CpuTemp => "cpu_temp",
        ItemId::GpuTemp => "gpu_temp",
        ItemId::HddTemp => "hdd_temp",
        ItemId::HddUsage => "hdd_usage",
        ItemId::CpuFreq => "cpu_freq",
        ItemId::TotalSpeed => "total_speed",
        ItemId::TodayTraffic => "today_traffic",
    }
}

/// Convenience helper for building temperature threshold gradient rules.
pub fn thermal_gradient(cool_c: f32, warm_c: f32, hot_c: f32) -> ColorRule {
    ColorRule::Gradient {
        space: crate::color::ColorSpace::Oklab,
        stops: vec![
            ColorStop {
                at: cool_c,
                color: ColorSpec::Static(Rgba::rgb(0x7F, 0xD3, 0x7F)),
            },
            ColorStop {
                at: warm_c,
                color: ColorSpec::Static(Rgba::rgb(0xF0, 0xC6, 0x74)),
            },
            ColorStop {
                at: hot_c,
                color: ColorSpec::Static(Rgba::rgb(0xF2, 0x6D, 0x6D)),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::find_builtin_preset;

    #[test]
    fn default_items_are_eight() {
        let cfg = Config::default();
        assert_eq!(cfg.version, CONFIG_VERSION);
        assert_eq!(cfg.items.len(), 8);
    }

    #[test]
    fn default_layout_keeps_the_legacy_294x35_exact_size() {
        let cfg = Config::default();
        assert_eq!(cfg.layout.size_mode, None);
        assert_eq!(cfg.layout.scale_pct, 100);
        assert!(cfg.layout.fit_to_taskbar);

        for dpi in [96u32, 106, 144, 192] {
            for taskbar_height in [40u32, 48, 56, 64, 72] {
                let size = cfg.layout.render_size(
                    294.0,
                    35.0,
                    dpi,
                    false,
                    Some(1920),
                    Some(taskbar_height),
                );
                assert_eq!(
                    (size.width_px, size.height_px),
                    (294, 35),
                    "classic must stay exact at dpi {dpi} / taskbar {taskbar_height}"
                );
            }
        }
    }

    #[test]
    fn scale_presets_produce_expected_physical_sizes() {
        let mut cfg = Config::default();
        cfg.layout.size_mode = Some(SizeMode::ExactPhysical);
        for (scale, expected) in [
            (75u16, (221u32, 27u32)),
            (100, (294, 35)),
            (125, (368, 44)),
            (150, (441, 53)),
            (200, (588, 70)),
        ] {
            cfg.layout.scale_pct = scale;
            let size = cfg.layout.render_size(294.0, 35.0, 144, false, None, None);
            assert_eq!((size.width_px, size.height_px), expected, "scale {scale}");
        }
    }

    #[test]
    fn fit_shrinks_uniformly_and_never_crops() {
        let mut cfg = Config::default();
        cfg.layout.size_mode = Some(SizeMode::ExactPhysical);
        for scale in [100u16, 150, 200, 300] {
            cfg.layout.scale_pct = scale;
            for taskbar_height in [40u32, 48, 56, 64, 72] {
                for (screen_w, _screen_h) in
                    [(1366u32, 768u32), (1920, 1080), (2560, 1440), (3840, 2160)]
                {
                    let size = cfg.layout.render_size(
                        294.0,
                        35.0,
                        96,
                        false,
                        Some(screen_w),
                        Some(taskbar_height),
                    );
                    assert!(
                        size.height_px <= taskbar_height,
                        "scale {scale}: {} > {taskbar_height}",
                        size.height_px
                    );
                    assert!(size.width_px <= screen_w);
                    if scale == 100 {
                        assert_eq!((size.width_px, size.height_px), (294, 35));
                    }
                    // Both axes are ceil-rounded independently, so the DIB can carry up to
                    // one extra transparent pixel row; the drawn content keeps the exact
                    // aspect ratio and nothing is clipped.
                    let expected_w = size.height_px as f32 * 294.0 / 35.0;
                    assert!(
                        (size.width_px as f32 - expected_w).abs() <= 294.0 / 35.0 + 1.0,
                        "scale {scale}: aspect drift {} vs {}",
                        size.width_px,
                        expected_w
                    );
                    let exact_w = 294.0 * size.dpi / 96.0;
                    let exact_h = 35.0 * size.dpi / 96.0;
                    assert!(size.width_px as f32 >= exact_w - 0.01);
                    assert!(size.height_px as f32 >= exact_h - 0.01);
                }
            }
        }
    }

    #[test]
    fn size_mode_normalizes_the_geometry_fields() {
        let mut cfg = Config::default();
        cfg.layout.size_mode = Some(SizeMode::AutoContent);
        cfg.layout.normalize();
        assert!(matches!(cfg.layout.width_mode, WidthMode::Auto));
        assert!(!cfg.layout.exact_physical_size);

        cfg.layout.size_mode = Some(SizeMode::FixedDip);
        cfg.layout.normalize();
        assert!(matches!(cfg.layout.width_mode, WidthMode::Fixed(294)));
        assert!(!cfg.layout.exact_physical_size);

        cfg.layout.size_mode = Some(SizeMode::ExactPhysical);
        cfg.layout.normalize();
        assert!(matches!(cfg.layout.width_mode, WidthMode::Fixed(294)));
        assert!(cfg.layout.exact_physical_size);

        cfg.layout.scale_pct = 900;
        cfg.layout.normalize();
        assert_eq!(cfg.layout.scale_pct, 300);
        cfg.layout.scale_pct = 1;
        cfg.layout.normalize();
        assert_eq!(cfg.layout.scale_pct, 50);
    }

    #[test]
    fn sizing_fields_round_trip_and_defaults_stay_implicit() {
        let mut cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(!text.contains("scale_pct"));
        assert!(!text.contains("size_mode"));
        assert!(!text.contains("fit_to_taskbar"));
        let legacy = Config::from_toml_str(&text).unwrap();
        assert_eq!(legacy.layout.size_mode, None);
        assert_eq!(legacy.layout.scale_pct, 100);
        assert!(legacy.layout.fit_to_taskbar);

        cfg.layout.size_mode = Some(SizeMode::FixedDip);
        cfg.layout.scale_pct = 150;
        cfg.layout.fit_to_taskbar = false;
        cfg.layout.width_mode = WidthMode::Fixed(420);
        cfg.layout.height = 44;
        let text = toml::to_string_pretty(&cfg).unwrap();
        let loaded = Config::from_toml_str(&text).unwrap();
        assert_eq!(loaded.layout.size_mode, Some(SizeMode::FixedDip));
        assert_eq!(loaded.layout.scale_pct, 150);
        assert!(!loaded.layout.fit_to_taskbar);
        assert_eq!(loaded.layout.width_mode, WidthMode::Fixed(420));
        assert_eq!(loaded.layout.height, 44);
    }

    #[test]
    fn lhm_auto_update_defaults_off_and_roundtrips() {
        let cfg = Config::default();
        assert!(!cfg.providers.lhm_auto_update);

        let mut cfg = Config::default();
        cfg.providers.lhm_http = true;
        cfg.providers.lhm_auto_update = true;
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(text.contains("lhm_auto_update = true"));
        let loaded = Config::from_toml_str(&text).unwrap();
        assert!(loaded.providers.lhm_auto_update);
        assert!(loaded.providers.lhm_http);
    }

    #[test]
    fn pawnio_defaults_off_and_roundtrips() {
        let cfg = Config::default();
        assert!(!cfg.providers.pawnio);

        let mut cfg = Config::default();
        cfg.providers.pawnio = true;
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(text.contains("pawnio = true"));
        let loaded = Config::from_toml_str(&text).unwrap();
        assert!(loaded.providers.pawnio);
    }

    #[test]
    fn presets_preserve_the_user_sizing_preferences() {
        let mut cfg = Config::default();
        cfg.layout.size_mode = Some(SizeMode::FixedDip);
        cfg.layout.width_mode = WidthMode::Fixed(400);
        cfg.layout.height = 40;
        cfg.layout.scale_pct = 150;
        cfg.layout.fit_to_taskbar = false;

        find_builtin_preset("gamer-sparklines")
            .unwrap()
            .apply_to(&mut cfg);

        assert_eq!(cfg.general.active_preset, "gamer-sparklines");
        assert_eq!(cfg.layout.size_mode, Some(SizeMode::FixedDip));
        assert_eq!(cfg.layout.width_mode, WidthMode::Fixed(400));
        assert_eq!(cfg.layout.height, 40);
        assert_eq!(cfg.layout.scale_pct, 150);
        assert!(!cfg.layout.fit_to_taskbar);
        // Content still comes from the preset.
        assert!(cfg.items.len() >= 5);
    }

    #[test]
    fn auto_content_mode_ignores_a_fixed_width() {
        let mut cfg = Config::default();
        cfg.layout.size_mode = Some(SizeMode::AutoContent);
        cfg.layout.width_mode = WidthMode::Fixed(500);
        cfg.layout.normalize();
        assert!(matches!(cfg.layout.width_mode, WidthMode::Auto));
        let size = cfg.layout.render_size(369.4, 31.0, 106, false, None, None);
        // 369.4 DIP at 106 DPI = 407.9 -> 408 px (no exact-pixel shortcut).
        assert_eq!((size.width_px, size.height_px), (408, 35));
    }

    #[test]
    fn roundtrip_and_backup() {
        let dir = std::env::temp_dir().join("oxide-core-test-v2");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");

        let mut config = Config::default();
        config.general.autostart = true;
        config
            .aliases
            .disks
            .insert("Apacer AS2280P4U 1TB".into(), "NVMe".into());
        config.game_mode.process_rules.push("cs2.exe".into());
        config.save_to_path(&path).unwrap();

        // Second save should create a .bak file.
        config.surface.corner_radius = 8.0;
        config.save_to_path(&path).unwrap();
        assert!(path.with_extension("toml.bak").exists());

        let loaded = Config::load_from_path(&path).unwrap();
        assert_eq!(loaded.version, 2);
        assert!(loaded.general.autostart);
        assert_eq!(loaded.surface.corner_radius, 8.0);
        assert_eq!(
            loaded.aliases.disk_alias("Apacer AS2280P4U 1TB"),
            Some("NVMe")
        );
        assert_eq!(loaded.game_mode.process_rules, vec!["cs2.exe".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrates_v1_config_seamlessly() {
        let v1_toml = r##"
version = 1

[general]
language = "uk"
theme = "dark"
cpu_temp_source = "provider"
gpu_poll_ms = 2500

[taskbar]
show = true
width = 320
height = 40
x_offset = 12
y_offset = -2
opacity = 210
font_name = "Cascadia Code"
font_size = 9
rows = 2
item_space = 8

[[items]]
id = "up"
label = "↑"
visible = true
color = "#7FD37F"

[[items]]
id = "cpu_temp"
label = "Tcpu:"
visible = true
color = "#F0A050"
"##;

        let migrated = Config::from_toml_str(v1_toml).unwrap();
        assert_eq!(migrated.version, 2);
        assert_eq!(migrated.layout.width_mode, WidthMode::Fixed(320));
        assert_eq!(migrated.layout.height, 40);
        assert_eq!(migrated.layout.gap_x, 8);
        assert_eq!(migrated.surface.opacity, 210);
        assert_eq!(migrated.typography.value.family, "Cascadia Code");
        assert_eq!(migrated.typography.value.size_pt, 9.0);
        assert_eq!(migrated.items.len(), 2);
        assert_eq!(
            migrated.items[0].effective_source(),
            MetricSource::NetUp {
                adapter: AdapterTarget::Auto
            }
        );
        assert!(migrated.items[1].color_rule.is_some());
        assert_eq!(migrated.taskbar.width, 320);
    }

    #[test]
    fn unknown_top_level_toml_keys_survive_roundtrip() {
        let raw = r#"
version = 2
custom_flag = "keep-me"

[general]
language = "uk"

[future_experimental]
threshold = 42
enabled = true
"#;
        let cfg = Config::from_toml_str(raw).unwrap();
        assert_eq!(
            cfg.extra.get("custom_flag").and_then(|v| v.as_str()),
            Some("keep-me")
        );
        let serialized = toml::to_string_pretty(&cfg).unwrap();
        let reloaded = Config::from_toml_str(&serialized).unwrap();
        assert_eq!(
            reloaded.extra.get("custom_flag").and_then(|v| v.as_str()),
            Some("keep-me")
        );
        assert_eq!(
            reloaded
                .extra
                .get("future_experimental")
                .and_then(|t| t.get("threshold"))
                .and_then(|v| v.as_integer()),
            Some(42)
        );
    }
}
