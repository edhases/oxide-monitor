//! Tab 3: Widget Size (`SizeMode`, width/height, scale %, fit-to-taskbar, screen readout),
//! `LayoutConfig` (including groups), and `SurfaceConfig` editor.

use eframe::egui;
use oxide_core::{
    BackgroundMode, ColorSpec, ColumnSizing, FillOrder, GroupStyle, LayoutConfig,
    LayoutGroupConfig, LayoutMode, MonitorDescriptor, MonitorTarget, Rgba, SizeMode, WidthMode,
};

use crate::i18n::SettingsI18n;
use crate::state::SettingsState;
use crate::tabs::color_spec_editor;
use crate::theme::card_frame;
use crate::win32_host::BackdropKind;

/// Default width prefilled when switching from content-driven auto width to a fixed mode.
pub const DEFAULT_FIXED_WIDTH: i32 = 294;
/// Default height prefilled when turning off auto height (`height == 0`).
pub const DEFAULT_FIXED_HEIGHT: i32 = 35;

/// Derive the effective [`SizeMode`] shown in the segmented control.
///
/// When `layout.size_mode` is `None` (legacy config or fresh preset), follows the documented
/// compatibility rule:
/// - `WidthMode::Auto && !exact_physical_size` → `SizeMode::AutoContent`
/// - `WidthMode::Fixed(_) && exact_physical_size` → `SizeMode::ExactPhysical`
/// - otherwise → `SizeMode::FixedDip`
pub fn effective_size_mode(layout: &LayoutConfig) -> SizeMode {
    if let Some(mode) = layout.size_mode {
        return mode;
    }
    if matches!(layout.width_mode, WidthMode::Auto) && !layout.exact_physical_size {
        SizeMode::AutoContent
    } else if matches!(layout.width_mode, WidthMode::Fixed(_)) && layout.exact_physical_size {
        SizeMode::ExactPhysical
    } else {
        SizeMode::FixedDip
    }
}

/// Write an explicit [`SizeMode`] choice made by the user and keep `width_mode` /
/// `exact_physical_size` consistent (prefilling `294` when leaving `Auto`).
pub fn apply_size_mode(layout: &mut LayoutConfig, mode: SizeMode) {
    layout.size_mode = Some(mode);
    match mode {
        SizeMode::AutoContent => {
            layout.width_mode = WidthMode::Auto;
            layout.exact_physical_size = false;
        }
        SizeMode::FixedDip => {
            if matches!(layout.width_mode, WidthMode::Auto) {
                layout.width_mode = WidthMode::Fixed(DEFAULT_FIXED_WIDTH);
            }
            layout.exact_physical_size = false;
        }
        SizeMode::ExactPhysical => {
            if matches!(layout.width_mode, WidthMode::Auto) {
                layout.width_mode = WidthMode::Fixed(DEFAULT_FIXED_WIDTH);
            }
            layout.exact_physical_size = true;
        }
    }
}

/// Reset widget sizing fields back to factory defaults.
pub fn reset_widget_size(layout: &mut LayoutConfig) {
    let defaults = LayoutConfig::default();
    layout.size_mode = defaults.size_mode;
    layout.width_mode = defaults.width_mode;
    layout.height = defaults.height;
    layout.exact_physical_size = defaults.exact_physical_size;
    layout.scale_pct = defaults.scale_pct;
    layout.fit_to_taskbar = defaults.fit_to_taskbar;
}

/// Resolve the target monitor's DPI from `HelloAck.monitors`, falling back to the window DPI
/// in Standalone mode.
pub fn resolve_target_monitor_dpi(
    monitors: &[MonitorDescriptor],
    target: &MonitorTarget,
    fallback_dpi: u32,
) -> u32 {
    let fallback = fallback_dpi.clamp(48, 480);
    if monitors.is_empty() {
        return fallback;
    }
    if let MonitorTarget::Device { device, .. } = target
        && let Some(mon) = monitors
            .iter()
            .find(|m| m.device.eq_ignore_ascii_case(device) && m.dpi > 0)
    {
        return mon.dpi.clamp(48, 480);
    }
    monitors
        .iter()
        .find(|m| m.is_primary && m.dpi > 0)
        .or_else(|| monitors.iter().find(|m| m.dpi > 0))
        .map(|m| m.dpi.clamp(48, 480))
        .unwrap_or(fallback)
}

