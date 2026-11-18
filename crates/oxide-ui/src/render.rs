//! GDI double-buffered renderer for the overlay.

use oxide_core::{
    Config, ItemId, Snapshot, TempSource, Theme, format_percent, format_speed, format_temp,
    parse_color,
};
use windows::Win32::Foundation::{COLORREF, HWND, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleBitmap, CreateCompatibleDC,
    CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DEFAULT_PITCH, DT_END_ELLIPSIS, DT_LEFT,
    DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW,
    FF_DONTCARE, FW_NORMAL, FillRect, GetDC, GetTextExtentPoint32W, HGDIOBJ, OUT_DEFAULT_PRECIS,
    ReleaseDC, SRCCOPY, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::core::PCWSTR;

use crate::{UiError, layout};

/// Owns the memory DC, back buffer, font and brush used for one overlay window.
pub struct Renderer {
    mem_dc: windows::Win32::Graphics::Gdi::HDC,
    bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
    old_bitmap: HGDIOBJ,
    font: windows::Win32::Graphics::Gdi::HFONT,
    old_font: HGDIOBJ,
    background: windows::Win32::Graphics::Gdi::HBRUSH,
    width: i32,
    height: i32,
    dpi: u32,
    font_name: String,
    font_size: i32,
}

impl Renderer {
    pub fn new(hwnd: HWND, config: &Config, size: (i32, i32), dpi: u32) -> Result<Self, UiError> {
        let (width, height) = (size.0.max(1), size.1.max(1));
        let theme = Theme::dark();

        let window_dc = unsafe { GetDC(Some(hwnd)) };
        if window_dc.is_invalid() {
            return Err(UiError::Win32("GetDC returned an invalid DC".into()));
        }

        let mem_dc = unsafe { CreateCompatibleDC(Some(window_dc)) };
        if mem_dc.is_invalid() {
            unsafe { ReleaseDC(Some(hwnd), window_dc) };
            return Err(UiError::Win32(
                "CreateCompatibleDC returned an invalid DC".into(),
            ));
        }

        let bitmap = unsafe { CreateCompatibleBitmap(window_dc, width, height) };
        if bitmap.is_invalid() {
            unsafe {
                let _ = DeleteDC(mem_dc);
                ReleaseDC(Some(hwnd), window_dc);
            }
            return Err(UiError::Win32(
                "CreateCompatibleBitmap returned an invalid bitmap".into(),
            ));
        }

        let old_bitmap = unsafe { SelectObject(mem_dc, HGDIOBJ(bitmap.0)) };
        let background = unsafe { CreateSolidBrush(COLORREF(theme.background)) };
        if background.is_invalid() {
            unsafe {
                SelectObject(mem_dc, old_bitmap);
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                let _ = DeleteDC(mem_dc);
                ReleaseDC(Some(hwnd), window_dc);
            }
            return Err(UiError::Win32(
                "CreateSolidBrush returned an invalid brush".into(),
            ));
        }

        let dpi = dpi.max(48);
        let font = match create_font(config, dpi) {
            Ok(font) => font,
            Err(err) => {
                unsafe {
                    SelectObject(mem_dc, old_bitmap);
                    let _ = DeleteObject(HGDIOBJ(bitmap.0));
                    let _ = DeleteObject(HGDIOBJ(background.0));
                    let _ = DeleteDC(mem_dc);
                    ReleaseDC(Some(hwnd), window_dc);
                }
                return Err(err);
            }
        };
        let old_font = unsafe { SelectObject(mem_dc, HGDIOBJ(font.0)) };

        unsafe { ReleaseDC(Some(hwnd), window_dc) };

        Ok(Self {
            mem_dc,
            bitmap,
            old_bitmap,
            font,
            old_font,
            background,
            width,
            height,
            dpi,
            font_name: config.taskbar.font_name.clone(),
            font_size: config.taskbar.font_size.max(1),
        })
    }

    /// Current effective font DPI (kept for diagnostics and tests).
    #[allow(dead_code)]
    pub fn dpi(&self) -> u32 {
        self.dpi
    }

    pub fn size(&self) -> (i32, i32) {
        (self.width, self.height)
    }

    /// Resize the back buffer when the configured widget size or DPI changes.
    ///
    /// Deselects before deleting so a failed `DeleteObject` cannot leak the old bitmap.
    pub fn ensure_size(&mut self, hwnd: HWND, width: i32, height: i32) -> Result<(), UiError> {
        let width = width.max(1);
        let height = height.max(1);
        if self.width == width && self.height == height {
            return Ok(());
        }
        let window_dc = unsafe { GetDC(Some(hwnd)) };
        if window_dc.is_invalid() {
            return Err(UiError::Win32("GetDC returned an invalid DC".into()));
        }
        let bitmap = unsafe { CreateCompatibleBitmap(window_dc, width, height) };
        unsafe { ReleaseDC(Some(hwnd), window_dc) };
        if bitmap.is_invalid() {
            return Err(UiError::Win32(
                "CreateCompatibleBitmap returned an invalid bitmap".into(),
            ));
        }
        unsafe {
            SelectObject(self.mem_dc, HGDIOBJ(bitmap.0));
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
        }
        self.bitmap = bitmap;
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Recreate the GDI font when either the DPI or the configured font changes.
    ///
    /// Kept as the public entry point used by display-change handling; `set_dpi` is a
    /// thin compatibility wrapper around it.
    /// Backwards-compatible alias for [`Renderer::update_from_config`].
    #[allow(dead_code)]
    pub fn set_dpi(&mut self, config: &Config, dpi: u32) -> Result<(), UiError> {
        self.update_from_config(config, dpi)
    }

    /// Refresh DPI-dependent and typography-dependent GDI state after a config apply.
    pub fn update_from_config(&mut self, config: &Config, dpi: u32) -> Result<(), UiError> {
        let dpi = if dpi == 0 { self.dpi } else { dpi };
        let font_size = config.taskbar.font_size.max(1);
        let font_changed = self.font_size != font_size
            || !self
                .font_name
                .eq_ignore_ascii_case(&config.taskbar.font_name);
        if dpi == self.dpi && !font_changed {
            return Ok(());
        }
        let font = create_font(config, dpi)?;
        unsafe {
            SelectObject(self.mem_dc, HGDIOBJ(font.0));
            let _ = DeleteObject(HGDIOBJ(self.font.0));
        }
        self.font = font;
        self.dpi = dpi;
        self.font_name = config.taskbar.font_name.clone();
        self.font_size = font_size;
        Ok(())
    }

    pub fn render(
        &mut self,
        hwnd: HWND,
        config: &Config,
        snapshot: &Snapshot,
    ) -> Result<(), UiError> {
        self.paint(config, snapshot);
        self.blit(hwnd)
    }

    fn paint(&mut self, config: &Config, snapshot: &Snapshot) {
        let theme = Theme::dark();
        let dim = dim_color(&theme);
        let bounds = RECT {
            left: 0,
            top: 0,
            right: self.width,
            bottom: self.height,
        };

        unsafe {
            FillRect(self.mem_dc, &bounds, self.background);
            SetBkMode(self.mem_dc, TRANSPARENT);
            SelectObject(self.mem_dc, HGDIOBJ(self.font.0));
        }

        let items: Vec<_> = config.items.iter().filter(|item| item.visible).collect();
        for placement in layout::placements(config, items.len(), self.width, self.height) {
            let Some(item) = items.get(placement.visible_index) else {
                continue;
            };
            let (value, is_missing) = value_text(item.id, snapshot, config);
            let value_color = if is_missing {
                dim
            } else {
                item.color
                    .as_deref()
                    .and_then(parse_color)
                    .unwrap_or(theme.value)
            };

            let value_size = text_extent(self.mem_dc, &value);
            let mut label_rect = placement.cell;
            label_rect.right = (label_rect.right - value_size.cx - 2).max(label_rect.left);

            draw_text(
                self.mem_dc,
                &item.label,
                label_rect,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
                theme.label,
            );
            draw_text(
                self.mem_dc,
                &value,
                placement.cell,
                DT_RIGHT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
                value_color,
            );
        }
    }

    fn blit(&self, hwnd: HWND) -> Result<(), UiError> {
        let window_dc = unsafe { GetDC(Some(hwnd)) };
        if window_dc.is_invalid() {
            return Err(UiError::Win32("GetDC returned an invalid DC".into()));
        }
        let result = unsafe {
            BitBlt(
                window_dc,
                0,
                0,
                self.width,
                self.height,
                Some(self.mem_dc),
                0,
                0,
                SRCCOPY,
            )
        };
        unsafe { ReleaseDC(Some(hwnd), window_dc) };
        result.map_err(|err| UiError::win32("BitBlt", err))
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem_dc, self.old_font);
            let _ = DeleteObject(HGDIOBJ(self.font.0));
            SelectObject(self.mem_dc, self.old_bitmap);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteObject(HGDIOBJ(self.background.0));
            let _ = DeleteDC(self.mem_dc);
        }
    }
}

