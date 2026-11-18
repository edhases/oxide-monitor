//! Platform-independent `DisplayModelBuilder` transforming `Config` + `Snapshot` +
//! `HistoryStore` into styled, dirty-hashed `DisplayItemModel`s ready for `oxide-render`.

use std::collections::BTreeMap;

use crate::color::Rgba;
use crate::config::{
    Config, CpuTempSource, DiskTarget, GpuTarget, GridSlot, ItemConfig, ItemVisual, MetricSource,
    SparklineScale,
};
use crate::format::{
    FormattedParts, format_bytes, format_freq_parts, format_memory_parts, format_percent_parts,
    format_speed_parts, format_temp_parts,
};
use crate::history::{HistoryStore, SeriesKey};
use crate::inventory::{GpuRole, infer_gpu_role};
use crate::metrics::{DiskSample, GpuSample, Snapshot, TempSource};
use crate::theme::ThemePalette;

/// Data freshness / trustworthiness used by the renderer for visual degradation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DataQuality {
    #[default]
    Good,
    Stale,
    Suspect,
    Missing,
}

/// Fully resolved, styled display item ready for layout and drawing.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayItemModel {
    pub uid: String,
    pub group: Option<String>,
    pub grid: Option<GridSlot>,
    pub is_separator: bool,
    pub label: String,
    pub value: String,
    pub unit: String,
    pub raw_numeric: Option<f32>,
    pub quality: DataQuality,
    pub label_color: Rgba,
    pub value_color: Rgba,
    pub unit_color: Rgba,
    pub visual: ItemVisual,
    pub sparkline_primary: Vec<f32>,
    pub sparkline_secondary: Vec<f32>,
    pub sparkline_min: f32,
    pub sparkline_max: f32,
    pub progress_ratio: Option<f32>,
}

/// Complete resolved frame model + FNV-1a dirty-check hash.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayModel {
    pub items: Vec<DisplayItemModel>,
    pub content_hash: u64,
}

/// Optional extra telemetry not stored inside `Snapshot` (e.g. external provider
/// custom sensors or accumulated daily traffic).
#[derive(Clone, Debug, Default)]
pub struct ExtraTelemetry<'a> {
    pub today_traffic_bytes: Option<u64>,
    pub external_sensors: Option<&'a BTreeMap<String, (f64, String)>>,
}

pub struct DisplayModelBuilder;

impl DisplayModelBuilder {
    pub fn build(
        config: &Config,
        snapshot: &Snapshot,
        history: &HistoryStore,
        palette: &ThemePalette,
        extra: &ExtraTelemetry<'_>,
    ) -> DisplayModel {
        let mut items = Vec::with_capacity(config.items.len());
        let label_color = config.typography.label.color.resolve(palette);
        let default_value_color = config.typography.value.color.resolve(palette);
        let default_unit_color = config.typography.unit.color.resolve(palette);

        for item in config.items.iter().filter(|i| i.visible) {
            let source = item.effective_source();
            if matches!(source, MetricSource::Separator) {
                items.push(DisplayItemModel {
                    uid: item.uid.clone(),
                    group: item.group.clone(),
                    grid: item.grid,
                    is_separator: true,
                    label: String::new(),
                    value: String::new(),
                    unit: String::new(),
                    raw_numeric: None,
                    quality: DataQuality::Good,
                    label_color: palette.separator,
                    value_color: palette.separator,
                    unit_color: palette.separator,
                    visual: ItemVisual::Text,
                    sparkline_primary: Vec::new(),
                    sparkline_secondary: Vec::new(),
                    sparkline_min: 0.0,
                    sparkline_max: 1.0,
                    progress_ratio: None,
                });
                continue;
            }

            let resolved = resolve_metric(&source, config, snapshot, extra);
            let label = resolve_label(item, &source, config, snapshot);

            let base_value_color = match resolved.quality {
                DataQuality::Missing => palette.muted,
                DataQuality::Stale | DataQuality::Suspect => item
                    .effective_color_rule()
                    .map(|r| r.resolve(resolved.numeric, palette))
                    .unwrap_or(default_value_color)
                    .scale_alpha(0.6),
                DataQuality::Good => item
                    .effective_color_rule()
                    .map(|r| r.resolve(resolved.numeric, palette))
                    .unwrap_or(default_value_color),
            };

            let unit_color = match resolved.quality {
                DataQuality::Missing => palette.muted,
                _ => {
                    if config.typography.unit.color == crate::color::ColorSpec::ThemeUnit {
                        base_value_color.scale_alpha(0.76)
                    } else {
                        default_unit_color
                    }
                }
            };

            let (spark_a, spark_b, spark_min, spark_max) =
                extract_sparkline(&source, &item.visual, snapshot, history);

            items.push(DisplayItemModel {
                uid: item.uid.clone(),
                group: item.group.clone(),
                grid: item.grid,
                is_separator: false,
                label,
                value: resolved.parts.value,
                unit: resolved.parts.unit,
                raw_numeric: resolved.numeric,
                quality: resolved.quality,
                label_color,
                value_color: base_value_color,
                unit_color,
                visual: item.visual.clone(),
                sparkline_primary: spark_a,
                sparkline_secondary: spark_b,
                sparkline_min: spark_min,
                sparkline_max: spark_max,
                progress_ratio: resolved.progress_ratio,
            });
        }

        let content_hash = hash_display_model(&items, config, palette);
        DisplayModel {
            items,
            content_hash,
        }
    }
}

