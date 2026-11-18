//! Zero-WARP, zero-thread Flyout card renderer (pure CPU antialiased 2D shapes + GDI ClearType
//! text into a premultiplied BGRA8 `DibSurface`).
//!
//! Because this renderer never touches Direct2D, DirectWrite, or `D3D10Warp.dll`, opening and
//! closing the Flyout cannot expand the background widget's WARP thread pool or backing texture:
//! dropping `FlyoutRenderer` immediately releases 100% of its DIB memory and GDI handles.

use oxide_core::{
    Config, HistoryStore, Localizer, Rgba, Snapshot, TempSource, ThemePalette, format_bytes,
    format_speed,
};
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, DEFAULT_CHARSET, DEFAULT_PITCH, DT_CENTER,
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteObject,
    DrawTextW, FF_DONTCARE, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::core::PCWSTR;

use crate::RenderError;
use crate::SystemThemeContext;
use crate::dib::DibSurface;
use crate::layout::RectF;

/// Interactive buttons rendered inside the Flyout header/sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FlyoutButton {
    ToggleGameMode,
    OpenSettings,
    OpenTaskManager,
}

/// Clickable region inside the Flyout, stored in physical pixels for direct hit-testing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlyoutHitZone {
    pub button: FlyoutButton,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl FlyoutHitZone {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// Dynamic inputs for rendering one Flyout frame.
pub struct FlyoutInput<'a> {
    pub config: &'a Config,
    pub snapshot: &'a Snapshot,
    pub history: &'a HistoryStore,
    pub per_core_pct: &'a [f32],
    pub theme_ctx: &'a SystemThemeContext,
    pub localizer: &'a Localizer,
    pub game_mode_active: bool,
    pub dpi: u32,
}

/// Result of a Flyout render pass.
pub struct FlyoutFrame {
    pub width_px: u32,
    pub height_px: u32,
    pub hit_zones: Vec<FlyoutHitZone>,
}

struct GdiFontSet {
    dpi: u32,
    title: HFONT,
    section: HFONT,
    body: HFONT,
    body_bold: HFONT,
    small: HFONT,
    button: HFONT,
}

impl GdiFontSet {
    fn new(dpi: u32) -> Result<Self, RenderError> {
        let dpi = dpi.max(48);
        let title = make_gdi_font(10.5, 600, dpi)?;
        let section = make_gdi_font(8.5, 600, dpi)?;
        let body = make_gdi_font(8.5, 400, dpi)?;
        let body_bold = make_gdi_font(8.5, 600, dpi)?;
        let small = make_gdi_font(7.5, 400, dpi)?;
        let button = make_gdi_font(8.0, 500, dpi)?;
        Ok(Self {
            dpi,
            title,
            section,
            body,
            body_bold,
            small,
            button,
        })
    }
}

impl Drop for GdiFontSet {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.title.0));
            let _ = DeleteObject(HGDIOBJ(self.section.0));
            let _ = DeleteObject(HGDIOBJ(self.body.0));
            let _ = DeleteObject(HGDIOBJ(self.body_bold.0));
            let _ = DeleteObject(HGDIOBJ(self.small.0));
            let _ = DeleteObject(HGDIOBJ(self.button.0));
        }
    }
}

/// Self-contained CPU + GDI ClearType Flyout card renderer (0 WARP threads, 0 COM objects).
pub struct FlyoutRenderer {
    dib: DibSurface,
    fonts: Option<GdiFontSet>,
}

impl FlyoutRenderer {
    pub fn new() -> Result<Self, RenderError> {
        let dib = DibSurface::new(380, 320)?;
        Ok(Self { dib, fonts: None })
    }

    pub fn dib(&self) -> &DibSurface {
        &self.dib
    }

