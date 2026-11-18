//! Number, speed, percentage, temperature, frequency, and byte-size formatting.

use serde::{Deserialize, Serialize};

use crate::config::MemoryDisplayMode;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeedUnit {
    #[default]
    Auto,
    Kbps,
    Mbps,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecimalSeparator {
    #[default]
    Dot,
    Comma,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TempUnit {
    #[default]
    Celsius,
    Fahrenheit,
}

/// Separate formatted numeric value and unit strings so the renderer can apply
/// distinct typography (`FontSpec`) and colors to numbers vs units.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct FormattedParts {
    pub value: String,
    pub unit: String,
}

impl FormattedParts {
    pub fn new(value: impl Into<String>, unit: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            unit: unit.into(),
        }
    }

    pub fn join(&self, separate_space: bool) -> String {
        if self.unit.is_empty() {
            self.value.clone()
        } else if separate_space {
            format!("{} {}", self.value, self.unit)
        } else {
            format!("{}{}", self.value, self.unit)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FormatOptions {
    /// Use bytes (B/s) instead of bits (b/s) for network speed.
    #[serde(default)]
    pub unit_byte: bool,
    #[serde(default)]
    pub speed_unit: SpeedUnit,
    /// Number of digits used by the layout engine (not the numeric precision).
    #[serde(default = "default_digits")]
    pub digits: usize,
    #[serde(default)]
    pub hide_unit: bool,
    #[serde(default)]
    pub hide_percent: bool,
    #[serde(default = "default_true")]
    pub separate_space: bool,
    /// Compact speed format: `2.7M` instead of `2.7 Mb/s`.
    #[serde(default)]
    pub short_mode: bool,
    #[serde(default)]
    pub decimal_separator: DecimalSeparator,
    #[serde(default)]
    pub temp_unit: TempUnit,
}

fn default_digits() -> usize {
    4
}
fn default_true() -> bool {
    true
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            unit_byte: false,
            speed_unit: SpeedUnit::Mbps,
            digits: 4,
            hide_unit: false,
            hide_percent: false,
            separate_space: true,
            short_mode: false,
            decimal_separator: DecimalSeparator::Dot,
            temp_unit: TempUnit::Celsius,
        }
    }
}

impl FormatOptions {
    pub fn normalize(&mut self) {
        self.digits = self.digits.clamp(2, 8);
    }

    fn apply_sep(&self, s: String) -> String {
        match self.decimal_separator {
            DecimalSeparator::Dot => s,
            DecimalSeparator::Comma => s.replace('.', ","),
        }
    }
}

/// Format a network throughput value into separate `(value, unit)` parts.
pub fn format_speed_parts(bytes_per_sec: f64, opts: &FormatOptions) -> FormattedParts {
    let value = if opts.unit_byte {
        bytes_per_sec.max(0.0)
    } else {
        bytes_per_sec.max(0.0) * 8.0
    };

    let (scaled_auto, unit_auto) = if opts.unit_byte {
        let (v, prefix) = scale(value, 1024.0);
        (v, format!("{prefix}B/s"))
    } else {
        let (v, prefix) = scale(value, 1000.0);
        (v, format!("{prefix}b/s"))
    };

    let scaled = match opts.speed_unit {
        SpeedUnit::Auto => scaled_auto,
        SpeedUnit::Kbps => {
            if opts.unit_byte {
                value / 1024.0
            } else {
                value / 1000.0
            }
        }
        SpeedUnit::Mbps => {
            if opts.unit_byte {
                value / (1024.0 * 1024.0)
            } else {
                value / 1_000_000.0
            }
        }
    };

    let unit = match opts.speed_unit {
        SpeedUnit::Auto => unit_auto,
        SpeedUnit::Kbps => {
            if opts.unit_byte {
                "KB/s".to_string()
            } else {
                "Kb/s".to_string()
            }
        }
        SpeedUnit::Mbps => {
            if opts.unit_byte {
                "MB/s".to_string()
            } else {
                "Mb/s".to_string()
            }
        }
    };

    let num = opts.apply_sep(format!("{scaled:.1}"));
    if opts.hide_unit {
        return FormattedParts::new(num, "");
    }
    if opts.short_mode {
        let short = match unit.as_str() {
            "B/s" => "B",
            "KB/s" => "K",
            "MB/s" => "M",
            "GB/s" => "G",
            "b/s" => "b",
            "Kb/s" => "K",
            "Mb/s" => "M",
            "Gb/s" => "G",
            other => other,
        };
        return FormattedParts::new(num, short);
    }
    FormattedParts::new(num, unit)
}

/// Format a network throughput value. `bytes_per_sec` is the raw byte rate.
pub fn format_speed(bytes_per_sec: f64, opts: &FormatOptions) -> String {
    let parts = format_speed_parts(bytes_per_sec, opts);
    if opts.short_mode {
        parts.join(false)
    } else {
        parts.join(opts.separate_space)
    }
}

fn scale(value: f64, base: f64) -> (f64, &'static str) {
    if value >= base * base * base {
        (value / (base * base * base), "G")
    } else if value >= base * base {
        (value / (base * base), "M")
    } else if value >= base {
        (value / base, "K")
    } else {
        (value, "")
    }
}

pub fn format_percent_parts(pct: f32, hide_percent: bool) -> FormattedParts {
    let pct = pct.clamp(0.0, 100.0);
    let num = format!("{pct:.0}");
    if hide_percent {
        FormattedParts::new(num, "")
    } else {
        FormattedParts::new(num, "%")
    }
}

pub fn format_percent(pct: f32, hide_percent: bool) -> String {
    format_percent_parts(pct, hide_percent).join(false)
}

pub fn format_temp_parts(
    temp_c: Option<f32>,
    opts: &FormatOptions,
    suffix: &str,
) -> FormattedParts {
    match temp_c {
        Some(value) if value.is_finite() => {
            let (display_val, unit_str) = match opts.temp_unit {
                TempUnit::Celsius => (value, "°C"),
                TempUnit::Fahrenheit => (value * 9.0 / 5.0 + 32.0, "°F"),
            };
            let mut unit = if opts.hide_unit {
                String::new()
            } else {
                unit_str.to_string()
            };
            if !suffix.is_empty() {
                unit.push_str(suffix);
            }
            FormattedParts::new(format!("{display_val:.0}"), unit)
        }
        _ => FormattedParts::new("—", ""),
    }
}

pub fn format_temp(temp_c: Option<f32>) -> String {
    match temp_c {
        Some(value) if value.is_finite() => format!("{value:.0} °C"),
        _ => "—".to_string(),
    }
}

pub fn format_freq_parts(freq_mhz: Option<f32>, opts: &FormatOptions) -> FormattedParts {
    match freq_mhz {
        Some(mhz) if mhz.is_finite() && mhz > 0.0 => {
            let ghz = mhz / 1000.0;
            let num = opts.apply_sep(format!("{ghz:.2}"));
            let unit = if opts.hide_unit {
                ""
            } else if opts.short_mode {
                "G"
            } else {
                "GHz"
            };
            FormattedParts::new(num, unit)
        }
        _ => FormattedParts::new("—", ""),
    }
}

pub fn format_memory_parts(
    used_bytes: u64,
    total_bytes: u64,
    usage_pct: f32,
    mode: MemoryDisplayMode,
    opts: &FormatOptions,
) -> FormattedParts {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    match mode {
        MemoryDisplayMode::Percent => format_percent_parts(usage_pct, opts.hide_percent),
        MemoryDisplayMode::UsedGb => {
            let used_gb = used_bytes as f64 / GIB;
            let num = opts.apply_sep(format!("{used_gb:.1}"));
            let unit = if opts.hide_unit {
                ""
            } else if opts.short_mode {
                "G"
            } else {
                "GB"
            };
            FormattedParts::new(num, unit)
        }
        MemoryDisplayMode::UsedTotalGb => {
            let used_gb = used_bytes as f64 / GIB;
            let total_gb = total_bytes as f64 / GIB;
            let num = opts.apply_sep(format!("{used_gb:.1}/{total_gb:.0}"));
            let unit = if opts.hide_unit {
                ""
            } else if opts.short_mode {
                "G"
            } else {
                "GB"
            };
            FormattedParts::new(num, unit)
        }
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_bits_auto() {
        let opts = FormatOptions {
            speed_unit: SpeedUnit::Auto,
            ..Default::default()
        };
        assert_eq!(format_speed(337_500.0, &opts), "2.7 Mb/s");
        assert_eq!(format_speed(37_500.0, &opts), "300.0 Kb/s");
    }

    #[test]
    fn speed_default_mbps() {
        let opts = FormatOptions::default();
        assert_eq!(format_speed(37_500.0, &opts), "0.3 Mb/s");
    }

    #[test]
    fn speed_bytes_mbps() {
        let opts = FormatOptions {
            unit_byte: true,
            speed_unit: SpeedUnit::Mbps,
            ..Default::default()
        };
        assert_eq!(format_speed(1_048_576.0, &opts), "1.0 MB/s");
    }

    #[test]
    fn temp_none_and_fahrenheit() {
        assert_eq!(format_temp(None), "—");
        assert_eq!(format_temp(Some(41.4)), "41 °C");

        let f_opts = FormatOptions {
            temp_unit: TempUnit::Fahrenheit,
            ..Default::default()
        };
        let parts = format_temp_parts(Some(100.0), &f_opts, "");
        assert_eq!(parts, FormattedParts::new("212", "°F"));
    }

    #[test]
    fn decimal_comma_and_memory_modes() {
        let opts = FormatOptions {
            decimal_separator: DecimalSeparator::Comma,
            ..Default::default()
        };
        let p = format_freq_parts(Some(3420.0), &opts);
        assert_eq!(p, FormattedParts::new("3,42", "GHz"));

        let m = format_memory_parts(
            10_737_418_240,
            17_179_869_184,
            62.5,
            MemoryDisplayMode::UsedTotalGb,
            &opts,
        );
        assert_eq!(m, FormattedParts::new("10,0/16", "GB"));
    }
}