struct ResolvedMetric {
    parts: FormattedParts,
    numeric: Option<f32>,
    progress_ratio: Option<f32>,
    quality: DataQuality,
}

fn resolve_metric(
    source: &MetricSource,
    config: &Config,
    snapshot: &Snapshot,
    extra: &ExtraTelemetry<'_>,
) -> ResolvedMetric {
    let fmt = &config.format;
    match source {
        MetricSource::NetUp { .. } => {
            let bps = snapshot.network.up_bps.max(0.0);
            ResolvedMetric {
                parts: format_speed_parts(bps, fmt),
                numeric: Some((bps * 8.0 / 1_000_000.0) as f32),
                progress_ratio: None,
                quality: DataQuality::Good,
            }
        }
        MetricSource::NetDown { .. } | MetricSource::NetCombined { .. } => {
            let bps = snapshot.network.down_bps.max(0.0);
            ResolvedMetric {
                parts: format_speed_parts(bps, fmt),
                numeric: Some((bps * 8.0 / 1_000_000.0) as f32),
                progress_ratio: None,
                quality: DataQuality::Good,
            }
        }
        MetricSource::NetTotalSpeed { .. } => {
            let bps = (snapshot.network.up_bps + snapshot.network.down_bps).max(0.0);
            ResolvedMetric {
                parts: format_speed_parts(bps, fmt),
                numeric: Some((bps * 8.0 / 1_000_000.0) as f32),
                progress_ratio: None,
                quality: DataQuality::Good,
            }
        }
        MetricSource::NetTodayTotal { .. } => match extra.today_traffic_bytes {
            Some(bytes) => {
                let text = format_bytes(bytes);
                let mut split = text.splitn(2, ' ');
                let val = split.next().unwrap_or("0").to_string();
                let unit = split.next().unwrap_or("").to_string();
                ResolvedMetric {
                    parts: FormattedParts::new(val, unit),
                    numeric: Some((bytes as f64 / (1024.0 * 1024.0 * 1024.0)) as f32),
                    progress_ratio: None,
                    quality: DataQuality::Good,
                }
            }
            None => missing_metric(),
        },
        MetricSource::CpuUsage => {
            let pct = snapshot.cpu.usage_pct.clamp(0.0, 100.0);
            ResolvedMetric {
                parts: format_percent_parts(pct, fmt.hide_percent),
                numeric: Some(pct),
                progress_ratio: Some(pct / 100.0),
                quality: DataQuality::Good,
            }
        }
        MetricSource::CpuFreq => {
            match snapshot.cpu.freq_mhz.filter(|v| v.is_finite() && *v > 0.0) {
                Some(mhz) => ResolvedMetric {
                    parts: format_freq_parts(Some(mhz), fmt),
                    numeric: Some(mhz / 1000.0),
                    progress_ratio: None,
                    quality: DataQuality::Good,
                },
                None => missing_metric(),
            }
        }
        MetricSource::CpuTemp { source: item_src } => {
            let effective = if *item_src == CpuTempSource::Auto {
                config.general.cpu_temp_source
            } else {
                *item_src
            };
            if effective == CpuTempSource::Off {
                return missing_metric();
            }
            let allowed = match effective {
                CpuTempSource::Auto => true,
                CpuTempSource::Acpi => snapshot.cpu.temp_source == TempSource::Acpi,
                CpuTempSource::Provider => matches!(
                    snapshot.cpu.temp_source,
                    TempSource::Hwinfo | TempSource::Lhm | TempSource::PawnIo
                ),
                CpuTempSource::Off => false,
            };
            if !allowed {
                return missing_metric();
            }
            resolve_temp(snapshot.cpu.temp_c, snapshot.cpu.temp_source, fmt)
        }
        MetricSource::MemoryUsage { display_as } => {
            let pct = snapshot.memory.usage_pct.clamp(0.0, 100.0);
            ResolvedMetric {
                parts: format_memory_parts(
                    snapshot.memory.used_bytes,
                    snapshot.memory.total_bytes,
                    pct,
                    *display_as,
                    fmt,
                ),
                numeric: Some(pct),
                progress_ratio: Some(pct / 100.0),
                quality: DataQuality::Good,
            }
        }
        MetricSource::GpuUsage { target } => match select_gpu(snapshot, target) {
            Some((_, gpu)) => match gpu.usage_pct.filter(|v| v.is_finite()) {
                Some(pct) => {
                    let pct = pct.clamp(0.0, 100.0);
                    ResolvedMetric {
                        parts: format_percent_parts(pct, fmt.hide_percent),
                        numeric: Some(pct),
                        progress_ratio: Some(pct / 100.0),
                        quality: DataQuality::Good,
                    }
                }
                None => missing_metric(),
            },
            None => missing_metric(),
        },
        MetricSource::GpuTemp { target } => match select_gpu(snapshot, target) {
            Some((_, gpu)) => resolve_temp(gpu.temp_c, gpu.temp_source, fmt),
            None => missing_metric(),
        },
        MetricSource::GpuVram { target } => match select_gpu(snapshot, target) {
            Some((_, gpu)) => match gpu.dedicated_vram_bytes {
                Some(bytes) if bytes > 0 => {
                    let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                    ResolvedMetric {
                        parts: FormattedParts::new(
                            format!("{gb:.1}"),
                            if fmt.hide_unit { "" } else { "GB" },
                        ),
                        numeric: Some(gb as f32),
                        progress_ratio: None,
                        quality: DataQuality::Good,
                    }
                }
                _ => missing_metric(),
            },
            None => missing_metric(),
        },
        MetricSource::DiskTemp { target } => match select_disk(snapshot, target) {
            Some((_, disk)) => resolve_temp(disk.temp_c, disk.temp_source, fmt),
            None => missing_metric(),
        },
        MetricSource::DiskActivity { target } => match select_disk(snapshot, target) {
            Some((_, disk)) => match disk.activity_pct.filter(|v| v.is_finite()) {
                Some(pct) => {
                    let pct = pct.clamp(0.0, 100.0);
                    ResolvedMetric {
                        parts: format_percent_parts(pct, fmt.hide_percent),
                        numeric: Some(pct),
                        progress_ratio: Some(pct / 100.0),
                        quality: DataQuality::Good,
                    }
                }
                None => missing_metric(),
            },
            None => missing_metric(),
        },
        MetricSource::DiskThroughput { target, .. } => {
            // Dedicated R/W throughput counters will populate in sensors; fallback to activity %.
            match select_disk(snapshot, target) {
                Some((_, disk)) => match disk.activity_pct.filter(|v| v.is_finite()) {
                    Some(pct) => {
                        let pct = pct.clamp(0.0, 100.0);
                        ResolvedMetric {
                            parts: format_percent_parts(pct, fmt.hide_percent),
                            numeric: Some(pct),
                            progress_ratio: Some(pct / 100.0),
                            quality: DataQuality::Good,
                        }
                    }
                    None => missing_metric(),
                },
                None => missing_metric(),
            }
        }
        MetricSource::ExternalSensor { sensor, .. } => {
            if let Some(map) = extra.external_sensors
                && let Some((val, unit)) = map.get(sensor)
                && val.is_finite()
            {
                return ResolvedMetric {
                    parts: FormattedParts::new(format!("{val:.1}"), unit.clone()),
                    numeric: Some(*val as f32),
                    progress_ratio: None,
                    quality: DataQuality::Good,
                };
            }
            missing_metric()
        }
        MetricSource::Separator => missing_metric(),
    }
}

