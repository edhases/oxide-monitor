//! Modular settings tabs covering all sections of `Config` v2.

pub mod flyout_gamemode;
pub mod general_presets;
pub mod items_aliases;
pub mod layout_surface;
pub mod sources;
pub mod taskbar_interactions;
pub mod typography_format;

use eframe::egui::{self, Color32};
use oxide_core::{ColorSpec, Rgba};

/// Inline RGBA color picker + `#RRGGBBAA` display for `oxide_core::Rgba`.
pub fn rgba_picker(ui: &mut egui::Ui, rgba: &mut Rgba) -> bool {
    let mut c = Color32::from_rgba_unmultiplied(rgba.r, rgba.g, rgba.b, rgba.a);
    let resp = ui.color_edit_button_srgba(&mut c);
    if resp.changed() {
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        *rgba = Rgba::new(r, g, b, a);
        return true;
    }
    false
}

/// Editor for `oxide_core::ColorSpec` (static RGBA or semantic theme token).
pub fn color_spec_editor(ui: &mut egui::Ui, id_salt: &str, spec: &mut ColorSpec) -> bool {
    let mut changed = false;
    let is_static = matches!(spec, ColorSpec::Static(_));
    ui.horizontal(|ui| {
        let label = match spec {
            ColorSpec::Static(_) => "RGBA",
            ColorSpec::ThemeBackground => "theme.background",
            ColorSpec::ThemeLabel => "theme.label",
            ColorSpec::ThemeValue => "theme.value",
            ColorSpec::ThemeUnit => "theme.unit",
            ColorSpec::ThemeAccent => "theme.accent",
            ColorSpec::ThemeBorder => "theme.border",
            ColorSpec::ThemeMuted => "theme.muted",
        };
        egui::ComboBox::from_id_salt(id_salt)
            .width(140.0)
            .selected_text(label)
            .show_ui(ui, |ui| {
                if ui.selectable_label(is_static, "RGBA").clicked() && !is_static {
                    *spec = ColorSpec::Static(Rgba::rgb(0xE6, 0xE6, 0xE6));
                    changed = true;
                }
                for (val, name) in [
                    (ColorSpec::ThemeValue, "theme.value"),
                    (ColorSpec::ThemeLabel, "theme.label"),
                    (ColorSpec::ThemeUnit, "theme.unit"),
                    (ColorSpec::ThemeAccent, "theme.accent"),
                    (ColorSpec::ThemeBorder, "theme.border"),
                    (ColorSpec::ThemeMuted, "theme.muted"),
                    (ColorSpec::ThemeBackground, "theme.background"),
                ] {
                    if ui.selectable_value(spec, val, name).changed() {
                        changed = true;
                    }
                }
            });

        if let ColorSpec::Static(rgba) = spec {
            if rgba_picker(ui, rgba) {
                changed = true;
            }
            ui.weak(rgba.to_hex());
        }
    });
    changed
}