    /// Render the full Flyout card into the internal premultiplied BGRA8 DIB and return
    /// its physical pixel dimensions and clickable button hit zones.
    pub fn render(&mut self, input: &FlyoutInput<'_>) -> Result<FlyoutFrame, RenderError> {
        let cfg = input.config;
        let fly = &cfg.flyout;
        let snap = input.snapshot;
        let palette = input.theme_ctx.resolve_palette(&cfg.general.theme);
        let dpi = input.dpi.max(48);
        let scale = dpi as f32 / 96.0;

        if self.fonts.as_ref().is_none_or(|f| f.dpi != dpi) {
            self.fonts = Some(GdiFontSet::new(dpi)?);
        }

        let width_dip = fly.width.clamp(300, 560) as f32;
        let height_dip = compute_flyout_height_dip(input);

        let width_px = (width_dip * scale).ceil().max(1.0) as u32;
        let height_px = (height_dip * scale).ceil().max(1.0) as u32;

        self.dib.ensure_size(width_px, height_px)?;

        // Card colors (we paint straight RGB onto the DIB first so GDI ClearType blends
        // cleanly against the true background, then apply the rounded-card alpha mask & premultiply).
        let card_alpha: u8 = if palette.is_dark { 244 } else { 246 };
        let card_bg = if palette.is_dark {
            Rgba::rgb(28, 30, 36)
        } else {
            Rgba::rgb(248, 249, 252)
        };
        let card_border = if palette.is_dark {
            Rgba::new(255, 255, 255, 34)
        } else {
            Rgba::new(0, 0, 0, 30)
        };
        let sub_card = if palette.is_dark {
            Rgba::new(255, 255, 255, 11)
        } else {
            Rgba::new(0, 0, 0, 8)
        };

        let w = width_px as usize;
        let h = height_px as usize;

        // Fill base straight RGB (alpha will be applied in the final SDF mask pass).
        fill_solid_rgb(self.dib.pixels_mut(), card_bg);

        let mut hit_zones = Vec::with_capacity(4);
        let mut text_cmds: Vec<TextCmd> = Vec::with_capacity(32);

        let pad = 14.0_f32;
        let content_w = width_dip - pad * 2.0;
        let mut y = 12.0_f32;

        // ---- Header ----
        text_cmds.push(TextCmd::new(
            input.localizer.text("app-name"),
            FontSlot::Title,
            RectF::new(pad, y, pad + 150.0, y + 22.0),
            palette.value,
            TextAlign::Left,
        ));

        let btn_h = 22.0_f32;
        let cfg_btn_w = 64.0_f32;
        let gm_btn_w = 68.0_f32;
        let cfg_btn = RectF::new(width_dip - pad - cfg_btn_w, y, width_dip - pad, y + btn_h);
        let gm_btn = RectF::new(
            cfg_btn.left - 6.0 - gm_btn_w,
            y,
            cfg_btn.left - 6.0,
            y + btn_h,
        );

        let gm_bg = if input.game_mode_active {
            palette.accent.scale_alpha(0.32)
        } else {
            sub_card
        };
        let gm_col = if input.game_mode_active {
            palette.accent
        } else {
            palette.value
        };

        {
            let px = self.dib.pixels_mut();
            fill_rounded_rect(px, w, h, gm_btn, 5.0, scale, gm_bg);
            stroke_rounded_rect(px, w, h, gm_btn, 5.0, 0.75, scale, card_border);
            fill_rounded_rect(px, w, h, cfg_btn, 5.0, scale, sub_card);
            stroke_rounded_rect(px, w, h, cfg_btn, 5.0, 0.75, scale, card_border);
        }
        text_cmds.push(TextCmd::new(
            input.localizer.text("flyout-game-mode"),
            FontSlot::Button,
            gm_btn,
            gm_col,
            TextAlign::Center,
        ));
        text_cmds.push(TextCmd::new(
            input.localizer.text("flyout-settings"),
            FontSlot::Button,
            cfg_btn,
            palette.value,
            TextAlign::Center,
        ));
        hit_zones.push(to_hit_zone(FlyoutButton::ToggleGameMode, gm_btn, scale));
        hit_zones.push(to_hit_zone(FlyoutButton::OpenSettings, cfg_btn, scale));

        y += 28.0;
        draw_hline(
            self.dib.pixels_mut(),
            w,
            h,
            pad,
            width_dip - pad,
            y,
            scale,
            palette.separator,
        );
        y += 8.0;

        // ---- CPU Section ----
        let cpu_header = RectF::new(pad, y, width_dip - pad, y + 18.0);
        hit_zones.push(to_hit_zone(
            FlyoutButton::OpenTaskManager,
            cpu_header,
            scale,
        ));
        text_cmds.push(TextCmd::new(
            input.localizer.text("flyout-cpu"),
            FontSlot::Section,
            cpu_header,
            palette.label,
            TextAlign::Left,
        ));
        text_cmds.push(TextCmd::new(
            format_cpu_summary(snap),
            FontSlot::BodyBold,
            cpu_header,
            Rgba::rgb(0xF0, 0xC6, 0x74),
            TextAlign::Right,
        ));
        y += 20.0;

        if fly.show_graphs {
            let mut pts = Vec::new();
            input.history.cpu_usage.copy_tail(60, &mut pts);
            let graph_rect = RectF::new(pad, y, width_dip - pad, y + 26.0);
            let px = self.dib.pixels_mut();
            fill_rounded_rect(px, w, h, graph_rect, 4.0, scale, sub_card);
            draw_sparkline(
                px,
                w,
                h,
                graph_rect.inset(3.0, 3.0),
                &pts,
                0.0,
                100.0,
                scale,
                Rgba::rgb(0xF0, 0xC6, 0x74),
                0.24,
            );
            y += 30.0;
        }

        if fly.show_cpu_per_core && !input.per_core_pct.is_empty() {
            let cores_rect = RectF::new(pad, y, width_dip - pad, y + 22.0);
            let px = self.dib.pixels_mut();
            fill_rounded_rect(px, w, h, cores_rect, 4.0, scale, sub_card);
            let inner = cores_rect.inset(4.0, 3.0);
            let n = input.per_core_pct.len();
            let gap = 2.0_f32;
            let bar_w = ((inner.width() - gap * (n.saturating_sub(1) as f32)) / n as f32).max(2.0);
            for (i, &pct) in input.per_core_pct.iter().enumerate() {
                let ratio = (pct / 100.0).clamp(0.0, 1.0);
                let bx = inner.left + i as f32 * (bar_w + gap);
                let bh = (inner.height() * ratio).max(2.0);
                let col = if pct >= 85.0 {
                    Rgba::rgb(0xF2, 0x6D, 0x6D)
                } else if pct >= 55.0 {
                    Rgba::rgb(0xF0, 0xC6, 0x74)
                } else {
                    Rgba::rgb(0x7F, 0xD3, 0x7F)
                };
                let bar = RectF::new(
                    bx,
                    inner.bottom - bh,
                    (bx + bar_w).min(inner.right),
                    inner.bottom,
                );
                fill_rounded_rect(px, w, h, bar, 1.5, scale, col);
            }
            y += 26.0;
        }

        // ---- Memory Section ----
        let mem_row = RectF::new(pad, y, width_dip - pad, y + 18.0);
        text_cmds.push(TextCmd::new(
            input.localizer.text("flyout-memory"),
            FontSlot::Section,
            mem_row,
            palette.label,
            TextAlign::Left,
        ));
        let mem_text = format!(
            "{} / {} ({:.0}%)",
            format_bytes(snap.memory.used_bytes),
            format_bytes(snap.memory.total_bytes),
            snap.memory.usage_pct.clamp(0.0, 100.0)
        );
        text_cmds.push(TextCmd::new(
            mem_text,
            FontSlot::BodyBold,
            mem_row,
            palette.value,
            TextAlign::Right,
        ));
        y += 20.0;

        let mem_bar = RectF::new(pad, y, width_dip - pad, y + 5.0);
        draw_progress_bar(
            self.dib.pixels_mut(),
            w,
            h,
            mem_bar,
            (snap.memory.usage_pct / 100.0).clamp(0.0, 1.0),
            scale,
            Rgba::rgb(0xF0, 0xC6, 0x74),
            &palette,
        );
        y += 12.0;

        // ---- GPUs Section ----
        if fly.show_all_gpus {
            let gpus: Vec<_> = if snap.gpus.is_empty() {
                vec![snap.primary_gpu()]
            } else {
                snap.gpus.iter().collect()
            };
            if !gpus.is_empty() {
                draw_hline(
                    self.dib.pixels_mut(),
                    w,
                    h,
                    pad,
                    width_dip - pad,
                    y,
                    scale,
                    palette.separator,
                );
                y += 6.0;
                text_cmds.push(TextCmd::new(
                    input.localizer.text("flyout-gpus"),
                    FontSlot::Section,
                    RectF::new(pad, y, width_dip - pad, y + 16.0),
                    palette.label,
                    TextAlign::Left,
                ));
                y += 17.0;

                for gpu in gpus {
                    let name = cfg
                        .aliases
                        .gpu_alias(&gpu.name)
                        .unwrap_or(if gpu.name.is_empty() {
                            "GPU"
                        } else {
                            &gpu.name
                        });
                    text_cmds.push(TextCmd::new(
                        name.to_string(),
                        FontSlot::Body,
                        RectF::new(pad, y, pad + content_w * 0.56, y + 17.0),
                        palette.value,
                        TextAlign::Left,
                    ));
                    text_cmds.push(TextCmd::new(
                        format_gpu_stats(gpu),
                        FontSlot::BodyBold,
                        RectF::new(pad, y, width_dip - pad, y + 17.0),
                        Rgba::rgb(0x7F, 0xBF, 0xEF),
                        TextAlign::Right,
                    ));
                    y += 18.0;
                }
                y += 3.0;
            }
        }

        // ---- Disks Section ----
        if fly.show_all_disks {
            let disks: Vec<_> = if snap.disks.is_empty() {
                vec![&snap.disk]
            } else {
                snap.disks.iter().collect()
            };
            if !disks.is_empty() {
                draw_hline(
                    self.dib.pixels_mut(),
                    w,
                    h,
                    pad,
                    width_dip - pad,
                    y,
                    scale,
                    palette.separator,
                );
                y += 6.0;
                text_cmds.push(TextCmd::new(
                    input.localizer.text("flyout-disks"),
                    FontSlot::Section,
                    RectF::new(pad, y, width_dip - pad, y + 16.0),
                    palette.label,
                    TextAlign::Left,
                ));
                y += 17.0;

                for disk in disks {
                    let display_name =
                        cfg.aliases
                            .disk_alias(&disk.name)
                            .unwrap_or(if disk.name.is_empty() {
                                "Disk"
                            } else {
                                &disk.name
                            });
                    let bus = if disk.is_nvme { "NVMe" } else { "SATA" };
                    let left_label = if display_name
                        .to_ascii_lowercase()
                        .contains(&bus.to_ascii_lowercase())
                    {
                        display_name.to_string()
                    } else {
                        format!("{display_name} ({bus})")
                    };
                    text_cmds.push(TextCmd::new(
                        left_label,
                        FontSlot::Body,
                        RectF::new(pad, y, pad + content_w * 0.62, y + 17.0),
                        palette.value,
                        TextAlign::Left,
                    ));
                    text_cmds.push(TextCmd::new(
                        format_disk_stats(disk),
                        FontSlot::BodyBold,
                        RectF::new(pad, y, width_dip - pad, y + 17.0),
                        Rgba::rgb(0xE0, 0x80, 0x80),
                        TextAlign::Right,
                    ));
                    y += 18.0;
                }
                y += 3.0;
            }
        }

        // ---- Network Section ----
        if fly.show_network_details {
            draw_hline(
                self.dib.pixels_mut(),
                w,
                h,
                pad,
                width_dip - pad,
                y,
                scale,
                palette.separator,
            );
            y += 6.0;
            let adapter_name = if snap.network.adapter.is_empty() {
                input.localizer.text("flyout-network")
            } else {
                let alias = cfg
                    .aliases
                    .adapter_alias(&snap.network.adapter)
                    .unwrap_or(&snap.network.adapter);
                format!("{} — {alias}", input.localizer.text("flyout-network"))
            };
            text_cmds.push(TextCmd::new(
                adapter_name,
                FontSlot::Section,
                RectF::new(pad, y, pad + content_w * 0.52, y + 17.0),
                palette.label,
                TextAlign::Left,
            ));
            let speeds = format!(
                "↑ {}  ↓ {}",
                format_speed(snap.network.up_bps, &cfg.format),
                format_speed(snap.network.down_bps, &cfg.format)
            );
            text_cmds.push(TextCmd::new(
                speeds,
                FontSlot::BodyBold,
                RectF::new(pad, y, width_dip - pad, y + 17.0),
                Rgba::rgb(0x7F, 0xD3, 0x7F),
                TextAlign::Right,
            ));
            y += 19.0;

            if fly.show_graphs {
                let mut down_pts = Vec::new();
                let mut up_pts = Vec::new();
                input.history.net_down.copy_tail(60, &mut down_pts);
                input.history.net_up.copy_tail(60, &mut up_pts);
                let peak = input.history.net_auto_scale_peak_bps();
                let graph_rect = RectF::new(pad, y, width_dip - pad, y + 26.0);
                let px = self.dib.pixels_mut();
                fill_rounded_rect(px, w, h, graph_rect, 4.0, scale, sub_card);
                draw_sparkline(
                    px,
                    w,
                    h,
                    graph_rect.inset(3.0, 3.0),
                    &down_pts,
                    0.0,
                    peak,
                    scale,
                    Rgba::rgb(0x7F, 0xD3, 0x7F),
                    0.24,
                );
                draw_sparkline(
                    px,
                    w,
                    h,
                    graph_rect.inset(3.0, 3.0),
                    &up_pts,
                    0.0,
                    peak,
                    scale,
                    Rgba::rgb(0x60, 0xCD, 0xFF),
                    0.16,
                );
                y += 30.0;
            }
        }

        // ---- Provenance / Sources Footer ----
        if fly.show_provider_status {
            draw_hline(
                self.dib.pixels_mut(),
                w,
                h,
                pad,
                width_dip - pad,
                y,
                scale,
                palette.separator,
            );
            y += 6.0;
            let badges = build_source_badges(snap);
            let mut bx = pad;
            text_cmds.push(TextCmd::new(
                format!("{}:", input.localizer.text("flyout-sources")),
                FontSlot::Small,
                RectF::new(bx, y, bx + 52.0, y + 16.0),
                palette.muted,
                TextAlign::Left,
            ));
            bx += 54.0;
            let px = self.dib.pixels_mut();
            for (label, active) in badges {
                let dot_col = if active {
                    Rgba::rgb(0x7F, 0xD3, 0x7F)
                } else {
                    palette.muted
                };
                fill_circle(
                    px,
                    w,
                    h,
                    (bx + 3.0) * scale,
                    (y + 8.0) * scale,
                    2.8 * scale,
                    dot_col,
                );
                bx += 9.0;
                let text_w = (label.len() as f32 * 6.4 + 8.0).max(36.0);
                text_cmds.push(TextCmd::new(
                    label.to_string(),
                    FontSlot::Small,
                    RectF::new(bx, y, bx + text_w, y + 16.0),
                    if active { palette.value } else { palette.muted },
                    TextAlign::Left,
                ));
                bx += text_w;
                if bx > width_dip - pad - 24.0 {
                    break;
                }
            }
        }

        // Outer border drawn before text so it never clips glyphs.
        stroke_rounded_rect(
            self.dib.pixels_mut(),
            w,
            h,
            RectF::new(0.5, 0.5, width_dip - 0.5, height_dip - 0.5),
            10.0,
            1.0,
            scale,
            card_border,
        );

        // Draw all text via GDI ClearType onto the DIB DC (blends natively against card RGB).
        if let Some(fonts) = self.fonts.as_ref() {
            let hdc = self.dib.hdc();
            unsafe {
                SetBkMode(hdc, TRANSPARENT);
            }
            for cmd in &text_cmds {
                let hfont = match cmd.font {
                    FontSlot::Title => fonts.title,
                    FontSlot::Section => fonts.section,
                    FontSlot::Body => fonts.body,
                    FontSlot::BodyBold => fonts.body_bold,
                    FontSlot::Small => fonts.small,
                    FontSlot::Button => fonts.button,
                };
                let align_flag = match cmd.align {
                    TextAlign::Left => DT_LEFT,
                    TextAlign::Right => DT_RIGHT,
                    TextAlign::Center => DT_CENTER,
                };
                let mut r = RECT {
                    left: (cmd.rect.left * scale).round() as i32,
                    top: (cmd.rect.top * scale).round() as i32,
                    right: (cmd.rect.right * scale).round() as i32,
                    bottom: (cmd.rect.bottom * scale).round() as i32,
                };
                let mut utf16: Vec<u16> = cmd.text.encode_utf16().collect();
                if !utf16.is_empty() {
                    unsafe {
                        let old = SelectObject(hdc, HGDIOBJ(hfont.0));
                        SetTextColor(hdc, COLORREF(cmd.color.to_colorref()));
                        DrawTextW(
                            hdc,
                            &mut utf16,
                            &mut r,
                            align_flag | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
                        );
                        SelectObject(hdc, old);
                    }
                }
            }
        }

        // Apply smooth SDF rounded-rectangle alpha mask and premultiply BGRA8 for ULW_ALPHA.
        apply_rounded_card_alpha_and_premultiply(
            self.dib.pixels_mut(),
            w,
            h,
            10.0 * scale,
            card_alpha,
        );

        Ok(FlyoutFrame {
            width_px,
            height_px,
            hit_zones,
        })
    }
}