fn resolve_temp(
    temp_c: Option<f32>,
    source: TempSource,
    fmt: &crate::format::FormatOptions,
) -> ResolvedMetric {
    match temp_c.filter(|v| v.is_finite()) {
        Some(c) => {
            let suffix = if source == TempSource::Acpi { "~" } else { "" };
            let quality = if source == TempSource::Acpi {
                DataQuality::Suspect
            } else {
                DataQuality::Good
            };
            ResolvedMetric {
                parts: format_temp_parts(Some(c), fmt, suffix),
                numeric: Some(c),
                progress_ratio: Some(((c - 25.0) / 75.0).clamp(0.0, 1.0)),
                quality,
            }
        }
        None => missing_metric(),
    }
}

fn missing_metric() -> ResolvedMetric {
    ResolvedMetric {
        parts: FormattedParts::new("—", ""),
        numeric: None,
        progress_ratio: None,
        quality: DataQuality::Missing,
    }
}

fn resolve_label(
    item: &ItemConfig,
    source: &MetricSource,
    config: &Config,
    snapshot: &Snapshot,
) -> String {
    if !item.label.is_empty() {
        return item.label.clone();
    }
    // Auto-derive label from device alias when user left label empty.
    match source {
        MetricSource::DiskTemp { target } | MetricSource::DiskActivity { target } => {
            if let Some((_, disk)) = select_disk(snapshot, target)
                && let Some(alias) = config.aliases.disk_alias(&disk.name)
            {
                return format!("{alias}:");
            }
            "DISK:".into()
        }
        MetricSource::GpuUsage { target } | MetricSource::GpuTemp { target } => {
            if let Some((_, gpu)) = select_gpu(snapshot, target)
                && let Some(alias) = config.aliases.gpu_alias(&gpu.name)
            {
                return format!("{alias}:");
            }
            "GPU:".into()
        }
        _ => String::new(),
    }
}

