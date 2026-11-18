//! Tab 2: Full `items` (`Vec<ItemConfig>`) editor + `aliases` (`AliasesConfig`) editor.

use std::collections::BTreeMap;

use eframe::egui::{self, Color32, CornerRadius};
use oxide_core::{
    AdapterTarget, ColorRule, ColorSpace, ColorSpec, ColorStop, CpuTempSource, DiskTarget,
    GpuTarget, GridSlot, ItemConfig, ItemVisual, MemoryDisplayMode, MetricSource, ProgressStyle,
    Rgba, SparklinePlacement, SparklineScale, SparklineStyle, ThroughputDir,
};

use crate::i18n::SettingsI18n;
use crate::state::SettingsState;
use crate::tabs::{color_spec_editor, rgba_picker};
use crate::theme::card_frame;
use crate::win32_host::BackdropKind;

#[derive(Default)]
pub struct ItemsAliasesUiState {
    pub selected_idx: usize,
    pub new_gpu_key: String,
    pub new_gpu_val: String,
    pub new_disk_key: String,
    pub new_disk_val: String,
    pub new_adapter_key: String,
    pub new_adapter_val: String,
}

pub fn ui(
    ui: &mut egui::Ui,
    state: &mut SettingsState,
    tab_state: &mut ItemsAliasesUiState,
    i18n: &SettingsI18n,
    dark: bool,
    backdrop: BackdropKind,
) -> bool {
    let mut changed = false;

    if !state.draft.items.is_empty() && tab_state.selected_idx >= state.draft.items.len() {
        tab_state.selected_idx = state.draft.items.len() - 1;
    }

    // 1. Items List + Order / Visibility / Add / Duplicate / Delete
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.heading(i18n.t("section-items"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(i18n.t("items-add")).clicked() {
                    let next_idx = state.draft.items.len() + 1;
                    state.draft.items.push(ItemConfig::new_v2(
                        format!("item_{next_idx}"),
                        "CPU:",
                        MetricSource::CpuUsage,
                        ItemVisual::Text,
                        Some(ColorRule::Static {
                            color: ColorSpec::Static(Rgba::rgb(0x7F, 0xD3, 0x7F)),
                        }),
                    ));
                    tab_state.selected_idx = state.draft.items.len() - 1;
                    changed = true;
                }
            });
        });
        ui.add_space(6.0);

        let mut move_op: Option<(usize, usize)> = None;
        let mut duplicate_idx: Option<usize> = None;
        let mut delete_idx: Option<usize> = None;
        let count = state.draft.items.len();

        for idx in 0..count {
            let selected = tab_state.selected_idx == idx;
            let item = &mut state.draft.items[idx];
            let source_summary = metric_source_short(&item.effective_source());
            let swatch = preview_swatch_color(item);

            egui::Frame::NONE
                .fill(if selected {
                    ui.visuals().selection.bg_fill
                } else {
                    Color32::TRANSPARENT
                })
                .stroke(if selected {
                    ui.visuals().selection.stroke
                } else {
                    ui.visuals().widgets.noninteractive.bg_stroke
                })
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut item.visible, "").changed() {
                            changed = true;
                        }
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(3), swatch);

                        let title = if item.label.is_empty() {
                            format!("{} ({})", item.uid, source_summary)
                        } else {
                            format!("{}  —  {}", item.label, source_summary)
                        };
                        if ui.selectable_label(selected, title).clicked() {
                            tab_state.selected_idx = idx;
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(count > 1, egui::Button::new("✕"))
                                .on_hover_text(i18n.t("items-remove"))
                                .clicked()
                            {
                                delete_idx = Some(idx);
                            }
                            if ui
                                .button("⧉")
                                .on_hover_text(i18n.t("items-duplicate"))
                                .clicked()
                            {
                                duplicate_idx = Some(idx);
                            }
                            if ui
                                .add_enabled(idx + 1 < count, egui::Button::new("↓"))
                                .clicked()
                            {
                                move_op = Some((idx, idx + 1));
                            }
                            if ui.add_enabled(idx > 0, egui::Button::new("↑")).clicked() {
                                move_op = Some((idx, idx - 1));
                            }
                        });
                    });
                });
            ui.add_space(2.0);
        }

        if let Some((from, to)) = move_op {
            state.draft.items.swap(from, to);
            if tab_state.selected_idx == from {
                tab_state.selected_idx = to;
            } else if tab_state.selected_idx == to {
                tab_state.selected_idx = from;
            }
            changed = true;
        }
        if let Some(idx) = duplicate_idx {
            let mut copy = state.draft.items[idx].clone();
            copy.uid = format!("{}_copy", copy.uid);
            state.draft.items.insert(idx + 1, copy);
            tab_state.selected_idx = idx + 1;
            changed = true;
        }
        if let Some(idx) = delete_idx
            && state.draft.items.len() > 1
        {
            state.draft.items.remove(idx);
            tab_state.selected_idx = tab_state
                .selected_idx
                .min(state.draft.items.len().saturating_sub(1));
            changed = true;
        }
    });

    ui.add_space(10.0);

    // 2. Selected Item Inspector (all ItemConfig fields)
    if let Some(item) = state.draft.items.get_mut(tab_state.selected_idx) {
        let group_ids: Vec<String> = state
            .draft
            .layout
            .groups
            .iter()
            .map(|g| g.id.clone())
            .collect();

        card_frame(dark, backdrop).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.heading(format!(
                "{} — #{}",
                i18n.t("items-inspector"),
                tab_state.selected_idx + 1
            ));
            ui.add_space(6.0);

            egui::Grid::new("item_inspector_grid")
                .num_columns(2)
                .spacing([18.0, 8.0])
                .min_col_width(180.0)
                .show(ui, |ui| {
                    // Visible
                    ui.label(i18n.t("field-item-visible"));
                    if ui.checkbox(&mut item.visible, "").changed() {
                        changed = true;
                    }
                    ui.end_row();

                    // UID
                    ui.label(i18n.t("field-item-uid"));
                    if ui
                        .add(egui::TextEdit::singleline(&mut item.uid).desired_width(200.0))
                        .changed()
                    {
                        changed = true;
                    }
                    ui.end_row();

                    // Label
                    ui.label(i18n.t("field-item-label"));
                    if ui
                        .add(egui::TextEdit::singleline(&mut item.label).desired_width(200.0))
                        .changed()
                    {
                        changed = true;
                    }
                    ui.end_row();

                    // Group
                    ui.label(i18n.t("field-item-group"));
                    let cur_group = item.group.clone().unwrap_or_default();
                    ui.horizontal(|ui| {
                        let mut g_text = cur_group;
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut g_text)
                                    .hint_text("none")
                                    .desired_width(140.0),
                            )
                            .changed()
                        {
                            let t = g_text.trim().to_string();
                            item.group = (!t.is_empty()).then_some(t);
                            changed = true;
                        }
                        if !group_ids.is_empty() {
                            egui::ComboBox::from_id_salt("item_group_picker")
                                .width(100.0)
                                .selected_text(item.group.as_deref().unwrap_or("—"))
                                .show_ui(ui, |ui| {
                                    if ui.selectable_value(&mut item.group, None, "—").changed() {
                                        changed = true;
                                    }
                                    for gid in &group_ids {
                                        if ui
                                            .selectable_value(
                                                &mut item.group,
                                                Some(gid.clone()),
                                                gid,
                                            )
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                        }
                    });
                    ui.end_row();

                    // MetricSource kind
                    ui.label(i18n.t("field-item-source"));
                    let mut source = item.effective_source();
                    let cur_kind = source_kind_index(&source);
                    let mut next_kind = cur_kind;
                    egui::ComboBox::from_id_salt("item_source_kind")
                        .width(220.0)
                        .selected_text(metric_source_short(&source))
                        .show_ui(ui, |ui| {
                            for (k_idx, name) in SOURCE_KIND_LABELS.iter().enumerate() {
                                ui.selectable_value(&mut next_kind, k_idx, *name);
                            }
                        });
                    if next_kind != cur_kind {
                        source = default_source_for_kind(next_kind);
                        item.id = source.legacy_id();
                        item.source = Some(source.clone());
                        changed = true;
                    }
                    ui.end_row();

                    // MetricSource target / sub-options
                    if edit_source_suboptions(ui, &mut source, i18n) {
                        item.id = source.legacy_id();
                        item.source = Some(source);
                        changed = true;
                    }

                    // ItemVisual mode
                    ui.label(i18n.t("field-item-visual"));
                    let cur_vis = visual_kind_index(&item.visual);
                    let mut next_vis = cur_vis;
                    let vis_names = [
                        i18n.t("visual-text"),
                        i18n.t("visual-sparkline"),
                        i18n.t("visual-progress"),
                        i18n.t("visual-graph-only"),
                    ];
                    egui::ComboBox::from_id_salt("item_visual_mode")
                        .width(220.0)
                        .selected_text(&vis_names[cur_vis])
                        .show_ui(ui, |ui| {
                            for (v_idx, label) in vis_names.iter().enumerate() {
                                ui.selectable_value(&mut next_vis, v_idx, label);
                            }
                        });
                    if next_vis != cur_vis {
                        item.visual = default_visual_for_kind(next_vis);
                        changed = true;
                    }
                    ui.end_row();

                    // ItemVisual subfields
                    if edit_visual_suboptions(ui, &mut item.visual, i18n) {
                        changed = true;
                    }

                    // ColorRule editor
                    ui.label(i18n.t("field-color-rule"));
                    let cur_rule_kind = color_rule_kind_index(&item.color_rule);
                    let mut next_rule_kind = cur_rule_kind;
                    let rule_names = [
                        i18n.t("color-rule-default"),
                        i18n.t("color-rule-static"),
                        i18n.t("color-rule-steps"),
                        i18n.t("color-rule-gradient"),
                    ];
                    egui::ComboBox::from_id_salt("item_color_rule_kind")
                        .width(220.0)
                        .selected_text(&rule_names[cur_rule_kind])
                        .show_ui(ui, |ui| {
                            for (r_idx, label) in rule_names.iter().enumerate() {
                                ui.selectable_value(&mut next_rule_kind, r_idx, label);
                            }
                        });
                    if next_rule_kind != cur_rule_kind {
                        item.color_rule = default_color_rule_for_kind(next_rule_kind);
                        sync_legacy_item_color(item);
                        changed = true;
                    }
                    ui.end_row();

                    if edit_color_rule_details(ui, &mut item.color_rule, i18n) {
                        sync_legacy_item_color(item);
                        changed = true;
                    }

                    // GridSlot
                    ui.label(i18n.t("field-grid-slot"));
                    let mut has_grid = item.grid.is_some();
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut has_grid, "").changed() {
                            item.grid = if has_grid {
                                Some(GridSlot {
                                    row: 0,
                                    col: 0,
                                    row_span: 1,
                                    col_span: 1,
                                })
                            } else {
                                None
                            };
                            changed = true;
                        }
                        if let Some(slot) = &mut item.grid {
                            ui.label(i18n.t("field-grid-row"));
                            if ui
                                .add(egui::DragValue::new(&mut slot.row).range(0..=16))
                                .changed()
                            {
                                changed = true;
                            }
                            ui.label(i18n.t("field-grid-col"));
                            if ui
                                .add(egui::DragValue::new(&mut slot.col).range(0..=32))
                                .changed()
                            {
                                changed = true;
                            }
                            ui.label("×");
                            if ui
                                .add(egui::DragValue::new(&mut slot.row_span).range(1..=8))
                                .on_hover_text(i18n.t("field-grid-row-span"))
                                .changed()
                            {
                                changed = true;
                            }
                            if ui
                                .add(egui::DragValue::new(&mut slot.col_span).range(1..=8))
                                .on_hover_text(i18n.t("field-grid-col-span"))
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
                    ui.end_row();
                });
        });
    }

    ui.add_space(10.0);

    // 3. Hardware Aliases (`AliasesConfig`: gpus, disks, adapters)
    let discovered_gpus: Vec<String> = state
        .inventory
        .gpus
        .iter()
        .map(|g| g.name.clone())
        .filter(|s| !s.is_empty())
        .collect();
    let discovered_disks: Vec<String> = state
        .inventory
        .disks
        .iter()
        .map(|d| d.name.clone())
        .filter(|s| !s.is_empty())
        .collect();
    let discovered_adapters: Vec<String> = state
        .inventory
        .adapters
        .iter()
        .map(|a| a.alias.clone())
        .filter(|s| !s.is_empty())
        .collect();

    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-aliases"));
        ui.add_space(6.0);

        if alias_map_editor(
            ui,
            &i18n.t("aliases-gpus"),
            &mut state.draft.aliases.gpus,
            &discovered_gpus,
            &mut tab_state.new_gpu_key,
            &mut tab_state.new_gpu_val,
            i18n,
        ) {
            changed = true;
        }
        ui.add_space(8.0);
        if alias_map_editor(
            ui,
            &i18n.t("aliases-disks"),
            &mut state.draft.aliases.disks,
            &discovered_disks,
            &mut tab_state.new_disk_key,
            &mut tab_state.new_disk_val,
            i18n,
        ) {
            changed = true;
        }
        ui.add_space(8.0);
        if alias_map_editor(
            ui,
            &i18n.t("aliases-adapters"),
            &mut state.draft.aliases.adapters,
            &discovered_adapters,
            &mut tab_state.new_adapter_key,
            &mut tab_state.new_adapter_val,
            i18n,
        ) {
            changed = true;
        }
    });

    changed
}

