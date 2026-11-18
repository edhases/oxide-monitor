//! RGBA colors, semantic theme tokens, OKLab interpolation, and dynamic `ColorRule`s.

use serde::{Deserialize, Serialize};

use crate::theme::ThemePalette;

/// Straight (non-premultiplied) 8-bit RGBA color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const TRANSPARENT: Self = Self::new(0, 0, 0, 0);
    pub const WHITE: Self = Self::new(255, 255, 255, 255);
    pub const BLACK: Self = Self::new(0, 0, 0, 255);

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// Scale alpha channel by `factor` in `0.0..=1.0`.
    pub fn scale_alpha(self, factor: f32) -> Self {
        let a = ((self.a as f32) * factor.clamp(0.0, 1.0)).round() as u8;
        Self { a, ..self }
    }

    /// Convert from Win32 `COLORREF` (`0x00BBGGRR`) with full opacity.
    pub const fn from_colorref(colorref: u32) -> Self {
        Self {
            r: (colorref & 0xff) as u8,
            g: ((colorref >> 8) & 0xff) as u8,
            b: ((colorref >> 16) & 0xff) as u8,
            a: 255,
        }
    }

    /// Convert to Win32 `COLORREF` (`0x00BBGGRR`), discarding alpha.
    pub const fn to_colorref(self) -> u32 {
        ((self.b as u32) << 16) | ((self.g as u32) << 8) | (self.r as u32)
    }

    /// Convert to normalized `f32` channels `(r, g, b, a)` in `0.0..=1.0`.
    pub fn to_f32(self) -> [f32; 4] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.a as f32 / 255.0,
        ]
    }

    /// Premultiplied `[b, g, r, a]` pixel suitable for Win32 DIB / `UpdateLayeredWindowIndirect`.
    pub const fn to_bgra_premultiplied(self) -> [u8; 4] {
        let a = self.a as u16;
        let r = ((self.r as u16 * a + 127) / 255) as u8;
        let g = ((self.g as u16 * a + 127) / 255) as u8;
        let b = ((self.b as u16 * a + 127) / 255) as u8;
        [b, g, r, self.a]
    }

    /// Parse `#RGB`, `#RGBA`, `#RRGGBB`, or `#RRGGBBAA` (leading `#` optional).
    pub fn parse_hex(input: &str) -> Option<Self> {
        let hex = input.trim().trim_start_matches('#');
        match hex.len() {
            3 => {
                let r = parse_nibble(&hex[0..1])?;
                let g = parse_nibble(&hex[1..2])?;
                let b = parse_nibble(&hex[2..3])?;
                Some(Self::rgb(r, g, b))
            }
            4 => {
                let r = parse_nibble(&hex[0..1])?;
                let g = parse_nibble(&hex[1..2])?;
                let b = parse_nibble(&hex[2..3])?;
                let a = parse_nibble(&hex[3..4])?;
                Some(Self::new(r, g, b, a))
            }
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some(Self::rgb(r, g, b))
            }
            8 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
                Some(Self::new(r, g, b, a))
            }
            _ => None,
        }
    }

    /// Format as `#RRGGBB` when opaque or `#RRGGBBAA` when translucent.
    pub fn to_hex(self) -> String {
        if self.a == 255 {
            format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
        } else {
            format!("#{:02X}{:02X}{:02X}{:02X}", self.r, self.g, self.b, self.a)
        }
    }
}

fn parse_nibble(s: &str) -> Option<u8> {
    let v = u8::from_str_radix(s, 16).ok()?;
    Some((v << 4) | v)
}

/// Either a concrete `#RRGGBB[AA]` color or a semantic theme token like `"theme.accent"`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ColorSpec {
    Static(Rgba),
    ThemeBackground,
    ThemeLabel,
    ThemeValue,
    ThemeUnit,
    ThemeAccent,
    ThemeBorder,
    ThemeMuted,
}

impl Default for ColorSpec {
    fn default() -> Self {
        Self::ThemeValue
    }
}

