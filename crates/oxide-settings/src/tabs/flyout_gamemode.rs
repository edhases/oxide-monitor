//! Tab 6: Complete `FlyoutConfig` + `GameModeConfig` editor (strictly no HWiNFO/LHM toggles).

use eframe::egui;
use oxide_core::{FlyoutBackdrop, FlyoutTrigger, GameModeAction};

use crate::i18n::SettingsI18n;
use crate::state::SettingsState;
use crate::theme::card_frame;
use crate::win32_host::BackdropKind;

#[derive(Default)]
pub struct FlyoutGameModeUiState {
    pub new_rule: String,
    pub new_whitelist: String,
}

pub fn ui(
    ui: &mut egui::Ui,
    state: &mut SettingsState,
    tab_state: &mut FlyoutGameModeUiState,
    i18n: &SettingsI18n,
    dark: bool,
    backdrop: BackdropKind,
) -> bool {
    let mut changed = false;

    // 1. Flyout (`FlyoutConfig`)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-flyout"));
        ui.add_space(6.0);

        let fly = &mut state.draft.flyout;
        egui::Grid::new("flyout_settings_grid")
            .num_columns(2)
            .spacing([18.0, 8.0])
            .min_col_width(260.0)
            .show(ui, |ui| {
                ui.label(i18n.t("field-flyout-enabled"));
                if ui.checkbox(&mut fly.enabled, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-flyout-trigger"));
                egui::ComboBox::from_id_salt("fly_trigger")
                    .selected_text(format!("{:?}", fly.trigger))
                    .show_ui(ui, |ui| {
                        for (val, lbl) in [
                            (FlyoutTrigger::HoverAndClick, "Hover & Click"),
                            (FlyoutTrigger::ClickOnly, "Click only"),
                            (FlyoutTrigger::HoverOnly, "Hover only"),
                        ] {
                            if ui.selectable_value(&mut fly.trigger, val, lbl).changed() {
                                changed = true;
                            }
                        }
                    });
                ui.end_row();

                ui.label(i18n.t("field-flyout-delay"));
                if ui
                    .add(
                        egui::Slider::new(&mut fly.hover_delay_ms, 100..=2_000)
                            .step_by(50.0)
                            .suffix(" ms"),
                    )
                    .changed()
                {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-flyout-backdrop"));
                egui::ComboBox::from_id_salt("fly_backdrop")
                    .selected_text(format!("{:?}", fly.backdrop))
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(&mut fly.backdrop, FlyoutBackdrop::Acrylic, "Acrylic")
                            .changed()
                        {
                            changed = true;
                        }
                        if ui
                            .selectable_value(&mut fly.backdrop, FlyoutBackdrop::Solid, "Solid")
                            .changed()
                        {
                            changed = true;
                        }
                    });
                ui.end_row();

                ui.label(i18n.t("field-flyout-width"));
                if ui
                    .add(egui::Slider::new(&mut fly.width, 280..=640).suffix(" DIP"))
                    .changed()
                {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-flyout-per-core"));
                if ui.checkbox(&mut fly.show_cpu_per_core, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-flyout-graphs"));
                if ui.checkbox(&mut fly.show_graphs, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-flyout-gpus"));
                if ui.checkbox(&mut fly.show_all_gpus, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-flyout-disks"));
                if ui.checkbox(&mut fly.show_all_disks, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-flyout-net"));
                if ui.checkbox(&mut fly.show_network_details, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-flyout-sources"));
                if ui.checkbox(&mut fly.show_provider_status, "").changed() {
                    changed = true;
                }
                ui.end_row();
            });
    });

    ui.add_space(10.0);

    // 2. Game Mode (`GameModeConfig` — no HWiNFO/LHM provider toggles)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-gamemode"));
        ui.add_space(6.0);

        let gm = &mut state.draft.game_mode;
        egui::Grid::new("gamemode_settings_grid")
            .num_columns(2)
            .spacing([18.0, 8.0])
            .min_col_width(260.0)
            .show(ui, |ui| {
                ui.label(i18n.t("field-gm-fullscreen"));
                if ui.checkbox(&mut gm.auto_hide_fullscreen, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-gm-borderless"));
                if ui.checkbox(&mut gm.auto_hide_borderless, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-gm-action"));
                egui::ComboBox::from_id_salt("gm_action")
                    .selected_text(format!("{:?}", gm.action))
                    .show_ui(ui, |ui| {
                        for (val, lbl) in [
                            (GameModeAction::HideOnly, "Hide only"),
                            (GameModeAction::HideAndLowPower, "Hide & low power"),
                            (GameModeAction::StrictAnticheat, "Strict anti-cheat"),
                        ] {
                            if ui.selectable_value(&mut gm.action, val, lbl).changed() {
                                changed = true;
                            }
                        }
                    });
                ui.end_row();
            });

        ui.add_space(8.0);
        if pattern_list_editor(
            ui,
            &i18n.t("field-gm-rules"),
            &mut gm.process_rules,
            &mut tab_state.new_rule,
            i18n,
        ) {
            changed = true;
        }

        ui.add_space(8.0);
        if pattern_list_editor(
            ui,
            &i18n.t("field-gm-whitelist"),
            &mut gm.whitelist_processes,
            &mut tab_state.new_whitelist,
            i18n,
        ) {
            changed = true;
        }
    });

    changed
}

fn pattern_list_editor(
    ui: &mut egui::Ui,
    title: &str,
    items: &mut Vec<String>,
    draft_new: &mut String,
    i18n: &SettingsI18n,
) -> bool {
    let mut changed = false;
    ui.strong(title);
    ui.add_space(3.0);

    let mut remove_idx = None;
    for (idx, pat) in items.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            if ui
                .add(egui::TextEdit::singleline(pat).desired_width(280.0))
                .changed()
            {
                changed = true;
            }
            if ui.small_button("✕").clicked() {
                remove_idx = Some(idx);
            }
        });
    }
    if let Some(rm) = remove_idx {
        items.remove(rm);
        changed = true;
    }

    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(draft_new)
                .hint_text(i18n.t("field-pattern-hint"))
                .desired_width(240.0),
        );
        let can_add = !draft_new.trim().is_empty();
        if ui
            .add_enabled(can_add, egui::Button::new(i18n.t("btn-add-rule")))
            .clicked()
        {
            items.push(draft_new.trim().to_string());
            draft_new.clear();
            changed = true;
        }
    });
    changed
}
