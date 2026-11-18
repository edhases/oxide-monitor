//! Shared Direct2D / DirectWrite widget renderer and `LayoutEngine v2` for Oxide Monitor.
//!
//! Renders into a premultiplied BGRA8 DIB without creating any D3D device (0 GPU wake-up),
//! serving both the live Win32 taskbar overlay (`UpdateLayeredWindowIndirect`) and
//! pixel-exact Live Preview inside `oxide-settings.exe`.

pub mod d2d;
pub mod dib;
pub mod flyout;
pub mod layout;

use std::ffi::c_void;
use std::mem::size_of;

use oxide_core::{Rgba, ThemePalette};
use thiserror::Error;
use windows::Win32::Graphics::Gdi::HDC;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::w;

pub use d2d::WidgetRenderer;
pub use dib::{DibSurface, bgra_premultiplied_to_rgba, present_layered_window};
pub use flyout::{FlyoutButton, FlyoutFrame, FlyoutHitZone, FlyoutInput, FlyoutRenderer};
pub use layout::{CellPlacement, GroupBox, LayoutEngine, RectF, WidgetLayout};

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("win32 error: {0}")]
    Win32(String),
    #[error("Direct2D error: {0}")]
    D2d(String),
    #[error("DirectWrite error: {0}")]
    DWrite(String),
}

impl RenderError {
    pub(crate) fn win32(call: &str, err: impl std::fmt::Display) -> Self {
        Self::Win32(format!("{call}: {err}"))
    }

    pub(crate) fn d2d(call: &str, err: impl std::fmt::Display) -> Self {
        Self::D2d(format!("{call}: {err}"))
    }

    pub(crate) fn dwrite(call: &str, err: impl std::fmt::Display) -> Self {
        Self::DWrite(format!("{call}: {err}"))
    }
}

/// Output of one render pass, borrowing the renderer's reusable DIB buffer.
pub struct RenderedFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub width_dip: f32,
    pub height_dip: f32,
    pub bgra_premultiplied: &'a [u8],
    pub hdc: HDC,
    pub content_hash: u64,
}

impl RenderedFrame<'_> {
    /// Convert to RGBA8 bytes suitable for `egui::ColorImage::from_rgba_premultiplied`.
    pub fn to_rgba_premultiplied(&self) -> Vec<u8> {
        bgra_premultiplied_to_rgba(self.bgra_premultiplied)
    }
}

/// OS theme context queried from the Windows Personalize registry keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemThemeContext {
    pub system_is_light: bool,
    pub accent: Option<Rgba>,
}

impl SystemThemeContext {
    /// Read `SystemUsesLightTheme` from `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize`.
    pub fn query_windows() -> Self {
        let system_is_light = read_reg_dword(
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
        )
        .is_some_and(|v| v != 0);

        let accent = read_reg_dword(w!("Software\\Microsoft\\Windows\\DWM"), w!("AccentColor"))
            .map(|abgr| {
                Rgba::rgb(
                    (abgr & 0xff) as u8,
                    ((abgr >> 8) & 0xff) as u8,
                    ((abgr >> 16) & 0xff) as u8,
                )
            });

        Self {
            system_is_light,
            accent,
        }
    }

    pub fn resolve_palette(&self, theme_name: &str) -> ThemePalette {
        let palette = ThemePalette::resolve(theme_name, self.system_is_light);
        match self.accent {
            Some(accent) => palette.with_accent(accent),
            None => palette,
        }
    }
}

fn read_reg_dword(subkey: windows::core::PCWSTR, value: windows::core::PCWSTR) -> Option<u32> {
    let mut data: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some((&mut data as *mut u32).cast::<c_void>()),
            Some(&mut size),
        )
    };
    if status.0 == 0 { Some(data) } else { None }
}
