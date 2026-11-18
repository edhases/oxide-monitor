//! Direct2D + DirectWrite software DC renderer producing premultiplied BGRA8 DIB frames.
//!
//! Uses `D2D1_RENDER_TARGET_TYPE_SOFTWARE` on an `ID2D1DCRenderTarget`, which guarantees
//! zero D3D device creation and zero dGPU wake-ups in the background process while still
//! providing antialiased vector sparklines, arcs, rounded corners, OpenType `tnum`
//! tabular figures, color emoji, and per-pixel alpha for `UpdateLayeredWindowIndirect`.

use std::f32::consts::{FRAC_PI_2, TAU};

use oxide_core::{
    BackgroundMode, Config, DisplayItemModel, DisplayModel, DisplayModelBuilder, ExtraTelemetry,
    FontSpec, GroupStyle, HistoryStore, ItemVisual, ProgressStyle, Rgba, Snapshot,
    SparklinePlacement, SparklineStyle, ThemePalette, ValueAlign,
};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_CAP_STYLE_ROUND,
    D2D1_DASH_STYLE_DASH, D2D1_DASH_STYLE_SOLID, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_LINE_JOIN_ROUND, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_STROKE_STYLE_PROPERTIES, D2D1_SWEEP_DIRECTION_CLOCKWISE,
    D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory,
    ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
    IDWriteTextLayout, IDWriteTypography,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::{PCWSTR, w};
use windows_numerics::Vector2;

use crate::dib::DibSurface;
use crate::layout::{LayoutEngine, RectF, WidgetLayout};
use crate::{RenderError, RenderedFrame, SystemThemeContext};

#[derive(Clone, Debug, PartialEq)]
struct FontCacheKey {
    label: FontKey,
    value: FontKey,
    unit: FontKey,
}

#[derive(Clone, Debug, PartialEq)]
struct FontKey {
    family: String,
    size_bits: u32,
    weight: u16,
    italic: bool,
}

impl From<&FontSpec> for FontKey {
    fn from(spec: &FontSpec) -> Self {
        Self {
            family: spec.family.clone(),
            size_bits: spec.size_pt.to_bits(),
            weight: spec.weight,
            italic: spec.italic,
        }
    }
}

struct CachedFonts {
    key: FontCacheKey,
    label: IDWriteTextFormat,
    value: IDWriteTextFormat,
    unit: IDWriteTextFormat,
    /// Stable single-digit width (`"0"`) used as a minimum numeric slot width.
    digit_width_dip: f32,
}

/// Shared Direct2D/DirectWrite widget renderer used by both `oxide-ui` (taskbar)
/// and `oxide-settings` (pixel-exact Live Preview).
pub struct WidgetRenderer {
    d2d_factory: ID2D1Factory,
    dwrite_factory: IDWriteFactory,
    render_target: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    round_stroke: ID2D1StrokeStyle,
    dashed_stroke: ID2D1StrokeStyle,
    tabular_typography: IDWriteTypography,
    fonts: Option<CachedFonts>,
    dib: DibSurface,
    last_hash: u64,
    last_dpi: u32,
    last_vertical: bool,
    last_layout: WidgetLayout,
    /// Optional fit bounds supplied by the monitor (taskbar thickness / work area).
    max_width_px: Option<u32>,
    max_height_px: Option<u32>,
}

impl WidgetRenderer {
    pub fn new(initial_width_px: u32, initial_height_px: u32) -> Result<Self, RenderError> {
        let d2d_factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                .map_err(|err| RenderError::d2d("D2D1CreateFactory", err))?;

        let dwrite_factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
                .map_err(|err| RenderError::dwrite("DWriteCreateFactory", err))?;

        let rt_props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };

        let render_target = unsafe { d2d_factory.CreateDCRenderTarget(&rt_props) }
            .map_err(|err| RenderError::d2d("CreateDCRenderTarget", err))?;

        let brush = unsafe { render_target.CreateSolidColorBrush(&d2d_color(Rgba::WHITE), None) }
            .map_err(|err| RenderError::d2d("CreateSolidColorBrush", err))?;