fn alias_map_editor(
    ui: &mut egui::Ui,
    title: &str,
    map: &mut BTreeMap<String, String>,
    discovered: &[String],
    new_key: &mut String,
    new_val: &mut String,
    i18n: &SettingsI18n,
) -> bool {
    let mut changed = false;
    ui.strong(title);
    ui.add_space(2.0);

    if map.is_empty() {
        ui.weak(i18n.t("aliases-empty"));
    } else {
        let mut to_remove: Option<String> = None;
        for (k, v) in map.iter_mut() {
            ui.horizontal(|ui| {
                ui.monospace(k);
                ui.label("→");
                if ui
                    .add(egui::TextEdit::singleline(v).desired_width(150.0))
                    .changed()
                {
                    changed = true;
                }
                if ui.button("✕").clicked() {
                    to_remove = Some(k.clone());
                }
            });
        }
        if let Some(rm) = to_remove {
            map.remove(&rm);
            changed = true;
        }
    }

    // Quick-fill from discovered hardware
    if !discovered.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for dev in discovered {
                if !map.contains_key(dev) && ui.small_button(format!("+ {dev}")).clicked() {
                    *new_key = dev.clone();
                    if new_val.is_empty() {
                        *new_val = dev.split_whitespace().next().unwrap_or(dev).to_string();
                    }
                }
            }
        });
    }

    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(new_key)
                .hint_text(i18n.t("aliases-device-hint"))
                .desired_width(240.0),
        );
        ui.label("→");
        ui.add(
            egui::TextEdit::singleline(new_val)
                .hint_text(i18n.t("aliases-short-hint"))
                .desired_width(130.0),
        );
        let can_add = !new_key.trim().is_empty() && !new_val.trim().is_empty();
        if ui
            .add_enabled(can_add, egui::Button::new(i18n.t("aliases-add")))
            .clicked()
        {
            map.insert(new_key.trim().to_string(), new_val.trim().to_string());
            new_key.clear();
            new_val.clear();
            changed = true;
        }
    });

    changed
}