impl ColorSpec {
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        match trimmed {
            "theme.background" => Some(Self::ThemeBackground),
            "theme.label" => Some(Self::ThemeLabel),
            "theme.value" => Some(Self::ThemeValue),
            "theme.unit" => Some(Self::ThemeUnit),
            "theme.accent" => Some(Self::ThemeAccent),
            "theme.border" => Some(Self::ThemeBorder),
            "theme.muted" => Some(Self::ThemeMuted),
            other => Rgba::parse_hex(other).map(Self::Static),
        }
    }

    pub fn resolve(&self, palette: &ThemePalette) -> Rgba {
        match self {
            Self::Static(rgba) => *rgba,
            Self::ThemeBackground => palette.background,
            Self::ThemeLabel => palette.label,
            Self::ThemeValue => palette.value,
            Self::ThemeUnit => palette.unit,
            Self::ThemeAccent => palette.accent,
            Self::ThemeBorder => palette.border,
            Self::ThemeMuted => palette.muted,
        }
    }

    pub fn to_config_string(&self) -> String {
        match self {
            Self::Static(rgba) => rgba.to_hex(),
            Self::ThemeBackground => "theme.background".into(),
            Self::ThemeLabel => "theme.label".into(),
            Self::ThemeValue => "theme.value".into(),
            Self::ThemeUnit => "theme.unit".into(),
            Self::ThemeAccent => "theme.accent".into(),
            Self::ThemeBorder => "theme.border".into(),
            Self::ThemeMuted => "theme.muted".into(),
        }
    }
}

impl Serialize for ColorSpec {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_config_string())
    }
}

impl<'de> Deserialize<'de> for ColorSpec {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "invalid color spec '{s}': expected #RRGGBB, #RRGGBBAA, or theme.<token>"
            ))
        })
    }
}

/// Interpolation space for continuous `ColorRule::Gradient`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorSpace {
    #[default]
    Oklab,
    Srgb,
}

/// A threshold stop in `ColorRule::Steps` or `ColorRule::Gradient`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorStop {
    pub at: f32,
    pub color: ColorSpec,
}

/// Dynamic coloring policy for a metric item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ColorRule {
    Static {
        color: ColorSpec,
    },
    Steps {
        stops: Vec<ColorStop>,
    },
    Gradient {
        #[serde(default)]
        space: ColorSpace,
        stops: Vec<ColorStop>,
    },
}

impl ColorRule {
    pub fn static_hex(hex: &str) -> Option<Self> {
        Some(Self::Static {
            color: ColorSpec::parse(hex)?,
        })
    }

    pub fn static_rgba(rgba: Rgba) -> Self {
        Self::Static {
            color: ColorSpec::Static(rgba),
        }
    }

    /// Sort stops ascending and discard non-finite threshold values.
    pub fn normalize(&mut self) {
        match self {
            Self::Static { .. } => {}
            Self::Steps { stops } | Self::Gradient { stops, .. } => {
                stops.retain(|stop| stop.at.is_finite());
                stops.sort_by(|a, b| a.at.total_cmp(&b.at));
            }
        }
    }

