//! Tab 5: Complete `TaskbarConfig` + `InteractionsConfig` editor.

use eframe::egui;
use oxide_core::{
    AutohideBehavior, ClickAction, MonitorTarget, TaskbarAnchor, TaskbarBackend, WheelAction,
    builtin_presets,
};

use crate::i18n::SettingsI18n;
use crate::state::SettingsState;
use crate::theme::card_frame;
use crate::win32_host::BackdropKind;

pub fn ui(
    ui: &mut egui::Ui,
    state: &mut SettingsState,
    i18n: &SettingsI18n,
    dark: bool,
    backdrop: BackdropKind,
) -> bool {
    let mut changed = false;

    // 1. Taskbar Positioning (`TaskbarConfig`)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-taskbar"));
        ui.add_space(6.0);

        let monitors = state.monitors.clone();
        let tb = &mut state.draft.taskbar;
        egui::Grid::new("taskbar_settings_grid")
            .num_columns(2)
            .spacing([18.0, 8.0])
            .min_col_width(240.0)
            .show(ui, |ui| {
                ui.label(i18n.t("field-tb-show"));
                if ui.checkbox(&mut tb.show, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-tb-backend"));
                egui::ComboBox::from_id_salt("tb_backend")
                    .selected_text(format!("{:?}", tb.backend))
                    .show_ui(ui, |ui| {
                        for (val, lbl) in [
                            (TaskbarBackend::Overlay, "Overlay (default)"),
                            (TaskbarBackend::Embed, "Embed (experimental)"),
                            (TaskbarBackend::TrayOnly, "Tray only"),
                        ] {
                            if ui.selectable_value(&mut tb.backend, val, lbl).changed() {
                                changed = true;
                            }
                        }
                    });
                ui.end_row();

                ui.label(i18n.t("field-tb-monitor"));
                let mon_text = match &tb.monitor {
                    MonitorTarget::Primary => i18n.t("monitor-primary"),
                    MonitorTarget::AllTaskbars => i18n.t("monitor-all"),
                    MonitorTarget::Device { device, name } => {
                        if name.is_empty() {
                            device.clone()
                        } else {
                            format!("{name} ({device})")
                        }
                    }
                };
                egui::ComboBox::from_id_salt("tb_monitor")
                    .selected_text(mon_text)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(
                                &mut tb.monitor,
                                MonitorTarget::Primary,
                                i18n.t("monitor-primary"),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        if ui
                            .selectable_value(
                                &mut tb.monitor,
                                MonitorTarget::AllTaskbars,
                                i18n.t("monitor-all"),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        for m in &monitors {
                            let target = MonitorTarget::Device {
                                device: m.device.clone(),
                                name: m.friendly_name.clone(),
                            };
                            let lbl = format!(
                                "{} — {}×{} ({} DPI)",
                                m.friendly_name, m.width, m.height, m.dpi
                            );
                            if ui.selectable_value(&mut tb.monitor, target, lbl).changed() {
                                changed = true;
                            }
                        }
                    });
                ui.end_row();

                ui.label(i18n.t("field-tb-anchor"));
                let anchor_lbl = match tb.anchor {
                    TaskbarAnchor::TrayLeft => i18n.t("anchor-tray-left"),
                    TaskbarAnchor::TaskbarLeft => i18n.t("anchor-taskbar-left"),
                    TaskbarAnchor::TaskbarRight => i18n.t("anchor-taskbar-right"),
                    TaskbarAnchor::FreeDrag => i18n.t("anchor-free-drag"),
                };
                egui::ComboBox::from_id_salt("tb_anchor")
                    .selected_text(anchor_lbl)
                    .show_ui(ui, |ui| {
                        for (val, key) in [
                            (TaskbarAnchor::TrayLeft, "anchor-tray-left"),
                            (TaskbarAnchor::TaskbarLeft, "anchor-taskbar-left"),
                            (TaskbarAnchor::TaskbarRight, "anchor-taskbar-right"),
                            (TaskbarAnchor::FreeDrag, "anchor-free-drag"),
                        ] {
                            if ui
                                .selectable_value(&mut tb.anchor, val, i18n.t(key))
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
                ui.end_row();

                ui.label(i18n.t("field-tb-offset"));
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::DragValue::new(&mut tb.x_offset)
                                .range(-2000..=2000)
                                .prefix("X: ")
                                .suffix(" DIP"),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    if ui
                        .add(
                            egui::DragValue::new(&mut tb.y_offset)
                                .range(-2000..=2000)
                                .prefix("Y: ")
                                .suffix(" DIP"),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                });
                ui.end_row();

                ui.label(i18n.t("field-tb-left-space"));
                if ui
                    .add(egui::Slider::new(&mut tb.left_space_win11, 0..=800).suffix(" DIP"))
                    .changed()
                {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-tb-right-space"));
                if ui
                    .add(egui::Slider::new(&mut tb.right_space_fallback, 0..=800).suffix(" DIP"))
                    .changed()
                {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-tb-snap"));
                if ui
                    .add(egui::Slider::new(&mut tb.snap_distance, 0..=32).suffix(" DIP"))
                    .changed()
                {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-tb-lock"));
                if ui.checkbox(&mut tb.lock_position, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-tb-click-through"));
                if ui.checkbox(&mut tb.click_through, "").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-tb-autohide"));
                let ah_lbl = match tb.autohide_behavior {
                    AutohideBehavior::HideWithTaskbar => i18n.t("autohide-hide"),
                    AutohideBehavior::KeepVisible => i18n.t("autohide-keep"),
                };
                egui::ComboBox::from_id_salt("tb_autohide")
                    .selected_text(ah_lbl)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(
                                &mut tb.autohide_behavior,
                                AutohideBehavior::HideWithTaskbar,
                                i18n.t("autohide-hide"),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        if ui
                            .selectable_value(
                                &mut tb.autohide_behavior,
                                AutohideBehavior::KeepVisible,
                                i18n.t("autohide-keep"),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                    });
                ui.end_row();
            });
    });

    ui.add_space(10.0);

    // 2. Mouse Interactions (`InteractionsConfig`)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-interactions"));
        ui.add_space(6.0);

        let inter = &mut state.draft.interactions;
        egui::Grid::new("interactions_settings_grid")
            .num_columns(2)
            .spacing([18.0, 8.0])
            .min_col_width(240.0)
            .show(ui, |ui| {
                ui.label(i18n.t("field-left-click"));
                if click_action_editor(ui, "act_left", &mut inter.left_click, i18n) {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-double-click"));
                if click_action_editor(ui, "act_dbl", &mut inter.double_click, i18n) {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-middle-click"));
                if click_action_editor(ui, "act_mid", &mut inter.middle_click, i18n) {
                    changed = true;
                }
                ui.end_row();

                ui.label(i18n.t("field-wheel-action"));
                egui::ComboBox::from_id_salt("act_wheel")
                    .selected_text(format!("{:?}", inter.wheel_action))
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(&mut inter.wheel_action, WheelAction::None, "None")
                            .changed()
                        {
                            changed = true;
                        }
                        if ui
                            .selectable_value(
                                &mut inter.wheel_action,
                                WheelAction::CyclePreset,
                                "Cycle preset",
                            )
                            .changed()
                        {
                            changed = true;
                        }
                    });
                ui.end_row();

                ui.label(i18n.t("field-cycle-presets"));
                ui.vertical(|ui| {
                    for preset in builtin_presets() {
                        let id = preset.preset.id;
                        let mut enabled = inter.cycle_presets.iter().any(|x| x == &id);
                        let label = i18n.t(&format!("preset-{id}"));
                        if ui.checkbox(&mut enabled, label).changed() {
                            if enabled {
                                if !inter.cycle_presets.iter().any(|x| x == &id) {
                                    inter.cycle_presets.push(id);
                                }
                            } else {
                                inter.cycle_presets.retain(|x| x != &id);
                            }
                            changed = true;
                        }
                    }
                });
                ui.end_row();
            });
    });

    changed
}

fn click_action_editor(
    ui: &mut egui::Ui,
    id_salt: &str,
    action: &mut ClickAction,
    i18n: &SettingsI18n,
) -> bool {
    let mut changed = false;
    ui.vertical(|ui| {
        let cur_idx = action_kind_index(action);
        let mut next_idx = cur_idx;
        egui::ComboBox::from_id_salt(id_salt)
            .width(210.0)
            .selected_text(ACTION_LABELS[cur_idx])
            .show_ui(ui, |ui| {
                for (i, lbl) in ACTION_LABELS.iter().enumerate() {
                    ui.selectable_value(&mut next_idx, i, *lbl);
                }
            });
        if next_idx != cur_idx {
            *action = default_action_for_kind(next_idx);
            changed = true;
        }
        if let ClickAction::RunCommand { path, args } = action {
            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::TextEdit::singleline(path)
                            .hint_text(i18n.t("field-cmd-path"))
                            .desired_width(160.0),
                    )
                    .changed()
                {
                    changed = true;
                }
                let mut args_text = args.join(" ");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut args_text)
                            .hint_text(i18n.t("field-cmd-args"))
                            .desired_width(120.0),
                    )
                    .changed()
                {
                    *args = args_text.split_whitespace().map(str::to_string).collect();
                    changed = true;
                }
            });
        }
    });
    changed
}