const SOURCE_KIND_LABELS: [&str; 16] = [
    "Net Upload",
    "Net Download",
    "Net Combined",
    "Net Total Speed",
    "Net Today Traffic",
    "CPU Usage",
    "CPU Frequency",
    "CPU Temperature",
    "Memory Usage",
    "GPU Usage",
    "GPU Temperature",
    "GPU VRAM",
    "Disk Temperature",
    "Disk Activity",
    "Disk Throughput",
    "Separator",
];

fn source_kind_index(src: &MetricSource) -> usize {
    match src {
        MetricSource::NetUp { .. } => 0,
        MetricSource::NetDown { .. } => 1,
        MetricSource::NetCombined { .. } => 2,
        MetricSource::NetTotalSpeed { .. } => 3,
        MetricSource::NetTodayTotal { .. } => 4,
        MetricSource::CpuUsage => 5,
        MetricSource::CpuFreq => 6,
        MetricSource::CpuTemp { .. } => 7,
        MetricSource::MemoryUsage { .. } => 8,
        MetricSource::GpuUsage { .. } => 9,
        MetricSource::GpuTemp { .. } => 10,
        MetricSource::GpuVram { .. } => 11,
        MetricSource::DiskTemp { .. } => 12,
        MetricSource::DiskActivity { .. } => 13,
        MetricSource::DiskThroughput { .. } => 14,
        MetricSource::Separator | MetricSource::ExternalSensor { .. } => 15,
    }
}