    /// Resolve a concrete RGBA color for a numeric reading.
    pub fn resolve(&self, value: Option<f32>, palette: &ThemePalette) -> Rgba {
        match self {
            Self::Static { color } => color.resolve(palette),
            Self::Steps { stops } => {
                if stops.is_empty() {
                    return palette.value;
                }
                let Some(v) = value.filter(|v| v.is_finite()) else {
                    return palette.muted;
                };
                // Pick the highest stop whose threshold `at <= v` (or first stop if below all).
                let mut chosen = &stops[0];
                for stop in stops {
                    if stop.at.is_finite() && stop.at <= v && stop.at >= chosen.at {
                        chosen = stop;
                    } else if stop.at.is_finite() && chosen.at > v && stop.at < chosen.at {
                        chosen = stop;
                    }
                }
                chosen.color.resolve(palette)
            }
            Self::Gradient { space, stops } => {
                if stops.is_empty() {
                    return palette.value;
                }
                let Some(v) = value.filter(|v| v.is_finite()) else {
                    return palette.muted;
                };
                let mut sorted: Vec<&ColorStop> =
                    stops.iter().filter(|s| s.at.is_finite()).collect();
                if sorted.is_empty() {
                    return palette.value;
                }
                sorted.sort_by(|a, b| a.at.total_cmp(&b.at));

                if v <= sorted[0].at {
                    return sorted[0].color.resolve(palette);
                }
                let last = sorted[sorted.len() - 1];
                if v >= last.at {
                    return last.color.resolve(palette);
                }

                for pair in sorted.windows(2) {
                    let lo = pair[0];
                    let hi = pair[1];
                    if v >= lo.at && v <= hi.at {
                        let span = hi.at - lo.at;
                        let t = if span <= f32::EPSILON {
                            1.0
                        } else {
                            ((v - lo.at) / span).clamp(0.0, 1.0)
                        };
                        let c0 = lo.color.resolve(palette);
                        let c1 = hi.color.resolve(palette);
                        return match space {
                            ColorSpace::Oklab => lerp_oklab(c0, c1, t),
                            ColorSpace::Srgb => lerp_srgb(c0, c1, t),
                        };
                    }
                }
                last.color.resolve(palette)
            }
        }
    }
}

