//! Tab: External Temperature & Sensor Sources (`HWiNFO`, `LibreHardwareMonitor`, `PawnIO`).
//!
//! Exposes provider toggles, LHM loopback URL configuration with strict local validation,
//! LHM auto-update settings, live health badges, and official version/update status for LHM and PawnIO.

use eframe::egui::{self, Color32, CornerRadius, RichText, Stroke};
use oxide_core::ProviderHealth;
use oxide_core::inventory::ProviderStatus;
use oxide_core::pawnio::{PawnioState, PawnioStatus};
use oxide_core::provider_update::{
    LHM_OFFICIAL_URL, PAWNIO_OFFICIAL_URL, ProviderInstallKind, ProviderUpdateState,
    ProviderUpdateStatus,
};
use oxide_core::{FormatOptions, format_temp_parts};

use crate::i18n::SettingsI18n;
use crate::state::SettingsState;
use crate::theme::card_frame;
use crate::win32_host::BackdropKind;

/// Strict loopback validator for LibreHardwareMonitor HTTP endpoint.
///
/// Refuses non-HTTP protocols, remote hosts, empty strings, and malformed IPv6 brackets.
/// Only plain `http://` loopback hosts (`localhost`, `127.0.0.1`, `::1`, `[::1]`) are allowed.
pub fn validate_lhm_url(url: &str) -> Result<(), &'static str> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("URL cannot be empty");
    }
    if !trimmed.starts_with("http://") {
        return Err("Only plain http:// URLs are supported");
    }
    let rest = &trimmed["http://".len()..];
    let host_and_port = rest.split('/').next().unwrap_or("");
    if host_and_port.is_empty() {
        return Err("Missing host");
    }
    let host = if host_and_port.starts_with('[') {
        let end_bracket = host_and_port.find(']').ok_or("Unclosed IPv6 bracket")?;
        &host_and_port[1..end_bracket]
    } else if host_and_port == "::1" {
        "::1"
    } else if let Some(last_colon) = host_and_port.rfind(':') {
        let maybe_port = &host_and_port[last_colon + 1..];
        let prefix = &host_and_port[..last_colon];
        if maybe_port.chars().all(|c| c.is_ascii_digit())
            && !maybe_port.is_empty()
            && (prefix == "::1" || !prefix.contains(':'))
        {
            prefix
        } else {
            host_and_port
        }
    } else {
        host_and_port
    };

    let normalized_host = host.trim_start_matches('[').trim_end_matches(']');
    if normalized_host.eq_ignore_ascii_case("localhost")
        || normalized_host == "127.0.0.1"
        || normalized_host == "::1"
    {
        Ok(())
    } else {
        Err("Only loopback hosts (localhost, 127.0.0.1, ::1) are allowed")
    }
}

/// Visual tone for the provider health badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderTone {
    Connected,
    Warning,
    Error,
    Disabled,
}

/// Rendered view model for a single provider status row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderStatusView {
    pub tone: ProviderTone,
    pub badge_text: String,
    pub detail_text: Option<String>,
    pub age_text: Option<String>,
}

/// Format the age of the last reading into a human-readable localized string.
pub fn format_age(age_ms: Option<u64>, i18n: &SettingsI18n) -> Option<String> {
    let ms = age_ms?;
    if ms < 1_500 {
        Some(i18n.t("provider-age-now"))
    } else if ms < 60_000 {
        let secs = ms / 1000;
        Some(i18n.t_args("provider-age-secs", &[("secs", &secs.to_string())]))
    } else {
        let mins = ms / 60_000;
        Some(i18n.t_args("provider-age-mins", &[("mins", &mins.to_string())]))
    }
}

/// Map `ProviderStatus` into a badge tone, localized badge label, detail, and age string.
pub fn map_provider_status(status: &ProviderStatus, i18n: &SettingsI18n) -> ProviderStatusView {
    let age_text = format_age(status.last_update_age_ms, i18n);

    if !status.enabled || status.health == ProviderHealth::Disabled {
        return ProviderStatusView {
            tone: ProviderTone::Disabled,
            badge_text: i18n.t("provider-status-disabled"),
            detail_text: (!status.detail.trim().is_empty()).then(|| status.detail.clone()),
            age_text: None,
        };
    }

    match status.health {
        ProviderHealth::Disabled => ProviderStatusView {
            tone: ProviderTone::Disabled,
            badge_text: i18n.t("provider-status-disabled"),
            detail_text: None,
            age_text: None,
        },
        ProviderHealth::Connected => ProviderStatusView {
            tone: ProviderTone::Connected,
            badge_text: i18n.t_args(
                "provider-status-connected",
                &[("count", &status.sensor_count.to_string())],
            ),
            detail_text: (!status.detail.trim().is_empty()).then(|| status.detail.clone()),
            age_text,
        },
        ProviderHealth::NotRunning => ProviderStatusView {
            tone: ProviderTone::Warning,
            badge_text: i18n.t("provider-status-not-running"),
            detail_text: (!status.detail.trim().is_empty()).then(|| status.detail.clone()),
            age_text,
        },
        ProviderHealth::Stale => ProviderStatusView {
            tone: ProviderTone::Warning,
            badge_text: i18n.t("provider-status-stale"),
            detail_text: (!status.detail.trim().is_empty()).then(|| status.detail.clone()),
            age_text,
        },
        ProviderHealth::Error | ProviderHealth::LimitExpired => {
            let badge_text = if status.detail.trim().is_empty() {
                i18n.t("provider-status-error")
            } else {
                i18n.t_args(
                    "provider-status-error-detail",
                    &[("detail", &status.detail)],
                )
            };
            ProviderStatusView {
                tone: ProviderTone::Error,
                badge_text,
                detail_text: None,
                age_text,
            }
        }
    }
}