fn default_source_for_kind(idx: usize) -> MetricSource {
    match idx {
        0 => MetricSource::NetUp {
            adapter: AdapterTarget::Auto,
        },
        1 => MetricSource::NetDown {
            adapter: AdapterTarget::Auto,
        },
        2 => MetricSource::NetCombined {
            adapter: AdapterTarget::Auto,
        },
        3 => MetricSource::NetTotalSpeed {
            adapter: AdapterTarget::Auto,
        },
        4 => MetricSource::NetTodayTotal {
            adapter: AdapterTarget::Auto,
        },
        5 => MetricSource::CpuUsage,
        6 => MetricSource::CpuFreq,
        7 => MetricSource::CpuTemp {
            source: CpuTempSource::Auto,
        },
        8 => MetricSource::MemoryUsage {
            display_as: MemoryDisplayMode::Percent,
        },
        9 => MetricSource::GpuUsage {
            target: GpuTarget::Primary,
        },
        10 => MetricSource::GpuTemp {
            target: GpuTarget::Primary,
        },
        11 => MetricSource::GpuVram {
            target: GpuTarget::Primary,
        },
        12 => MetricSource::DiskTemp {
            target: DiskTarget::Primary,
        },
        13 => MetricSource::DiskActivity {
            target: DiskTarget::Primary,
        },
        14 => MetricSource::DiskThroughput {
            target: DiskTarget::Primary,
            dir: ThroughputDir::Rw,
        },
        _ => MetricSource::Separator,
    }
}

fn metric_source_short(src: &MetricSource) -> String {
    match src {
        MetricSource::NetUp { .. } => "Net Upload".into(),
        MetricSource::NetDown { .. } => "Net Download".into(),
        MetricSource::NetCombined { .. } => "Net Combined".into(),
        MetricSource::NetTotalSpeed { .. } => "Net Total Speed".into(),
        MetricSource::NetTodayTotal { .. } => "Net Today Traffic".into(),
        MetricSource::CpuUsage => "CPU Usage".into(),
        MetricSource::CpuFreq => "CPU Frequency".into(),
        MetricSource::CpuTemp { .. } => "CPU Temperature".into(),
        MetricSource::MemoryUsage { .. } => "Memory Usage".into(),
        MetricSource::GpuUsage { .. } => "GPU Usage".into(),
        MetricSource::GpuTemp { .. } => "GPU Temperature".into(),
        MetricSource::GpuVram { .. } => "GPU VRAM".into(),
        MetricSource::DiskTemp { .. } => "Disk Temperature".into(),
        MetricSource::DiskActivity { .. } => "Disk Activity".into(),
        MetricSource::DiskThroughput { .. } => "Disk Throughput".into(),
        MetricSource::ExternalSensor { provider, sensor } => format!("{provider}:{sensor}"),
        MetricSource::Separator => "Separator".into(),
    }
}

