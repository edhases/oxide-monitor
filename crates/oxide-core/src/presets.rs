//! Built-in and shareable visual presets (`.oxide-preset.toml`).
//!
//! Presets intentionally contain ONLY visual/layout settings and never private
//! paths, autostart flags, provider URLs, or hardware-specific LUIDs.

use serde::{Deserialize, Serialize};

use crate::color::{ColorRule, ColorSpec, Rgba};
use crate::config::{
    AdapterTarget, BackgroundMode, ColumnSizing, Config, CpuTempSource, DiskTarget, EdgeInsets,
    FillOrder, FontSpec, GpuTarget, GridSlot, GroupStyle, ItemConfig, ItemVisual, LayoutConfig,
    LayoutGroupConfig, LayoutMode, MemoryDisplayMode, MetricSource, ProgressStyle,
    SparklinePlacement, SparklineScale, SparklineStyle, SurfaceConfig, TypographyConfig, WidthMode,
    default_items, thermal_gradient,
};
use crate::format::{FormatOptions, SpeedUnit};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default = "default_preset_version")]
    pub version: String,
    #[serde(default)]
    pub description: String,
}

fn default_preset_version() -> String {
    "1.0".into()
}

/// Shareable visual preset (serialized as `.oxide-preset.toml`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetDefinition {
    pub preset: PresetMeta,
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
}

impl PresetDefinition {
    /// Apply only visual/layout/item sections onto an existing user `Config`,
    /// preserving hardware bindings, aliases, providers, autostart, and Game Mode.
    ///
    /// Widget sizing (`size_mode`, `scale_pct`, `fit_to_taskbar` and the explicit fixed
    /// width/height when a size mode is selected) is a global user preference and is
    /// preserved across preset switches.
    pub fn apply_to(&self, config: &mut Config) {
        let previous_layout = config.layout.clone();
        config.general.active_preset = self.preset.id.clone();
        config.surface = self.surface.clone();
        config.layout = self.layout.clone();
        config.layout.scale_pct = previous_layout.scale_pct;
        config.layout.fit_to_taskbar = previous_layout.fit_to_taskbar;
        if previous_layout.size_mode.is_some() {
            config.layout.size_mode = previous_layout.size_mode;
            config.layout.width_mode = previous_layout.width_mode;
            config.layout.height = previous_layout.height;
        }
        config.typography = self.typography.clone();
        config.format = self.format.clone();
        config.items = self.items.clone();
        config.normalize();
    }

    /// Extract a clean, privacy-safe preset from the user's current `Config`.
    pub fn extract_from(config: &Config, meta: PresetMeta) -> Self {
        Self {
            preset: meta,
            surface: config.surface.clone(),
            layout: config.layout.clone(),
            typography: config.typography.clone(),
            format: config.format.clone(),
            items: config.items.clone(),
        }
    }