/// Format the screen pixel & effective scale readout (`"Екран: 441×53 px · 150 %"`), combining
/// `LivePreviewPanel::last_size_px` with the resolved monitor DPI from `HelloAck.monitors`.
pub fn format_screen_readout(
    i18n: &SettingsI18n,
    layout: &LayoutConfig,
    size_px: (u32, u32),
    monitor_dpi: u32,
) -> String {
    let dpi = monitor_dpi.clamp(48, 480);
    let dpi_pct = ((dpi as f32 / 96.0) * 100.0).round() as u32;
    let effective_dpi = layout.base_dpi(dpi, false) * layout.scale_factor();
    let effective_pct = ((effective_dpi / 96.0) * 100.0).round() as u32;
    let prefix = i18n.t("size-readout-screen");
    if dpi != 96 && effective_pct != dpi_pct {
        format!(
            "{prefix}: {}×{} px · {effective_pct} % (DPI {dpi_pct} %)",
            size_px.0, size_px.1
        )
    } else {
        format!(
            "{prefix}: {}×{} px · {effective_pct} %",
            size_px.0, size_px.1
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ui(
    ui: &mut egui::Ui,
    state: &mut SettingsState,
    i18n: &SettingsI18n,
    dark: bool,
    backdrop: BackdropKind,
    preview_size_px: (u32, u32),
    monitor_dpi: u32,
) -> bool {
    let mut changed = false;

    // 1. Widget Size (`size_mode`, width/height, scale %, fit-to-taskbar, screen readout)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.heading(i18n.t("section-widget-size"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(i18n.t("btn-size-reset")).clicked() {
                    reset_widget_size(&mut state.draft.layout);
                    changed = true;
                }
                if ui.button(i18n.t("btn-exact-294x35")).clicked() {
                    apply_size_mode(&mut state.draft.layout, SizeMode::ExactPhysical);
                    state.draft.layout.width_mode = WidthMode::Fixed(DEFAULT_FIXED_WIDTH);
                    state.draft.layout.height = DEFAULT_FIXED_HEIGHT;
                    state.draft.layout.scale_pct = 100;
                    state.draft.layout.rows = 2;
                    changed = true;
                }
            });
        });
        ui.add_space(6.0);

        let lay = &mut state.draft.layout;
        let current_mode = effective_size_mode(lay);
        let unit_suffix = if current_mode == SizeMode::ExactPhysical {
            " px"
        } else {
            " DIP"
        };

        egui::Grid::new("widget_size_grid")
            .num_columns(2)
            .spacing([18.0, 8.0])
            .min_col_width(220.0)
            .show(ui, |ui| {
                // 1) Size mode segmented control
                ui.label(i18n.t("field-size-mode"));
                ui.horizontal_wrapped(|ui| {
                    for (mode, key) in [
                        (SizeMode::AutoContent, "size-mode-auto"),
                        (SizeMode::FixedDip, "size-mode-fixed-dip"),
                        (SizeMode::ExactPhysical, "size-mode-exact-px"),
                    ] {
                        let selected = current_mode == mode;
                        if ui.selectable_label(selected, i18n.t(key)).clicked() && !selected {
                            apply_size_mode(lay, mode);
                            changed = true;
                        }
                    }
                });
                ui.end_row();

                // 2) Width drag 80..=2000 (disabled in AutoContent; prefills 294)
                ui.label(i18n.t("field-widget-width"));
                let width_enabled = current_mode != SizeMode::AutoContent;
                let mut width_val = match lay.width_mode {
                    WidthMode::Fixed(w) => w.clamp(80, 2000),
                    WidthMode::Auto => DEFAULT_FIXED_WIDTH,
                };
                if ui
                    .add_enabled(
                        width_enabled,
                        egui::DragValue::new(&mut width_val)
                            .range(80..=2000)
                            .speed(1.0)
                            .suffix(unit_suffix),
                    )
                    .changed()
                {
                    lay.width_mode = WidthMode::Fixed(width_val.clamp(80, 2000));
                    changed = true;
                }
                ui.end_row();

                // 3) Height drag 18..=400 + Auto toggle (height = 0)
                ui.label(i18n.t("field-widget-height"));
                ui.horizontal(|ui| {
                    let mut is_auto_h = lay.height == 0;
                    if ui
                        .checkbox(&mut is_auto_h, i18n.t("field-height-auto"))
                        .changed()
                    {
                        lay.height = if is_auto_h { 0 } else { DEFAULT_FIXED_HEIGHT };
                        changed = true;
                    }
                    let mut height_val = if lay.height == 0 {
                        DEFAULT_FIXED_HEIGHT
                    } else {
                        lay.height.clamp(18, 400)
                    };
                    if ui
                        .add_enabled(
                            !is_auto_h,
                            egui::DragValue::new(&mut height_val)
                                .range(18..=400)
                                .speed(1.0)
                                .suffix(unit_suffix),
                        )
                        .changed()
                    {
                        lay.height = height_val.clamp(18, 400);
                        changed = true;
                    }
                });
                ui.end_row();

                // 4) Scale slider 50..=300 (step 5) + quick buttons 75/100/125/150/200
                ui.label(i18n.t("field-scale-pct"));
                ui.vertical(|ui| {
                    if ui
                        .add(
                            egui::Slider::new(&mut lay.scale_pct, 50..=300)
                                .step_by(5.0)
                                .suffix("%"),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        let presets: [(u16, String); 5] = [
                            (75, i18n.t("scale-preset-compact")),
                            (100, i18n.t("scale-preset-normal")),
                            (125, "125%".to_string()),
                            (150, i18n.t("scale-preset-large")),
                            (200, i18n.t("scale-preset-tv")),
                        ];
                        for (pct, label) in presets {
                            let selected = lay.scale_pct == pct;
                            if ui.selectable_label(selected, label).clicked() && !selected {
                                lay.scale_pct = pct;
                                changed = true;
                            }
                        }
                    });
                });
                ui.end_row();

                // 5) Fit to taskbar checkbox + tooltip
                let fit_tip = i18n.t("tooltip-fit-to-taskbar");
                ui.label(i18n.t("field-fit-to-taskbar"))
                    .on_hover_text(&fit_tip);
                if ui
                    .checkbox(&mut lay.fit_to_taskbar, "")
                    .on_hover_text(&fit_tip)
                    .changed()
                {
                    changed = true;
                }
                ui.end_row();
            });

        // 6) Readout: "Екран: 441×53 px · 150 %"
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);
        let readout = format_screen_readout(i18n, lay, preview_size_px, monitor_dpi);
        ui.weak(readout);
    });

    ui.add_space(10.0);

    // 2. Layout (`LayoutConfig`)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-layout"));
        ui.add_space(6.0);

        let lay = &mut state.draft.layout;
        egui::Grid::new("layout_settings_grid")
            .num_columns(2)
            .spacing([18.0, 8.0])
            .min_col_width(220.0)
            .show(ui, |ui| {
                // Layout mode
                ui.label(i18n.t("field-layout-mode"));
                egui::ComboBox::from_id_salt("lay_mode")
                    .selected_text(format!("{:?}", lay.mode))
                    .show_ui(ui, |ui| {
                        for (val, name) in [
                            (LayoutMode::FlowRows, "Flow rows"),
                            (LayoutMode::Grid, "Grid"),
                            (LayoutMode::AbsoluteSkin, "Absolute skin"),
                        ] {
                            if ui.selectable_value(&mut lay.mode, val, name).changed() {
                                changed = true;
                            }
                        }
                    });
                ui.end_row();

                // Rows
                ui.label(i18n.t("field-layout-rows"));
                if ui.add(egui::Slider::new(&mut lay.rows, 1..=4)).changed() {
                    changed = true;
                }
                ui.end_row();

                // Fill order
                ui.label(i18n.t("field-fill-order"));
                egui::ComboBox::from_id_salt("lay_fill_order")
                    .selected_text(format!("{:?}", lay.fill_order))
                    .show_ui(ui, |ui| {
                        for (val, name) in [
                            (FillOrder::ColumnMajor, "Column-major (TrafficMonitor)"),
                            (FillOrder::RowMajor, "Row-major"),
                        ] {
                            if ui
                                .selectable_value(&mut lay.fill_order, val, name)
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
                ui.end_row();

                // Column sizing
                ui.label(i18n.t("field-col-sizing"));
                egui::ComboBox::from_id_salt("lay_col_sizing")
                    .selected_text(format!("{:?}", lay.column_sizing))
                    .show_ui(ui, |ui| {
                        for (val, name) in [
                            (ColumnSizing::FitContent, "Fit content"),
                            (ColumnSizing::Equal, "Equal"),
                        ] {
                            if ui
                                .selectable_value(&mut lay.column_sizing, val, name)
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
                ui.end_row();

                // Max width (auto mode)
                ui.label(i18n.t("field-max-width"));
                if ui
                    .add(egui::Slider::new(&mut lay.max_width, 120..=2000).suffix(" DIP"))
                    .changed()
                {
                    changed = true;
                }
                ui.end_row();

                // Gap X / Y
                ui.label(i18n.t("field-gap-x"));
                if ui.add(egui::Slider::new(&mut lay.gap_x, 0..=32)).changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-gap-y"));
                if ui.add(egui::Slider::new(&mut lay.gap_y, 0..=16)).changed() {
                    changed = true;
                }
                ui.end_row();

                // Vertical auto-adapt
                ui.label(i18n.t("field-vertical-adapt"));
                if ui.checkbox(&mut lay.vertical_auto_adapt, "").changed() {
                    changed = true;
                }
                ui.end_row();
            });

        // Layout groups
        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.strong(i18n.t("section-layout-groups"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(i18n.t("group-add")).clicked() {
                    let n = lay.groups.len() + 1;
                    lay.groups.push(LayoutGroupConfig {
                        id: format!("group_{n}"),
                        style: GroupStyle::Pill,
                        pill_bg: ColorSpec::Static(Rgba::new(255, 255, 255, 16)),
                        pill_radius: 5.0,
                        pill_padding_x: 5,
                    });
                    changed = true;
                }
            });
        });
        let mut remove_group = None;
        for (i, g) in lay.groups.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(egui::TextEdit::singleline(&mut g.id).desired_width(90.0))
                    .changed()
                {
                    changed = true;
                }
                egui::ComboBox::from_id_salt(format!("grp_style_{i}"))
                    .width(110.0)
                    .selected_text(format!("{:?}", g.style))
                    .show_ui(ui, |ui| {
                        for val in [
                            GroupStyle::None,
                            GroupStyle::Pill,
                            GroupStyle::SeparatorAfter,
                        ] {
                            if ui
                                .selectable_value(&mut g.style, val, format!("{val:?}"))
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
                if g.style == GroupStyle::Pill {
                    if color_spec_editor(ui, &format!("grp_bg_{i}"), &mut g.pill_bg) {
                        changed = true;
                    }
                    if ui
                        .add(
                            egui::DragValue::new(&mut g.pill_radius)
                                .range(0.0..=16.0)
                                .speed(0.5)
                                .prefix("r="),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    if ui
                        .add(
                            egui::DragValue::new(&mut g.pill_padding_x)
                                .range(0..=24)
                                .prefix("px="),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                }
                if ui.small_button("✕").clicked() {
                    remove_group = Some(i);
                }
            });
        }
        if let Some(rm) = remove_group {
            lay.groups.remove(rm);
            changed = true;
        }
    });

    ui.add_space(10.0);

    // 2. Surface (`SurfaceConfig`)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-surface"));
        ui.add_space(6.0);

        let surf = &mut state.draft.surface;
        egui::Grid::new("surface_settings_grid")
            .num_columns(2)
            .spacing([18.0, 8.0])
            .min_col_width(220.0)
            .show(ui, |ui| {
                // Background mode
                ui.label(i18n.t("field-bg-mode"));
                egui::ComboBox::from_id_salt("surf_bg_mode")
                    .selected_text(format!("{:?}", surf.background_mode))
                    .show_ui(ui, |ui| {
                        for (val, lbl) in [
                            (BackgroundMode::Transparent, "Transparent"),
                            (BackgroundMode::SolidAuto, "Solid auto"),
                            (BackgroundMode::Custom, "Custom color"),
                            (BackgroundMode::HdrSafe, "HDR-safe"),
                        ] {
                            if ui
                                .selectable_value(&mut surf.background_mode, val, lbl)
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
                ui.end_row();

                // Background color
                ui.label(i18n.t("field-bg-color"));
                if color_spec_editor(ui, "surf_bg_color", &mut surf.background_color) {
                    changed = true;
                }
                ui.end_row();

                // Opacity
                ui.label(i18n.t("field-opacity"));
                if ui
                    .add(egui::Slider::new(&mut surf.opacity, 30..=255))
                    .changed()
                {
                    changed = true;
                }
                ui.end_row();

                // Corner radius
                ui.label(i18n.t("field-corner-radius"));
                if ui
                    .add(egui::Slider::new(&mut surf.corner_radius, 0.0..=20.0).suffix(" DIP"))
                    .changed()
                {
                    changed = true;
                }
                ui.end_row();

                // Border width
                ui.label(i18n.t("field-border-width"));
                if ui
                    .add(egui::Slider::new(&mut surf.border_width, 0.0..=3.0).suffix(" DIP"))
                    .changed()
                {
                    changed = true;
                }
                ui.end_row();

                // Border color
                ui.label(i18n.t("field-border-color"));
                if color_spec_editor(ui, "surf_border_color", &mut surf.border_color) {
                    changed = true;
                }
                ui.end_row();

                // Text shadow
                ui.label(i18n.t("field-text-shadow"));
                if ui.checkbox(&mut surf.text_shadow, "").changed() {
                    changed = true;
                }
                ui.end_row();

                // Padding (top/right/bottom/left)
                ui.label(i18n.t("field-padding"));
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::DragValue::new(&mut surf.padding.top)
                                .range(0..=32)
                                .prefix("T:"),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    if ui
                        .add(
                            egui::DragValue::new(&mut surf.padding.right)
                                .range(0..=32)
                                .prefix("R:"),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    if ui
                        .add(
                            egui::DragValue::new(&mut surf.padding.bottom)
                                .range(0..=32)
                                .prefix("B:"),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    if ui
                        .add(
                            egui::DragValue::new(&mut surf.padding.left)
                                .range(0..=32)
                                .prefix("L:"),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                });
                ui.end_row();

                // Cell padding (x / y)
                ui.label(i18n.t("field-cell-padding"));
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::DragValue::new(&mut surf.cell_padding.x)
                                .range(0..=24)
                                .prefix("X:"),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    if ui
                        .add(
                            egui::DragValue::new(&mut surf.cell_padding.y)
                                .range(0..=16)
                                .prefix("Y:"),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                });
                ui.end_row();

                // Background image (optional)
                ui.label(i18n.t("field-bg-image"));
                let mut bg_img = surf.background_image.clone().unwrap_or_default();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut bg_img)
                            .hint_text("none")
                            .desired_width(240.0),
                    )
                    .changed()
                {
                    let trimmed = bg_img.trim().to_string();
                    surf.background_image = (!trimmed.is_empty()).then_some(trimmed);
                    changed = true;
                }
                ui.end_row();
            });
    });

    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::LivePreviewPanel;
    use oxide_core::Config;
    use std::path::PathBuf;

    #[test]
    fn effective_size_mode_derives_from_legacy_fields_when_absent() {
        let mut lay = LayoutConfig::default();
        assert_eq!(lay.size_mode, None);

        // Auto width && !exact -> AutoContent
        lay.width_mode = WidthMode::Auto;
        lay.exact_physical_size = false;
        assert_eq!(effective_size_mode(&lay), SizeMode::AutoContent);

        // Fixed width && exact -> ExactPhysical
        lay.width_mode = WidthMode::Fixed(294);
        lay.exact_physical_size = true;
        assert_eq!(effective_size_mode(&lay), SizeMode::ExactPhysical);

        // Fixed width && !exact -> FixedDip
        lay.width_mode = WidthMode::Fixed(320);
        lay.exact_physical_size = false;
        assert_eq!(effective_size_mode(&lay), SizeMode::FixedDip);

        // Auto width && exact -> FixedDip (else branch)
        lay.width_mode = WidthMode::Auto;
        lay.exact_physical_size = true;
        assert_eq!(effective_size_mode(&lay), SizeMode::FixedDip);

        // Explicit size_mode always wins
        lay.size_mode = Some(SizeMode::AutoContent);
        lay.width_mode = WidthMode::Fixed(300);
        lay.exact_physical_size = true;
        assert_eq!(effective_size_mode(&lay), SizeMode::AutoContent);
    }

    #[test]
    fn apply_size_mode_writes_explicit_mode_and_prefills_294() {
        let mut lay = LayoutConfig::default();
        lay.size_mode = None;
        lay.width_mode = WidthMode::Auto;
        lay.exact_physical_size = false;

        // Leaving Auto -> FixedDip writes explicit mode and prefills 294
        apply_size_mode(&mut lay, SizeMode::FixedDip);
        assert_eq!(lay.size_mode, Some(SizeMode::FixedDip));
        assert_eq!(lay.width_mode, WidthMode::Fixed(294));
        assert!(!lay.exact_physical_size);

        // Preserves custom width when switching FixedDip -> ExactPhysical
        lay.width_mode = WidthMode::Fixed(420);
        apply_size_mode(&mut lay, SizeMode::ExactPhysical);
        assert_eq!(lay.size_mode, Some(SizeMode::ExactPhysical));
        assert_eq!(lay.width_mode, WidthMode::Fixed(420));
        assert!(lay.exact_physical_size);

        // Switching to AutoContent sets WidthMode::Auto and clears exact_physical_size
        apply_size_mode(&mut lay, SizeMode::AutoContent);
        assert_eq!(lay.size_mode, Some(SizeMode::AutoContent));
        assert_eq!(lay.width_mode, WidthMode::Auto);
        assert!(!lay.exact_physical_size);

        // Switching back to ExactPhysical prefills 294 again
        apply_size_mode(&mut lay, SizeMode::ExactPhysical);
        assert_eq!(lay.size_mode, Some(SizeMode::ExactPhysical));
        assert_eq!(lay.width_mode, WidthMode::Fixed(294));
        assert!(lay.exact_physical_size);

        // Reset restores defaults
        lay.scale_pct = 200;
        lay.fit_to_taskbar = false;
        lay.height = 0;
        reset_widget_size(&mut lay);
        assert_eq!(lay.size_mode, None);
        assert_eq!(lay.width_mode, WidthMode::Fixed(294));
        assert_eq!(lay.height, 35);
        assert!(lay.exact_physical_size);
        assert_eq!(lay.scale_pct, 100);
        assert!(lay.fit_to_taskbar);
    }

    #[test]
    fn target_monitor_dpi_and_screen_readout_formatting() {
        let monitors = vec![
            MonitorDescriptor {
                device: r"\\.\DISPLAY1".into(),
                friendly_name: "Primary".into(),
                is_primary: true,
                dpi: 96,
                width: 1920,
                height: 1080,
            },
            MonitorDescriptor {
                device: r"\\.\DISPLAY2".into(),
                friendly_name: "4K TV".into(),
                is_primary: false,
                dpi: 144,
                width: 3840,
                height: 2160,
            },
        ];

        assert_eq!(
            resolve_target_monitor_dpi(&monitors, &MonitorTarget::Primary, 120),
            96
        );
        assert_eq!(
            resolve_target_monitor_dpi(
                &monitors,
                &MonitorTarget::Device {
                    device: r"\\.\DISPLAY2".into(),
                    name: "4K TV".into(),
                },
                96
            ),
            144
        );
        assert_eq!(
            resolve_target_monitor_dpi(&[], &MonitorTarget::Primary, 120),
            120
        );

        let uk = SettingsI18n::new("uk");
        let en = SettingsI18n::new("en");
        let ru = SettingsI18n::new("ru");

        // ExactPhysical 294x35 at 150% scale on 96 DPI -> "Екран: 441×53 px · 150 %"
        let mut lay = LayoutConfig::default();
        lay.scale_pct = 150;
        assert_eq!(
            format_screen_readout(&uk, &lay, (441, 53), 96),
            "Екран: 441×53 px · 150 %"
        );
        assert_eq!(
            format_screen_readout(&en, &lay, (441, 53), 96),
            "Screen: 441×53 px · 150 %"
        );
        assert_eq!(
            format_screen_readout(&ru, &lay, (441, 53), 96),
            "Экран: 441×53 px · 150 %"
        );

        // FixedDip at 100% scale on 144 DPI (150%) -> "Екран: 441×53 px · 150 %"
        apply_size_mode(&mut lay, SizeMode::FixedDip);
        lay.scale_pct = 100;
        assert_eq!(
            format_screen_readout(&uk, &lay, (441, 53), 144),
            "Екран: 441×53 px · 150 %"
        );

        // ExactPhysical at 100% scale on 144 DPI -> shows both effective 100% and monitor DPI 150%
        apply_size_mode(&mut lay, SizeMode::ExactPhysical);
        lay.scale_pct = 100;
        assert_eq!(
            format_screen_readout(&uk, &lay, (294, 35), 144),
            "Екран: 294×35 px · 100 % (DPI 150 %)"
        );
    }

    #[test]
    fn headless_egui_layout_surface_and_preview_update_for_scale_and_dpi() {
        let mut state = SettingsState::load_local(PathBuf::from("nonexistent_test_config.toml"));
        state.draft = Config::default();
        state.monitors = vec![MonitorDescriptor {
            device: r"\\.\DISPLAY1".into(),
            friendly_name: "Primary".into(),
            is_primary: true,
            dpi: 96,
            width: 1920,
            height: 1080,
        }];
        let mut preview = LivePreviewPanel::new();
        let i18n = SettingsI18n::new("uk");
        let ctx = egui::Context::default();

        // Frame 1: classic 294x35 at 100% -> 294x35 px
        let dpi = resolve_target_monitor_dpi(&state.monitors, &state.draft.taskbar.monitor, 96);
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                preview.ui(
                    ui,
                    &i18n,
                    &state.draft,
                    &state.snapshot,
                    &state.history,
                    false,
                    dpi,
                );
                let sz = preview.last_size_px();
                let changed = super::ui(
                    ui,
                    &mut state,
                    &i18n,
                    true,
                    BackdropKind::SolidFallback,
                    sz,
                    dpi,
                );
                assert!(
                    !changed,
                    "rendering without user input must not dirty draft"
                );
            });
        });
        out.textures_delta.clear();
        assert_eq!(preview.last_size_px(), (294, 35));

        // Frame 2: scale_pct = 150 -> LivePreviewPanel updates to 441x53 px
        state.draft.layout.scale_pct = 150;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                preview.ui(
                    ui,
                    &i18n,
                    &state.draft,
                    &state.snapshot,
                    &state.history,
                    false,
                    dpi,
                );
                let sz = preview.last_size_px();
                let changed = super::ui(
                    ui,
                    &mut state,
                    &i18n,
                    true,
                    BackdropKind::SolidFallback,
                    sz,
                    dpi,
                );
                assert!(!changed);
            });
        });
        out.textures_delta.clear();
        assert_eq!(preview.last_size_px(), (441, 53));
        assert_eq!(
            format_screen_readout(&i18n, &state.draft.layout, preview.last_size_px(), dpi),
            "Екран: 441×53 px · 150 %"
        );

        // Frame 3: FixedDip on 144 DPI monitor at 100% scale -> 441x53 px
        state.monitors[0].dpi = 144;
        apply_size_mode(&mut state.draft.layout, SizeMode::FixedDip);
        state.draft.layout.scale_pct = 100;
        let dpi_150 = resolve_target_monitor_dpi(&state.monitors, &state.draft.taskbar.monitor, 96);
        assert_eq!(dpi_150, 144);
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                preview.ui(
                    ui,
                    &i18n,
                    &state.draft,
                    &state.snapshot,
                    &state.history,
                    false,
                    dpi_150,
                );
                let sz = preview.last_size_px();
                let _ = super::ui(
                    ui,
                    &mut state,
                    &i18n,
                    false,
                    BackdropKind::SolidFallback,
                    sz,
                    dpi_150,
                );
            });
        });
        out.textures_delta.clear();
        assert_eq!(preview.last_size_px(), (441, 53));

        // Frame 4: AutoContent with height = 0 (Auto) and TV 200% scale renders cleanly
        apply_size_mode(&mut state.draft.layout, SizeMode::AutoContent);
        state.draft.layout.height = 0;
        state.draft.layout.scale_pct = 200;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                preview.ui(
                    ui,
                    &i18n,
                    &state.draft,
                    &state.snapshot,
                    &state.history,
                    false,
                    96,
                );
                let sz = preview.last_size_px();
                let _ = super::ui(
                    ui,
                    &mut state,
                    &i18n,
                    true,
                    BackdropKind::SolidFallback,
                    sz,
                    96,
                );
            });
        });
        out.textures_delta.clear();
        assert!(preview.last_size_px().0 > 400);
        assert!(preview.last_size_px().1 > 50);
    }
}
