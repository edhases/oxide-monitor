//! Tab 1: Built-in Presets gallery + complete `GeneralConfig` editor (no HWiNFO/LHM toggles).

use eframe::egui::{self, Color32, RichText};
use oxide_core::{AUTOSTART_TASK_NAME, AutostartStatusFile, CpuTempSource, builtin_presets};

use crate::i18n::SettingsI18n;
use crate::state::SettingsState;
use crate::theme::card_frame;
use crate::win32_host::BackdropKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutostartStatusTone {
    Ok,
    Warning,
    Muted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutostartStatusView {
    pub tone: AutostartStatusTone,
    pub message: String,
    pub hint: Option<String>,
    pub tooltip: Option<String>,
}

/// Map the versioned `autostart.toml` sidecar status into a localized UI presentation.
pub fn map_autostart_status(
    file: Option<&AutostartStatusFile>,
    i18n: &SettingsI18n,
) -> AutostartStatusView {
    let Some(file) = file else {
        return AutostartStatusView {
            tone: AutostartStatusTone::Muted,
            message: i18n.t("autostart-status-unavailable"),
            hint: None,
            tooltip: None,
        };
    };

    let s = &file.status;
    let task_name = if s.task_name.trim().is_empty() {
        AUTOSTART_TASK_NAME
    } else {
        s.task_name.as_str()
    };

    let mut tip_lines = vec![i18n.t_args("autostart-tooltip-task", &[("task", task_name)])];
    if file.updated_at_unix_ms > 0 {
        let ts = format_unix_ms_utc(file.updated_at_unix_ms);
        tip_lines.push(i18n.t_args("autostart-tooltip-updated", &[("time", &ts)]));
    }
    if let Some(err) = s.last_error.as_deref().filter(|e| !e.trim().is_empty()) {
        tip_lines.push(i18n.t_args("autostart-tooltip-error", &[("error", err)]));
    }
    let tooltip = Some(tip_lines.join("\n"));

    if s.desired_enabled && s.enabled && s.last_error.is_none() {
        AutostartStatusView {
            tone: AutostartStatusTone::Ok,
            message: format!(
                "✓ {}",
                i18n.t_args("autostart-status-enabled", &[("task", task_name)])
            ),
            hint: None,
            tooltip,
        }
    } else if s.desired_enabled {
        let err_text = s
            .last_error
            .as_deref()
            .filter(|e| !e.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| i18n.t("autostart-status-error-unknown"));
        AutostartStatusView {
            tone: AutostartStatusTone::Warning,
            message: format!(
                "⚠ {}",
                i18n.t_args("autostart-status-error", &[("error", &err_text)])
            ),
            hint: Some(i18n.t("autostart-status-retry-hint")),
            tooltip,
        }
    } else if s.enabled {
        AutostartStatusView {
            tone: AutostartStatusTone::Warning,
            message: format!("⚠ {}", i18n.t("autostart-status-still-registered")),
            hint: None,
            tooltip,
        }
    } else {
        AutostartStatusView {
            tone: AutostartStatusTone::Muted,
            message: i18n.t("autostart-status-disabled"),
            hint: None,
            tooltip,
        }
    }
}

/// Format a UNIX millisecond timestamp as `YYYY-MM-DD HH:MM:SS UTC` without external date crates.
pub fn format_unix_ms_utc(unix_ms: u64) -> String {
    let total_secs = unix_ms / 1000;
    let secs_of_day = total_secs % 86_400;
    let days_since_epoch = (total_secs / 86_400) as i64;

    // Howard Hinnant's civil_from_days algorithm (Gregorian calendar).
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(month <= 2);

    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

pub fn ui(
    ui: &mut egui::Ui,
    state: &mut SettingsState,
    i18n: &SettingsI18n,
    dark: bool,
    backdrop: BackdropKind,
) -> bool {
    let mut changed = false;

    // 1. Built-in Presets Gallery
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-presets"));
        ui.weak(i18n.t("presets-hint"));
        ui.add_space(6.0);

        let presets = builtin_presets();
        for preset in presets {
            let id = preset.preset.id.as_str();
            let is_active = state.draft.general.active_preset.eq_ignore_ascii_case(id);
            let title = i18n.t(&format!("preset-{id}"));
            let desc = i18n.t(&format!("preset-{id}-desc"));

            egui::Frame::NONE
                .fill(if is_active {
                    ui.visuals().selection.bg_fill
                } else {
                    egui::Color32::TRANSPARENT
                })
                .stroke(if is_active {
                    ui.visuals().selection.stroke
                } else {
                    ui.visuals().widgets.noninteractive.bg_stroke
                })
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(10, 7))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.strong(&title);
                            ui.weak(&desc);
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if is_active {
                                ui.strong(i18n.t("preset-active"));
                            } else if ui.button(i18n.t("preset-apply")).clicked()
                                && state.apply_preset_preview(id)
                            {
                                changed = true;
                            }
                        });
                    });
                });
            ui.add_space(4.0);
        }
    });

    ui.add_space(10.0);

    // 2. General Settings (`GeneralConfig`)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-general"));
        ui.add_space(6.0);

        egui::Grid::new("general_settings_grid")
            .num_columns(2)
            .spacing([18.0, 8.0])
            .min_col_width(220.0)
            .show(ui, |ui| {
                // Language
                ui.label(i18n.t("field-language"));
                let cur_lang = state.draft.general.language.clone();
                let lang_label = match cur_lang.as_str() {
                    "uk" => "Українська (uk)",
                    "ru" => "Русский (ru)",
                    _ => "English (en)",
                };
                egui::ComboBox::from_id_salt("general_language")
                    .selected_text(lang_label)
                    .show_ui(ui, |ui| {
                        for (code, name) in [
                            ("uk", "Українська (uk)"),
                            ("en", "English (en)"),
                            ("ru", "Русский (ru)"),
                        ] {
                            if ui
                                .selectable_value(
                                    &mut state.draft.general.language,
                                    code.into(),
                                    name,
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
                ui.end_row();

                // Theme
                ui.label(i18n.t("field-theme"));
                let theme_label = match state.draft.general.theme.as_str() {
                    "auto" => i18n.t("theme-auto"),
                    "light" => i18n.t("theme-light"),
                    _ => i18n.t("theme-dark"),
                };
                egui::ComboBox::from_id_salt("general_theme")
                    .selected_text(theme_label)
                    .show_ui(ui, |ui| {
                        for (code, key) in [
                            ("auto", "theme-auto"),
                            ("dark", "theme-dark"),
                            ("light", "theme-light"),
                        ] {
                            if ui
                                .selectable_value(
                                    &mut state.draft.general.theme,
                                    code.into(),
                                    i18n.t(key),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
                ui.end_row();

                // Autostart (Windows Task Scheduler) + sidecar status
                ui.label(i18n.t("field-autostart"));
                ui.vertical(|ui| {
                    if ui
                        .checkbox(&mut state.draft.general.autostart, "")
                        .changed()
                    {
                        changed = true;
                    }
                    let view = map_autostart_status(state.autostart_status.as_ref(), i18n);
                    let status_resp = match view.tone {
                        AutostartStatusTone::Ok => {
                            let color = if dark {
                                Color32::from_rgb(127, 211, 127)
                            } else {
                                Color32::from_rgb(22, 130, 56)
                            };
                            ui.label(RichText::new(&view.message).small().color(color))
                        }
                        AutostartStatusTone::Warning => {
                            let color = if dark {
                                Color32::from_rgb(240, 198, 116)
                            } else {
                                Color32::from_rgb(168, 104, 0)
                            };
                            ui.label(RichText::new(&view.message).small().color(color))
                        }
                        AutostartStatusTone::Muted => ui.weak(RichText::new(&view.message).small()),
                    };
                    if let Some(tip) = &view.tooltip {
                        status_resp.on_hover_text(tip);
                    }
                    if let Some(hint) = &view.hint {
                        ui.weak(RichText::new(hint).small());
                    }
                });
                ui.end_row();

                // Poll interval ms
                ui.label(i18n.t("field-poll-interval"));
                if ui
                    .add(
                        egui::Slider::new(&mut state.draft.general.poll_interval_ms, 500..=5_000)
                            .step_by(100.0)
                            .suffix(" ms"),
                    )
                    .changed()
                {
                    changed = true;
                }
                ui.end_row();

                // GPU poll interval ms
                ui.label(i18n.t("field-gpu-poll"));
                if ui
                    .add(
                        egui::Slider::new(&mut state.draft.general.gpu_poll_ms, 1_000..=60_000)
                            .step_by(500.0)
                            .suffix(" ms"),
                    )
                    .changed()
                {
                    changed = true;
                }
                ui.end_row();

                // CPU temperature source
                ui.label(i18n.t("field-cpu-temp-source"));
                let cpu_src_label = match state.draft.general.cpu_temp_source {
                    CpuTempSource::Auto => i18n.t("cpu-temp-auto"),
                    CpuTempSource::Acpi => i18n.t("cpu-temp-acpi"),
                    CpuTempSource::Provider => i18n.t("cpu-temp-provider"),
                    CpuTempSource::Off => i18n.t("cpu-temp-off"),
                };
                egui::ComboBox::from_id_salt("general_cpu_temp_source")
                    .selected_text(cpu_src_label)
                    .show_ui(ui, |ui| {
                        for (val, key) in [
                            (CpuTempSource::Auto, "cpu-temp-auto"),
                            (CpuTempSource::Acpi, "cpu-temp-acpi"),
                            (CpuTempSource::Provider, "cpu-temp-provider"),
                            (CpuTempSource::Off, "cpu-temp-off"),
                        ] {
                            if ui
                                .selectable_value(
                                    &mut state.draft.general.cpu_temp_source,
                                    val,
                                    i18n.t(key),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
                ui.end_row();
            });
    });

    ui.add_space(10.0);

    // 3. Preferred Hardware Targets (`disk_index`, `adapter`, `adapter_luid`, `default_gpu_luid`)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-hardware-targets"));
        ui.add_space(6.0);

        egui::Grid::new("general_hw_targets_grid")
            .num_columns(2)
            .spacing([18.0, 8.0])
            .min_col_width(220.0)
            .show(ui, |ui| {
                // Primary disk index
                ui.label(i18n.t("field-primary-disk"));
                let disk_label = match state.draft.general.disk_index {
                    None => i18n.t("field-auto"),
                    Some(idx) => {
                        let name = state
                            .inventory
                            .disks
                            .iter()
                            .find(|d| d.index == idx)
                            .map(|d| format!("#{idx} — {}", d.name))
                            .unwrap_or_else(|| format!("#{idx}"));
                        name
                    }
                };
                egui::ComboBox::from_id_salt("general_disk_index")
                    .selected_text(disk_label)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(
                                &mut state.draft.general.disk_index,
                                None,
                                i18n.t("field-auto"),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        for d in &state.inventory.disks {
                            let label = format!("#{} — {}", d.index, d.name);
                            if ui
                                .selectable_value(
                                    &mut state.draft.general.disk_index,
                                    Some(d.index),
                                    label,
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
                ui.end_row();

                // Primary network adapter
                ui.label(i18n.t("field-primary-adapter"));
                let mut adapter_text = state.draft.general.adapter.clone().unwrap_or_default();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut adapter_text)
                            .hint_text(i18n.t("field-auto"))
                            .desired_width(220.0),
                    )
                    .changed()
                {
                    let trimmed = adapter_text.trim().to_string();
                    state.draft.general.adapter = if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed)
                    };
                    state.draft.general.adapter_luid = None;
                    changed = true;
                }
                ui.end_row();

                // Preferred primary GPU LUID
                ui.label(i18n.t("field-primary-gpu-luid"));
                let gpu_label = match state.draft.general.default_gpu_luid {
                    None => i18n.t("field-auto"),
                    Some(luid) => state
                        .inventory
                        .gpus
                        .iter()
                        .find(|g| g.luid == Some(luid))
                        .map(|g| format!("{} (0x{luid:X})", g.name))
                        .unwrap_or_else(|| format!("0x{luid:X}")),
                };
                egui::ComboBox::from_id_salt("general_gpu_luid")
                    .selected_text(gpu_label)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(
                                &mut state.draft.general.default_gpu_luid,
                                None,
                                i18n.t("field-auto"),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        for g in &state.inventory.gpus {
                            if let Some(luid) = g.luid {
                                let label = format!("{} (0x{luid:X})", g.name);
                                if ui
                                    .selectable_value(
                                        &mut state.draft.general.default_gpu_luid,
                                        Some(luid),
                                        label,
                                    )
                                    .changed()
                                {
                                    changed = true;
                                }
                            }
                        }
                    });
                ui.end_row();
            });
    });

    ui.add_space(10.0);

    // 4. Configuration File Info
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-config-info"));
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{}:", i18n.t("field-config-path")));
            ui.monospace(state.config_path.display().to_string());
        });
        ui.horizontal(|ui| {
            ui.label(format!("{}:", i18n.t("field-portable-mode")));
            ui.strong(if state.portable_mode {
                i18n.t("yes")
            } else {
                i18n.t("no")
            });
            ui.separator();
            ui.weak(format!("rev #{}", state.generation));
        });
    });

    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide_core::AutostartStatus;
    use std::path::PathBuf;

    fn status_file(
        desired_enabled: bool,
        enabled: bool,
        last_error: Option<&str>,
        updated_at_unix_ms: u64,
    ) -> AutostartStatusFile {
        AutostartStatusFile {
            version: 1,
            updated_at_unix_ms,
            status: AutostartStatus {
                desired_enabled,
                enabled,
                task_name: AUTOSTART_TASK_NAME.into(),
                last_error: last_error.map(str::to_string),
            },
        }
    }

    #[test]
    fn autostart_status_mapping_covers_all_five_states_and_locales() {
        let uk = SettingsI18n::new("uk");
        let en = SettingsI18n::new("en");
        let ru = SettingsI18n::new("ru");

        // 1) File missing -> unavailable
        let missing = map_autostart_status(None, &uk);
        assert_eq!(missing.tone, AutostartStatusTone::Muted);
        assert_eq!(
            missing.message,
            "Статус недоступний (запустіть oxide-monitor)"
        );
        assert!(missing.hint.is_none());
        assert!(missing.tooltip.is_none());

        // 2) enabled && no error -> green OK with task name & tooltip
        let ok_file = status_file(true, true, None, 1_700_000_000_000);
        let ok_uk = map_autostart_status(Some(&ok_file), &uk);
        assert_eq!(ok_uk.tone, AutostartStatusTone::Ok);
        assert!(ok_uk.message.contains("Oxide Monitor"));
        assert!(ok_uk.hint.is_none());
        let tip = ok_uk.tooltip.expect("tooltip present");
        assert!(tip.contains("Oxide Monitor"));
        assert!(tip.contains("2023-11-14 22:13:20 UTC"));

        let ok_en = map_autostart_status(Some(&ok_file), &en);
        assert_eq!(ok_en.tone, AutostartStatusTone::Ok);
        assert!(ok_en.message.contains("scheduled task"));

        let ok_ru = map_autostart_status(Some(&ok_file), &ru);
        assert_eq!(ok_ru.tone, AutostartStatusTone::Ok);
        assert!(ok_ru.message.contains("задача планировщика"));

        // 3) desired_enabled && !enabled -> warning with last_error + retry hint
        let err_file = status_file(true, false, Some("Access is denied (0x80070005)"), 0);
        let err_uk = map_autostart_status(Some(&err_file), &uk);
        assert_eq!(err_uk.tone, AutostartStatusTone::Warning);
        assert!(err_uk.message.contains("Access is denied (0x80070005)"));
        assert!(err_uk.hint.is_some());
        assert!(
            err_uk
                .tooltip
                .as_deref()
                .unwrap()
                .contains("Access is denied (0x80070005)")
        );

        // 3b) desired_enabled && !enabled with None error falls back to unknown error text
        let err_none = status_file(true, false, None, 0);
        let err_en = map_autostart_status(Some(&err_none), &en);
        assert_eq!(err_en.tone, AutostartStatusTone::Warning);
        assert!(err_en.message.contains("unknown error"));
        assert!(err_en.hint.is_some());

        // 4) !desired_enabled && enabled -> warning still registered
        let stale_file = status_file(false, true, None, 1_700_000_000_000);
        let stale_uk = map_autostart_status(Some(&stale_file), &uk);
        assert_eq!(stale_uk.tone, AutostartStatusTone::Warning);
        assert!(stale_uk.message.contains("Задача ще зареєстрована"));
        assert!(stale_uk.hint.is_none());

        // 5) !desired_enabled && !enabled -> disabled (muted)
        let off_file = status_file(false, false, None, 1_700_000_000_000);
        let off_uk = map_autostart_status(Some(&off_file), &uk);
        assert_eq!(off_uk.tone, AutostartStatusTone::Muted);
        assert_eq!(off_uk.message, "Автозапуск вимкнено");
        assert!(off_uk.hint.is_none());
    }

    #[test]
    fn format_unix_ms_utc_formats_epoch_and_modern_timestamp() {
        assert_eq!(format_unix_ms_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(
            format_unix_ms_utc(1_700_000_000_000),
            "2023-11-14 22:13:20 UTC"
        );
    }

    #[test]
    fn headless_egui_general_tab_renders_all_autostart_states_cleanly() {
        let mut state = SettingsState::load_local(PathBuf::from("nonexistent_general_test.toml"));
        let i18n = SettingsI18n::new("uk");
        let ctx = egui::Context::default();

        let cases = [
            None,
            Some(status_file(true, true, None, 1_700_000_000_000)),
            Some(status_file(
                true,
                false,
                Some("schtasks rc=1"),
                1_700_000_000_000,
            )),
            Some(status_file(false, true, None, 1_700_000_000_000)),
            Some(status_file(false, false, None, 1_700_000_000_000)),
        ];

        for (idx, status) in cases.into_iter().enumerate() {
            state.autostart_status = status;
            let dark = idx % 2 == 0;
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let changed =
                        super::ui(ui, &mut state, &i18n, dark, BackdropKind::SolidFallback);
                    assert!(!changed, "passive render must not mutate draft");
                });
            });
            out.textures_delta.clear();
        }
    }
}