#[derive(Clone, Copy)]
enum FontSlot {
    Title,
    Section,
    Body,
    BodyBold,
    Small,
    Button,
}

#[derive(Clone, Copy)]
enum TextAlign {
    Left,
    Right,
    Center,
}

struct TextCmd {
    text: String,
    font: FontSlot,
    rect: RectF,
    color: Rgba,
    align: TextAlign,
}

impl TextCmd {
    fn new(text: String, font: FontSlot, rect: RectF, color: Rgba, align: TextAlign) -> Self {
        Self {
            text,
            font,
            rect,
            color,
            align,
        }
    }
}

// ============================================================================
// Pure CPU antialiased 2D primitives on straight-RGB BGRA buffer
// ============================================================================

fn fill_solid_rgb(buf: &mut [u8], c: Rgba) {
    for px in buf.chunks_exact_mut(4) {
        px[0] = c.b;
        px[1] = c.g;
        px[2] = c.r;
        px[3] = 255;
    }
}

#[inline]
fn blend_pixel_straight(buf: &mut [u8], w: usize, h: usize, x: i32, y: i32, c: Rgba, cov: f32) {
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 || cov <= 0.002 {
        return;
    }
    let a = (c.a as f32 / 255.0) * cov.clamp(0.0, 1.0);
    if a <= 0.002 {
        return;
    }
    let idx = (y as usize * w + x as usize) * 4;
    let inv = 1.0 - a;
    buf[idx] = (buf[idx] as f32 * inv + c.b as f32 * a).round() as u8;
    buf[idx + 1] = (buf[idx + 1] as f32 * inv + c.g as f32 * a).round() as u8;
    buf[idx + 2] = (buf[idx + 2] as f32 * inv + c.r as f32 * a).round() as u8;
}