/// Visual tone for the component update badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateTone {
    Ok,
    Action,
    Warning,
    Error,
    Muted,
}

/// Rendered view model for an update status row (LHM / PawnIO).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderUpdateView {
    pub tone: UpdateTone,
    pub badge_text: String,
    pub detail_text: Option<String>,
    pub hint_text: Option<String>,
    pub version_summary: String,
    pub install_kind_text: String,
}

/// Map `ProviderUpdateStatus` into a badge tone, localized labels, and version summary.
pub fn map_provider_update_status(
    status: &ProviderUpdateStatus,
    i18n: &SettingsI18n,
) -> ProviderUpdateView {
    let (tone, badge_text, detail_text, hint_text) = match status.state {
        ProviderUpdateState::UpToDate => (
            UpdateTone::Ok,
            i18n.t("update-state-up-to-date"),
            (!status.detail.trim().is_empty()).then(|| status.detail.clone()),
            None,
        ),
        ProviderUpdateState::UpdateAvailable => (
            UpdateTone::Warning,
            i18n.t("update-state-available"),
            (!status.detail.trim().is_empty()).then(|| status.detail.clone()),
            None,
        ),
        ProviderUpdateState::UpdateReady => {
            let path_str = status.staged_path.as_deref().unwrap_or("staged");
            (
                UpdateTone::Action,
                i18n.t_args("update-state-ready", &[("path", path_str)]),
                (!status.detail.trim().is_empty()).then(|| status.detail.clone()),
                Some(i18n.t("update-state-ready-hint")),
            )
        }
        ProviderUpdateState::Updating => (
            UpdateTone::Warning,
            i18n.t("update-state-updating"),
            (!status.detail.trim().is_empty()).then(|| status.detail.clone()),
            None,
        ),
        ProviderUpdateState::Error => {
            let err = status
                .last_error
                .as_deref()
                .filter(|e| !e.trim().is_empty())
                .unwrap_or(if status.detail.is_empty() {
                    "unknown"
                } else {
                    &status.detail
                });
            (
                UpdateTone::Error,
                i18n.t_args("update-state-error", &[("error", err)]),
                None,
                None,
            )
        }
        ProviderUpdateState::Unknown => (
            UpdateTone::Muted,
            i18n.t("update-state-unknown"),
            (!status.detail.trim().is_empty()).then(|| status.detail.clone()),
            None,
        ),
    };

    let installed_str = status.installed_version.as_deref().unwrap_or("—");
    let latest_str = status.latest_version.as_deref().unwrap_or("—");
    let version_summary = format!(
        "{}: {}  ·  {}: {}",
        i18n.t("update-installed"),
        installed_str,
        i18n.t("update-latest"),
        latest_str
    );

    let install_kind_text = match status.install_kind {
        ProviderInstallKind::Managed => format!("({})", i18n.t("update-kind-managed")),
        ProviderInstallKind::External => format!("({})", i18n.t("update-kind-external")),
        ProviderInstallKind::Driver => format!("({})", i18n.t("update-kind-driver")),
        ProviderInstallKind::None => format!("({})", i18n.t("update-kind-none")),
    };

    ProviderUpdateView {
        tone,
        badge_text,
        detail_text,
        hint_text,
        version_summary,
        install_kind_text,
    }
}

/// Visual tone + localized labels for the optional PawnIO helper status row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PawnioStatusView {
    pub tone: UpdateTone,
    pub badge_text: String,
    pub version_text: String,
    pub temperature_text: Option<String>,
    pub detail_text: Option<String>,
    pub error_text: Option<String>,
}

/// Localized badge key and badge tone for every `PawnioState`.
pub fn map_pawnio_state(state: PawnioState) -> (&'static str, UpdateTone) {
    match state {
        PawnioState::Disabled => ("pawnio-state-disabled", UpdateTone::Muted),
        PawnioState::NotInstalled => ("pawnio-state-not-installed", UpdateTone::Warning),
        PawnioState::Stopped => ("pawnio-state-stopped", UpdateTone::Warning),
        PawnioState::Starting => ("pawnio-state-starting", UpdateTone::Action),
        PawnioState::Running => ("pawnio-state-running", UpdateTone::Ok),
        PawnioState::Denied => ("pawnio-state-denied", UpdateTone::Error),
        PawnioState::Error => ("pawnio-state-error", UpdateTone::Error),
    }
}

/// Format the live CPU temperature, only while the helper is actually `running`.
pub fn format_pawnio_temperature(status: &PawnioStatus, opts: &FormatOptions) -> Option<String> {
    if status.state != PawnioState::Running {
        return None;
    }
    let parts = format_temp_parts(status.temperature_c, opts, "");
    Some(parts.join(opts.separate_space))
}

/// Map `PawnioStatus` into a badge tone, localized labels, versions, and temperature.
pub fn map_pawnio_status(
    status: &PawnioStatus,
    temperature_text: Option<String>,
    i18n: &SettingsI18n,
) -> PawnioStatusView {
    let (badge_key, tone) = map_pawnio_state(status.state);
    let version_text = i18n.t_args(
        "pawnio-versions",
        &[
            ("pawnio", status.installed_version.as_deref().unwrap_or("—")),
            ("helper", status.helper_version.as_deref().unwrap_or("—")),
        ],
    );

    PawnioStatusView {
        tone,
        badge_text: i18n.t(badge_key),
        version_text,
        temperature_text,
        detail_text: (!status.detail.trim().is_empty()).then(|| status.detail.clone()),
        error_text: status
            .last_error
            .as_deref()
            .filter(|err| !err.trim().is_empty())
            .map(str::to_string),
    }
}

