//! Pixel-accurate Live Preview widget using the shared `oxide_render::WidgetRenderer`.
//!
//! Renders the exact Direct2D/DirectWrite BGRA8 DIB used on the real taskbar and displays it
//! inside `egui` via `TextureOptions::NEAREST` over a toggleable dark/light taskbar strip.

use eframe::egui::{
    self, Color32, ColorImage, CornerRadius, Stroke, TextureHandle, TextureOptions,
};
use oxide_core::{
    BackgroundMode, ColumnSizing, Config, ExtraTelemetry, FillOrder, HistoryStore, LayoutMode,
    SizeMode, Snapshot, WidthMode,
};
use oxide_render::{SystemThemeContext, WidgetRenderer, bgra_premultiplied_to_rgba};

use crate::i18n::SettingsI18n;
use crate::state::sample_telemetry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PreviewSizeKey {
    mode: LayoutMode,
    fill_order: FillOrder,
    column_sizing: ColumnSizing,
    size_mode: Option<SizeMode>,
    width_mode: WidthMode,
    max_width: i32,
    height: i32,
    exact_physical_size: bool,
    scale_pct: u16,
    fit_to_taskbar: bool,
    background_mode: BackgroundMode,
    dpi: u32,
}

impl PreviewSizeKey {
    fn from_config(config: &Config, dpi: u32) -> Self {
        Self {
            mode: config.layout.mode,
            fill_order: config.layout.fill_order,
            column_sizing: config.layout.column_sizing,
            size_mode: config.layout.size_mode,
            width_mode: config.layout.width_mode,
            max_width: config.layout.max_width,
            height: config.layout.height,
            exact_physical_size: config.layout.exact_physical_size,
            scale_pct: config.layout.scale_pct,
            fit_to_taskbar: config.layout.fit_to_taskbar,
            background_mode: config.surface.background_mode,
            dpi,
        }
    }
}

pub struct LivePreviewPanel {
    renderer: Option<WidgetRenderer>,
    texture: Option<TextureHandle>,
    last_size_px: (u32, u32),
    last_size_key: Option<PreviewSizeKey>,
    pub dark_taskbar_bg: bool,
    pub use_simulator: bool,
    pub stress_level: f32,
    pub zoom_pct: u32,
}

impl LivePreviewPanel {
    pub fn new() -> Self {
        let renderer = WidgetRenderer::new(294, 35).ok();
        Self {
            renderer,
            texture: None,
            last_size_px: (294, 35),
            last_size_key: None,
            dark_taskbar_bg: true,
            use_simulator: false,
            stress_level: 0.35,
            zoom_pct: 150,
        }
    }

    pub fn last_size_px(&self) -> (u32, u32) {
        self.last_size_px
    }