pub fn dpi_for_window(hwnd: HWND) -> u32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 96 } else { dpi }
}

fn create_font(config: &Config, dpi: u32) -> Result<windows::Win32::Graphics::Gdi::HFONT, UiError> {
    let dpi = dpi.max(48) as i32;
    let point_size = config.taskbar.font_size.max(1);
    let height = -(((point_size * dpi) + 36) / 72);
    let face: Vec<u16> = config
        .taskbar
        .font_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let font = unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            PCWSTR(face.as_ptr()),
        )
    };

    if font.is_invalid() {
        Err(UiError::Win32("CreateFontW failed".into()))
    } else {
        Ok(font)
    }
}

fn draw_text(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    text: &str,
    mut rect: RECT,
    flags: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
    color: u32,
) {
    if text.is_empty() {
        return;
    }
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        SetTextColor(hdc, COLORREF(color));
        DrawTextW(hdc, &mut utf16, &mut rect, flags);
    }
}

fn text_extent(hdc: windows::Win32::Graphics::Gdi::HDC, text: &str) -> SIZE {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    if !utf16.is_empty() {
        unsafe {
            let _ = GetTextExtentPoint32W(hdc, &utf16, &mut size);
        }
    }
    size
}

fn dim_color(theme: &Theme) -> u32 {
    // Blend the label color towards the background so missing values read as muted.
    let a = theme.background;
    let b = theme.label;
    let mix = |channel_a: u32, channel_b: u32| (channel_a + channel_b) / 2;
    let r = mix(a & 0xff, b & 0xff);
    let g = mix((a >> 8) & 0xff, (b >> 8) & 0xff);
    let b_out = mix((a >> 16) & 0xff, (b >> 16) & 0xff);
    (b_out << 16) | (g << 8) | r
}