const ACTION_LABELS: [&str; 10] = [
    "None",
    "Toggle Flyout",
    "Open Settings",
    "Open Task Manager",
    "Open Resource Monitor",
    "Open Network Stats",
    "Toggle Game Mode",
    "Cycle Preset",
    "Copy Summary",
    "Run Command…",
];

fn action_kind_index(a: &ClickAction) -> usize {
    match a {
        ClickAction::None => 0,
        ClickAction::ToggleFlyout => 1,
        ClickAction::OpenSettings => 2,
        ClickAction::OpenTaskManager => 3,
        ClickAction::OpenResourceMonitor => 4,
        ClickAction::OpenNetworkStats => 5,
        ClickAction::ToggleGameMode => 6,
        ClickAction::CyclePreset => 7,
        ClickAction::CopySummary => 8,
        ClickAction::RunCommand { .. } => 9,
    }
}

fn default_action_for_kind(idx: usize) -> ClickAction {
    match idx {
        1 => ClickAction::ToggleFlyout,
        2 => ClickAction::OpenSettings,
        3 => ClickAction::OpenTaskManager,
        4 => ClickAction::OpenResourceMonitor,
        5 => ClickAction::OpenNetworkStats,
        6 => ClickAction::ToggleGameMode,
        7 => ClickAction::CyclePreset,
        8 => ClickAction::CopySummary,
        9 => ClickAction::RunCommand {
            path: String::new(),
            args: Vec::new(),
        },
        _ => ClickAction::None,
    }
}