/// Resolve a `GpuTarget` against `snapshot.gpus` (with fallback to `snapshot.primary_gpu()`).
pub fn select_gpu<'a>(
    snapshot: &'a Snapshot,
    target: &GpuTarget,
) -> Option<(usize, &'a GpuSample)> {
    if snapshot.gpus.is_empty() {
        return Some((0, snapshot.primary_gpu()));
    }
    match target {
        GpuTarget::Primary => {
            let primary = snapshot.primary_gpu();
            let idx = snapshot
                .gpus
                .iter()
                .position(|g| g.luid.is_some() && g.luid == primary.luid)
                .unwrap_or(0);
            Some((idx, primary))
        }
        GpuTarget::Index(i) => snapshot.gpus.get(*i).map(|g| (*i, g)),
        GpuTarget::Luid(luid) => snapshot
            .gpus
            .iter()
            .enumerate()
            .find(|(_, g)| g.luid == Some(*luid)),
        GpuTarget::Discrete => snapshot
            .gpus
            .iter()
            .enumerate()
            .find(|(_, g)| {
                infer_gpu_role(g.vendor, &g.name, g.dedicated_vram_bytes) == GpuRole::Discrete
            })
            .or_else(|| Some((0, snapshot.primary_gpu()))),
        GpuTarget::Integrated => snapshot.gpus.iter().enumerate().find(|(_, g)| {
            infer_gpu_role(g.vendor, &g.name, g.dedicated_vram_bytes) == GpuRole::Integrated
        }),
    }
}

/// Resolve a `DiskTarget` against `snapshot.disks` (with fallback to `snapshot.disk`).
pub fn select_disk<'a>(
    snapshot: &'a Snapshot,
    target: &DiskTarget,
) -> Option<(usize, &'a DiskSample)> {
    if snapshot.disks.is_empty() {
        return Some((0, &snapshot.disk));
    }
    match target {
        DiskTarget::Primary => {
            let idx = snapshot
                .disks
                .iter()
                .position(|d| d.index.is_some() && d.index == snapshot.disk.index)
                .unwrap_or(0);
            Some((idx, &snapshot.disk))
        }
        DiskTarget::Index(drive_index) => snapshot
            .disks
            .iter()
            .enumerate()
            .find(|(_, d)| d.index == Some(*drive_index)),
        DiskTarget::Model(hint) => {
            let needle = hint.to_ascii_lowercase();
            snapshot
                .disks
                .iter()
                .enumerate()
                .find(|(_, d)| d.name.to_ascii_lowercase().contains(&needle))
        }
    }
}