fn edit_source_suboptions(
    ui: &mut egui::Ui,
    source: &mut MetricSource,
    i18n: &SettingsI18n,
) -> bool {
    let mut changed = false;
    match source {
        MetricSource::NetUp { adapter }
        | MetricSource::NetDown { adapter }
        | MetricSource::NetCombined { adapter }
        | MetricSource::NetTotalSpeed { adapter }
        | MetricSource::NetTodayTotal { adapter } => {
            ui.label(i18n.t("field-item-target"));
            ui.horizontal(|ui| {
                let is_auto = matches!(adapter, AdapterTarget::Auto);
                let is_name = matches!(adapter, AdapterTarget::Name(_));
                egui::ComboBox::from_id_salt("item_adapter_target")
                    .selected_text(if is_auto {
                        i18n.t("field-auto")
                    } else if is_name {
                        "Name".into()
                    } else {
                        "LUID".into()
                    })
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(is_auto, i18n.t("field-auto")).clicked() {
                            *adapter = AdapterTarget::Auto;
                            changed = true;
                        }
                        if ui.selectable_label(is_name, "Name").clicked() && !is_name {
                            *adapter = AdapterTarget::Name(String::new());
                            changed = true;
                        }
                    });
                if let AdapterTarget::Name(name) = adapter
                    && ui
                        .add(egui::TextEdit::singleline(name).desired_width(140.0))
                        .changed()
                {
                    changed = true;
                }
            });
            ui.end_row();
        }
        MetricSource::CpuTemp { source: src } => {
            ui.label(i18n.t("field-item-target"));
            egui::ComboBox::from_id_salt("item_cpu_temp_src")
                .selected_text(format!("{src:?}"))
                .show_ui(ui, |ui| {
                    for (val, key) in [
                        (CpuTempSource::Auto, "cpu-temp-auto"),
                        (CpuTempSource::Acpi, "cpu-temp-acpi"),
                        (CpuTempSource::Provider, "cpu-temp-provider"),
                        (CpuTempSource::Off, "cpu-temp-off"),
                    ] {
                        if ui.selectable_value(src, val, i18n.t(key)).changed() {
                            changed = true;
                        }
                    }
                });
            ui.end_row();
        }
        MetricSource::MemoryUsage { display_as } => {
            ui.label(i18n.t("field-item-target"));
            egui::ComboBox::from_id_salt("item_mem_mode")
                .selected_text(format!("{display_as:?}"))
                .show_ui(ui, |ui| {
                    for (val, label) in [
                        (MemoryDisplayMode::Percent, "Percent (%)"),
                        (MemoryDisplayMode::UsedGb, "Used GB"),
                        (MemoryDisplayMode::UsedTotalGb, "Used / Total GB"),
                    ] {
                        if ui.selectable_value(display_as, val, label).changed() {
                            changed = true;
                        }
                    }
                });
            ui.end_row();
        }
        MetricSource::GpuUsage { target }
        | MetricSource::GpuTemp { target }
        | MetricSource::GpuVram { target } => {
            ui.label(i18n.t("field-item-target"));
            ui.horizontal(|ui| {
                let kind_label = match target {
                    GpuTarget::Primary => "Primary",
                    GpuTarget::Discrete => "Discrete (dGPU)",
                    GpuTarget::Integrated => "Integrated (iGPU)",
                    GpuTarget::Index(_) => "Index",
                    GpuTarget::Luid(_) => "LUID",
                };
                egui::ComboBox::from_id_salt("item_gpu_target")
                    .selected_text(kind_label)
                    .show_ui(ui, |ui| {
                        for (val, lbl) in [
                            (GpuTarget::Primary, "Primary"),
                            (GpuTarget::Discrete, "Discrete (dGPU)"),
                            (GpuTarget::Integrated, "Integrated (iGPU)"),
                            (GpuTarget::Index(0), "Index"),
                        ] {
                            let same =
                                std::mem::discriminant(target) == std::mem::discriminant(&val);
                            if ui.selectable_label(same, lbl).clicked() && !same {
                                *target = val;
                                changed = true;
                            }
                        }
                    });
                if let GpuTarget::Index(idx) = target
                    && ui.add(egui::DragValue::new(idx).range(0..=8)).changed()
                {
                    changed = true;
                }
            });
            ui.end_row();
        }
        MetricSource::DiskTemp { target } | MetricSource::DiskActivity { target } => {
            ui.label(i18n.t("field-item-target"));
            if edit_disk_target(ui, "item_disk_target", target) {
                changed = true;
            }
            ui.end_row();
        }
        MetricSource::DiskThroughput { target, dir } => {
            ui.label(i18n.t("field-item-target"));
            ui.horizontal(|ui| {
                if edit_disk_target(ui, "item_disk_tp_target", target) {
                    changed = true;
                }
                egui::ComboBox::from_id_salt("item_disk_dir")
                    .width(80.0)
                    .selected_text(format!("{dir:?}"))
                    .show_ui(ui, |ui| {
                        for (val, lbl) in [
                            (ThroughputDir::Rw, "R/W"),
                            (ThroughputDir::Read, "Read"),
                            (ThroughputDir::Write, "Write"),
                        ] {
                            if ui.selectable_value(dir, val, lbl).changed() {
                                changed = true;
                            }
                        }
                    });
            });
            ui.end_row();
        }
        _ => {}
    }
    changed
}