/// Badge colors for an `UpdateTone` pill (provider updates and PawnIO).
fn update_tone_colors(
    tone: UpdateTone,
    dark: bool,
    muted_text: Color32,
) -> (Color32, Color32, Color32) {
    match tone {
        UpdateTone::Ok => (
            if dark {
                Color32::from_rgba_unmultiplied(127, 211, 127, 28)
            } else {
                Color32::from_rgba_unmultiplied(22, 130, 56, 22)
            },
            if dark {
                Color32::from_rgb(127, 211, 127)
            } else {
                Color32::from_rgb(22, 130, 56)
            },
            if dark {
                Color32::from_rgba_unmultiplied(127, 211, 127, 60)
            } else {
                Color32::from_rgba_unmultiplied(22, 130, 56, 60)
            },
        ),
        UpdateTone::Action => (
            if dark {
                Color32::from_rgba_unmultiplied(96, 205, 255, 30)
            } else {
                Color32::from_rgba_unmultiplied(0, 103, 192, 24)
            },
            if dark {
                Color32::from_rgb(96, 205, 255)
            } else {
                Color32::from_rgb(0, 103, 192)
            },
            if dark {
                Color32::from_rgba_unmultiplied(96, 205, 255, 60)
            } else {
                Color32::from_rgba_unmultiplied(0, 103, 192, 60)
            },
        ),
        UpdateTone::Warning => (
            if dark {
                Color32::from_rgba_unmultiplied(240, 198, 116, 28)
            } else {
                Color32::from_rgba_unmultiplied(168, 104, 0, 22)
            },
            if dark {
                Color32::from_rgb(240, 198, 116)
            } else {
                Color32::from_rgb(168, 104, 0)
            },
            if dark {
                Color32::from_rgba_unmultiplied(240, 198, 116, 60)
            } else {
                Color32::from_rgba_unmultiplied(168, 104, 0, 60)
            },
        ),
        UpdateTone::Error => (
            if dark {
                Color32::from_rgba_unmultiplied(242, 109, 109, 32)
            } else {
                Color32::from_rgba_unmultiplied(196, 43, 28, 24)
            },
            if dark {
                Color32::from_rgb(242, 109, 109)
            } else {
                Color32::from_rgb(196, 43, 28)
            },
            if dark {
                Color32::from_rgba_unmultiplied(242, 109, 109, 60)
            } else {
                Color32::from_rgba_unmultiplied(196, 43, 28, 60)
            },
        ),
        UpdateTone::Muted => (
            if dark {
                Color32::from_rgba_unmultiplied(255, 255, 255, 12)
            } else {
                Color32::from_rgba_unmultiplied(0, 0, 0, 12)
            },
            muted_text,
            Color32::TRANSPARENT,
        ),
    }
}