/// Linear interpolation directly in sRGB space.
pub fn lerp_srgb(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| {
        ((x as f32) + ((y as f32) - (x as f32)) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Rgba::new(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b), mix(a.a, b.a))
}

/// Perceptually uniform color interpolation in Björn Ottosson's OKLab space.
pub fn lerp_oklab(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    let (l0, a0, b0) = srgb_to_oklab(a);
    let (l1, a1, b1) = srgb_to_oklab(b);
    let l = l0 + (l1 - l0) * t;
    let ca = a0 + (a1 - a0) * t;
    let cb = b0 + (b1 - b0) * t;
    let alpha = ((a.a as f32) + ((b.a as f32) - (a.a as f32)) * t)
        .round()
        .clamp(0.0, 255.0) as u8;
    oklab_to_srgb(l, ca, cb, alpha)
}

fn srgb_to_linear(c: u8) -> f32 {
    let v = c as f32 / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let v = if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Convert sRGB `Rgba` to OKLab `(L, a, b)`.
pub fn srgb_to_oklab(c: Rgba) -> (f32, f32, f32) {
    let r = srgb_to_linear(c.r);
    let g = srgb_to_linear(c.g);
    let b = srgb_to_linear(c.b);

    let l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
    let m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
    let s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;

    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    (
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    )
}

/// Convert OKLab `(L, a, b)` + alpha back to sRGB `Rgba`.
pub fn oklab_to_srgb(l: f32, a: f32, b: f32, alpha: u8) -> Rgba {
    let l_ = l + 0.3963377774 * a + 0.2158037573 * b;
    let m_ = l - 0.1055613458 * a - 0.0638541728 * b;
    let s_ = l - 0.0894841775 * a - 1.2914855480 * b;

    let l3 = l_ * l_ * l_;
    let m3 = m_ * m_ * m_;
    let s3 = s_ * s_ * s_;

    let r = 4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3;
    let g = -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3;
    let bv = -0.0041960863 * l3 - 0.7034186147 * m3 + 1.7076147010 * s3;

    Rgba::new(
        linear_to_srgb(r),
        linear_to_srgb(g),
        linear_to_srgb(bv),
        alpha,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parsing_and_formatting() {
        assert_eq!(
            Rgba::parse_hex("#7FD37F"),
            Some(Rgba::rgb(0x7F, 0xD3, 0x7F))
        );
        assert_eq!(
            Rgba::parse_hex("#202020D8"),
            Some(Rgba::new(0x20, 0x20, 0x20, 0xD8))
        );
        assert_eq!(Rgba::parse_hex("#F0A"), Some(Rgba::rgb(0xFF, 0x00, 0xAA)));
        assert_eq!(
            Rgba::parse_hex("#F0A8"),
            Some(Rgba::new(0xFF, 0x00, 0xAA, 0x88))
        );
        assert_eq!(Rgba::parse_hex("xyz"), None);
        assert_eq!(Rgba::parse_hex("#12345"), None);

        assert_eq!(Rgba::rgb(0x7F, 0xD3, 0x7F).to_hex(), "#7FD37F");
        assert_eq!(Rgba::new(0x20, 0x20, 0x20, 0xD8).to_hex(), "#202020D8");
    }

    #[test]
    fn colorref_roundtrip() {
        let c = Rgba::rgb(0x12, 0x34, 0x56);
        assert_eq!(Rgba::from_colorref(c.to_colorref()), c);
    }

    #[test]
    fn oklab_roundtrip_is_accurate() {
        for c in [
            Rgba::rgb(0x4A, 0xDE, 0x80),
            Rgba::rgb(0xF0, 0xC6, 0x74),
            Rgba::rgb(0xF2, 0x6D, 0x6D),
            Rgba::rgb(0x00, 0x78, 0xD4),
        ] {
            let (l, a, b) = srgb_to_oklab(c);
            let out = oklab_to_srgb(l, a, b, 255);
            assert!((out.r as i16 - c.r as i16).abs() <= 1);
            assert!((out.g as i16 - c.g as i16).abs() <= 1);
            assert!((out.b as i16 - c.b as i16).abs() <= 1);
        }
    }

    #[test]
    fn oklab_green_to_red_midpoint_stays_bright_amber() {
        let green = Rgba::rgb(0x4A, 0xDE, 0x80);
        let red = Rgba::rgb(0xEF, 0x44, 0x44);
        let mid_oklab = lerp_oklab(green, red, 0.5);
        let mid_srgb = lerp_srgb(green, red, 0.5);
        // OKLab preserves perceptual lightness instead of dipping into muddy brown-gray.
        assert!(mid_oklab.r > mid_srgb.r);
        assert!(mid_oklab.g > mid_srgb.g);
    }

    #[test]
    fn color_rule_steps_and_gradient() {
        let palette = ThemePalette::dark();
        let steps = ColorRule::Steps {
            stops: vec![
                ColorStop {
                    at: 0.0,
                    color: ColorSpec::Static(Rgba::rgb(0, 200, 0)),
                },
                ColorStop {
                    at: 60.0,
                    color: ColorSpec::Static(Rgba::rgb(240, 200, 0)),
                },
                ColorStop {
                    at: 80.0,
                    color: ColorSpec::Static(Rgba::rgb(240, 50, 50)),
                },
            ],
        };
        assert_eq!(steps.resolve(Some(45.0), &palette), Rgba::rgb(0, 200, 0));
        assert_eq!(steps.resolve(Some(60.0), &palette), Rgba::rgb(240, 200, 0));
        assert_eq!(steps.resolve(Some(85.0), &palette), Rgba::rgb(240, 50, 50));
        assert_eq!(steps.resolve(None, &palette), palette.muted);

        let grad = ColorRule::Gradient {
            space: ColorSpace::Oklab,
            stops: vec![
                ColorStop {
                    at: 40.0,
                    color: ColorSpec::Static(Rgba::rgb(0x4A, 0xDE, 0x80)),
                },
                ColorStop {
                    at: 80.0,
                    color: ColorSpec::Static(Rgba::rgb(0xEF, 0x44, 0x44)),
                },
            ],
        };
        assert_eq!(
            grad.resolve(Some(20.0), &palette),
            Rgba::rgb(0x4A, 0xDE, 0x80)
        );
        assert_eq!(
            grad.resolve(Some(95.0), &palette),
            Rgba::rgb(0xEF, 0x44, 0x44)
        );
        let mid = grad.resolve(Some(60.0), &palette);
        assert_ne!(mid, Rgba::rgb(0x4A, 0xDE, 0x80));
        assert_ne!(mid, Rgba::rgb(0xEF, 0x44, 0x44));
    }
}