fn edit_disk_target(ui: &mut egui::Ui, id_salt: &str, target: &mut DiskTarget) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let lbl = match target {
            DiskTarget::Primary => "Primary",
            DiskTarget::Index(_) => "Index",
            DiskTarget::Model(_) => "Model",
        };
        egui::ComboBox::from_id_salt(id_salt)
            .selected_text(lbl)
            .show_ui(ui, |ui| {
                for (val, name) in [
                    (DiskTarget::Primary, "Primary"),
                    (DiskTarget::Index(0), "Index"),
                    (DiskTarget::Model(String::new()), "Model"),
                ] {
                    let same = std::mem::discriminant(target) == std::mem::discriminant(&val);
                    if ui.selectable_label(same, name).clicked() && !same {
                        *target = val;
                        changed = true;
                    }
                }
            });
        match target {
            DiskTarget::Index(idx) => {
                if ui.add(egui::DragValue::new(idx).range(0..=32)).changed() {
                    changed = true;
                }
            }
            DiskTarget::Model(m) => {
                if ui
                    .add(egui::TextEdit::singleline(m).desired_width(120.0))
                    .changed()
                {
                    changed = true;
                }
            }
            DiskTarget::Primary => {}
        }
    });
    changed
}

fn visual_kind_index(v: &ItemVisual) -> usize {
    match v {
        ItemVisual::Text => 0,
        ItemVisual::TextSparkline { .. } => 1,
        ItemVisual::TextProgress { .. } => 2,
        ItemVisual::GraphOnly { .. } => 3,
    }
}

fn default_visual_for_kind(idx: usize) -> ItemVisual {
    match idx {
        1 => ItemVisual::TextSparkline {
            placement: SparklinePlacement::Background,
            style: SparklineStyle::Area,
            window_sec: 60,
            scale: SparklineScale::AutoDecay,
            fill_alpha: 0.22,
            stroke_width: 1.25,
            graph_width: 36.0,
        },
        2 => ItemVisual::TextProgress {
            style: ProgressStyle::BarBottom,
            thickness: 2.5,
            track_alpha: 0.16,
            bar_width: 28.0,
        },
        3 => ItemVisual::GraphOnly {
            style: SparklineStyle::Area,
            window_sec: 60,
            scale: SparklineScale::AutoDecay,
            fill_alpha: 0.22,
            stroke_width: 1.25,
            dashed_border: false,
            secondary_color: None,
            width: 48.0,
        },
        _ => ItemVisual::Text,
    }
}