pub fn ui(
    ui: &mut egui::Ui,
    state: &mut SettingsState,
    i18n: &SettingsI18n,
    dark: bool,
    backdrop: BackdropKind,
) -> bool {
    let mut changed = false;

    // 1. Provider Configuration (`ProvidersConfig`)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-sources-config"));
        ui.weak(i18n.t("sources-explanation"));
        ui.add_space(8.0);

        let prv = &mut state.draft.providers;
        egui::Grid::new("providers_config_grid")
            .num_columns(2)
            .spacing([18.0, 10.0])
            .min_col_width(220.0)
            .show(ui, |ui| {
                // HWiNFO (Shared Memory)
                ui.label(i18n.t("field-provider-hwinfo"));
                ui.vertical(|ui| {
                    if ui.checkbox(&mut prv.hwinfo, "").changed() {
                        changed = true;
                    }
                    ui.weak(RichText::new(i18n.t("hint-provider-hwinfo")).small());
                });
                ui.end_row();

                // LibreHardwareMonitor (HTTP)
                ui.label(i18n.t("field-provider-lhm"));
                ui.vertical(|ui| {
                    if ui.checkbox(&mut prv.lhm_http, "").changed() {
                        changed = true;
                    }
                    ui.weak(RichText::new(i18n.t("hint-provider-lhm")).small());

                    if prv.lhm_http {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(format!("{}:", i18n.t("field-lhm-url")));
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut prv.lhm_url).desired_width(260.0),
                            );
                            if resp.changed() {
                                changed = true;
                            }
                        });
                        if validate_lhm_url(&prv.lhm_url).is_err() {
                            let err_col = if dark {
                                Color32::from_rgb(242, 109, 109)
                            } else {
                                Color32::from_rgb(196, 43, 28)
                            };
                            ui.label(
                                RichText::new(i18n.t("lhm-url-loopback-error"))
                                    .small()
                                    .color(err_col),
                            );
                        } else {
                            ui.weak(RichText::new(i18n.t("hint-lhm-loopback")).small());
                        }

                        // LHM Auto-update checkbox
                        ui.add_space(4.0);
                        if ui
                            .checkbox(&mut prv.lhm_auto_update, i18n.t("field-lhm-auto-update"))
                            .changed()
                        {
                            changed = true;
                        }
                        ui.weak(RichText::new(i18n.t("hint-lhm-auto-update")).small());
                    }
                });
                ui.end_row();
            });
    });

    ui.add_space(10.0);

    // 2. PawnIO (optional elevated CPU-temperature helper)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-pawnio"));
        ui.weak(i18n.t("pawnio-explanation"));
        ui.add_space(8.0);

        // Prefer live telemetry; fall back to the inventory snapshot when idle.
        let p = if state.has_live_telemetry || state.snapshot.pawnio != PawnioStatus::default() {
            state.snapshot.pawnio.clone()
        } else {
            state.inventory.pawnio.clone()
        };

        let temperature_text = format_pawnio_temperature(&p, &state.draft.format);
        let view = map_pawnio_status(&p, temperature_text, i18n);
        let (badge_bg, badge_fg, badge_stroke) =
            update_tone_colors(view.tone, dark, ui.visuals().text_color());
        let warn_col = if dark {
            Color32::from_rgb(240, 198, 116)
        } else {
            Color32::from_rgb(168, 104, 0)
        };
        let err_col = if dark {
            Color32::from_rgb(242, 109, 109)
        } else {
            Color32::from_rgb(196, 43, 28)
        };

        egui::Frame::NONE
            .fill(if dark {
                Color32::from_rgba_unmultiplied(255, 255, 255, 6)
            } else {
                Color32::from_rgba_unmultiplied(0, 0, 0, 8)
            })
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("PawnIO");

                    // Helper state badge
                    egui::Frame::NONE
                        .fill(badge_bg)
                        .stroke(Stroke::new(1.0, badge_stroke))
                        .corner_radius(CornerRadius::same(4))
                        .inner_margin(egui::Margin::symmetric(7, 3))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(&view.badge_text)
                                    .small()
                                    .color(badge_fg)
                                    .strong(),
                            );
                        });

                    // Official site only — the app never installs or updates PawnIO.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.hyperlink_to(
                            RichText::new(i18n.t("btn-official-site")).small(),
                            &p.official_url,
                        );
                    });
                });

                ui.add_space(2.0);
                ui.weak(RichText::new(&view.version_text).small());

                if let Some(temp) = &view.temperature_text {
                    ui.add_space(1.0);
                    ui.label(
                        RichText::new(i18n.t_args("pawnio-temperature", &[("value", temp)]))
                            .small()
                            .strong(),
                    );
                }
                if let Some(detail) = &view.detail_text {
                    ui.add_space(1.0);
                    ui.weak(RichText::new(detail).small());
                }
                if let Some(error) = &view.error_text {
                    ui.add_space(1.0);
                    ui.label(RichText::new(error).small().color(err_col));
                }
            });

        ui.add_space(8.0);

        // Config opt-in only: never launches the helper by itself.
        if ui
            .checkbox(
                &mut state.draft.providers.pawnio,
                i18n.t("field-provider-pawnio"),
            )
            .changed()
        {
            changed = true;
        }
        ui.weak(RichText::new(i18n.t("hint-provider-pawnio")).small());

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let can_enable =
                p.installed && !matches!(p.state, PawnioState::Starting | PawnioState::Running);
            let enable = ui.add_enabled(can_enable, egui::Button::new(i18n.t("btn-pawnio-enable")));
            if enable.clicked() {
                state.draft.providers.pawnio = true;
                changed = true;
                state.request_pawnio_control(true);
            }

            if matches!(p.state, PawnioState::Starting | PawnioState::Running)
                && ui.button(i18n.t("btn-pawnio-stop")).clicked()
            {
                state.request_pawnio_control(false);
            }
        });

        if !p.installed {
            ui.add_space(4.0);
            ui.label(
                RichText::new(i18n.t("pawnio-not-installed"))
                    .small()
                    .color(warn_col),
            );
        }

        ui.add_space(4.0);
        ui.weak(RichText::new(i18n.t("pawnio-warning")).small());
        ui.weak(RichText::new(i18n.t("pawnio-manual-install")).small());
    });

    ui.add_space(10.0);

    // 3. Component Updates & Versions (`ProviderUpdateStatus` for LHM & PawnIO)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-sources-updates"));
        ui.weak(i18n.t("sources-updates-explanation"));
        ui.add_space(8.0);

        let updates: Vec<ProviderUpdateStatus> = if !state.snapshot.provider_updates.is_empty() {
            state
                .snapshot
                .provider_updates
                .iter()
                .filter(|u| u.id != "helper")
                .cloned()
                .collect()
        } else if !state.inventory.provider_updates.is_empty() {
            state
                .inventory
                .provider_updates
                .iter()
                .filter(|u| u.id != "helper")
                .cloned()
                .collect()
        } else {
            // Standalone or unpolled defaults
            vec![
                ProviderUpdateStatus {
                    id: "lhm".into(),
                    display_name: "LibreHardwareMonitor".into(),
                    installed_version: None,
                    latest_version: None,
                    install_kind: ProviderInstallKind::None,
                    state: ProviderUpdateState::Unknown,
                    auto_update: state.draft.providers.lhm_auto_update,
                    official_url: LHM_OFFICIAL_URL.into(),
                    staged_path: None,
                    detail: String::new(),
                    last_error: None,
                    checked_at_unix_ms: None,
                },
                ProviderUpdateStatus {
                    id: "pawnio".into(),
                    display_name: "PawnIO".into(),
                    installed_version: None,
                    latest_version: None,
                    install_kind: ProviderInstallKind::Driver,
                    state: ProviderUpdateState::Unknown,
                    auto_update: false,
                    official_url: PAWNIO_OFFICIAL_URL.into(),
                    staged_path: None,
                    detail: String::new(),
                    last_error: None,
                    checked_at_unix_ms: None,
                },
            ]
        };

        for status in &updates {
            let view = map_provider_update_status(status, i18n);
            let (badge_bg, badge_fg, badge_stroke) =
                update_tone_colors(view.tone, dark, ui.visuals().text_color());

            egui::Frame::NONE
                .fill(if dark {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 6)
                } else {
                    Color32::from_rgba_unmultiplied(0, 0, 0, 8)
                })
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong(&status.display_name);
                        ui.weak(RichText::new(&view.install_kind_text).small());

                        // Update state badge
                        egui::Frame::NONE
                            .fill(badge_bg)
                            .stroke(Stroke::new(1.0, badge_stroke))
                            .corner_radius(CornerRadius::same(4))
                            .inner_margin(egui::Margin::symmetric(7, 3))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(&view.badge_text)
                                        .small()
                                        .color(badge_fg)
                                        .strong(),
                                );
                            });

                        // Official website / releases link on the right
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let link_text = if status.id == "pawnio" {
                                i18n.t("btn-official-site")
                            } else {
                                i18n.t("btn-official-releases")
                            };
                            ui.hyperlink_to(RichText::new(link_text).small(), &status.official_url);
                        });
                    });

                    ui.add_space(2.0);
                    ui.weak(RichText::new(&view.version_summary).small());

                    if let Some(detail) = &view.detail_text {
                        ui.add_space(1.0);
                        ui.weak(RichText::new(detail).small());
                    }
                    if let Some(hint) = &view.hint_text {
                        ui.add_space(1.0);
                        let hint_col = if dark {
                            Color32::from_rgb(96, 205, 255)
                        } else {
                            Color32::from_rgb(0, 103, 192)
                        };
                        ui.label(RichText::new(hint).small().color(hint_col));
                    }
                });
            ui.add_space(4.0);
        }
    });

    ui.add_space(10.0);

    // 4. Provider Status List (`Snapshot.providers` / `HardwareInventory.providers`)
    card_frame(dark, backdrop).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.heading(i18n.t("section-sources-status"));
        ui.add_space(6.0);

        let live_list: Vec<ProviderStatus> = if !state.snapshot.providers.is_empty() {
            state
                .snapshot
                .providers
                .iter()
                .filter(|p| p.id != "helper")
                .cloned()
                .collect()
        } else if !state.inventory.providers.is_empty() {
            state
                .inventory
                .providers
                .iter()
                .filter(|p| p.id != "helper")
                .cloned()
                .collect()
        } else {
            // Standalone or unpolled fallback representing current draft intent
            vec![
                ProviderStatus {
                    id: "hwinfo".into(),
                    display_name: "HWiNFO".into(),
                    enabled: state.draft.providers.hwinfo,
                    health: if state.draft.providers.hwinfo {
                        ProviderHealth::NotRunning
                    } else {
                        ProviderHealth::Disabled
                    },
                    sensor_count: 0,
                    detail: String::new(),
                    last_update_age_ms: None,
                },
                ProviderStatus {
                    id: "lhm_http".into(),
                    display_name: "LibreHardwareMonitor".into(),
                    enabled: state.draft.providers.lhm_http,
                    health: if state.draft.providers.lhm_http {
                        ProviderHealth::NotRunning
                    } else {
                        ProviderHealth::Disabled
                    },
                    sensor_count: 0,
                    detail: String::new(),
                    last_update_age_ms: None,
                },
            ]
        };

        for status in &live_list {
            let view = map_provider_status(status, i18n);
            let (badge_bg, badge_fg, badge_stroke) = match view.tone {
                ProviderTone::Connected => (
                    if dark {
                        Color32::from_rgba_unmultiplied(127, 211, 127, 28)
                    } else {
                        Color32::from_rgba_unmultiplied(22, 130, 56, 22)
                    },
                    if dark {
                        Color32::from_rgb(127, 211, 127)
                    } else {
                        Color32::from_rgb(22, 130, 56)
                    },
                    if dark {
                        Color32::from_rgba_unmultiplied(127, 211, 127, 60)
                    } else {
                        Color32::from_rgba_unmultiplied(22, 130, 56, 60)
                    },
                ),
                ProviderTone::Warning => (
                    if dark {
                        Color32::from_rgba_unmultiplied(240, 198, 116, 28)
                    } else {
                        Color32::from_rgba_unmultiplied(168, 104, 0, 22)
                    },
                    if dark {
                        Color32::from_rgb(240, 198, 116)
                    } else {
                        Color32::from_rgb(168, 104, 0)
                    },
                    if dark {
                        Color32::from_rgba_unmultiplied(240, 198, 116, 60)
                    } else {
                        Color32::from_rgba_unmultiplied(168, 104, 0, 60)
                    },
                ),
                ProviderTone::Error => (
                    if dark {
                        Color32::from_rgba_unmultiplied(242, 109, 109, 32)
                    } else {
                        Color32::from_rgba_unmultiplied(196, 43, 28, 24)
                    },
                    if dark {
                        Color32::from_rgb(242, 109, 109)
                    } else {
                        Color32::from_rgb(196, 43, 28)
                    },
                    if dark {
                        Color32::from_rgba_unmultiplied(242, 109, 109, 60)
                    } else {
                        Color32::from_rgba_unmultiplied(196, 43, 28, 60)
                    },
                ),
                ProviderTone::Disabled => (
                    if dark {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 12)
                    } else {
                        Color32::from_rgba_unmultiplied(0, 0, 0, 12)
                    },
                    ui.visuals().text_color(),
                    Color32::TRANSPARENT,
                ),
            };

            egui::Frame::NONE
                .fill(if dark {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 6)
                } else {
                    Color32::from_rgba_unmultiplied(0, 0, 0, 8)
                })
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong(&status.display_name);

                        // Health badge pill
                        egui::Frame::NONE
                            .fill(badge_bg)
                            .stroke(Stroke::new(1.0, badge_stroke))
                            .corner_radius(CornerRadius::same(4))
                            .inner_margin(egui::Margin::symmetric(7, 3))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(&view.badge_text)
                                        .small()
                                        .color(badge_fg)
                                        .strong(),
                                );
                            });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Some(age) = &view.age_text {
                                ui.weak(RichText::new(age).small());
                            }
                        });
                    });

                    if let Some(detail) = &view.detail_text {
                        ui.add_space(2.0);
                        ui.weak(RichText::new(detail).small());
                    }
                });
            ui.add_space(4.0);
        }

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(2.0);
        ui.weak(i18n.t("sources-no-providers-hint"));
    });

    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide_core::Config;
    use oxide_core::{FormatOptions, PawnioState, PawnioStatus, TempUnit};
    use std::path::PathBuf;

    #[test]
    fn validate_lhm_url_accepts_loopback_and_refuses_remote_or_non_http() {
        // Valid loopback URLs
        assert!(validate_lhm_url("http://127.0.0.1:8085/metrics").is_ok());
        assert!(validate_lhm_url("http://localhost:8085/").is_ok());
        assert!(validate_lhm_url("http://localhost").is_ok());
        assert!(validate_lhm_url("http://[::1]:8085/metrics").is_ok());
        assert!(validate_lhm_url("http://::1:8085/metrics").is_ok());
        assert!(validate_lhm_url("  http://127.0.0.1:8085  ").is_ok());

        // Refused: non-plain http
        assert!(validate_lhm_url("https://127.0.0.1:8085/metrics").is_err());
        assert!(validate_lhm_url("ftp://localhost:8085").is_err());
        assert!(validate_lhm_url("127.0.0.1:8085").is_err());

        // Refused: remote hosts
        assert!(validate_lhm_url("http://192.168.1.10:8085/metrics").is_err());
        assert!(validate_lhm_url("http://10.0.0.1:8085/metrics").is_err());
        assert!(validate_lhm_url("http://example.com/metrics").is_err());
        assert!(validate_lhm_url("http://127.0.0.2:8085/metrics").is_err());

        // Refused: empty or broken
        assert!(validate_lhm_url("").is_err());
        assert!(validate_lhm_url("http://").is_err());
        assert!(validate_lhm_url("http://[::1:8085").is_err());
    }

    #[test]
    fn map_provider_status_covers_all_health_variants() {
        let uk = SettingsI18n::new("uk");

        // Disabled
        let dis = ProviderStatus {
            id: "hwinfo".into(),
            display_name: "HWiNFO".into(),
            enabled: false,
            health: ProviderHealth::Disabled,
            sensor_count: 0,
            detail: String::new(),
            last_update_age_ms: None,
        };
        let view = map_provider_status(&dis, &uk);
        assert_eq!(view.tone, ProviderTone::Disabled);
        assert_eq!(view.badge_text, "Вимкнено");
        assert!(view.age_text.is_none());

        // Connected with sensor count & age
        let conn = ProviderStatus {
            id: "hwinfo".into(),
            display_name: "HWiNFO".into(),
            enabled: true,
            health: ProviderHealth::Connected,
            sensor_count: 14,
            detail: String::new(),
            last_update_age_ms: Some(2300),
        };
        let view = map_provider_status(&conn, &uk);
        assert_eq!(view.tone, ProviderTone::Connected);
        assert!(view.badge_text.contains("14"));
        assert_eq!(view.age_text.as_deref(), Some("2 с тому"));

        // Not running
        let not_run = ProviderStatus {
            id: "lhm_http".into(),
            display_name: "LibreHardwareMonitor".into(),
            enabled: true,
            health: ProviderHealth::NotRunning,
            sensor_count: 0,
            detail: "connection refused".into(),
            last_update_age_ms: Some(500),
        };
        let view = map_provider_status(&not_run, &uk);
        assert_eq!(view.tone, ProviderTone::Warning);
        assert_eq!(view.badge_text, "Не запущено");
        assert_eq!(view.detail_text.as_deref(), Some("connection refused"));
        assert_eq!(view.age_text.as_deref(), Some("щойно"));

        // Stale
        let stale = ProviderStatus {
            id: "hwinfo".into(),
            display_name: "HWiNFO".into(),
            enabled: true,
            health: ProviderHealth::Stale,
            sensor_count: 5,
            detail: String::new(),
            last_update_age_ms: Some(120_000),
        };
        let view = map_provider_status(&stale, &uk);
        assert_eq!(view.tone, ProviderTone::Warning);
        assert_eq!(view.badge_text, "Застарілі дані");
        assert_eq!(view.age_text.as_deref(), Some("2 хв тому"));

        // Error with detail
        let err = ProviderStatus {
            id: "lhm_http".into(),
            display_name: "LibreHardwareMonitor".into(),
            enabled: true,
            health: ProviderHealth::Error,
            sensor_count: 0,
            detail: "HTTP 500".into(),
            last_update_age_ms: None,
        };
        let view = map_provider_status(&err, &uk);
        assert_eq!(view.tone, ProviderTone::Error);
        assert!(view.badge_text.contains("HTTP 500"));
    }

    #[test]
    fn map_provider_update_status_covers_all_states_and_versions() {
        let uk = SettingsI18n::new("uk");

        // 1) UpToDate
        let up = ProviderUpdateStatus {
            id: "lhm".into(),
            display_name: "LibreHardwareMonitor".into(),
            installed_version: Some("0.9.5".into()),
            latest_version: Some("0.9.5".into()),
            install_kind: ProviderInstallKind::Managed,
            state: ProviderUpdateState::UpToDate,
            auto_update: true,
            official_url: LHM_OFFICIAL_URL.into(),
            staged_path: None,
            detail: "up to date".into(),
            last_error: None,
            checked_at_unix_ms: None,
        };
        let view = map_provider_update_status(&up, &uk);
        assert_eq!(view.tone, UpdateTone::Ok);
        assert_eq!(view.badge_text, "Оновлено");
        assert!(view.version_summary.contains("0.9.5"));
        assert!(view.install_kind_text.contains("portable-копія"));

        // 2) UpdateAvailable
        let avail = ProviderUpdateStatus {
            id: "lhm".into(),
            display_name: "LibreHardwareMonitor".into(),
            installed_version: Some("0.9.5".into()),
            latest_version: Some("0.9.6".into()),
            install_kind: ProviderInstallKind::External,
            state: ProviderUpdateState::UpdateAvailable,
            auto_update: false,
            official_url: LHM_OFFICIAL_URL.into(),
            staged_path: None,
            detail: "new release on GitHub".into(),
            last_error: None,
            checked_at_unix_ms: None,
        };
        let view = map_provider_update_status(&avail, &uk);
        assert_eq!(view.tone, UpdateTone::Warning);
        assert_eq!(view.badge_text, "Доступне оновлення");
        assert_eq!(view.detail_text.as_deref(), Some("new release on GitHub"));
        assert!(view.install_kind_text.contains("зовнішня інсталяція"));

        // 3) UpdateReady with staged path and hint
        let ready = ProviderUpdateStatus {
            id: "lhm".into(),
            display_name: "LibreHardwareMonitor".into(),
            installed_version: Some("0.9.5".into()),
            latest_version: Some("0.9.6".into()),
            install_kind: ProviderInstallKind::Managed,
            state: ProviderUpdateState::UpdateReady,
            auto_update: true,
            official_url: LHM_OFFICIAL_URL.into(),
            staged_path: Some(r"C:\App\LHM_0.9.6.exe".into()),
            detail: "sha256 verified".into(),
            last_error: None,
            checked_at_unix_ms: None,
        };
        let view = map_provider_update_status(&ready, &uk);
        assert_eq!(view.tone, UpdateTone::Action);
        assert!(view.badge_text.contains(r"C:\App\LHM_0.9.6.exe"));
        assert!(view.hint_text.is_some());

        // 4) PawnIO driver installed
        let pawn = ProviderUpdateStatus {
            id: "pawnio".into(),
            display_name: "PawnIO".into(),
            installed_version: Some("1.2.0".into()),
            latest_version: Some("1.2.0".into()),
            install_kind: ProviderInstallKind::Driver,
            state: ProviderUpdateState::UpToDate,
            auto_update: false,
            official_url: PAWNIO_OFFICIAL_URL.into(),
            staged_path: None,
            detail: String::new(),
            last_error: None,
            checked_at_unix_ms: None,
        };
        let view = map_provider_update_status(&pawn, &uk);
        assert_eq!(view.tone, UpdateTone::Ok);
        assert!(view.install_kind_text.contains("драйвер ядра"));

        // 5) Error state
        let err = ProviderUpdateStatus {
            id: "lhm".into(),
            display_name: "LibreHardwareMonitor".into(),
            installed_version: None,
            latest_version: None,
            install_kind: ProviderInstallKind::None,
            state: ProviderUpdateState::Error,
            auto_update: false,
            official_url: LHM_OFFICIAL_URL.into(),
            staged_path: None,
            detail: String::new(),
            last_error: Some("network timeout".into()),
            checked_at_unix_ms: None,
        };
        let view = map_provider_update_status(&err, &uk);
        assert_eq!(view.tone, UpdateTone::Error);
        assert!(view.badge_text.contains("network timeout"));
    }

    #[test]
    fn format_age_renders_sub_second_seconds_and_minutes() {
        let uk = SettingsI18n::new("uk");
        let en = SettingsI18n::new("en");

        assert_eq!(format_age(None, &uk), None);
        assert_eq!(format_age(Some(800), &uk).as_deref(), Some("щойно"));
        assert_eq!(format_age(Some(800), &en).as_deref(), Some("just now"));
        assert_eq!(format_age(Some(3500), &uk).as_deref(), Some("3 с тому"));
        assert_eq!(format_age(Some(3500), &en).as_deref(), Some("3s ago"));
        assert_eq!(format_age(Some(125_000), &uk).as_deref(), Some("2 хв тому"));
        assert_eq!(format_age(Some(125_000), &en).as_deref(), Some("2m ago"));
    }

    #[test]
    fn map_pawnio_status_covers_all_states_and_versions() {
        let uk = SettingsI18n::new("uk");

        let cases = [
            (PawnioState::Disabled, "Вимкнено", UpdateTone::Muted),
            (
                PawnioState::NotInstalled,
                "Драйвер не встановлено",
                UpdateTone::Warning,
            ),
            (PawnioState::Stopped, "Зупинено", UpdateTone::Warning),
            (PawnioState::Starting, "Запуск…", UpdateTone::Action),
            (PawnioState::Running, "Працює", UpdateTone::Ok),
            (PawnioState::Denied, "Дозвіл відхилено", UpdateTone::Error),
            (PawnioState::Error, "Помилка", UpdateTone::Error),
        ];

        for (state, badge, tone) in cases {
            let status = PawnioStatus {
                installed: true,
                installed_version: Some("2.1.0".into()),
                enabled: true,
                state,
                temperature_c: Some(72.0),
                helper_version: Some("0.1.0".into()),
                detail: "helper detail".into(),
                last_error: (state == PawnioState::Error).then(|| "pipe denied".into()),
                ..PawnioStatus::default()
            };

            let temp = format_pawnio_temperature(&status, &FormatOptions::default());
            let view = map_pawnio_status(&status, temp, &uk);

            assert_eq!(view.tone, tone, "tone mismatch for {state:?}");
            assert_eq!(view.badge_text, badge, "badge mismatch for {state:?}");
            assert!(view.version_text.contains("2.1.0"));
            assert!(view.version_text.contains("0.1.0"));
            assert_eq!(view.detail_text.as_deref(), Some("helper detail"));
            assert_eq!(
                view.temperature_text.is_some(),
                state == PawnioState::Running,
                "temperature shown only while running ({state:?})"
            );
            assert_eq!(
                view.error_text.as_deref(),
                (state == PawnioState::Error).then_some("pipe denied")
            );
        }
    }

    #[test]
    fn format_pawnio_temperature_only_when_running_and_respects_units() {
        let mut status = PawnioStatus {
            installed: true,
            state: PawnioState::Running,
            temperature_c: Some(72.0),
            ..PawnioStatus::default()
        };
        assert_eq!(
            format_pawnio_temperature(&status, &FormatOptions::default()).as_deref(),
            Some("72 °C")
        );

        let fahrenheit = FormatOptions {
            temp_unit: TempUnit::Fahrenheit,
            ..FormatOptions::default()
        };
        assert_eq!(
            format_pawnio_temperature(&status, &fahrenheit).as_deref(),
            Some("162 °F")
        );

        // Running without a reading still renders a placeholder.
        status.temperature_c = None;
        assert_eq!(
            format_pawnio_temperature(&status, &FormatOptions::default()).as_deref(),
            Some("—")
        );

        // Any other state hides the temperature entirely.
        status.temperature_c = Some(50.0);
        status.state = PawnioState::Stopped;
        assert_eq!(
            format_pawnio_temperature(&status, &FormatOptions::default()),
            None
        );
    }

    #[test]
    fn headless_egui_sources_tab_renders_cleanly() {
        let mut state = SettingsState::load_local(PathBuf::from("nonexistent_sources_test.toml"));
        state.draft = Config::default();
        state.draft.providers.hwinfo = true;
        state.draft.providers.lhm_http = true;
        state.draft.providers.lhm_auto_update = true;
        state.snapshot.providers = vec![
            ProviderStatus {
                id: "hwinfo".into(),
                display_name: "HWiNFO".into(),
                enabled: true,
                health: ProviderHealth::Connected,
                sensor_count: 12,
                detail: String::new(),
                last_update_age_ms: Some(1200),
            },
            ProviderStatus {
                id: "lhm_http".into(),
                display_name: "LibreHardwareMonitor".into(),
                enabled: true,
                health: ProviderHealth::NotRunning,
                sensor_count: 0,
                detail: "no web server".into(),
                last_update_age_ms: None,
            },
            ProviderStatus {
                id: "helper".into(),
                display_name: "Helper".into(),
                enabled: true,
                health: ProviderHealth::Error,
                sensor_count: 0,
                detail: "hidden".into(),
                last_update_age_ms: None,
            },
        ];
        state.snapshot.provider_updates = vec![
            ProviderUpdateStatus {
                id: "lhm".into(),
                display_name: "LibreHardwareMonitor".into(),
                installed_version: Some("0.9.5".into()),
                latest_version: Some("0.9.6".into()),
                install_kind: ProviderInstallKind::Managed,
                state: ProviderUpdateState::UpdateReady,
                auto_update: true,
                official_url: LHM_OFFICIAL_URL.into(),
                staged_path: Some(r"C:\LHM\staged.exe".into()),
                detail: "verified".into(),
                last_error: None,
                checked_at_unix_ms: Some(100),
            },
            ProviderUpdateStatus {
                id: "pawnio".into(),
                display_name: "PawnIO".into(),
                installed_version: Some("1.2.0".into()),
                latest_version: Some("1.2.0".into()),
                install_kind: ProviderInstallKind::Driver,
                state: ProviderUpdateState::UpToDate,
                auto_update: false,
                official_url: PAWNIO_OFFICIAL_URL.into(),
                staged_path: None,
                detail: String::new(),
                last_error: None,
                checked_at_unix_ms: Some(100),
            },
        ];

        state.snapshot.pawnio = PawnioStatus {
            installed: true,
            installed_version: Some("2.1.0".into()),
            enabled: true,
            state: PawnioState::Running,
            temperature_c: Some(65.0),
            helper_version: Some("0.1.0".into()),
            detail: "Tctl / Tdie".into(),
            ..PawnioStatus::default()
        };

        let i18n = SettingsI18n::new("uk");
        let ctx = egui::Context::default();

        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let changed = super::ui(ui, &mut state, &i18n, true, BackdropKind::SolidFallback);
                assert!(!changed, "passive render must not mutate draft");
            });
        });
        out.textures_delta.clear();
    }
}
