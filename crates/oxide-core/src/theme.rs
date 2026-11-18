//! Color themes (`Theme` for legacy GDI and `ThemePalette` for Direct2D/Fluent).

use crate::color::Rgba;

/// A small color theme. Colors are Win32 `COLORREF` values: `0x00BBGGRR`.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub background: u32,
    pub label: u32,
    pub value: u32,
    pub accent: u32,
    pub separator: u32,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            background: rgb(32, 32, 32),
            label: rgb(138, 155, 168),
            value: rgb(230, 230, 230),
            accent: rgb(0, 120, 212),
            separator: rgb(64, 64, 64),
        }
    }

    pub fn light() -> Self {
        Self {
            background: rgb(243, 243, 243),
            label: rgb(90, 100, 110),
            value: rgb(30, 30, 30),
            accent: rgb(0, 95, 184),
            separator: rgb(210, 210, 210),
        }
    }
}

/// Full RGBA palette for Direct2D rendering, Flyout cards, and semantic color tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemePalette {
    pub is_dark: bool,
    pub background: Rgba,
    pub surface_card: Rgba,
    pub label: Rgba,
    pub value: Rgba,
    pub unit: Rgba,
    pub accent: Rgba,
    pub border: Rgba,
    pub separator: Rgba,
    pub muted: Rgba,
    pub shadow: Rgba,
}

impl ThemePalette {
    pub fn dark() -> Self {
        Self {
            is_dark: true,
            background: Rgba::new(32, 32, 32, 235),
            surface_card: Rgba::new(44, 44, 44, 210),
            label: Rgba::rgb(154, 166, 178),
            value: Rgba::rgb(240, 240, 240),
            unit: Rgba::new(200, 208, 216, 184),
            accent: Rgba::rgb(96, 205, 255),
            border: Rgba::new(255, 255, 255, 24),
            separator: Rgba::new(255, 255, 255, 36),
            muted: Rgba::new(138, 155, 168, 140),
            shadow: Rgba::new(0, 0, 0, 100),
        }
    }

    pub fn light() -> Self {
        Self {
            is_dark: false,
            background: Rgba::new(243, 243, 243, 235),
            surface_card: Rgba::new(255, 255, 255, 215),
            label: Rgba::rgb(90, 100, 110),
            value: Rgba::rgb(24, 24, 24),
            unit: Rgba::new(64, 72, 80, 190),
            accent: Rgba::rgb(0, 95, 184),
            border: Rgba::new(0, 0, 0, 24),
            separator: Rgba::new(0, 0, 0, 32),
            muted: Rgba::new(110, 120, 130, 150),
            shadow: Rgba::new(255, 255, 255, 110),
        }
    }

    /// Resolve palette from a config theme name (`"auto"`, `"dark"`, `"light"`)
    /// and the OS taskbar theme (`system_is_light`).
    pub fn resolve(theme_name: &str, system_is_light: bool) -> Self {
        match theme_name.trim().to_ascii_lowercase().as_str() {
            "light" => Self::light(),
            "dark" => Self::dark(),
            _ => {
                if system_is_light {
                    Self::light()
                } else {
                    Self::dark()
                }
            }
        }
    }

    /// Override the accent color (e.g. from `DwmGetColorizationColor`).
    pub const fn with_accent(mut self, accent: Rgba) -> Self {
        self.accent = accent;
        self
    }

    /// Project down to the legacy `COLORREF` `Theme`.
    pub const fn to_legacy(self) -> Theme {
        Theme {
            background: self.background.to_colorref(),
            label: self.label.to_colorref(),
            value: self.value.to_colorref(),
            accent: self.accent.to_colorref(),
            separator: self.separator.to_colorref(),
        }
    }
}

pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    ((b as u32) << 16) | ((g as u32) << 8) | (r as u32)
}

/// Parse `#RRGGBB` (or `RRGGBB`) into a `COLORREF`.
pub fn parse_color(value: &str) -> Option<u32> {
    let value = value.trim().trim_start_matches('#');
    if value.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&value[0..2], 16).ok()?;
    let g = u8::from_str_radix(&value[2..4], 16).ok()?;
    let b = u8::from_str_radix(&value[4..6], 16).ok()?;
    Some(rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_roundtrip() {
        let color = parse_color("#7FD37F").unwrap();
        assert_eq!(color, rgb(0x7F, 0xD3, 0x7F));
    }

    #[test]
    fn palette_auto_resolves_system_theme() {
        assert!(!ThemePalette::resolve("auto", true).is_dark);
        assert!(ThemePalette::resolve("auto", false).is_dark);
        assert!(ThemePalette::resolve("dark", true).is_dark);
        assert!(!ThemePalette::resolve("light", false).is_dark);
    }
}