    pub fn to_toml_pretty(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    pub fn from_toml_str(text: &str) -> Result<Self, toml::de::Error> {
        let mut preset: Self = toml::from_str(text)?;
        preset.surface.normalize();
        preset.layout.normalize();
        preset.typography.normalize();
        preset.format.normalize();
        for (i, item) in preset.items.iter_mut().enumerate() {
            item.normalize(i);
        }
        Ok(preset)
    }
}

/// Return the 5 built-in presets shipped with Oxide Monitor.
pub fn builtin_presets() -> Vec<PresetDefinition> {
    vec![
        preset_classic_2row(),
        preset_minimal_1row(),
        preset_gamer_sparklines(),
        preset_win11_fluent(),
        preset_vertical_stack(),
    ]
}

pub fn find_builtin_preset(id: &str) -> Option<PresetDefinition> {
    builtin_presets().into_iter().find(|p| p.preset.id == id)
}

fn preset_classic_2row() -> PresetDefinition {
    PresetDefinition {
        preset: PresetMeta {
            id: "classic-2row".into(),
            name: "Класичний 2-рядковий (TrafficMonitor)".into(),
            author: "Oxide Monitor".into(),
            version: "1.0".into(),
            description: "Звичний компактний віджет 294×35 px на 8 елементів у 2 рядки.".into(),
        },
        surface: SurfaceConfig {
            background_mode: BackgroundMode::SolidAuto,
            opacity: 235,
            corner_radius: 4.0,
            ..SurfaceConfig::default()
        },
        layout: LayoutConfig {
            mode: LayoutMode::FlowRows,
            rows: 2,
            fill_order: FillOrder::ColumnMajor,
            column_sizing: ColumnSizing::Equal,
            width_mode: WidthMode::Fixed(294),
            height: 35,
            gap_x: 6,
            gap_y: 1,
            exact_physical_size: true,
            ..LayoutConfig::default()
        },
        typography: TypographyConfig::default(),
        format: FormatOptions::default(),
        items: default_items(),
    }
}

fn preset_minimal_1row() -> PresetDefinition {
    PresetDefinition {
        preset: PresetMeta {
            id: "minimal-1row".into(),
            name: "Мінімалізм (1 рядок)".into(),
            author: "Oxide Monitor".into(),
            version: "1.0".into(),
            description: "Тонка прозора смуга в 1 рядок: мережа, CPU та RAM.".into(),
        },
        surface: SurfaceConfig {
            background_mode: BackgroundMode::Transparent,
            opacity: 255,
            corner_radius: 0.0,
            text_shadow: true,
            padding: EdgeInsets {
                top: 2,
                right: 6,
                bottom: 2,
                left: 6,
            },
            ..SurfaceConfig::default()
        },
        layout: LayoutConfig {
            mode: LayoutMode::FlowRows,
            rows: 1,
            fill_order: FillOrder::RowMajor,
            column_sizing: ColumnSizing::FitContent,
            width_mode: WidthMode::Auto,
            height: 28,
            gap_x: 10,
            gap_y: 0,
            exact_physical_size: false,
            ..LayoutConfig::default()
        },
        typography: TypographyConfig {
            label: FontSpec {
                family: "Segoe UI Variable Small".into(),
                size_pt: 8.0,
                weight: 400,
                italic: false,
                color: ColorSpec::ThemeLabel,
            },
            value: FontSpec {
                family: "Segoe UI Variable Small".into(),
                size_pt: 9.0,
                weight: 600,
                italic: false,
                color: ColorSpec::ThemeValue,
            },
            unit: FontSpec {
                family: "Segoe UI Variable Small".into(),
                size_pt: 7.5,
                weight: 400,
                italic: false,
                color: ColorSpec::ThemeUnit,
            },
            ..TypographyConfig::default()
        },
        format: FormatOptions {
            speed_unit: SpeedUnit::Auto,
            short_mode: true,
            separate_space: false,
            ..FormatOptions::default()
        },
        items: vec![
            ItemConfig::new_v2(
                "down",
                "↓",
                MetricSource::NetDown {
                    adapter: AdapterTarget::Auto,
                },
                ItemVisual::Text,
                ColorRule::static_hex("#7FD37F"),
            ),
            ItemConfig::new_v2(
                "up",
                "↑",
                MetricSource::NetUp {
                    adapter: AdapterTarget::Auto,
                },
                ItemVisual::Text,
                ColorRule::static_hex("#7FBFEF"),
            ),
            ItemConfig::new_v2(
                "cpu",
                "CPU",
                MetricSource::CpuUsage,
                ItemVisual::Text,
                Some(thermal_gradient(0.0, 65.0, 90.0)),
            ),
            ItemConfig::new_v2(
                "ram",
                "RAM",
                MetricSource::MemoryUsage {
                    display_as: MemoryDisplayMode::Percent,
                },
                ItemVisual::Text,
                Some(thermal_gradient(20.0, 75.0, 92.0)),
            ),
        ],
    }
}

fn preset_gamer_sparklines() -> PresetDefinition {
    let mut net_graph = ItemConfig::new_v2(
        "net_graph",
        "",
        MetricSource::NetCombined {
            adapter: AdapterTarget::Auto,
        },
        ItemVisual::GraphOnly {
            style: SparklineStyle::Area,
            window_sec: 60,
            scale: SparklineScale::AutoDecay,
            fill_alpha: 0.25,
            stroke_width: 1.25,
            dashed_border: true,
            secondary_color: ColorSpec::parse("#F0A050"),
            width: 52.0,
        },
        ColorRule::static_hex("#4ADE80"),
    );
    net_graph.grid = Some(GridSlot {
        row: 0,
        col: 0,
        row_span: 2,
        col_span: 1,
    });

    let mut cpu = ItemConfig::new_v2(
        "cpu_load",
        "CPU",
        MetricSource::CpuUsage,
        ItemVisual::TextSparkline {
            placement: SparklinePlacement::Background,
            style: SparklineStyle::Area,
            window_sec: 60,
            scale: SparklineScale::Fixed {
                min: 0.0,
                max: 100.0,
            },
            fill_alpha: 0.20,
            stroke_width: 1.0,
            graph_width: 36.0,
        },
        Some(thermal_gradient(0.0, 65.0, 90.0)),
    );
    cpu.grid = Some(GridSlot {
        row: 0,
        col: 1,
        row_span: 1,
        col_span: 1,
    });

    let mut ram = ItemConfig::new_v2(
        "ram_load",
        "RAM",
        MetricSource::MemoryUsage {
            display_as: MemoryDisplayMode::Percent,
        },
        ItemVisual::TextProgress {
            style: ProgressStyle::BarBottom,
            thickness: 2.0,
            track_alpha: 0.16,
            bar_width: 28.0,
        },
        Some(thermal_gradient(30.0, 78.0, 92.0)),
    );
    ram.grid = Some(GridSlot {
        row: 1,
        col: 1,
        row_span: 1,
        col_span: 1,
    });

    let mut gpu = ItemConfig::new_v2(
        "dgpu_load",
        "GPU",
        MetricSource::GpuUsage {
            target: GpuTarget::Discrete,
        },
        ItemVisual::TextSparkline {
            placement: SparklinePlacement::Background,
            style: SparklineStyle::Area,
            window_sec: 60,
            scale: SparklineScale::Fixed {
                min: 0.0,
                max: 100.0,
            },
            fill_alpha: 0.20,
            stroke_width: 1.0,
            graph_width: 36.0,
        },
        ColorRule::static_hex("#60CDFF"),
    );
    gpu.grid = Some(GridSlot {
        row: 0,
        col: 2,
        row_span: 1,
        col_span: 1,
    });

    let mut gpu_temp = ItemConfig::new_v2(
        "dgpu_temp",
        "dGPU",
        MetricSource::GpuTemp {
            target: GpuTarget::Discrete,
        },
        ItemVisual::Text,
        Some(thermal_gradient(45.0, 72.0, 85.0)),
    );
    gpu_temp.grid = Some(GridSlot {
        row: 1,
        col: 2,
        row_span: 1,
        col_span: 1,
    });

    let mut cpu_temp = ItemConfig::new_v2(
        "cpu_temp",
        "Tcpu",
        MetricSource::CpuTemp {
            source: CpuTempSource::Auto,
        },
        ItemVisual::Text,
        Some(thermal_gradient(45.0, 75.0, 88.0)),
    );
    cpu_temp.grid = Some(GridSlot {
        row: 0,
        col: 3,
        row_span: 1,
        col_span: 1,
    });

    let mut nvme = ItemConfig::new_v2(
        "nvme_temp",
        "NVMe",
        MetricSource::DiskTemp {
            target: DiskTarget::Primary,
        },
        ItemVisual::Text,
        Some(thermal_gradient(35.0, 58.0, 72.0)),
    );
    nvme.grid = Some(GridSlot {
        row: 1,
        col: 3,
        row_span: 1,
        col_span: 1,
    });

    PresetDefinition {
        preset: PresetMeta {
            id: "gamer-sparklines".into(),
            name: "Геймерський з графіками (Telemetry Pro)".into(),
            author: "Oxide Monitor".into(),
            version: "1.0".into(),
            description:
                "Сітка з 60-секундними sparklines та плавними OKLab-градієнтами температур.".into(),
        },
        surface: SurfaceConfig {
            background_mode: BackgroundMode::SolidAuto,
            opacity: 230,
            corner_radius: 6.0,
            border_width: 1.0,
            ..SurfaceConfig::default()
        },
        layout: LayoutConfig {
            mode: LayoutMode::Grid,
            rows: 2,
            width_mode: WidthMode::Auto,
            height: 38,
            gap_x: 8,
            gap_y: 2,
            exact_physical_size: false,
            ..LayoutConfig::default()
        },
        typography: TypographyConfig::default(),
        format: FormatOptions {
            speed_unit: SpeedUnit::Auto,
            ..FormatOptions::default()
        },
        items: vec![net_graph, cpu, ram, gpu, gpu_temp, cpu_temp, nvme],
    }
}

fn preset_win11_fluent() -> PresetDefinition {
    let mut items = default_items();
    for (idx, item) in items.iter_mut().enumerate() {
        // Align groups with 2-row column-major boundaries so pills never overlap:
        // col 0 (0,1) = net, col 1 (2,3) = compute, cols 2..3 (4..7) = gpu & thermals.
        item.group = Some(match idx {
            0 | 1 => "net".into(),
            2 | 3 => "compute".into(),
            _ => "thermals".into(),
        });
        if idx == 2 || idx == 3 {
            item.visual = ItemVisual::TextProgress {
                style: ProgressStyle::BarBottom,
                thickness: 2.0,
                track_alpha: 0.14,
                bar_width: 24.0,
            };
        }
    }

    PresetDefinition {
        preset: PresetMeta {
            id: "win11-fluent".into(),
            name: "Windows 11 Fluent (Pills)".into(),
            author: "Oxide Monitor".into(),
            version: "1.0".into(),
            description:
                "Напівпрозорі групи-пігулки у стилі Win11 Widgets із авто-адаптацією теми.".into(),
        },
        surface: SurfaceConfig {
            background_mode: BackgroundMode::Transparent,
            opacity: 255,
            corner_radius: 8.0,
            text_shadow: false,
            ..SurfaceConfig::default()
        },
        layout: LayoutConfig {
            mode: LayoutMode::FlowRows,
            rows: 2,
            fill_order: FillOrder::ColumnMajor,
            column_sizing: ColumnSizing::FitContent,
            width_mode: WidthMode::Auto,
            height: 38,
            gap_x: 6,
            gap_y: 1,
            groups: vec![
                LayoutGroupConfig {
                    id: "net".into(),
                    style: GroupStyle::Pill,
                    pill_bg: ColorSpec::Static(Rgba::new(255, 255, 255, 18)),
                    pill_radius: 6.0,
                    pill_padding_x: 6,
                },
                LayoutGroupConfig {
                    id: "compute".into(),
                    style: GroupStyle::Pill,
                    pill_bg: ColorSpec::Static(Rgba::new(255, 255, 255, 18)),
                    pill_radius: 6.0,
                    pill_padding_x: 6,
                },
                LayoutGroupConfig {
                    id: "thermals".into(),
                    style: GroupStyle::Pill,
                    pill_bg: ColorSpec::Static(Rgba::new(255, 255, 255, 18)),
                    pill_radius: 6.0,
                    pill_padding_x: 6,
                },
            ],
            exact_physical_size: false,
            ..LayoutConfig::default()
        },
        typography: TypographyConfig {
            label: FontSpec {
                family: "Segoe UI Variable Small".into(),
                size_pt: 7.5,
                weight: 400,
                italic: false,
                color: ColorSpec::ThemeLabel,
            },
            value: FontSpec {
                family: "Segoe UI Variable Small".into(),
                size_pt: 8.5,
                weight: 600,
                italic: false,
                color: ColorSpec::ThemeValue,
            },
            unit: FontSpec {
                family: "Segoe UI Variable Small".into(),
                size_pt: 7.0,
                weight: 400,
                italic: false,
                color: ColorSpec::ThemeUnit,
            },
            ..TypographyConfig::default()
        },
        format: FormatOptions {
            speed_unit: SpeedUnit::Auto,
            ..FormatOptions::default()
        },
        items,
    }
}

fn preset_vertical_stack() -> PresetDefinition {
    PresetDefinition {
        preset: PresetMeta {
            id: "vertical-stack".into(),
            name: "Вертикальний / 3-рядковий".into(),
            author: "Oxide Monitor".into(),
            version: "1.0".into(),
            description: "Компактна розкладка з радіальними індикаторами для вертикального або високого таскбара."
                .into(),
        },
        surface: SurfaceConfig {
            background_mode: BackgroundMode::SolidAuto,
            opacity: 235,
            corner_radius: 6.0,
            ..SurfaceConfig::default()
        },
        layout: LayoutConfig {
            mode: LayoutMode::FlowRows,
            rows: 3,
            fill_order: FillOrder::RowMajor,
            column_sizing: ColumnSizing::FitContent,
            width_mode: WidthMode::Auto,
            height: 54,
            gap_x: 8,
            gap_y: 2,
            vertical_auto_adapt: true,
            exact_physical_size: false,
            size_mode: None,
            scale_pct: 100,
            fit_to_taskbar: true,
            groups: Vec::new(),
            max_width: 480,
        },
        typography: TypographyConfig::default(),
        format: FormatOptions {
            speed_unit: SpeedUnit::Auto,
            short_mode: true,
            ..FormatOptions::default()
        },
        items: vec![
            ItemConfig::new_v2(
                "down",
                "↓",
                MetricSource::NetDown {
                    adapter: AdapterTarget::Auto,
                },
                ItemVisual::Text,
                ColorRule::static_hex("#7FD37F"),
            ),
            ItemConfig::new_v2(
                "up",
                "↑",
                MetricSource::NetUp {
                    adapter: AdapterTarget::Auto,
                },
                ItemVisual::Text,
                ColorRule::static_hex("#7FBFEF"),
            ),
            ItemConfig::new_v2(
                "cpu",
                "CPU",
                MetricSource::CpuUsage,
                ItemVisual::TextProgress {
                    style: ProgressStyle::RadialRing,
                    thickness: 2.0,
                    track_alpha: 0.18,
                    bar_width: 14.0,
                },
                Some(thermal_gradient(0.0, 65.0, 90.0)),
            ),
            ItemConfig::new_v2(
                "ram",
                "RAM",
                MetricSource::MemoryUsage {
                    display_as: MemoryDisplayMode::Percent,
                },
                ItemVisual::TextProgress {
                    style: ProgressStyle::RadialRing,
                    thickness: 2.0,
                    track_alpha: 0.18,
                    bar_width: 14.0,
                },
                Some(thermal_gradient(25.0, 75.0, 92.0)),
            ),
            ItemConfig::new_v2(
                "gpu",
                "GPU",
                MetricSource::GpuUsage {
                    target: GpuTarget::Primary,
                },
                ItemVisual::Text,
                ColorRule::static_hex("#60CDFF"),
            ),
            ItemConfig::new_v2(
                "nvme",
                "SSD",
                MetricSource::DiskTemp {
                    target: DiskTarget::Primary,
                },
                ItemVisual::Text,
                Some(thermal_gradient(35.0, 60.0, 75.0)),
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_five_builtin_presets_roundtrip_toml_and_apply_cleanly() {
        let presets = builtin_presets();
        assert_eq!(presets.len(), 5);

        for preset in presets {
            let toml_text = preset.to_toml_pretty().unwrap();
            let parsed = PresetDefinition::from_toml_str(&toml_text).unwrap();
            assert_eq!(parsed.preset.id, preset.preset.id);

            let mut cfg = Config::default();
            cfg.general.autostart = true;
            cfg.providers.hwinfo = true;
            parsed.apply_to(&mut cfg);
            // Applying a preset must preserve non-visual user settings.
            assert!(cfg.general.autostart);
            assert!(cfg.providers.hwinfo);
            assert_eq!(cfg.general.active_preset, preset.preset.id);
        }
    }
}