fn extract_sparkline(
    source: &MetricSource,
    visual: &ItemVisual,
    snapshot: &Snapshot,
    history: &HistoryStore,
) -> (Vec<f32>, Vec<f32>, f32, f32) {
    let (window, scale) = match visual {
        ItemVisual::TextSparkline {
            window_sec, scale, ..
        }
        | ItemVisual::GraphOnly {
            window_sec, scale, ..
        } => (*window_sec as usize, *scale),
        _ => return (Vec::new(), Vec::new(), 0.0, 100.0),
    };

    let (primary_key, secondary_key, default_min, default_max) = match source {
        MetricSource::NetUp { .. } => (
            Some(SeriesKey::NetUp),
            None,
            0.0,
            history.net_auto_scale_peak_bps(),
        ),
        MetricSource::NetDown { .. } => (
            Some(SeriesKey::NetDown),
            None,
            0.0,
            history.net_auto_scale_peak_bps(),
        ),
        MetricSource::NetCombined { .. } => (
            Some(SeriesKey::NetDown),
            Some(SeriesKey::NetUp),
            0.0,
            history.net_auto_scale_peak_bps(),
        ),
        MetricSource::NetTotalSpeed { .. } => (
            Some(SeriesKey::NetTotal),
            None,
            0.0,
            history.net_auto_scale_peak_bps(),
        ),
        MetricSource::CpuUsage => (Some(SeriesKey::CpuUsage), None, 0.0, 100.0),
        MetricSource::CpuFreq => (Some(SeriesKey::CpuFreq), None, 0.0, 6000.0),
        MetricSource::CpuTemp { .. } => (Some(SeriesKey::CpuTemp), None, 30.0, 95.0),
        MetricSource::MemoryUsage { .. } => (Some(SeriesKey::MemoryUsage), None, 0.0, 100.0),
        MetricSource::GpuUsage { target } => {
            let idx = select_gpu(snapshot, target).map(|(i, _)| i).unwrap_or(0);
            (Some(SeriesKey::GpuUsage { index: idx }), None, 0.0, 100.0)
        }
        MetricSource::GpuTemp { target } => {
            let idx = select_gpu(snapshot, target).map(|(i, _)| i).unwrap_or(0);
            (Some(SeriesKey::GpuTemp { index: idx }), None, 30.0, 95.0)
        }
        MetricSource::DiskActivity { target } | MetricSource::DiskThroughput { target, .. } => {
            let idx = select_disk(snapshot, target).map(|(i, _)| i).unwrap_or(0);
            (
                Some(SeriesKey::DiskActivity { index: idx }),
                None,
                0.0,
                100.0,
            )
        }
        MetricSource::DiskTemp { target } => {
            let idx = select_disk(snapshot, target).map(|(i, _)| i).unwrap_or(0);
            (Some(SeriesKey::DiskTemp { index: idx }), None, 25.0, 80.0)
        }
        MetricSource::ExternalSensor { sensor, .. } => (
            Some(SeriesKey::External {
                name: sensor.clone(),
            }),
            None,
            0.0,
            100.0,
        ),
        _ => (None, None, 0.0, 100.0),
    };

    let mut primary = Vec::new();
    let mut secondary = Vec::new();
    if let Some(key) = &primary_key
        && let Some(buf) = history.series(key)
    {
        buf.copy_tail(window, &mut primary);
    }
    if let Some(key) = &secondary_key
        && let Some(buf) = history.series(key)
    {
        buf.copy_tail(window, &mut secondary);
    }

    let (min, max) = match scale {
        SparklineScale::Fixed { min, max } if max > min => (min, max),
        _ => {
            let mut peak = default_max;
            for &v in primary.iter().chain(secondary.iter()) {
                if v.is_finite() && v > peak {
                    peak = v;
                }
            }
            (default_min, peak.max(default_min + 1.0))
        }
    };

    (primary, secondary, min, max)
}