/// Signed distance from point `(px, py)` to a rounded rectangle `[l, t, r, b]` with radius `rad`.
#[inline]
fn sd_rounded_rect(px: f32, py: f32, l: f32, t: f32, r: f32, b: f32, rad: f32) -> f32 {
    let cx = (l + r) * 0.5;
    let cy = (t + b) * 0.5;
    let hx = ((r - l) * 0.5 - rad).max(0.0);
    let hy = ((b - t) * 0.5 - rad).max(0.0);
    let dx = (px - cx).abs() - hx;
    let dy = (py - cy).abs() - hy;
    let outside = dx.max(0.0).hypot(dy.max(0.0));
    let inside = dx.max(dy).min(0.0);
    outside + inside - rad
}

fn fill_rounded_rect(
    buf: &mut [u8],
    w: usize,
    h: usize,
    rect: RectF,
    radius: f32,
    scale: f32,
    c: Rgba,
) {
    let l = rect.left * scale;
    let t = rect.top * scale;
    let r = rect.right * scale;
    let b = rect.bottom * scale;
    let rad = (radius * scale)
        .min((r - l) * 0.5)
        .min((b - t) * 0.5)
        .max(0.0);

    let x0 = (l.floor() as i32 - 1).max(0);
    let y0 = (t.floor() as i32 - 1).max(0);
    let x1 = (r.ceil() as i32 + 1).min(w as i32);
    let y1 = (b.ceil() as i32 + 1).min(h as i32);

    for y in y0..y1 {
        let py = y as f32 + 0.5;
        for x in x0..x1 {
            let px = x as f32 + 0.5;
            let d = sd_rounded_rect(px, py, l, t, r, b, rad);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.002 {
                blend_pixel_straight(buf, w, h, x, y, c, cov);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn stroke_rounded_rect(
    buf: &mut [u8],
    w: usize,
    h: usize,
    rect: RectF,
    radius: f32,
    thickness: f32,
    scale: f32,
    c: Rgba,
) {
    let l = rect.left * scale;
    let t = rect.top * scale;
    let r = rect.right * scale;
    let b = rect.bottom * scale;
    let rad = (radius * scale)
        .min((r - l) * 0.5)
        .min((b - t) * 0.5)
        .max(0.0);
    let half_w = (thickness * scale * 0.5).max(0.5);

    let x0 = (l.floor() as i32 - 2).max(0);
    let y0 = (t.floor() as i32 - 2).max(0);
    let x1 = (r.ceil() as i32 + 2).min(w as i32);
    let y1 = (b.ceil() as i32 + 2).min(h as i32);

    for y in y0..y1 {
        let py = y as f32 + 0.5;
        for x in x0..x1 {
            let px = x as f32 + 0.5;
            let d = sd_rounded_rect(px, py, l, t, r, b, rad).abs() - half_w;
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.002 {
                blend_pixel_straight(buf, w, h, x, y, c, cov);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_hline(buf: &mut [u8], w: usize, h: usize, x0: f32, x1: f32, y: f32, scale: f32, c: Rgba) {
    let yi = (y * scale).round() as i32;
    let xi0 = (x0 * scale).floor() as i32;
    let xi1 = (x1 * scale).ceil() as i32;
    for x in xi0..xi1 {
        blend_pixel_straight(buf, w, h, x, yi, c, 1.0);
    }
}

fn fill_circle(buf: &mut [u8], w: usize, h: usize, cx: f32, cy: f32, radius: f32, c: Rgba) {
    let x0 = ((cx - radius).floor() as i32 - 1).max(0);
    let y0 = ((cy - radius).floor() as i32 - 1).max(0);
    let x1 = ((cx + radius).ceil() as i32 + 1).min(w as i32);
    let y1 = ((cy + radius).ceil() as i32 + 1).min(h as i32);
    for y in y0..y1 {
        let dy = y as f32 + 0.5 - cy;
        for x in x0..x1 {
            let dx = x as f32 + 0.5 - cx;
            let d = dx.hypot(dy) - radius;
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.002 {
                blend_pixel_straight(buf, w, h, x, y, c, cov);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_progress_bar(
    buf: &mut [u8],
    w: usize,
    h: usize,
    rect: RectF,
    ratio: f32,
    scale: f32,
    color: Rgba,
    palette: &ThemePalette,
) {
    let r = rect.height() * 0.5;
    let track = if palette.is_dark {
        Rgba::new(255, 255, 255, 36)
    } else {
        Rgba::new(0, 0, 0, 32)
    };
    fill_rounded_rect(buf, w, h, rect, r, scale, track);
    let fw = rect.width() * ratio.clamp(0.0, 1.0);
    if fw >= 1.5 {
        fill_rounded_rect(
            buf,
            w,
            h,
            RectF::new(rect.left, rect.top, rect.left + fw, rect.bottom),
            r,
            scale,
            color,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_sparkline(
    buf: &mut [u8],
    w: usize,
    h: usize,
    rect: RectF,
    points: &[f32],
    min_v: f32,
    max_v: f32,
    scale: f32,
    color: Rgba,
    fill_alpha: f32,
) {
    if points.len() < 2 || rect.width() < 4.0 || rect.height() < 4.0 {
        return;
    }
    let l = rect.left * scale;
    let r = rect.right * scale;
    let b = rect.bottom * scale;
    let gh = (rect.height() * scale - 1.0).max(1.0);
    let span = (max_v - min_v).max(1e-3);
    let n = points.len();
    let gw = (r - l).max(1.0);

    let xi0 = l.floor().max(0.0) as i32;
    let xi1 = r.ceil().min(w as f32) as i32;
    let yi_bot = b.ceil().min(h as f32) as i32;
    let fill_col = color.scale_alpha(fill_alpha);
    let stroke_col = color.scale_alpha(0.92);
    let half_stroke = (0.75 * scale).max(0.65);

    // Sample curve height at each pixel column for smooth area fill + antialiased stroke.
    let sample_y = |xf: f32| -> f32 {
        let u = ((xf - l) / gw).clamp(0.0, 1.0) * (n - 1) as f32;
        let idx = (u.floor() as usize).min(n - 2);
        let frac = u - idx as f32;
        let val = points[idx] + (points[idx + 1] - points[idx]) * frac;
        let norm = ((val - min_v) / span).clamp(0.0, 1.0);
        b - norm * gh
    };

    for x in xi0..xi1 {
        let xf = x as f32 + 0.5;
        let cy = sample_y(xf);
        let cy_prev = sample_y(xf - 1.0);
        let cy_next = sample_y(xf + 1.0);
        let slope_scale = (1.0 + ((cy_next - cy_prev) * 0.5).powi(2)).sqrt();

        let y_top = (cy - half_stroke * slope_scale - 1.5).floor().max(0.0) as i32;
        for y in y_top..yi_bot {
            let yf = y as f32 + 0.5;
            if yf > cy {
                let fill_cov = (yf - cy).clamp(0.0, 1.0);
                blend_pixel_straight(buf, w, h, x, y, fill_col, fill_cov);
            }
            let dist = (yf - cy).abs() / slope_scale;
            let stroke_cov = (half_stroke + 0.5 - dist).clamp(0.0, 1.0);
            if stroke_cov > 0.002 {
                blend_pixel_straight(buf, w, h, x, y, stroke_col, stroke_cov);
            }
        }
    }
}

fn apply_rounded_card_alpha_and_premultiply(
    buf: &mut [u8],
    w: usize,
    h: usize,
    radius_px: f32,
    card_alpha: u8,
) {
    let wf = w as f32;
    let hf = h as f32;
    let rad = radius_px.min(wf * 0.5).min(hf * 0.5).max(0.0);

    for y in 0..h {
        let py = y as f32 + 0.5;
        let row_off = y * w * 4;
        for x in 0..w {
            let px = x as f32 + 0.5;
            let d = sd_rounded_rect(px, py, 0.0, 0.0, wf, hf, rad);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            let a = ((card_alpha as f32) * cov).round() as u8;
            let idx = row_off + x * 4;
            if a == 0 {
                buf[idx] = 0;
                buf[idx + 1] = 0;
                buf[idx + 2] = 0;
                buf[idx + 3] = 0;
            } else {
                let a16 = a as u16;
                buf[idx] = ((buf[idx] as u16 * a16 + 127) / 255) as u8;
                buf[idx + 1] = ((buf[idx + 1] as u16 * a16 + 127) / 255) as u8;
                buf[idx + 2] = ((buf[idx + 2] as u16 * a16 + 127) / 255) as u8;
                buf[idx + 3] = a;
            }
        }
    }
}

fn make_gdi_font(size_pt: f32, weight: i32, dpi: u32) -> Result<HFONT, RenderError> {
    let height = -((size_pt * dpi as f32 / 72.0).round() as i32).max(8);
    let face: Vec<u16> = "Segoe UI"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let font = unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
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
        Err(RenderError::Win32("CreateFontW(flyout) failed".into()))
    } else {
        Ok(font)
    }
}

fn compute_flyout_height_dip(input: &FlyoutInput<'_>) -> f32 {
    let fly = &input.config.flyout;
    let snap = input.snapshot;
    let mut h = 12.0 + 28.0 + 8.0; // header + divider
    h += 20.0; // CPU header
    if fly.show_graphs {
        h += 30.0;
    }
    if fly.show_cpu_per_core && !input.per_core_pct.is_empty() {
        h += 26.0;
    }
    h += 20.0 + 12.0; // RAM
    if fly.show_all_gpus {
        let count = snap.gpus.len().max(1);
        h += 6.0 + 17.0 + (count as f32 * 18.0) + 3.0;
    }
    if fly.show_all_disks {
        let count = snap.disks.len().max(1);
        h += 6.0 + 17.0 + (count as f32 * 18.0) + 3.0;
    }
    if fly.show_network_details {
        h += 6.0 + 19.0;
        if fly.show_graphs {
            h += 30.0;
        }
    }
    if fly.show_provider_status {
        h += 6.0 + 20.0;
    }
    (h + 10.0).clamp(160.0, 680.0)
}

fn format_cpu_summary(snap: &Snapshot) -> String {
    let mut parts = vec![format!("{:.0}%", snap.cpu.usage_pct.clamp(0.0, 100.0))];
    if let Some(mhz) = snap.cpu.freq_mhz.filter(|v| v.is_finite() && *v > 0.0) {
        parts.push(format!("{:.2} GHz", mhz / 1000.0));
    }
    if let Some(t) = snap.cpu.temp_c.filter(|v| v.is_finite()) {
        let suffix = if snap.cpu.temp_source == TempSource::Acpi {
            "~"
        } else {
            ""
        };
        parts.push(format!("{t:.0} °C{suffix}"));
    } else {
        parts.push("—".into());
    }
    parts.join(" • ")
}

fn format_gpu_stats(gpu: &oxide_core::GpuSample) -> String {
    let mut parts = Vec::with_capacity(3);
    if let Some(u) = gpu.usage_pct.filter(|v| v.is_finite()) {
        parts.push(format!("{:.0}%", u.clamp(0.0, 100.0)));
    } else {
        parts.push("—".into());
    }
    if let Some(t) = gpu.temp_c.filter(|v| v.is_finite()) {
        parts.push(format!("{t:.0} °C"));
    }
    if let Some(vram) = gpu.dedicated_vram_bytes.filter(|&b| b > 0) {
        parts.push(format_bytes(vram));
    }
    parts.join(" • ")
}

fn format_disk_stats(disk: &oxide_core::DiskSample) -> String {
    let act = disk
        .activity_pct
        .filter(|v| v.is_finite())
        .map(|v| format!("{:.0}%", v.clamp(0.0, 100.0)))
        .unwrap_or_else(|| "—".into());
    let temp = disk
        .temp_c
        .filter(|v| v.is_finite())
        .map(|v| format!("{v:.0} °C"))
        .unwrap_or_else(|| "—".into());
    format!("{act} • {temp}")
}

fn build_source_badges(snap: &Snapshot) -> Vec<(&'static str, bool)> {
    let has_nvml = snap.gpu.temp_source == TempSource::Nvml
        || snap.gpus.iter().any(|g| g.temp_source == TempSource::Nvml);
    let has_adlx = snap.gpu.temp_source == TempSource::Adlx
        || snap.gpus.iter().any(|g| g.temp_source == TempSource::Adlx);
    let has_nvme = snap.disk.temp_source == TempSource::Nvme
        || snap.disks.iter().any(|d| d.temp_source == TempSource::Nvme);
    let has_hwinfo = snap.cpu.temp_source == TempSource::Hwinfo;
    let has_lhm = snap.cpu.temp_source == TempSource::Lhm;

    vec![
        ("Native", true),
        ("NVML", has_nvml),
        ("ADLX", has_adlx),
        ("NVMe", has_nvme),
        ("HWiNFO", has_hwinfo),
        ("LHM", has_lhm),
    ]
}

fn to_hit_zone(button: FlyoutButton, rect: RectF, scale: f32) -> FlyoutHitZone {
    FlyoutHitZone {
        button,
        left: (rect.left * scale).floor() as i32,
        top: (rect.top * scale).floor() as i32,
        right: (rect.right * scale).ceil() as i32,
        bottom: (rect.bottom * scale).ceil() as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide_core::{DiskSample, GpuSample, GpuVendor};

    #[test]
    fn flyout_renders_deterministically_with_hit_zones_and_per_core_bars() {
        let mut renderer = FlyoutRenderer::new().unwrap();
        let cfg = Config::default();
        let mut snap = Snapshot::default();
        snap.cpu.usage_pct = 32.0;
        snap.cpu.freq_mhz = Some(3400.0);
        snap.memory.used_bytes = 9_500_000_000;
        snap.memory.total_bytes = 16_000_000_000;
        snap.memory.usage_pct = 59.0;
        snap.network.adapter = "Wi-Fi".into();
        snap.network.down_bps = 1_500_000.0;
        snap.network.up_bps = 250_000.0;
        snap.gpus = vec![
            GpuSample {
                name: "NVIDIA GeForce GTX 1650".into(),
                vendor: GpuVendor::Nvidia,
                luid: Some(1),
                usage_pct: Some(14.0),
                temp_c: Some(44.0),
                temp_source: TempSource::Nvml,
                dedicated_vram_bytes: Some(4 * 1024 * 1024 * 1024),
            },
            GpuSample {
                name: "AMD Radeon(TM) Graphics".into(),
                vendor: GpuVendor::Amd,
                luid: Some(2),
                usage_pct: Some(5.0),
                temp_c: Some(47.0),
                temp_source: TempSource::Adlx,
                dedicated_vram_bytes: Some(512 * 1024 * 1024),
            },
        ];
        snap.gpu = snap.gpus[0].clone();
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
        for i in 0..20 {
            let mut s = snap.clone();
            s.cpu.usage_pct = (20 + i * 3) as f32;
            history.record_snapshot(&s);
        }

        let per_core = [12.0, 48.0, 88.0, 22.0, 61.0, 15.0, 30.0, 72.0];
        let theme_ctx = SystemThemeContext::default();
        let localizer = Localizer::new("uk");

        let input = FlyoutInput {
            config: &cfg,
            snapshot: &snap,
            history: &history,
            per_core_pct: &per_core,
            theme_ctx: &theme_ctx,
            localizer: &localizer,
            game_mode_active: false,
            dpi: 96,
        };

        let frame1 = renderer.render(&input).unwrap();
        let pixels1 = renderer.dib().pixels().to_vec();
        assert!(frame1.width_px >= 300);
        assert!(frame1.height_px >= 200);
        assert_eq!(frame1.hit_zones.len(), 3);
        for hz in &frame1.hit_zones {
            assert!(hz.left >= 0 && hz.right <= frame1.width_px as i32);
            assert!(hz.top >= 0 && hz.bottom <= frame1.height_px as i32);
        }
        // Corners outside rounded rect must have 0 alpha; center must be opaque (~244).
        assert_eq!(pixels1[3], 0);
        let center_idx =
            ((frame1.height_px / 2) * frame1.width_px + (frame1.width_px / 2)) as usize * 4;
        assert!(pixels1[center_idx + 3] >= 240);

        let _frame2 = renderer.render(&input).unwrap();
        assert_eq!(pixels1, renderer.dib().pixels());
    }
}
