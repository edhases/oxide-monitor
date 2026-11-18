//! Tab 4: Complete `TypographyConfig` (`label`/`value`/`unit` FontSpecs) + `FormatOptions` editor.

use eframe::egui;
use oxide_core::{DecimalSeparator, FontSpec, SpeedUnit, TempUnit, ValueAlign};

use crate::i18n::SettingsI18n;
use crate::state::SettingsState;
use crate::tabs::color_spec_editor;
use crate::theme::card_frame;
use crate::win32_host::BackdropKind;

const COMMON_WIN_FONTS: [&str; 6] = [
    "Segoe UI Variable Small",
    "Segoe UI",
    "Cascadia Mono",
    "Consolas",
    "Bahnschrift",
    "JetBrains Mono",
];

pub fn ui(
    ui: &mut egui::Ui,
    state: &mut SettingsState,
    i18n: &SettingsI18n,
    dark: bool,
    backdrop: BackdropKind,
) -> bool {
    let mut changed = false;

    // 1. Typography (`TypographyConfig`)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-typography"));
        ui.add_space(6.0);

        let typo = &mut state.draft.typography;
        egui::Grid::new("typo_global_grid")
            .num_columns(2)
            .spacing([18.0, 8.0])
            .min_col_width(240.0)
            .show(ui, |ui| {
                ui.label(i18n.t("field-tabular-nums"));
                if ui.checkbox(&mut typo.tabular_nums, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-color-emoji"));
                if ui.checkbox(&mut typo.color_emoji, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-value-align"));
                egui::ComboBox::from_id_salt("typo_val_align")
                    .selected_text(format!("{:?}", typo.value_align))
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(&mut typo.value_align, ValueAlign::Right, "Right")
                            .changed()
                        {
                            changed = true;
                        }
                        if ui
                            .selectable_value(&mut typo.value_align, ValueAlign::Left, "Left")
                            .changed()
                        {
                            changed = true;
                        }
                    });
                ui.end_row();
            });

        ui.add_space(8.0);
        if font_spec_section(
            ui,
            "font_label",
            &i18n.t("font-section-label"),
            &mut typo.label,
            i18n,
        ) {
            changed = true;
        }
        ui.add_space(6.0);
        if font_spec_section(
            ui,
            "font_value",
            &i18n.t("font-section-value"),
            &mut typo.value,
            i18n,
        ) {
            changed = true;
        }
        ui.add_space(6.0);
        if font_spec_section(
            ui,
            "font_unit",
            &i18n.t("font-section-unit"),
            &mut typo.unit,
            i18n,
        ) {
            changed = true;
        }
    });

    ui.add_space(10.0);

    // 2. Format Options (`FormatOptions`)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-format"));
        ui.add_space(6.0);

        let fmt = &mut state.draft.format;
        egui::Grid::new("format_settings_grid")
            .num_columns(2)
            .spacing([18.0, 8.0])
            .min_col_width(240.0)
            .show(ui, |ui| {
                ui.label(i18n.t("field-unit-byte"));
                if ui.checkbox(&mut fmt.unit_byte, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-speed-unit"));
                egui::ComboBox::from_id_salt("fmt_speed_unit")
                    .selected_text(format!("{:?}", fmt.speed_unit))
                    .show_ui(ui, |ui| {
                        for (val, lbl) in [
                            (SpeedUnit::Auto, "Auto"),
                            (SpeedUnit::Kbps, "Kb/s / KB/s"),
                            (SpeedUnit::Mbps, "Mb/s / MB/s"),
                        ] {
                            if ui.selectable_value(&mut fmt.speed_unit, val, lbl).changed() {
                                changed = true;
                            }
                        }
                    });
                ui.end_row();

                ui.label(i18n.t("field-digits"));
                if ui.add(egui::Slider::new(&mut fmt.digits, 1..=8)).changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-hide-unit"));
                if ui.checkbox(&mut fmt.hide_unit, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-hide-percent"));
                if ui.checkbox(&mut fmt.hide_percent, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-separate-space"));
                if ui.checkbox(&mut fmt.separate_space, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-short-mode"));
                if ui.checkbox(&mut fmt.short_mode, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-decimal-sep"));
                egui::ComboBox::from_id_salt("fmt_decimal_sep")
                    .selected_text(match fmt.decimal_separator {
                        DecimalSeparator::Dot => "Dot (.)",
                        DecimalSeparator::Comma => "Comma (,)",
                    })
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(
                                &mut fmt.decimal_separator,
                                DecimalSeparator::Dot,
                                "Dot (.)",
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        if ui
                            .selectable_value(
                                &mut fmt.decimal_separator,
                                DecimalSeparator::Comma,
                                "Comma (,)",
                            )
                            .changed()
                        {
                            changed = true;
                        }
                    });
                ui.end_row();

                ui.label(i18n.t("field-temp-unit"));
                egui::ComboBox::from_id_salt("fmt_temp_unit")
                    .selected_text(match fmt.temp_unit {
                        TempUnit::Celsius => "°C (Celsius)",
                        TempUnit::Fahrenheit => "°F (Fahrenheit)",
                    })
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(&mut fmt.temp_unit, TempUnit::Celsius, "°C (Celsius)")
                            .changed()
                        {
                            changed = true;
                        }
                        if ui
                            .selectable_value(
                                &mut fmt.temp_unit,
                                TempUnit::Fahrenheit,
                                "°F (Fahrenheit)",
                            )
                            .changed()
                        {
                            changed = true;
                        }
                    });
                ui.end_row();
            });
    });

    changed
}

fn font_spec_section(
    ui: &mut egui::Ui,
    id: &str,
    title: &str,
    spec: &mut FontSpec,
    i18n: &SettingsI18n,
) -> bool {
    let mut changed = false;
    ui.separator();
    ui.strong(title);
    ui.add_space(4.0);
    egui::Grid::new(format!("{id}_grid"))
        .num_columns(2)
        .spacing([18.0, 6.0])
        .min_col_width(240.0)
        .show(ui, |ui| {
            ui.label(i18n.t("field-font-family"));
            ui.horizontal(|ui| {
                if ui
                    .add(egui::TextEdit::singleline(&mut spec.family).desired_width(170.0))
                    .changed()
                {
                    changed = true;
                }
                egui::ComboBox::from_id_salt(format!("{id}_fam_pick"))
                    .width(24.0)
                    .selected_text("")
                    .show_ui(ui, |ui| {
                        for fam in COMMON_WIN_FONTS {
                            if ui
                                .selectable_value(&mut spec.family, fam.to_string(), fam)
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
            });
            ui.end_row();

            ui.label(i18n.t("field-font-size"));
            if ui
                .add(
                    egui::Slider::new(&mut spec.size_pt, 6.0..=28.0)
                        .step_by(0.5)
                        .suffix(" pt"),
                )
                .changed()
            {
                changed = true;
            }
            ui.end_row();

            ui.label(i18n.t("field-font-weight"));
            if ui
                .add(egui::Slider::new(&mut spec.weight, 100..=900).step_by(100.0))
                .changed()
            {
                changed = true;
            }
            ui.end_row();

            ui.label(i18n.t("field-font-italic"));
            if ui.checkbox(&mut spec.italic, "").changed() {
                changed = true;
            }
            ui.end_row();

            ui.label(i18n.t("field-color"));
            if color_spec_editor(ui, &format!("{id}_color"), &mut spec.color) {
                changed = true;
            }
            ui.end_row();
        });
    changed
}