/// Fast, deterministic FNV-1a hash used by `oxide-render` to skip redundant repaints.
fn hash_display_model(items: &[DisplayItemModel], config: &Config, palette: &ThemePalette) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut feed_bytes = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    };

    feed_bytes(&[palette.is_dark as u8, config.surface.opacity]);
    feed_bytes(&config.surface.corner_radius.to_bits().to_le_bytes());
    feed_bytes(&config.layout.rows.to_le_bytes());
    feed_bytes(&config.layout.gap_x.to_le_bytes());
    feed_bytes(&config.layout.gap_y.to_le_bytes());
    feed_bytes(&[config.typography.tabular_nums as u8]);
    feed_bytes(config.typography.value.family.as_bytes());
    feed_bytes(&config.typography.value.size_pt.to_bits().to_le_bytes());

    for item in items {
        feed_bytes(item.uid.as_bytes());
        feed_bytes(item.label.as_bytes());
        feed_bytes(item.value.as_bytes());
        feed_bytes(item.unit.as_bytes());
        feed_bytes(&[
            item.is_separator as u8,
            item.quality as u8,
            item.label_color.r,
            item.label_color.g,
            item.label_color.b,
            item.label_color.a,
            item.value_color.r,
            item.value_color.g,
            item.value_color.b,
            item.value_color.a,
            item.unit_color.r,
            item.unit_color.g,
            item.unit_color.b,
            item.unit_color.a,
        ]);
        if let Some(ratio) = item.progress_ratio {
            // Quantize ratio to 1% steps to avoid pointless redraws on tiny noise.
            let q = (ratio * 100.0).round() as i32;
            feed_bytes(&q.to_le_bytes());
        }
        feed_bytes(&(item.sparkline_primary.len() as u32).to_le_bytes());
        if let Some(&last) = item.sparkline_primary.last() {
            feed_bytes(&last.to_bits().to_le_bytes());
        }
        if let Some(&last) = item.sparkline_secondary.last() {
            feed_bytes(&last.to_bits().to_le_bytes());
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GpuTarget, thermal_gradient};
    use crate::metrics::GpuVendor;

    #[test]
    fn resolves_multi_gpu_and_disk_aliases_and_dirty_hash() {
        let mut config = Config::default();
        config
            .aliases
            .disks
            .insert("Apacer AS2280P4U 1TB".into(), "NVMe".into());
        config.items = vec![
            ItemConfig::new_v2(
                "igpu",
                "iGPU:",
                MetricSource::GpuTemp {
                    target: GpuTarget::Integrated,
                },
                ItemVisual::Text,
                Some(thermal_gradient(40.0, 65.0, 85.0)),
            ),
            ItemConfig::new_v2(
                "dgpu",
                "GTX:",
                MetricSource::GpuTemp {
                    target: GpuTarget::Discrete,
                },
                ItemVisual::Text,
                Some(thermal_gradient(40.0, 65.0, 85.0)),
            ),
            ItemConfig::new_v2(
                "disk0",
                "",
                MetricSource::DiskTemp {
                    target: DiskTarget::Index(0),
                },
                ItemVisual::Text,
                None,
            ),
        ];

        let mut snap = Snapshot::default();
        snap.gpus = vec![
            GpuSample {
                name: "AMD Radeon(TM) Graphics".into(),
                vendor: GpuVendor::Amd,
                luid: Some(1),
                usage_pct: Some(5.0),
                temp_c: Some(47.0),
                temp_source: TempSource::Adlx,
                dedicated_vram_bytes: Some(512 * 1024 * 1024),
            },
            GpuSample {
                name: "NVIDIA GeForce GTX 1650".into(),
                vendor: GpuVendor::Nvidia,
                luid: Some(2),
                usage_pct: Some(80.0),
                temp_c: Some(82.0),
                temp_source: TempSource::Nvml,
                dedicated_vram_bytes: Some(4 * 1024 * 1024 * 1024),
            },
        ];
        snap.gpu = snap.gpus[1].clone();
        snap.disks = vec![DiskSample {
            name: "Apacer AS2280P4U 1TB".into(),
            index: Some(0),
            is_nvme: true,
            activity_pct: Some(3.0),
            temp_c: Some(41.0),
            temp_source: TempSource::Nvme,
        }];
        snap.disk = snap.disks[0].clone();

        let mut history = HistoryStore::new();
        history.record_snapshot(&snap);
        let palette = ThemePalette::dark();
        let extra = ExtraTelemetry::default();

        let model1 = DisplayModelBuilder::build(&config, &snap, &history, &palette, &extra);
        assert_eq!(model1.items.len(), 3);
        assert_eq!(model1.items[0].value, "47");
        assert_eq!(model1.items[1].value, "82");
        assert_eq!(model1.items[2].label, "NVMe:");
        assert_eq!(model1.items[2].value, "41");
        // Hotter dGPU (82C) should be redder than cooler iGPU (47C).
        assert!(model1.items[1].value_color.r > model1.items[0].value_color.r);

        let model2 = DisplayModelBuilder::build(&config, &snap, &history, &palette, &extra);
        assert_eq!(model1.content_hash, model2.content_hash);

        snap.disks[0].temp_c = Some(43.0);
        let model3 = DisplayModelBuilder::build(&config, &snap, &history, &palette, &extra);
        assert_ne!(model1.content_hash, model3.content_hash);
    }
}
