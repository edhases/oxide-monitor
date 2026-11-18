//! Windows 11 Fluent styling and system Segoe UI Variable font loader for `egui 0.37`.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Stroke, Visuals};

use crate::win32_host::BackdropKind;

/// Load `Segoe UI Variable` (Win11) or `Segoe UI` (Win10) from `%WINDIR%\Fonts`
/// so Ukrainian/Cyrillic and Win11 typography render crisply.
pub fn load_windows_ui_font(ctx: &egui::Context) {
    let win_dir = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let candidates = [
        win_dir.join(r"Fonts\SegUIVar.ttf"),
        win_dir.join(r"Fonts\segoeui.ttf"),
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(&path) {
            let mut defs = egui::FontDefinitions::default();
            defs.font_data.insert(
                "segoe_ui".into(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            if let Some(family) = defs.families.get_mut(&egui::FontFamily::Proportional) {
                family.insert(0, "segoe_ui".into());
            }
            ctx.set_fonts(defs);
            return;
        }
    }
}

/// Apply Win11 Fluent theme visuals (dark or light), adapting panel transparency to Mica Alt.
pub fn apply_fluent_theme(ctx: &egui::Context, dark: bool, backdrop: BackdropKind) {
    let mut visuals = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };

    let accent = if dark {
        Color32::from_rgb(96, 205, 255)
    } else {
        Color32::from_rgb(0, 103, 192)
    };

    let border_subtle = if dark {
        Color32::from_rgba_unmultiplied(255, 255, 255, 22)
    } else {
        Color32::from_rgba_unmultiplied(0, 0, 0, 24)
    };

    let card_fill = if dark {
        if backdrop.is_translucent() {
            Color32::from_rgba_unmultiplied(38, 41, 49, 205)
        } else {
            Color32::from_rgb(38, 41, 49)
        }
    } else if backdrop.is_translucent() {
        Color32::from_rgba_unmultiplied(255, 255, 255, 220)
    } else {
        Color32::from_rgb(252, 252, 254)
    };

    let panel_fill = if backdrop.is_translucent() {
        if dark {
            Color32::from_rgba_unmultiplied(24, 26, 32, 175)
        } else {
            Color32::from_rgba_unmultiplied(243, 245, 249, 185)
        }
    } else if dark {
        Color32::from_rgb(28, 30, 36)
    } else {
        Color32::from_rgb(243, 245, 249)
    };

    visuals.panel_fill = panel_fill;
    visuals.window_fill = card_fill;
    visuals.faint_bg_color = card_fill;
    visuals.extreme_bg_color = if dark {
        Color32::from_rgb(22, 24, 29)
    } else {
        Color32::from_rgb(236, 239, 244)
    };
    visuals.selection.bg_fill = accent.linear_multiply(if dark { 0.30 } else { 0.22 });
    visuals.selection.stroke = Stroke::new(1.0, accent);
    visuals.hyperlink_color = accent;

    let r = CornerRadius::same(6);
    visuals.widgets.noninteractive.corner_radius = r;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border_subtle);
    visuals.widgets.inactive.corner_radius = r;
    visuals.widgets.inactive.bg_fill = if dark {
        Color32::from_rgba_unmultiplied(52, 56, 66, 220)
    } else {
        Color32::from_rgba_unmultiplied(235, 238, 245, 235)
    };
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, border_subtle);
    visuals.widgets.hovered.corner_radius = r;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, accent);
    visuals.widgets.active.corner_radius = r;
    visuals.widgets.active.bg_stroke = Stroke::new(1.5, accent);

    let theme_kind = if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_theme(theme_kind);
    ctx.set_visuals_of(theme_kind, visuals);

    ctx.style_mut_of(theme_kind, |style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 5.0);
        style.spacing.slider_width = 160.0;
    });
}

/// Reusable Fluent card container frame.
pub fn card_frame(dark: bool, backdrop: BackdropKind) -> egui::Frame {
    let fill = if dark {
        if backdrop.is_translucent() {
            Color32::from_rgba_unmultiplied(38, 41, 49, 200)
        } else {
            Color32::from_rgb(38, 41, 49)
        }
    } else if backdrop.is_translucent() {
        Color32::from_rgba_unmultiplied(255, 255, 255, 225)
    } else {
        Color32::from_rgb(255, 255, 255)
    };
    let stroke = if dark {
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 20))
    } else {
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 0, 0, 22))
    };
    egui::Frame::NONE
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(12))
}