    pub fn invalidate(&mut self) {
        self.last_size_key = None;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.invalidate();
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        i18n: &SettingsI18n,
        config: &Config,
        live_snap: &Snapshot,
        live_hist: &HistoryStore,
        has_live: bool,
        monitor_dpi: u32,
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.strong(i18n.t("preview-title"));
            ui.separator();

            // Dark / Light taskbar backdrop switcher
            if ui
                .selectable_label(self.dark_taskbar_bg, i18n.t("preview-bg-dark"))
                .clicked()
            {
                self.dark_taskbar_bg = true;
            }
            if ui
                .selectable_label(!self.dark_taskbar_bg, i18n.t("preview-bg-light"))
                .clicked()
            {
                self.dark_taskbar_bg = false;
            }

            ui.separator();

            // Live data vs Stress Simulator toggle
            if has_live {
                if ui
                    .selectable_label(!self.use_simulator, i18n.t("preview-live"))
                    .clicked()
                {
                    self.use_simulator = false;
                }
                if ui
                    .selectable_label(self.use_simulator, i18n.t("preview-sim"))
                    .clicked()
                {
                    self.use_simulator = true;
                }
            }

            if self.use_simulator || !has_live {
                ui.label(i18n.t("preview-stress"));
                let mut pct = (self.stress_level * 100.0).round() as u32;
                if ui
                    .add(egui::Slider::new(&mut pct, 0..=100).suffix("%"))
                    .changed()
                {
                    self.stress_level = pct as f32 / 100.0;
                }
            }

            ui.separator();
            ui.label(i18n.t("preview-scale"));
            for z in [100u32, 150, 200] {
                if ui
                    .selectable_label(self.zoom_pct == z, format!("{z}%"))
                    .clicked()
                {
                    self.zoom_pct = z;
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!(
                    "{}: {}×{} px",
                    i18n.t("preview-size"),
                    self.last_size_px.0,
                    self.last_size_px.1
                ));
            });
        });

        ui.add_space(6.0);

        let sim_pair = if self.use_simulator || !has_live {
            Some(sample_telemetry(self.stress_level))
        } else {
            None
        };
        let (snap_ref, hist_ref) = match &sim_pair {
            Some((s, h)) => (s, h),
            None => (live_snap, live_hist),
        };

        let mut theme_ctx = SystemThemeContext::default();
        theme_ctx.system_is_light = !self.dark_taskbar_bg;
        let dpi = monitor_dpi.clamp(48, 480);
        let size_key = PreviewSizeKey::from_config(config, dpi);
        let force = self.last_size_key != Some(size_key);
        let extra = ExtraTelemetry::default();

        if let Some(renderer) = self.renderer.as_mut() {
            if let Ok(Some(frame)) = renderer.render_if_dirty(
                config, snap_ref, hist_ref, &theme_ctx, &extra, dpi, false, force,
            ) {
                let (fw, fh) = (frame.width, frame.height);
                self.last_size_px = (fw, fh);
                self.last_size_key = Some(size_key);
                let rgba = bgra_premultiplied_to_rgba(renderer.dib().pixels());
                let image = unpremultiply_to_color_image([fw as usize, fh as usize], &rgba);
                match &mut self.texture {
                    Some(tex) => tex.set(image, TextureOptions::NEAREST),
                    None => {
                        self.texture = Some(ui.ctx().load_texture(
                            "oxide_live_preview",
                            image,
                            TextureOptions::NEAREST,
                        ));
                    }
                }
            }
        }

        // Simulated Windows 11 taskbar strip
        let strip_bg = if self.dark_taskbar_bg {
            Color32::from_rgb(28, 30, 36)
        } else {
            Color32::from_rgb(235, 238, 244)
        };
        let strip_border = if self.dark_taskbar_bg {
            Color32::from_rgba_unmultiplied(255, 255, 255, 26)
        } else {
            Color32::from_rgba_unmultiplied(0, 0, 0, 26)
        };

        // Compute bounded preview dimensions so Live Preview NEVER starves the tab ScrollArea below.
        let ppp = ui.ctx().pixels_per_point().max(1.0);
        let scale = self.zoom_pct as f32 / 100.0;
        let raw_w = (self.last_size_px.0 as f32 / ppp) * scale;
        let raw_h = (self.last_size_px.1 as f32 / ppp) * scale;
        let max_preview_h = 104.0f32;
        let max_preview_w = (ui.available_width() - 28.0).max(120.0);
        let fit_scale = (max_preview_h / raw_h.max(1.0))
            .min(max_preview_w / raw_w.max(1.0))
            .min(1.0);
        let draw_size = egui::vec2(raw_w * fit_scale, raw_h * fit_scale);
        let strip_h = draw_size.y.clamp(40.0, max_preview_h);

        egui::Frame::NONE
            .fill(strip_bg)
            .stroke(Stroke::new(1.0, strip_border))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(14, 8))
            .show(ui, |ui| {
                let avail_w = ui.available_width();
                ui.allocate_ui_with_layout(
                    egui::vec2(avail_w, strip_h),
                    egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                    |ui| {
                        if let Some(tex) = &self.texture {
                            ui.image((tex.id(), draw_size));
                        }
                    },
                );
            });
    }
}

/// Convert premultiplied RGBA8 bytes (from D2D DIB) to `egui::ColorImage`.
fn unpremultiply_to_color_image(size: [usize; 2], premul_rgba: &[u8]) -> ColorImage {
    let mut pixels = Vec::with_capacity(size[0] * size[1]);
    for px in premul_rgba.chunks_exact(4) {
        pixels.push(Color32::from_rgba_premultiplied(px[0], px[1], px[2], px[3]));
    }
    ColorImage {
        size,
        source_size: egui::vec2(size[0] as f32, size[1] as f32),
        pixels,
    }
}