fn edit_visual_suboptions(ui: &mut egui::Ui, visual: &mut ItemVisual, i18n: &SettingsI18n) -> bool {
    let mut changed = false;
    match visual {
        ItemVisual::Text => {}
        ItemVisual::TextSparkline {
            placement,
            style,
            window_sec,
            scale,
            fill_alpha,
            stroke_width,
            graph_width,
        } => {
            ui.label(i18n.t("field-placement"));
            egui::ComboBox::from_id_salt("vis_spark_place")
                .selected_text(format!("{placement:?}"))
                .show_ui(ui, |ui| {
                    for val in [
                        SparklinePlacement::Background,
                        SparklinePlacement::InlineRight,
                        SparklinePlacement::InlineLeft,
                    ] {
                        if ui
                            .selectable_value(placement, val, format!("{val:?}"))
                            .changed()
                        {
                            changed = true;
                        }
                    }
                });
            ui.end_row();

            ui.label(i18n.t("field-style"));
            egui::ComboBox::from_id_salt("vis_spark_style")
                .selected_text(format!("{style:?}"))
                .show_ui(ui, |ui| {
                    for val in [
                        SparklineStyle::Area,
                        SparklineStyle::Line,
                        SparklineStyle::Bars,
                    ] {
                        if ui
                            .selectable_value(style, val, format!("{val:?}"))
                            .changed()
                        {
                            changed = true;
                        }
                    }
                });
            ui.end_row();

            ui.label(i18n.t("field-window-sec"));
            if ui
                .add(egui::Slider::new(window_sec, 10..=60).suffix(" s"))
                .changed()
            {
                changed = true;
            }
            ui.end_row();

            ui.label(i18n.t("field-scale-mode"));
            if edit_sparkline_scale(ui, "vis_spark_scale", scale, i18n) {
                changed = true;
            }
            ui.end_row();

            ui.label(i18n.t("field-fill-alpha"));
            if ui.add(egui::Slider::new(fill_alpha, 0.0..=1.0)).changed() {
                changed = true;
            }
            ui.end_row();

            ui.label(i18n.t("field-stroke-width"));
            if ui.add(egui::Slider::new(stroke_width, 0.5..=3.0)).changed() {
                changed = true;
            }
            ui.end_row();

            if *placement != SparklinePlacement::Background {
                ui.label(i18n.t("field-graph-width"));
                if ui
                    .add(egui::Slider::new(graph_width, 16.0..=120.0))
                    .changed()
                {
                    changed = true;
                }
                ui.end_row();
            }
        }
        ItemVisual::TextProgress {
            style,
            thickness,
            track_alpha,
            bar_width,
        } => {
            ui.label(i18n.t("field-style"));
            egui::ComboBox::from_id_salt("vis_prog_style")
                .selected_text(format!("{style:?}"))
                .show_ui(ui, |ui| {
                    for val in [
                        ProgressStyle::BarBottom,
                        ProgressStyle::BarBackground,
                        ProgressStyle::BarInline,
                        ProgressStyle::RadialRing,
                    ] {
                        if ui
                            .selectable_value(style, val, format!("{val:?}"))
                            .changed()
                        {
                            changed = true;
                        }
                    }
                });
            ui.end_row();

            ui.label(i18n.t("field-bar-thickness"));
            if ui.add(egui::Slider::new(thickness, 1.0..=8.0)).changed() {
                changed = true;
            }
            ui.end_row();

            ui.label(i18n.t("field-track-alpha"));
            if ui.add(egui::Slider::new(track_alpha, 0.0..=1.0)).changed() {
                changed = true;
            }
            ui.end_row();

            ui.label(i18n.t("field-bar-width"));
            if ui.add(egui::Slider::new(bar_width, 12.0..=80.0)).changed() {
                changed = true;
            }
            ui.end_row();
        }
        ItemVisual::GraphOnly {
            style,
            window_sec,
            scale,
            fill_alpha,
            stroke_width,
            dashed_border,
            secondary_color,
            width,
        } => {
            ui.label(i18n.t("field-style"));
            egui::ComboBox::from_id_salt("vis_graph_style")
                .selected_text(format!("{style:?}"))
                .show_ui(ui, |ui| {
                    for val in [
                        SparklineStyle::Area,
                        SparklineStyle::Line,
                        SparklineStyle::Bars,
                    ] {
                        if ui
                            .selectable_value(style, val, format!("{val:?}"))
                            .changed()
                        {
                            changed = true;
                        }
                    }
                });
            ui.end_row();

            ui.label(i18n.t("field-window-sec"));
            if ui
                .add(egui::Slider::new(window_sec, 10..=60).suffix(" s"))
                .changed()
            {
                changed = true;
            }
            ui.end_row();

            ui.label(i18n.t("field-scale-mode"));
            if edit_sparkline_scale(ui, "vis_graph_scale", scale, i18n) {
                changed = true;
            }
            ui.end_row();

            ui.label(i18n.t("field-fill-alpha"));
            if ui.add(egui::Slider::new(fill_alpha, 0.0..=1.0)).changed() {
                changed = true;
            }
            ui.end_row();

            ui.label(i18n.t("field-stroke-width"));
            if ui.add(egui::Slider::new(stroke_width, 0.5..=3.0)).changed() {
                changed = true;
            }
            ui.end_row();

            ui.label(i18n.t("field-graph-width"));
            if ui.add(egui::Slider::new(width, 20.0..=160.0)).changed() {
                changed = true;
            }
            ui.end_row();

            ui.label(i18n.t("field-dashed-border"));
            if ui.checkbox(dashed_border, "").changed() {
                changed = true;
            }
            ui.end_row();

            ui.label("Secondary color");
            let mut has_sec = secondary_color.is_some();
            ui.horizontal(|ui| {
                if ui.checkbox(&mut has_sec, "").changed() {
                    *secondary_color =
                        has_sec.then_some(ColorSpec::Static(Rgba::rgb(0x60, 0xCD, 0xFF)));
                    changed = true;
                }
                if let Some(spec) = secondary_color
                    && color_spec_editor(ui, "vis_graph_sec_col", spec)
                {
                    changed = true;
                }
            });
            ui.end_row();
        }
    }
    changed
}

fn edit_sparkline_scale(
    ui: &mut egui::Ui,
    id_salt: &str,
    scale: &mut SparklineScale,
    i18n: &SettingsI18n,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let is_fixed = matches!(scale, SparklineScale::Fixed { .. });
        egui::ComboBox::from_id_salt(id_salt)
            .selected_text(if is_fixed {
                i18n.t("scale-fixed")
            } else {
                i18n.t("scale-auto")
            })
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(!is_fixed, i18n.t("scale-auto"))
                    .clicked()
                    && is_fixed
                {
                    *scale = SparklineScale::AutoDecay;
                    changed = true;
                }
                if ui
                    .selectable_label(is_fixed, i18n.t("scale-fixed"))
                    .clicked()
                    && !is_fixed
                {
                    *scale = SparklineScale::Fixed {
                        min: 0.0,
                        max: 100.0,
                    };
                    changed = true;
                }
            });
        if let SparklineScale::Fixed { min, max } = scale {
            if ui.add(egui::DragValue::new(min).speed(1.0)).changed() {
                changed = true;
            }
            ui.label("..");
            if ui.add(egui::DragValue::new(max).speed(1.0)).changed() {
                changed = true;
            }
        }
    });
    changed
}

fn color_rule_kind_index(rule: &Option<ColorRule>) -> usize {
    match rule {
        None => 0,
        Some(ColorRule::Static { .. }) => 1,
        Some(ColorRule::Steps { .. }) => 2,
        Some(ColorRule::Gradient { .. }) => 3,
    }
}