fn value_text(id: ItemId, snapshot: &Snapshot, config: &Config) -> (String, bool) {
    let format = &config.format;
    match id {
        ItemId::Up => (format_speed(snapshot.network.up_bps, format), false),
        ItemId::Down => (format_speed(snapshot.network.down_bps, format), false),
        ItemId::Cpu => (
            format_percent(snapshot.cpu.usage_pct, format.hide_percent),
            false,
        ),
        ItemId::Memory => (
            format_percent(snapshot.memory.usage_pct, format.hide_percent),
            false,
        ),
        ItemId::Gpu => match snapshot.gpu.usage_pct {
            Some(usage) => (format_percent(usage, format.hide_percent), false),
            None => ("—".to_string(), true),
        },
        ItemId::CpuTemp => temp_text(snapshot.cpu.temp_c, snapshot.cpu.temp_source),
        ItemId::GpuTemp => temp_text(snapshot.gpu.temp_c, snapshot.gpu.temp_source),
        ItemId::HddTemp => temp_text(snapshot.disk.temp_c, snapshot.disk.temp_source),
        ItemId::HddUsage => match snapshot.disk.activity_pct {
            Some(usage) => (format_percent(usage, format.hide_percent), false),
            None => ("—".to_string(), true),
        },
        ItemId::CpuFreq => match snapshot.cpu.freq_mhz {
            Some(mhz) if mhz.is_finite() => (format!("{:.2} GHz", mhz / 1000.0), false),
            _ => ("—".to_string(), true),
        },
        ItemId::TotalSpeed => (
            format_speed(snapshot.network.up_bps + snapshot.network.down_bps, format),
            false,
        ),
        ItemId::TodayTraffic => ("—".to_string(), true),
    }
}

fn temp_text(temp_c: Option<f32>, source: TempSource) -> (String, bool) {
    match temp_c {
        Some(value) if value.is_finite() => {
            let mut text = format_temp(Some(value));
            if source == TempSource::Acpi {
                text.push('~');
            }
            (text, false)
        }
        _ => ("—".to_string(), true),
    }
}