        let round_props = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            miterLimit: 10.0,
            dashStyle: D2D1_DASH_STYLE_SOLID,
            dashOffset: 0.0,
        };
        let round_stroke = unsafe { d2d_factory.CreateStrokeStyle(&round_props, None) }
            .map_err(|err| RenderError::d2d("CreateStrokeStyle(round)", err))?;

        let dashed_props = D2D1_STROKE_STYLE_PROPERTIES {
            dashStyle: D2D1_DASH_STYLE_DASH,
            ..round_props
        };
        let dashed_stroke = unsafe { d2d_factory.CreateStrokeStyle(&dashed_props, None) }
            .map_err(|err| RenderError::d2d("CreateStrokeStyle(dashed)", err))?;

        let tabular_typography = unsafe { dwrite_factory.CreateTypography() }
            .map_err(|err| RenderError::dwrite("CreateTypography", err))?;
        unsafe {
            tabular_typography
                .AddFontFeature(DWRITE_FONT_FEATURE {
                    nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                    parameter: 1,
                })
                .map_err(|err| RenderError::dwrite("AddFontFeature(tnum)", err))?;
        }

        let dib = DibSurface::new(initial_width_px.max(1), initial_height_px.max(1))?;

        Ok(Self {
            d2d_factory,
            dwrite_factory,
            render_target,
            brush,
            round_stroke,
            dashed_stroke,
            tabular_typography,
            fonts: None,
            dib,
            last_hash: 0,
            last_dpi: 0,
            last_vertical: false,
            last_layout: WidgetLayout::default(),
            max_width_px: None,
            max_height_px: None,
        })
    }

    /// Force the next `render_if_dirty` call to redraw even if the content hash matches.
    pub fn invalidate(&mut self) {
        self.last_hash = 0;
    }

    /// Fit bounds in physical pixels, normally the taskbar thickness and work-area width.
    /// Changing them invalidates the current frame; passing `None` disables fitting
    /// (used by `oxide-settings` Live Preview).
    pub fn set_max_physical_size(&mut self, width_px: Option<u32>, height_px: Option<u32>) {
        if self.max_width_px != width_px || self.max_height_px != height_px {
            self.max_width_px = width_px.filter(|value| *value > 0);
            self.max_height_px = height_px.filter(|value| *value > 0);
            self.last_hash = 0;
        }
    }

    pub fn last_layout(&self) -> &WidgetLayout {
        &self.last_layout
    }

    pub fn dib(&self) -> &DibSurface {
        &self.dib
    }

    /// Render the widget into the internal BGRA8 DIB only when the `DisplayModel` hash,
    /// DPI, orientation, or `force` flag indicates a visual change.
    #[allow(clippy::too_many_arguments)]
    pub fn render_if_dirty(
        &mut self,
        config: &Config,
        snapshot: &Snapshot,
        history: &HistoryStore,
        theme_ctx: &SystemThemeContext,
        extra: &ExtraTelemetry<'_>,
        dpi: u32,
        vertical_taskbar: bool,
        force: bool,
    ) -> Result<Option<RenderedFrame<'_>>, RenderError> {
        let dpi = dpi.max(48);
        let palette = theme_ctx.resolve_palette(&config.general.theme);
        let model = DisplayModelBuilder::build(config, snapshot, history, &palette, extra);

        if !force
            && self.last_hash == model.content_hash
            && self.last_dpi == dpi
            && self.last_vertical == vertical_taskbar
        {
            return Ok(None);
        }

        self.render_model(config, &model, &palette, dpi, vertical_taskbar)?;
        self.last_hash = model.content_hash;
        self.last_dpi = dpi;
        self.last_vertical = vertical_taskbar;

        Ok(Some(RenderedFrame {
            width: self.dib.width(),
            height: self.dib.height(),
            width_dip: self.last_layout.width_dip,
            height_dip: self.last_layout.height_dip,
            bgra_premultiplied: self.dib.pixels(),
            hdc: self.dib.hdc(),
            content_hash: model.content_hash,
        }))
    }

    /// Unconditionally render a prebuilt `DisplayModel` (useful for golden tests and Live Preview).
    pub fn render_model(
        &mut self,
        config: &Config,
        model: &DisplayModel,
        palette: &ThemePalette,
        dpi: u32,
        vertical_taskbar: bool,
    ) -> Result<(), RenderError> {
        self.ensure_fonts(config)?;

        let layout = self.compute_layout(config, model, palette, vertical_taskbar);
        let size = config.layout.render_size(
            layout.width_dip,
            layout.height_dip,
            dpi,
            vertical_taskbar,
            self.max_width_px,
            self.max_height_px,
        );
        let (width_px, height_px) = (size.width_px, size.height_px);

        self.dib.ensure_size(width_px, height_px)?;
        self.dib.clear();

        let bind_rect = RECT {
            left: 0,
            top: 0,
            right: width_px as i32,
            bottom: height_px as i32,
        };

        unsafe {
            self.render_target
                .BindDC(self.dib.hdc(), &bind_rect)
                .map_err(|err| RenderError::d2d("BindDC", err))?;
            self.render_target.SetDpi(size.dpi, size.dpi);

            // ClearType corrupts alpha halos on translucent premultiplied DIBs; use
            // Grayscale whenever the surface can have per-pixel translucency.
            let is_opaque = matches!(config.surface.background_mode, BackgroundMode::HdrSafe)
                || (matches!(
                    config.surface.background_mode,
                    BackgroundMode::SolidAuto | BackgroundMode::Custom
                ) && config.surface.opacity == 255
                    && config.surface.corner_radius <= 0.1);
            let aa_mode = if is_opaque {
                D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE
            } else {
                D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE
            };
            self.render_target.SetTextAntialiasMode(aa_mode);

            self.render_target.BeginDraw();
            self.render_target
                .Clear(Some(&d2d_color(Rgba::TRANSPARENT)));
        }

        self.draw_surface(config, &layout, palette);
        self.draw_groups(&layout, palette);

        for cell in &layout.cells {
            if let Some(item) = model.items.get(cell.item_index) {
                self.draw_cell(config, item, cell.rect, palette);
            }
        }

        let end_result = unsafe { self.render_target.EndDraw(None, None) };
        self.last_layout = layout;
        end_result.map_err(|err| RenderError::d2d("EndDraw", err))
    }

    fn ensure_fonts(&mut self, config: &Config) -> Result<(), RenderError> {
        let key = FontCacheKey {
            label: FontKey::from(&config.typography.label),
            value: FontKey::from(&config.typography.value),
            unit: FontKey::from(&config.typography.unit),
        };
        if self.fonts.as_ref().is_some_and(|c| c.key == key) {
            return Ok(());
        }

        let label = create_text_format(&self.dwrite_factory, &config.typography.label)?;
        let value = create_text_format(&self.dwrite_factory, &config.typography.value)?;
        let unit = create_text_format(&self.dwrite_factory, &config.typography.unit)?;
        let digit_width_dip = measure_text_width(
            &self.dwrite_factory,
            &value,
            "0",
            Some(&self.tabular_typography),
        )
        .max(4.5);

        self.fonts = Some(CachedFonts {
            key,
            label,
            value,
            unit,
            digit_width_dip,
        });
        Ok(())
    }

    fn compute_layout(
        &self,
        config: &Config,
        model: &DisplayModel,
        palette: &ThemePalette,
        vertical_taskbar: bool,
    ) -> WidgetLayout {
        let fonts = self.fonts.as_ref().expect("fonts initialized");
        let typo = config
            .typography
            .tabular_nums
            .then_some(&self.tabular_typography);

        LayoutEngine::compute(config, &model.items, palette, vertical_taskbar, |item| {
            if let ItemVisual::GraphOnly { width, .. } = &item.visual {
                return *width;
            }
            let label_w = if item.label.is_empty() {
                0.0
            } else {
                measure_text_width(&self.dwrite_factory, &fonts.label, &item.label, None) + 3.0
            };
            let slot_digits = if item.unit == "%" || item.unit.starts_with('°') {
                3.min(config.format.digits.max(2))
            } else {
                config.format.digits.max(2)
            };
            let min_value_slot = fonts.digit_width_dip * slot_digits as f32;
            let value_w = measure_text_width(&self.dwrite_factory, &fonts.value, &item.value, typo)
                .max(min_value_slot);
            let unit_w = if item.unit.is_empty() {
                0.0
            } else {
                let space = unit_gap_dip(&item.unit, config);
                space + measure_text_width(&self.dwrite_factory, &fonts.unit, &item.unit, None)
            };
            let text_total = label_w + value_w + unit_w;

            match &item.visual {
                ItemVisual::Text => text_total,
                ItemVisual::TextSparkline {
                    placement,
                    graph_width,
                    ..
                } => match placement {
                    SparklinePlacement::Background => text_total,
                    SparklinePlacement::InlineLeft | SparklinePlacement::InlineRight => {
                        text_total + *graph_width + 4.0
                    }
                },
                ItemVisual::TextProgress {
                    style, bar_width, ..
                } => match style {
                    ProgressStyle::BarBottom | ProgressStyle::BarBackground => text_total,
                    ProgressStyle::BarInline => text_total + *bar_width + 6.0,
                    ProgressStyle::RadialRing => text_total + (*bar_width).min(18.0) + 5.0,
                },
                ItemVisual::GraphOnly { width, .. } => *width,
            }
        })
    }

    fn draw_surface(&self, config: &Config, layout: &WidgetLayout, palette: &ThemePalette) {
        let surface = &config.surface;
        let bg = match surface.background_mode {
            BackgroundMode::Transparent => {
                // Keep a visually imperceptible alpha = 1 hit-target when interactive so
                // right-click/double-click hits the full widget box instead of only glyph pixels.
                (!config.taskbar.click_through).then_some(Rgba::new(0, 0, 0, 1))
            }
            BackgroundMode::SolidAuto => Some(palette.background.with_alpha(surface.opacity)),
            BackgroundMode::Custom => {
                let raw = surface.background_color.resolve(palette);
                Some(raw.scale_alpha(surface.opacity as f32 / 255.0))
            }
            BackgroundMode::HdrSafe => Some(palette.background.with_alpha(255)),
        };

        let half_border = surface.border_width * 0.5;
        let rect = D2D_RECT_F {
            left: half_border,
            top: half_border,
            right: (layout.width_dip - half_border).max(half_border),
            bottom: (layout.height_dip - half_border).max(half_border),
        };
        let rounded = D2D1_ROUNDED_RECT {
            rect,
            radiusX: surface.corner_radius,
            radiusY: surface.corner_radius,
        };

        if let Some(fill) = bg {
            unsafe {
                self.brush.SetColor(&d2d_color(fill));
                self.render_target
                    .FillRoundedRectangle(&rounded, &self.brush);
            }
        }

        if surface.border_width > 0.05 {
            let border_col = surface.border_color.resolve(palette);
            unsafe {
                self.brush.SetColor(&d2d_color(border_col));
                self.render_target.DrawRoundedRectangle(
                    &rounded,
                    &self.brush,
                    surface.border_width,
                    None,
                );
            }
        }
    }

    fn draw_groups(&self, layout: &WidgetLayout, palette: &ThemePalette) {
        for group in &layout.groups {
            match group.style {
                GroupStyle::None => {}
                GroupStyle::Pill => {
                    let rounded = D2D1_ROUNDED_RECT {
                        rect: to_d2d_rect(group.rect),
                        radiusX: group.pill_radius,
                        radiusY: group.pill_radius,
                    };
                    unsafe {
                        self.brush.SetColor(&d2d_color(group.pill_bg));
                        self.render_target
                            .FillRoundedRectangle(&rounded, &self.brush);
                        self.brush.SetColor(&d2d_color(palette.border));
                        self.render_target
                            .DrawRoundedRectangle(&rounded, &self.brush, 0.75, None);
                    }
                }
                GroupStyle::SeparatorAfter => {
                    let x = group.rect.right + 1.0;
                    unsafe {
                        self.brush.SetColor(&d2d_color(palette.separator));
                        self.render_target.DrawLine(
                            Vector2 {
                                X: x,
                                Y: group.rect.top + 3.0,
                            },
                            Vector2 {
                                X: x,
                                Y: (group.rect.bottom - 3.0).max(group.rect.top + 3.0),
                            },
                            &self.brush,
                            1.0,
                            None,
                        );
                    }
                }
            }
        }
    }

    fn draw_cell(
        &self,
        config: &Config,
        item: &DisplayItemModel,
        cell: RectF,
        palette: &ThemePalette,
    ) {
        if cell.width() < 2.0 || cell.height() < 2.0 {
            return;
        }
        if item.is_separator {
            let cx = (cell.left + cell.right) * 0.5;
            unsafe {
                self.brush.SetColor(&d2d_color(palette.separator));
                self.render_target.DrawLine(
                    Vector2 {
                        X: cx,
                        Y: cell.top + 2.0,
                    },
                    Vector2 {
                        X: cx,
                        Y: (cell.bottom - 2.0).max(cell.top + 2.0),
                    },
                    &self.brush,
                    1.0,
                    None,
                );
            }
            return;
        }

        match &item.visual {
            ItemVisual::Text => {
                self.draw_item_text(config, item, cell, palette);
            }
            ItemVisual::TextSparkline {
                placement,
                style,
                fill_alpha,
                stroke_width,
                graph_width,
                ..
            } => match placement {
                SparklinePlacement::Background => {
                    self.draw_sparkline(
                        cell,
                        &item.sparkline_primary,
                        item.sparkline_min,
                        item.sparkline_max,
                        item.value_color,
                        *style,
                        *fill_alpha,
                        *stroke_width,
                    );
                    self.draw_item_text(config, item, cell, palette);
                }
                SparklinePlacement::InlineRight => {
                    let gw = (*graph_width).min(cell.width() * 0.45);
                    let text_rect = RectF::new(
                        cell.left,
                        cell.top,
                        (cell.right - gw - 3.0).max(cell.left),
                        cell.bottom,
                    );
                    let graph_rect = RectF::new(
                        (cell.right - gw).max(cell.left),
                        cell.top + 1.0,
                        cell.right,
                        cell.bottom - 1.0,
                    );
                    self.draw_item_text(config, item, text_rect, palette);
                    self.draw_sparkline(
                        graph_rect,
                        &item.sparkline_primary,
                        item.sparkline_min,
                        item.sparkline_max,
                        item.value_color,
                        *style,
                        *fill_alpha,
                        *stroke_width,
                    );
                }
                SparklinePlacement::InlineLeft => {
                    let gw = (*graph_width).min(cell.width() * 0.45);
                    let graph_rect = RectF::new(
                        cell.left,
                        cell.top + 1.0,
                        (cell.left + gw).min(cell.right),
                        cell.bottom - 1.0,
                    );
                    let text_rect = RectF::new(
                        (cell.left + gw + 3.0).min(cell.right),
                        cell.top,
                        cell.right,
                        cell.bottom,
                    );
                    self.draw_sparkline(
                        graph_rect,
                        &item.sparkline_primary,
                        item.sparkline_min,
                        item.sparkline_max,
                        item.value_color,
                        *style,
                        *fill_alpha,
                        *stroke_width,
                    );
                    self.draw_item_text(config, item, text_rect, palette);
                }
            },
            ItemVisual::TextProgress {
                style,
                thickness,
                track_alpha,
                bar_width,
            } => {
                let ratio = item.progress_ratio.unwrap_or(0.0).clamp(0.0, 1.0);
                match style {
                    ProgressStyle::BarBottom => {
                        let bar_h = thickness.clamp(1.5, 4.0);
                        let text_rect = RectF::new(
                            cell.left,
                            cell.top,
                            cell.right,
                            (cell.bottom - bar_h - 1.5).max(cell.top + 1.0),
                        );
                        let bar_rect = RectF::new(
                            cell.left,
                            (cell.bottom - bar_h).max(cell.top),
                            cell.right,
                            cell.bottom,
                        );
                        self.draw_item_text(config, item, text_rect, palette);
                        self.draw_linear_bar(
                            bar_rect,
                            ratio,
                            item.value_color,
                            *track_alpha,
                            palette,
                        );
                    }
                    ProgressStyle::BarBackground => {
                        let fill_w = cell.width() * ratio;
                        let bg_rect = D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: cell.left,
                                top: cell.top,
                                right: cell.left + fill_w,
                                bottom: cell.bottom,
                            },
                            radiusX: 3.0,
                            radiusY: 3.0,
                        };
                        if fill_w > 0.5 {
                            unsafe {
                                self.brush
                                    .SetColor(&d2d_color(item.value_color.scale_alpha(0.24)));
                                self.render_target
                                    .FillRoundedRectangle(&bg_rect, &self.brush);
                            }
                        }
                        self.draw_item_text(config, item, cell, palette);
                    }
                    ProgressStyle::BarInline => {
                        let bw = (*bar_width).min(cell.width() * 0.35);
                        let text_rect = RectF::new(
                            cell.left,
                            cell.top,
                            (cell.right - bw - 4.0).max(cell.left),
                            cell.bottom,
                        );
                        let mid_y = (cell.top + cell.bottom) * 0.5;
                        let half_t = (*thickness * 0.5).clamp(1.5, 3.5);
                        let bar_rect = RectF::new(
                            (cell.right - bw).max(cell.left),
                            mid_y - half_t,
                            cell.right,
                            mid_y + half_t,
                        );
                        self.draw_item_text(config, item, text_rect, palette);
                        self.draw_linear_bar(
                            bar_rect,
                            ratio,
                            item.value_color,
                            *track_alpha,
                            palette,
                        );
                    }
                    ProgressStyle::RadialRing => {
                        let diameter = cell.height().min(*bar_width).clamp(10.0, 22.0);
                        let cx = cell.left + diameter * 0.5;
                        let cy = (cell.top + cell.bottom) * 0.5;
                        let radius = (diameter * 0.5 - *thickness * 0.5).max(3.0);
                        self.draw_radial_ring(
                            cx,
                            cy,
                            radius,
                            *thickness,
                            ratio,
                            item.value_color,
                            *track_alpha,
                            palette,
                        );
                        let text_rect = RectF::new(
                            (cell.left + diameter + 4.0).min(cell.right),
                            cell.top,
                            cell.right,
                            cell.bottom,
                        );
                        self.draw_item_text(config, item, text_rect, palette);
                    }
                }
            }
            ItemVisual::GraphOnly {
                style,
                fill_alpha,
                stroke_width,
                dashed_border,
                secondary_color,
                ..
            } => {
                let inner = cell.inset(1.0, 1.0);
                self.draw_sparkline(
                    inner,
                    &item.sparkline_primary,
                    item.sparkline_min,
                    item.sparkline_max,
                    item.value_color,
                    *style,
                    *fill_alpha,
                    *stroke_width,
                );
                if !item.sparkline_secondary.is_empty() {
                    let sec = secondary_color
                        .as_ref()
                        .map(|c| c.resolve(palette))
                        .unwrap_or(palette.accent);
                    self.draw_sparkline(
                        inner,
                        &item.sparkline_secondary,
                        item.sparkline_min,
                        item.sparkline_max,
                        sec,
                        *style,
                        *fill_alpha * 0.75,
                        *stroke_width,
                    );
                }
                if *dashed_border {
                    let border_rect = to_d2d_rect(cell);
                    unsafe {
                        self.brush
                            .SetColor(&d2d_color(palette.border.scale_alpha(1.8)));
                        self.render_target.DrawRectangle(
                            &border_rect,
                            &self.brush,
                            0.75,
                            Some(&self.dashed_stroke),
                        );
                    }
                }
            }
        }
    }

    fn draw_item_text(
        &self,
        config: &Config,
        item: &DisplayItemModel,
        cell: RectF,
        palette: &ThemePalette,
    ) {
        let fonts = self.fonts.as_ref().expect("fonts initialized");
        let use_shadow = config.surface.text_shadow
            && matches!(config.surface.background_mode, BackgroundMode::Transparent);
        let options = if config.typography.color_emoji {
            D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT | D2D1_DRAW_TEXT_OPTIONS_CLIP
        } else {
            D2D1_DRAW_TEXT_OPTIONS_CLIP
        };
        let typo = config
            .typography
            .tabular_nums
            .then_some(&self.tabular_typography);

        let unit_w = if item.unit.is_empty() {
            0.0
        } else {
            measure_text_width(&self.dwrite_factory, &fonts.unit, &item.unit, None)
        };
        let unit_space = if item.unit.is_empty() {
            0.0
        } else {
            unit_gap_dip(&item.unit, config)
        };
        let value_w = measure_text_width(&self.dwrite_factory, &fonts.value, &item.value, typo);
        let value_block_w = value_w + unit_space + unit_w;

        let (label_x, label_max_w, value_x, unit_x) = match config.typography.value_align {
            ValueAlign::Right => {
                let vx = (cell.right - value_block_w).max(cell.left);
                let ux = vx + value_w + unit_space;
                let lw = (vx - cell.left - 2.0).max(0.0);
                (cell.left, lw, vx, ux)
            }
            ValueAlign::Left => {
                let lw = if item.label.is_empty() {
                    0.0
                } else {
                    measure_text_width(&self.dwrite_factory, &fonts.label, &item.label, None)
                        .min(cell.width() * 0.5)
                };
                let vx = if lw > 0.0 {
                    cell.left + lw + 3.0
                } else {
                    cell.left
                };
                let ux = vx + value_w + unit_space;
                (cell.left, lw, vx, ux)
            }
        };

        let h = cell.height().max(1.0);

        if !item.label.is_empty() && label_max_w > 1.0 {
            self.draw_run(
                &item.label,
                &fonts.label,
                None,
                label_x,
                cell.top,
                label_max_w,
                h,
                item.label_color,
                use_shadow.then_some(palette.shadow),
                options,
            );
        }

        if !item.value.is_empty() {
            let max_vw = (cell.right - value_x).max(1.0);
            self.draw_run(
                &item.value,
                &fonts.value,
                typo,
                value_x,
                cell.top,
                max_vw,
                h,
                item.value_color,
                use_shadow.then_some(palette.shadow),
                options,
            );
        }

        if !item.unit.is_empty() && unit_x < cell.right {
            let max_uw = (cell.right - unit_x).max(1.0);
            self.draw_run(
                &item.unit,
                &fonts.unit,
                None,
                unit_x,
                cell.top,
                max_uw,
                h,
                item.unit_color,
                use_shadow.then_some(palette.shadow),
                options,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_run(
        &self,
        text: &str,
        format: &IDWriteTextFormat,
        typography: Option<&IDWriteTypography>,
        x: f32,
        y: f32,
        max_w: f32,
        max_h: f32,
        color: Rgba,
        shadow: Option<Rgba>,
        options: windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS,
    ) {
        let Ok(layout) =
            create_text_layout(&self.dwrite_factory, format, text, max_w, max_h, typography)
        else {
            return;
        };
        unsafe {
            if let Some(sh) = shadow {
                self.brush.SetColor(&d2d_color(sh));
                self.render_target.DrawTextLayout(
                    Vector2 { X: x, Y: y + 0.8 },
                    &layout,
                    &self.brush,
                    options,
                );
            }
            self.brush.SetColor(&d2d_color(color));
            self.render_target.DrawTextLayout(
                Vector2 { X: x, Y: y },
                &layout,
                &self.brush,
                options,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_sparkline(
        &self,
        rect: RectF,
        points: &[f32],
        min_val: f32,
        max_val: f32,
        color: Rgba,
        style: SparklineStyle,
        fill_alpha: f32,
        stroke_width: f32,
    ) {
        if points.len() < 2 || rect.width() < 4.0 || rect.height() < 4.0 {
            return;
        }
        let span = (max_val - min_val).max(1e-3);
        let n = points.len();

        match style {
            SparklineStyle::Bars => {
                let bar_w = (rect.width() / n as f32).max(1.0);
                let fill_col = color.scale_alpha(fill_alpha.max(0.35));
                unsafe {
                    self.brush.SetColor(&d2d_color(fill_col));
                }
                for (i, &val) in points.iter().enumerate() {
                    let norm = ((val - min_val) / span).clamp(0.0, 1.0);
                    let bh = norm * rect.height();
                    if bh < 0.5 {
                        continue;
                    }
                    let x0 = rect.left + i as f32 * bar_w;
                    let r = D2D_RECT_F {
                        left: x0,
                        top: rect.bottom - bh,
                        right: (x0 + (bar_w - 0.5).max(0.8)).min(rect.right),
                        bottom: rect.bottom,
                    };
                    unsafe {
                        self.render_target.FillRectangle(&r, &self.brush);
                    }
                }
            }
            SparklineStyle::Area | SparklineStyle::Line => {
                let coords: Vec<Vector2> = points
                    .iter()
                    .enumerate()
                    .map(|(i, &val)| {
                        let t = i as f32 / (n - 1) as f32;
                        let norm = ((val - min_val) / span).clamp(0.0, 1.0);
                        Vector2 {
                            X: rect.left + t * rect.width(),
                            Y: rect.bottom - norm * (rect.height() - 1.0),
                        }
                    })
                    .collect();

                if style == SparklineStyle::Area
                    && fill_alpha > 0.01
                    && let Ok(geom) = unsafe { self.d2d_factory.CreatePathGeometry() }
                    && let Ok(sink) = unsafe { geom.Open() }
                {
                    unsafe {
                        sink.BeginFigure(
                            Vector2 {
                                X: coords[0].X,
                                Y: rect.bottom,
                            },
                            D2D1_FIGURE_BEGIN_FILLED,
                        );
                        for pt in &coords {
                            sink.AddLine(*pt);
                        }
                        sink.AddLine(Vector2 {
                            X: coords[coords.len() - 1].X,
                            Y: rect.bottom,
                        });
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        if sink.Close().is_ok() {
                            self.brush
                                .SetColor(&d2d_color(color.scale_alpha(fill_alpha)));
                            self.render_target.FillGeometry(&geom, &self.brush, None);
                        }
                    }
                }

                if let Ok(geom) = unsafe { self.d2d_factory.CreatePathGeometry() }
                    && let Ok(sink) = unsafe { geom.Open() }
                {
                    unsafe {
                        sink.BeginFigure(coords[0], D2D1_FIGURE_BEGIN_HOLLOW);
                        for pt in &coords[1..] {
                            sink.AddLine(*pt);
                        }
                        sink.EndFigure(D2D1_FIGURE_END_OPEN);
                        if sink.Close().is_ok() {
                            self.brush.SetColor(&d2d_color(color.scale_alpha(0.85)));
                            self.render_target.DrawGeometry(
                                &geom,
                                &self.brush,
                                stroke_width,
                                Some(&self.round_stroke),
                            );
                        }
                    }
                }
            }
        }
    }

    fn draw_linear_bar(
        &self,
        rect: RectF,
        ratio: f32,
        color: Rgba,
        track_alpha: f32,
        palette: &ThemePalette,
    ) {
        let radius = rect.height() * 0.5;
        let track_base = if palette.is_dark {
            Rgba::WHITE
        } else {
            Rgba::BLACK
        };
        let track = D2D1_ROUNDED_RECT {
            rect: to_d2d_rect(rect),
            radiusX: radius,
            radiusY: radius,
        };
        unsafe {
            self.brush
                .SetColor(&d2d_color(track_base.scale_alpha(track_alpha)));
            self.render_target.FillRoundedRectangle(&track, &self.brush);
        }

        let fill_w = rect.width() * ratio.clamp(0.0, 1.0);
        if fill_w >= 1.0 {
            let fill = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: rect.left,
                    top: rect.top,
                    right: rect.left + fill_w,
                    bottom: rect.bottom,
                },
                radiusX: radius,
                radiusY: radius,
            };
            unsafe {
                self.brush.SetColor(&d2d_color(color));
                self.render_target.FillRoundedRectangle(&fill, &self.brush);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_radial_ring(
        &self,
        cx: f32,
        cy: f32,
        radius: f32,
        thickness: f32,
        ratio: f32,
        color: Rgba,
        track_alpha: f32,
        palette: &ThemePalette,
    ) {
        let track_base = if palette.is_dark {
            Rgba::WHITE
        } else {
            Rgba::BLACK
        };
        let ellipse = D2D1_ELLIPSE {
            point: Vector2 { X: cx, Y: cy },
            radiusX: radius,
            radiusY: radius,
        };
        unsafe {
            self.brush
                .SetColor(&d2d_color(track_base.scale_alpha(track_alpha)));
            self.render_target
                .DrawEllipse(&ellipse, &self.brush, thickness, None);
        }

        let ratio = ratio.clamp(0.0, 1.0);
        if ratio < 0.01 {
            return;
        }
        if ratio >= 0.995 {
            unsafe {
                self.brush.SetColor(&d2d_color(color));
                self.render_target
                    .DrawEllipse(&ellipse, &self.brush, thickness, None);
            }
            return;
        }

        let start_angle = -FRAC_PI_2;
        let end_angle = start_angle + ratio * TAU;
        let start = Vector2 {
            X: cx + radius * start_angle.cos(),
            Y: cy + radius * start_angle.sin(),
        };
        let end = Vector2 {
            X: cx + radius * end_angle.cos(),
            Y: cy + radius * end_angle.sin(),
        };
        let arc_size = if ratio > 0.5 {
            D2D1_ARC_SIZE_LARGE
        } else {
            D2D1_ARC_SIZE_SMALL
        };

        if let Ok(geom) = unsafe { self.d2d_factory.CreatePathGeometry() }
            && let Ok(sink) = unsafe { geom.Open() }
        {
            unsafe {
                sink.BeginFigure(start, D2D1_FIGURE_BEGIN_HOLLOW);
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: end,
                    size: D2D_SIZE_F {
                        width: radius,
                        height: radius,
                    },
                    rotationAngle: 0.0,
                    sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                    arcSize: arc_size,
                });
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                if sink.Close().is_ok() {
                    self.brush.SetColor(&d2d_color(color));
                    self.render_target.DrawGeometry(
                        &geom,
                        &self.brush,
                        thickness,
                        Some(&self.round_stroke),
                    );
                }
            }
        }
    }
}

fn create_text_format(
    factory: &IDWriteFactory,
    spec: &FontSpec,
) -> Result<IDWriteTextFormat, RenderError> {
    let family: Vec<u16> = spec
        .family
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let style = if spec.italic {
        DWRITE_FONT_STYLE_ITALIC
    } else {
        DWRITE_FONT_STYLE_NORMAL
    };
    let size_dip = (spec.size_pt * (96.0 / 72.0)).clamp(7.0, 48.0);
    let format = unsafe {
        factory.CreateTextFormat(
            PCWSTR(family.as_ptr()),
            None,
            DWRITE_FONT_WEIGHT(spec.weight as i32),
            style,
            DWRITE_FONT_STRETCH_NORMAL,
            size_dip,
            w!("en-us"),
        )
    }
    .map_err(|err| RenderError::dwrite("CreateTextFormat", err))?;

    unsafe {
        let _ = format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        let trimming = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            delimiter: 0,
            delimiterCount: 0,
        };
        let _ = format.SetTrimming(&trimming, None);
    }
    Ok(format)
}

fn create_text_layout(
    factory: &IDWriteFactory,
    format: &IDWriteTextFormat,
    text: &str,
    max_width: f32,
    max_height: f32,
    typography: Option<&IDWriteTypography>,
) -> Result<IDWriteTextLayout, RenderError> {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let layout = unsafe {
        factory.CreateTextLayout(&utf16, format, max_width.max(1.0), max_height.max(1.0))
    }
    .map_err(|err| RenderError::dwrite("CreateTextLayout", err))?;

    if let Some(typo) = typography
        && !utf16.is_empty()
    {
        let range = DWRITE_TEXT_RANGE {
            startPosition: 0,
            length: utf16.len() as u32,
        };
        unsafe {
            let _ = layout.SetTypography(typo, range);
        }
    }
    Ok(layout)
}

fn measure_text_width(
    factory: &IDWriteFactory,
    format: &IDWriteTextFormat,
    text: &str,
    typography: Option<&IDWriteTypography>,
) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let Ok(layout) = create_text_layout(factory, format, text, 1000.0, 100.0, typography) else {
        return text.chars().count() as f32 * 6.0;
    };
    let mut metrics = DWRITE_TEXT_METRICS::default();
    if unsafe { layout.GetMetrics(&mut metrics) }.is_ok() {
        metrics.widthIncludingTrailingWhitespace.ceil()
    } else {
        text.chars().count() as f32 * 6.0
    }
}

fn unit_gap_dip(unit: &str, config: &Config) -> f32 {
    if unit == "%" || config.format.short_mode || !config.format.separate_space {
        0.5
    } else {
        2.5
    }
}

fn d2d_color(c: Rgba) -> D2D1_COLOR_F {
    let [r, g, b, a] = c.to_f32();
    D2D1_COLOR_F { r, g, b, a }
}

fn to_d2d_rect(r: RectF) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide_core::{
        DiskSample, GpuSample, GpuVendor, TempSource, builtin_presets, find_builtin_preset,
    };

    fn mock_snapshot() -> (Snapshot, HistoryStore) {
        let mut snap = Snapshot::default();
        snap.seq = 42;
        snap.network.adapter = "Wi-Fi".into();
        snap.network.connected = true;
        snap.network.up_bps = 340_000.0;
        snap.network.down_bps = 2_400_000.0;
        snap.cpu.usage_pct = 38.0;
        snap.cpu.freq_mhz = Some(3450.0);
        snap.cpu.temp_c = Some(58.0);
        snap.cpu.temp_source = TempSource::Hwinfo;
        snap.memory.used_bytes = 10_200_000_000;
        snap.memory.total_bytes = 16_000_000_000;
        snap.memory.usage_pct = 64.0;
        snap.gpus = vec![
            GpuSample {
                name: "AMD Radeon(TM) Graphics".into(),
                vendor: GpuVendor::Amd,
                luid: Some(1),
                usage_pct: Some(6.0),
                temp_c: Some(46.0),
                temp_source: TempSource::Adlx,
                dedicated_vram_bytes: Some(512 * 1024 * 1024),
            },
            GpuSample {
                name: "NVIDIA GeForce GTX 1650".into(),
                vendor: GpuVendor::Nvidia,
                luid: Some(2),
                usage_pct: Some(44.0),
                temp_c: Some(68.0),
                temp_source: TempSource::Nvml,
                dedicated_vram_bytes: Some(4 * 1024 * 1024 * 1024),
            },
        ];
        snap.gpu = snap.gpus[1].clone();
        snap.disks = vec![DiskSample {
            name: "Apacer AS2280P4U 1TB".into(),
            index: Some(0),
            is_nvme: true,
            activity_pct: Some(8.0),
            temp_c: Some(42.0),
            temp_source: TempSource::Nvme,
        }];
        snap.disk = snap.disks[0].clone();

        let mut history = HistoryStore::new();
        for step in 0..30 {
            let mut s = snap.clone();
            s.cpu.usage_pct = (20 + (step * 7) % 70) as f32;
            s.network.down_bps = 150_000.0 + (step as f64 * 95_000.0);
            s.network.up_bps = 40_000.0 + (step as f64 * 25_000.0);
            history.record_snapshot(&s);
        }
        (snap, history)
    }

    fn count_nonzero_alpha(bgra: &[u8]) -> usize {
        bgra.chunks_exact(4).filter(|px| px[3] > 0).count()
    }

    #[test]
    fn renders_all_five_presets_with_non_empty_pixels_and_dirty_check() {
        let mut renderer = WidgetRenderer::new(294, 35).unwrap();
        let (snap, history) = mock_snapshot();
        let theme_ctx = SystemThemeContext::default();
        let extra = ExtraTelemetry::default();

        for preset in builtin_presets() {
            let mut cfg = Config::default();
            preset.apply_to(&mut cfg);
            renderer.invalidate();

            let frame = renderer
                .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 96, false, false)
                .unwrap()
                .expect("first render must produce frame");
            assert!(frame.width >= 80, "preset {} too narrow", preset.preset.id);
            assert!(frame.height >= 20, "preset {} too short", preset.preset.id);
            assert!(
                count_nonzero_alpha(frame.bgra_premultiplied) > 100,
                "preset {} rendered blank",
                preset.preset.id
            );

            // Second call with unchanged snapshot/config must skip rendering (dirty check).
            let skipped = renderer
                .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 96, false, false)
                .unwrap();
            assert!(skipped.is_none(), "dirty check should skip unchanged frame");
        }
    }

    #[test]
    fn high_dpi_scales_pixel_buffer_proportionally() {
        let mut renderer = WidgetRenderer::new(294, 35).unwrap();
        let (snap, history) = mock_snapshot();
        let mut cfg = Config::default();
        cfg.layout.exact_physical_size = false;
        let theme_ctx = SystemThemeContext::default();
        let extra = ExtraTelemetry::default();

        let f96 = renderer
            .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 96, false, true)
            .unwrap()
            .unwrap();
        let (w96, h96) = (f96.width, f96.height);

        let f144 = renderer
            .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 144, false, false)
            .unwrap()
            .unwrap();
        assert_eq!(f144.width, ((w96 as f32) * 1.5).ceil() as u32);
        assert_eq!(f144.height, ((h96 as f32) * 1.5).ceil() as u32);
    }

    #[test]
    fn exact_physical_size_keeps_294x35_at_106_and_144_dpi() {
        let mut renderer = WidgetRenderer::new(294, 35).unwrap();
        let (snap, history) = mock_snapshot();
        let cfg = Config::default(); // classic-2row: exact_physical_size = true, 294x35
        let theme_ctx = SystemThemeContext::default();
        let extra = ExtraTelemetry::default();

        for dpi in [96u32, 106, 144] {
            let frame = renderer
                .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, dpi, false, true)
                .unwrap()
                .unwrap();
            assert_eq!(frame.width, 294, "expected exact 294px width at DPI {dpi}");
            assert_eq!(frame.height, 35, "expected exact 35px height at DPI {dpi}");
            assert!(count_nonzero_alpha(frame.bgra_premultiplied) > 100);
        }
    }

    #[test]
    fn scale_pct_produces_expected_pixel_sizes() {
        let mut renderer = WidgetRenderer::new(294, 35).unwrap();
        let (snap, history) = mock_snapshot();
        let theme_ctx = SystemThemeContext::default();
        let extra = ExtraTelemetry::default();
        let mut cfg = Config::default();
        cfg.layout.size_mode = Some(oxide_core::SizeMode::ExactPhysical);

        for (scale, expected) in [
            (75u16, (221u32, 27u32)),
            (100, (294, 35)),
            (125, (368, 44)),
            (150, (441, 53)),
            (200, (588, 70)),
        ] {
            cfg.layout.scale_pct = scale;
            renderer.invalidate();
            let frame = renderer
                .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 96, false, false)
                .unwrap()
                .unwrap();
            assert_eq!(
                (frame.width, frame.height),
                expected,
                "scale {scale}% must produce {expected:?}"
            );
            assert!(count_nonzero_alpha(frame.bgra_premultiplied) > 100);
        }
    }

    #[test]
    fn fit_bounds_shrink_without_cropping() {
        let mut renderer = WidgetRenderer::new(294, 35).unwrap();
        let (snap, history) = mock_snapshot();
        let theme_ctx = SystemThemeContext::default();
        let extra = ExtraTelemetry::default();
        let mut cfg = Config::default();
        cfg.layout.size_mode = Some(oxide_core::SizeMode::ExactPhysical);
        cfg.layout.scale_pct = 200;

        let unclamped = {
            let frame = renderer
                .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 96, false, true)
                .unwrap()
                .unwrap();
            (frame.width, frame.height)
        };
        assert_eq!(unclamped, (588, 70));

        renderer.set_max_physical_size(None, Some(48));
        let clamped = renderer
            .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 96, false, false)
            .unwrap()
            .unwrap();
        assert_eq!(clamped.height, 48);
        assert!(
            (395..=403).contains(&clamped.width),
            "width must shrink proportionally, got {}",
            clamped.width
        );
        assert!(count_nonzero_alpha(clamped.bgra_premultiplied) > 100);
    }

    #[test]
    fn fixed_dip_size_follows_dpi_and_fits_small_taskbars() {
        let mut renderer = WidgetRenderer::new(294, 35).unwrap();
        let (snap, history) = mock_snapshot();
        let theme_ctx = SystemThemeContext::default();
        let extra = ExtraTelemetry::default();
        let mut cfg = Config::default();
        cfg.layout.size_mode = Some(oxide_core::SizeMode::FixedDip);
        cfg.normalize();

        for (dpi, expected) in [
            (96u32, (294u32, 35u32)),
            (106, (325, 39)),
            (144, (441, 53)),
            (192, (588, 70)),
        ] {
            renderer.set_max_physical_size(None, None);
            renderer.invalidate();
            let frame = renderer
                .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, dpi, false, false)
                .unwrap()
                .unwrap();
            assert_eq!(
                (frame.width, frame.height),
                expected,
                "fixed DIP at {dpi} DPI"
            );
            assert!(count_nonzero_alpha(frame.bgra_premultiplied) > 100);
        }

        // A 48 px taskbar must clamp the 192 DPI (70 px) widget without cropping it.
        renderer.set_max_physical_size(None, Some(48));
        let clamped = renderer
            .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 192, false, true)
            .unwrap()
            .unwrap();
        assert!(clamped.height <= 48, "height {} > 48", clamped.height);
        assert!(clamped.width < 588);
        assert!(count_nonzero_alpha(clamped.bgra_premultiplied) > 100);
    }

    #[test]
    fn auto_content_scales_with_dpi_and_scale_pct() {
        let mut renderer = WidgetRenderer::new(294, 35).unwrap();
        let (snap, history) = mock_snapshot();
        let theme_ctx = SystemThemeContext::default();
        let extra = ExtraTelemetry::default();
        let mut cfg = Config::default();
        cfg.layout.size_mode = Some(oxide_core::SizeMode::AutoContent);
        cfg.layout.height = 28;

        let base = {
            let frame = renderer
                .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 96, false, true)
                .unwrap()
                .unwrap();
            (frame.width, frame.height)
        };
        assert_eq!(base.1, 28);

        cfg.layout.scale_pct = 150;
        renderer.invalidate();
        let scaled = renderer
            .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 96, false, false)
            .unwrap()
            .unwrap();
        assert_eq!(scaled.height, 42);
        assert!(
            (scaled.width as f32 - base.0 as f32 * 1.5).abs() <= 3.0,
            "width {} must be ~1.5x of {}",
            scaled.width,
            base.0
        );
    }

    #[test]
    fn tabular_nums_keeps_numeric_width_identical_for_111_and_888() {
        let renderer = WidgetRenderer::new(100, 35).unwrap();
        let spec = FontSpec {
            family: "Segoe UI".into(),
            size_pt: 9.0,
            weight: 600,
            italic: false,
            color: oxide_core::ColorSpec::ThemeValue,
        };
        let fmt = create_text_format(&renderer.dwrite_factory, &spec).unwrap();
        let w1 = measure_text_width(
            &renderer.dwrite_factory,
            &fmt,
            "11.1",
            Some(&renderer.tabular_typography),
        );
        let w8 = measure_text_width(
            &renderer.dwrite_factory,
            &fmt,
            "88.8",
            Some(&renderer.tabular_typography),
        );
        assert!(
            (w1 - w8).abs() <= 1.0,
            "tabular-nums should equalize '11.1' ({w1}) and '88.8' ({w8})"
        );
    }

    #[test]
    fn golden_render_is_deterministic_across_runs() {
        let mut renderer = WidgetRenderer::new(294, 35).unwrap();
        let (snap, history) = mock_snapshot();
        let mut cfg = Config::default();
        find_builtin_preset("gamer-sparklines")
            .unwrap()
            .apply_to(&mut cfg);
        let theme_ctx = SystemThemeContext::default();
        let extra = ExtraTelemetry::default();

        let first = renderer
            .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 96, false, true)
            .unwrap()
            .unwrap()
            .bgra_premultiplied
            .to_vec();

        let second = renderer
            .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 96, false, true)
            .unwrap()
            .unwrap()
            .bgra_premultiplied
            .to_vec();

        assert_eq!(
            first, second,
            "D2D software DIB render must be bit-exact deterministic"
        );
    }

    #[test]
    fn renders_all_four_visual_modes_and_substyles_cleanly() {
        use oxide_core::{
            AdapterTarget, ColorRule, ColorSpec, DiskTarget, ItemConfig, MetricSource,
            SparklineScale, thermal_gradient,
        };

        let mut renderer = WidgetRenderer::new(400, 40).unwrap();
        let (snap, history) = mock_snapshot();
        let theme_ctx = SystemThemeContext::default();
        let extra = ExtraTelemetry::default();

        let mut cfg = Config::default();
        cfg.aliases
            .disks
            .insert("Apacer AS2280P4U 1TB".into(), "NVMe".into());
        cfg.layout.rows = 2;
        cfg.layout.width_mode = oxide_core::WidthMode::Auto;
        cfg.layout.height = 42;
        cfg.items = vec![
            ItemConfig::new_v2(
                "m_text",
                "",
                MetricSource::DiskTemp {
                    target: DiskTarget::Index(0),
                },
                ItemVisual::Text,
                Some(thermal_gradient(35.0, 60.0, 75.0)),
            ),
            ItemConfig::new_v2(
                "m_spark_bg",
                "CPU:",
                MetricSource::CpuUsage,
                ItemVisual::TextSparkline {
                    placement: SparklinePlacement::Background,
                    style: SparklineStyle::Area,
                    window_sec: 30,
                    scale: SparklineScale::Fixed {
                        min: 0.0,
                        max: 100.0,
                    },
                    fill_alpha: 0.25,
                    stroke_width: 1.2,
                    graph_width: 36.0,
                },
                ColorRule::static_hex("#7FD37F"),
            ),
            ItemConfig::new_v2(
                "m_spark_inline_bars",
                "↓",
                MetricSource::NetDown {
                    adapter: AdapterTarget::Auto,
                },
                ItemVisual::TextSparkline {
                    placement: SparklinePlacement::InlineRight,
                    style: SparklineStyle::Bars,
                    window_sec: 30,
                    scale: SparklineScale::AutoDecay,
                    fill_alpha: 0.4,
                    stroke_width: 1.0,
                    graph_width: 32.0,
                },
                ColorRule::static_hex("#60CDFF"),
            ),
            ItemConfig::new_v2(
                "m_prog_bottom",
                "RAM:",
                MetricSource::MemoryUsage {
                    display_as: oxide_core::MemoryDisplayMode::Percent,
                },
                ItemVisual::TextProgress {
                    style: ProgressStyle::BarBottom,
                    thickness: 2.5,
                    track_alpha: 0.16,
                    bar_width: 24.0,
                },
                ColorRule::static_hex("#F0C674"),
            ),
            ItemConfig::new_v2(
                "m_prog_radial",
                "GPU:",
                MetricSource::GpuUsage {
                    target: oxide_core::GpuTarget::Discrete,
                },
                ItemVisual::TextProgress {
                    style: ProgressStyle::RadialRing,
                    thickness: 2.0,
                    track_alpha: 0.2,
                    bar_width: 16.0,
                },
                ColorRule::static_hex("#7FBFEF"),
            ),
            ItemConfig::new_v2(
                "m_graph_dual",
                "",
                MetricSource::NetCombined {
                    adapter: AdapterTarget::Auto,
                },
                ItemVisual::GraphOnly {
                    style: SparklineStyle::Line,
                    window_sec: 30,
                    scale: SparklineScale::AutoDecay,
                    fill_alpha: 0.2,
                    stroke_width: 1.25,
                    dashed_border: true,
                    secondary_color: ColorSpec::parse("#F0A050"),
                    width: 48.0,
                },
                ColorRule::static_hex("#4ADE80"),
            ),
        ];

        let frame = renderer
            .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 96, false, true)
            .unwrap()
            .unwrap();
        assert!(frame.width >= 200);
        assert_eq!(frame.height, 42);
        assert!(count_nonzero_alpha(frame.bgra_premultiplied) > 500);
    }
}