fn default_color_rule_for_kind(idx: usize) -> Option<ColorRule> {
    match idx {
        1 => Some(ColorRule::Static {
            color: ColorSpec::Static(Rgba::rgb(0x7F, 0xD3, 0x7F)),
        }),
        2 => Some(ColorRule::Steps {
            stops: vec![
                ColorStop {
                    at: 0.0,
                    color: ColorSpec::Static(Rgba::rgb(0x7F, 0xD3, 0x7F)),
                },
                ColorStop {
                    at: 70.0,
                    color: ColorSpec::Static(Rgba::rgb(0xF0, 0xC6, 0x74)),
                },
                ColorStop {
                    at: 85.0,
                    color: ColorSpec::Static(Rgba::rgb(0xF2, 0x6D, 0x6D)),
                },
            ],
        }),
        3 => Some(ColorRule::Gradient {
            space: ColorSpace::Oklab,
            stops: vec![
                ColorStop {
                    at: 40.0,
                    color: ColorSpec::Static(Rgba::rgb(0x7F, 0xD3, 0x7F)),
                },
                ColorStop {
                    at: 70.0,
                    color: ColorSpec::Static(Rgba::rgb(0xF0, 0xC6, 0x74)),
                },
                ColorStop {
                    at: 85.0,
                    color: ColorSpec::Static(Rgba::rgb(0xF2, 0x6D, 0x6D)),
                },
            ],
        }),
        _ => None,
    }
}

fn edit_color_rule_details(
    ui: &mut egui::Ui,
    rule: &mut Option<ColorRule>,
    i18n: &SettingsI18n,
) -> bool {
    let mut changed = false;
    match rule {
        None => {}
        Some(ColorRule::Static { color }) => {
            ui.label(i18n.t("field-color"));
            if color_spec_editor(ui, "item_static_color", color) {
                changed = true;
            }
            ui.end_row();
        }
        Some(ColorRule::Steps { stops }) => {
            ui.label("Stops");
            if edit_color_stops(ui, stops, i18n) {
                changed = true;
            }
            ui.end_row();
        }
        Some(ColorRule::Gradient { space, stops }) => {
            ui.label(i18n.t("field-color-space"));
            egui::ComboBox::from_id_salt("item_grad_space")
                .selected_text(format!("{space:?}"))
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(space, ColorSpace::Oklab, "OKLab")
                        .changed()
                    {
                        changed = true;
                    }
                    if ui
                        .selectable_value(space, ColorSpace::Srgb, "sRGB")
                        .changed()
                    {
                        changed = true;
                    }
                });
            ui.end_row();

            ui.label("Stops");
            if edit_color_stops(ui, stops, i18n) {
                changed = true;
            }
            ui.end_row();
        }
    }
    changed
}

fn edit_color_stops(ui: &mut egui::Ui, stops: &mut Vec<ColorStop>, i18n: &SettingsI18n) -> bool {
    let mut changed = false;
    let mut remove_idx = None;
    let len = stops.len();

    ui.vertical(|ui| {
        for (i, stop) in stops.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label("≥");
                if ui
                    .add(
                        egui::DragValue::new(&mut stop.at)
                            .speed(1.0)
                            .range(0.0..=150.0),
                    )
                    .changed()
                {
                    changed = true;
                }
                match &mut stop.color {
                    ColorSpec::Static(rgba) => {
                        if rgba_picker(ui, rgba) {
                            changed = true;
                        }
                        ui.weak(rgba.to_hex());
                    }
                    _ => {
                        let mut rgba = Rgba::rgb(0x7F, 0xD3, 0x7F);
                        if rgba_picker(ui, &mut rgba) {
                            stop.color = ColorSpec::Static(rgba);
                            changed = true;
                        }
                    }
                }
                if len > 1 && ui.small_button("✕").clicked() {
                    remove_idx = Some(i);
                }
            });
        }
        if ui.small_button(i18n.t("color-stop-add")).clicked() {
            let next_at = stops
                .last()
                .map(|s| (s.at + 10.0).min(100.0))
                .unwrap_or(50.0);
            stops.push(ColorStop {
                at: next_at,
                color: ColorSpec::Static(Rgba::rgb(0xF2, 0x6D, 0x6D)),
            });
            changed = true;
        }
    });

    if let Some(rm) = remove_idx {
        stops.remove(rm);
        changed = true;
    }
    changed
}

fn sync_legacy_item_color(item: &mut ItemConfig) {
    item.color = match &item.color_rule {
        Some(ColorRule::Static {
            color: ColorSpec::Static(rgba),
        }) => Some(rgba.to_hex()),
        _ => None,
    };
}

fn preview_swatch_color(item: &ItemConfig) -> Color32 {
    let rgba = match &item.color_rule {
        Some(ColorRule::Static {
            color: ColorSpec::Static(c),
        }) => *c,
        Some(ColorRule::Steps { stops }) | Some(ColorRule::Gradient { stops, .. }) => stops
            .first()
            .and_then(|s| match s.color {
                ColorSpec::Static(c) => Some(c),
                _ => None,
            })
            .unwrap_or(Rgba::rgb(0x90, 0xB8, 0xE8)),
        _ => item
            .color
            .as_deref()
            .and_then(Rgba::parse_hex)
            .unwrap_or(Rgba::rgb(0xD8, 0xDC, 0xE6)),
    };
    Color32::from_rgba_unmultiplied(rgba.r, rgba.g, rgba.b, rgba.a)
}
