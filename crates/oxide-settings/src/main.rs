#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! `oxide-settings.exe` — Isolated Windows 11 Fluent settings application for Oxide Monitor.
//!
//! Communicates with `oxide-monitor.exe` over `\\.\pipe\oxide-ui-v1` (`oxide-ipc`) with
//! live preview, optimistic-concurrency commits, standalone fallback, and pixel-accurate
//! rendering via the shared `oxide-render` crate.

mod i18n;
mod ipc_worker;
mod preview;
mod state;
mod tabs;
mod theme;
mod win32_host;

use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, Stroke};
use oxide_core::Config;
use oxide_render::SystemThemeContext;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::HWND;

use crate::i18n::SettingsI18n;
use crate::ipc_worker::{IpcUiEvent, IpcUiRequest, IpcWorker};
use crate::preview::LivePreviewPanel;
use crate::state::{ConnectionMode, SettingsState, SettingsTab, StatusBanner};
use crate::tabs::flyout_gamemode::FlyoutGameModeUiState;
use crate::tabs::items_aliases::ItemsAliasesUiState;
use crate::theme::{apply_fluent_theme, card_frame, load_windows_ui_font};
use crate::win32_host::{
    BackdropKind, SingleInstanceGuard, apply_win11_backdrop, bring_window_to_front,
    handle_dx12_init_fallback, open_config_in_notepad, update_dwm_dark_mode,
};

struct CliArgs {
    focus_tab: Option<String>,
    spike_probe: Option<PathBuf>,
}

fn parse_cli_args() -> CliArgs {
    let mut focus_tab = None;
    let mut spike_probe = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--tab" => focus_tab = args.next(),
            "--spike-probe" => spike_probe = args.next().map(PathBuf::from),
            _ => {}
        }
    }
    CliArgs {
        focus_tab,
        spike_probe,
    }
}

fn main() {
    let start = Instant::now();
    let cli = parse_cli_args();

    // Enforce single instance (except during automated --spike-probe measurement).
    let _instance_guard = if cli.spike_probe.is_none() {
        match SingleInstanceGuard::acquire_or_focus_existing() {
            Some(guard) => Some(guard),
            None => return,
        }
    } else {
        None
    };

    let config_path = Config::resolve_path();
    let initial_state = SettingsState::load_local(config_path.clone());
    let initial_lang = initial_state.draft.general.language.clone();
    let initial_i18n = SettingsI18n::new(&initial_lang);

    let mut create_new = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    create_new.instance_descriptor.backends = wgpu::Backends::DX12;
    create_new.power_preference = wgpu::PowerPreference::LowPower;
    // Favor minimal DX12 sub-allocation blocks (4..8 MiB) over huge 256 MiB game heaps
    // so settings RAM stays lean on integrated GPUs.
    create_new.device_descriptor = std::sync::Arc::new(|_adapter| wgpu::DeviceDescriptor {
        label: Some("oxide-settings wgpu device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits {
            max_texture_dimension_2d: 2048,
            ..wgpu::Limits::downlevel_defaults()
        },
        memory_hints: wgpu::MemoryHints::Manual {
            suballocated_device_memory_block_size: (4 * 1024 * 1024)..(8 * 1024 * 1024),
        },
        trace: wgpu::Trace::Off,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
    });

    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    wgpu_options.surface.desired_maximum_frame_latency = Some(1);
    wgpu_options.wgpu_setup = eframe::egui_wgpu::WgpuSetup::CreateNew(create_new);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(initial_i18n.t("window-title"))
            .with_inner_size([980.0, 700.0])
            .with_min_inner_size([780.0, 520.0])
            .with_transparent(true),
        renderer: eframe::Renderer::Wgpu,
        depth_buffer: 0,
        multisampling: 0,
        wgpu_options,
        ..Default::default()
    };

    let fallback_path = config_path.clone();
    let fallback_lang = initial_lang.clone();

    let run_result = eframe::run_native(
        "Oxide Monitor Settings",
        native_options,
        Box::new(move |cc| {
            let init_ms = start.elapsed().as_secs_f64() * 1000.0;
            load_windows_ui_font(&cc.egui_ctx);

            let dark = resolve_is_dark(&initial_state.draft.general.theme);
            let backdrop = apply_win11_backdrop(cc, dark);
            apply_fluent_theme(&cc.egui_ctx, dark, backdrop);

            let hwnd = extract_hwnd(cc);
            if cli.spike_probe.is_none()
                && let Some(h) = hwnd
            {
                bring_window_to_front(h);
            }
            let adapter_info = cc
                .wgpu_render_state
                .as_ref()
                .map(|rs| format!("{:?}", rs.adapter.get_info()))
                .unwrap_or_else(|| "none".into());

            let worker = if cli.spike_probe.is_none() {
                Some(IpcWorker::spawn(cli.focus_tab.clone(), cc.egui_ctx.clone()))
            } else {
                None
            };

            let mut state = initial_state;
            if let Some(tab) = cli.focus_tab.as_deref().and_then(SettingsTab::from_name) {
                state.active_tab = tab;
            }

            Ok(Box::new(SettingsApp {
                state,
                i18n: SettingsI18n::new(&initial_lang),
                preview: LivePreviewPanel::new(),
                items_tab: ItemsAliasesUiState::default(),
                flyout_tab: FlyoutGameModeUiState::default(),
                worker,
                backdrop,
                hwnd,
                applied_dark: dark,
                applied_lang: initial_lang,
                start,
                init_ms,
                adapter_info,
                spike_probe: cli.spike_probe,
                first_frame_done: false,
                autostart_recheck_at: None,
            }))
        }),
    );

    if let Err(err) = run_result {
        let i18n = SettingsI18n::new(&fallback_lang);
        handle_dx12_init_fallback(&i18n, &fallback_path, &err.to_string());
    }
}

struct SettingsApp {
    state: SettingsState,
    i18n: SettingsI18n,
    preview: LivePreviewPanel,
    items_tab: ItemsAliasesUiState,
    flyout_tab: FlyoutGameModeUiState,
    worker: Option<IpcWorker>,
    backdrop: BackdropKind,
    hwnd: Option<HWND>,
    applied_dark: bool,
    applied_lang: String,
    start: Instant,
    init_ms: f64,
    adapter_info: String,
    spike_probe: Option<PathBuf>,
    first_frame_done: bool,
    autostart_recheck_at: Option<Instant>,
}

impl SettingsApp {
    fn drain_ipc(&mut self, ctx: &egui::Context) {
        let Some(worker) = &self.worker else {
            return;
        };
        let events = worker.drain_events();
        for ev in events {
            match ev {
                IpcUiEvent::Connected(msg) | IpcUiEvent::Message(msg) => {
                    let should_focus =
                        matches!(&*msg, oxide_ipc::MonitorToSettings::FocusTab { .. });
                    let invalidates_preview = matches!(
                        &*msg,
                        oxide_ipc::MonitorToSettings::HelloAck { .. }
                            | oxide_ipc::MonitorToSettings::MonitorsChanged { .. }
                    );
                    let is_commit_result =
                        matches!(&*msg, oxide_ipc::MonitorToSettings::CommitResult { .. });
                    self.state.apply_ipc_message(*msg, &self.i18n);
                    if invalidates_preview {
                        self.preview.invalidate();
                    }
                    if is_commit_result {
                        self.autostart_recheck_at =
                            Some(Instant::now() + Duration::from_millis(300));
                    }
                    self.sync_theme_and_lang(ctx);
                    if should_focus && let Some(h) = self.hwnd {
                        bring_window_to_front(h);
                    }
                }
                IpcUiEvent::Standalone(reason) => {
                    self.state.enter_standalone(reason);
                }
                IpcUiEvent::Disconnected(reason) => {
                    self.state.enter_standalone(reason);
                }
            }
        }
    }

    fn flush_debounced_preview(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if let Some(cfg) = self.state.take_due_preview(now) {
            if let Some(worker) = &self.worker {
                worker.send(IpcUiRequest::Preview(Box::new(cfg)));
            }
        } else if self.state.preview_pending_since.is_some() {
            ctx.request_repaint_after(Duration::from_millis(25));
        }
    }

    /// Forward a one-shot explicit PawnIO helper request (set by a button click in the
    /// Sources tab). Never derived from `providers.pawnio` / preview config.
    ///
    /// The request is always consumed so a click made while standalone never turns into
    /// a surprise UAC prompt after a later reconnect.
    fn flush_pawnio_control(&mut self) {
        let Some(start) = self.state.take_pawnio_control() else {
            return;
        };
        if let Some(worker) = &self.worker
            && self.state.is_connected()
        {
            worker.send(IpcUiRequest::PawnioControl { start });
        }
    }

    fn sync_theme_and_lang(&mut self, ctx: &egui::Context) {
        let desired_dark = resolve_is_dark(&self.state.draft.general.theme);
        if desired_dark != self.applied_dark {
            self.applied_dark = desired_dark;
            apply_fluent_theme(ctx, desired_dark, self.backdrop);
            if let Some(hwnd) = self.hwnd {
                update_dwm_dark_mode(hwnd, desired_dark);
            }
        }
        if self.applied_lang != self.state.draft.general.language {
            self.applied_lang = self.state.draft.general.language.clone();
            self.i18n.set_language(&self.applied_lang);
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.i18n.t("window-title")));
        }
    }

    fn trigger_apply(&mut self) {
        let errors = self.state.validate_draft(&self.i18n);
        if !errors.is_empty() {
            self.state.status = Some(StatusBanner::Error(errors.join(" ")));
            return;
        }
        if self.state.is_connected() {
            let (expected_generation, expected_hash, config) = self.state.prepare_ipc_commit();
            if let Some(worker) = &self.worker {
                worker.send(IpcUiRequest::Commit {
                    expected_generation,
                    expected_hash,
                    config: Box::new(config),
                });
            }
        } else if let Err(err) = self.state.commit_standalone(&self.i18n) {
            self.state.status = Some(StatusBanner::Error(err));
        }
    }

    fn trigger_cancel(&mut self, ctx: &egui::Context) {
        let send_revert = self.state.cancel_edits(&self.i18n);
        if send_revert && let Some(worker) = &self.worker {
            worker.send(IpcUiRequest::Revert);
        }
        self.preview.invalidate();
        self.sync_theme_and_lang(ctx);
    }

    fn trigger_reset(&mut self, ctx: &egui::Context) {
        self.state.reset_to_defaults_preview(&self.i18n);
        self.preview.invalidate();
        self.sync_theme_and_lang(ctx);
    }
}

impl eframe::App for SettingsApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.backdrop.is_translucent() {
            [0.0, 0.0, 0.0, 0.0]
        } else if self.applied_dark {
            [28.0 / 255.0, 30.0 / 255.0, 36.0 / 255.0, 1.0]
        } else {
            [243.0 / 255.0, 245.0 / 255.0, 249.0 / 255.0, 1.0]
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if !self.first_frame_done {
            // Delay closing spike_probe until after the layout runs once so we also report scroll_avail_h.
        }

        self.drain_ipc(&ctx);
        self.flush_debounced_preview(&ctx);
        if let Some(at) = self.autostart_recheck_at {
            let now = Instant::now();
            if now >= at {
                self.autostart_recheck_at = None;
                self.state.refresh_autostart_status();
            } else {
                ctx.request_repaint_after(at.saturating_duration_since(now));
            }
        }

        let dark = self.applied_dark;
        let backdrop = self.backdrop;

        // 1. Bottom sticky action bar: Apply / Cancel / Reset / Open TOML
        egui::Panel::bottom("settings_bottom_bar")
            .frame(
                egui::Frame::NONE
                    .fill(if dark {
                        Color32::from_rgba_unmultiplied(24, 26, 32, 225)
                    } else {
                        Color32::from_rgba_unmultiplied(240, 243, 248, 235)
                    })
                    .stroke(Stroke::new(
                        1.0,
                        if dark {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 18)
                        } else {
                            Color32::from_rgba_unmultiplied(0, 0, 0, 20)
                        },
                    ))
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Connection status pill
                    let (dot_color, conn_text) = match &self.state.connection {
                        ConnectionMode::Connected => (
                            Color32::from_rgb(127, 211, 127),
                            self.i18n.t("status-connected"),
                        ),
                        ConnectionMode::Connecting => (
                            Color32::from_rgb(240, 198, 116),
                            self.i18n.t("status-connecting"),
                        ),
                        ConnectionMode::Standalone { .. } => {
                            (Color32::from_rgb(240, 198, 116), "Standalone".to_string())
                        }
                    };
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 4.0, dot_color);
                    ui.weak(conn_text);

                    if self.state.preview_active_on_monitor && self.state.is_dirty() {
                        ui.separator();
                        ui.colored_label(
                            Color32::from_rgb(96, 205, 255),
                            self.i18n.t("status-preview-active"),
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let validation_errors = self.state.validate_draft(&self.i18n);
                        let can_apply = self.state.is_dirty()
                            && validation_errors.is_empty()
                            && !self.state.commit_in_flight;

                        let apply_btn = egui::Button::new(self.i18n.t("btn-apply"));
                        if ui.add_enabled(can_apply, apply_btn).clicked() {
                            self.trigger_apply();
                        }

                        if ui
                            .add_enabled(
                                self.state.is_dirty() || self.state.preview_active_on_monitor,
                                egui::Button::new(self.i18n.t("btn-cancel")),
                            )
                            .clicked()
                        {
                            self.trigger_cancel(&ctx);
                        }

                        if ui.button(self.i18n.t("btn-reset")).clicked() {
                            self.trigger_reset(&ctx);
                        }

                        ui.separator();
                        if ui.button(self.i18n.t("btn-open-toml")).clicked() {
                            let _ = open_config_in_notepad(&self.state.config_path);
                        }
                    });
                });
            });

        // 2. Left Win11 Navigation Sidebar
        egui::Panel::left("settings_sidebar")
            .resizable(false)
            .exact_size(210.0)
            .frame(
                egui::Frame::NONE
                    .fill(if dark {
                        if backdrop.is_translucent() {
                            Color32::from_rgba_unmultiplied(20, 22, 28, 120)
                        } else {
                            Color32::from_rgb(24, 26, 32)
                        }
                    } else if backdrop.is_translucent() {
                        Color32::from_rgba_unmultiplied(238, 241, 247, 150)
                    } else {
                        Color32::from_rgb(236, 239, 245)
                    })
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.heading("Oxide Monitor");
                ui.weak(format!("v{}", env!("CARGO_PKG_VERSION")));
                ui.add_space(12.0);

                for tab in SettingsTab::ALL {
                    let selected = self.state.active_tab == tab;
                    let resp = ui.add_sized(
                        [ui.available_width(), 32.0],
                        egui::Button::selectable(selected, self.i18n.t(tab.i18n_key())),
                    );
                    if resp.clicked() {
                        let switched_to_general =
                            self.state.active_tab != tab && tab == SettingsTab::GeneralAndPresets;
                        self.state.active_tab = tab;
                        if switched_to_general {
                            self.state.refresh_autostart_status();
                        }
                    }
                    ui.add_space(2.0);
                }
            });

        // 3. Top Live Preview Bar + Banners + Scrollable Main Content
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::same(14)),
            )
            .show(ui, |ui| {
                // Standalone banner (when IPC is unavailable)
                if matches!(self.state.connection, ConnectionMode::Standalone { .. }) {
                    banner_frame(
                        Color32::from_rgba_unmultiplied(240, 198, 116, 32),
                        Color32::from_rgb(240, 198, 116),
                    )
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            ui.label(self.i18n.t("status-standalone"));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button(self.i18n.t("btn-reload-from-monitor")).clicked()
                                        && let Some(worker) = &self.worker
                                    {
                                        self.state.connection = ConnectionMode::Connecting;
                                        worker.send(IpcUiRequest::Reconnect);
                                    }
                                },
                            );
                        });
                    });
                    ui.add_space(8.0);
                }

                // Conflict banner
                if let Some(detail) = self.state.conflict_detail.clone() {
                    banner_frame(
                        Color32::from_rgba_unmultiplied(242, 109, 109, 36),
                        Color32::from_rgb(242, 109, 109),
                    )
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            ui.vertical(|ui| {
                                ui.strong(self.i18n.t("status-conflict"));
                                ui.weak(detail);
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button(self.i18n.t("btn-reload-from-monitor")).clicked()
                                        && let Some(worker) = &self.worker
                                    {
                                        self.state.conflict_detail = None;
                                        worker.send(IpcUiRequest::Reconnect);
                                    }
                                },
                            );
                        });
                    });
                    ui.add_space(8.0);
                } else if let Some(banner) = &self.state.status {
                    let (bg, border, text) = match banner {
                        StatusBanner::Success(t) => (
                            Color32::from_rgba_unmultiplied(127, 211, 127, 28),
                            Color32::from_rgb(127, 211, 127),
                            t.as_str(),
                        ),
                        StatusBanner::Warning(t) => (
                            Color32::from_rgba_unmultiplied(240, 198, 116, 28),
                            Color32::from_rgb(240, 198, 116),
                            t.as_str(),
                        ),
                        StatusBanner::Error(t) => (
                            Color32::from_rgba_unmultiplied(242, 109, 109, 34),
                            Color32::from_rgb(242, 109, 109),
                            t.as_str(),
                        ),
                        StatusBanner::Info(t) => (
                            Color32::from_rgba_unmultiplied(96, 205, 255, 26),
                            Color32::from_rgb(96, 205, 255),
                            t.as_str(),
                        ),
                    };
                    banner_frame(bg, border).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(text);
                    });
                    ui.add_space(8.0);
                }

                // Validation errors banner
                let val_errors = self.state.validate_draft(&self.i18n);
                if !val_errors.is_empty() {
                    banner_frame(
                        Color32::from_rgba_unmultiplied(242, 109, 109, 34),
                        Color32::from_rgb(242, 109, 109),
                    )
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.strong(self.i18n.t("validation-header"));
                        for err in &val_errors {
                            ui.label(format!("• {err}"));
                        }
                    });
                    ui.add_space(8.0);
                }

                let fallback_dpi = ((ctx.pixels_per_point() * 96.0).round() as u32).clamp(96, 288);
                let monitor_dpi = tabs::layout_surface::resolve_target_monitor_dpi(
                    &self.state.monitors,
                    &self.state.draft.taskbar.monitor,
                    fallback_dpi,
                );

                // Live Preview card (always visible on top)
                card_frame(dark, backdrop).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    self.preview.ui(
                        ui,
                        &self.i18n,
                        &self.state.draft,
                        &self.state.snapshot,
                        &self.state.history,
                        self.state.has_live_telemetry,
                        monitor_dpi,
                    );
                });

                ui.add_space(10.0);

                let scroll_avail_h = ui.available_height();
                let preview_size_px = self.preview.last_size_px();

                // Scrollable active tab body
                let mut mutated = false;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.state.active_tab {
                        SettingsTab::GeneralAndPresets => {
                            mutated = tabs::general_presets::ui(
                                ui,
                                &mut self.state,
                                &self.i18n,
                                dark,
                                backdrop,
                            );
                        }
                        SettingsTab::ItemsAndAliases => {
                            mutated = tabs::items_aliases::ui(
                                ui,
                                &mut self.state,
                                &mut self.items_tab,
                                &self.i18n,
                                dark,
                                backdrop,
                            );
                        }
                        SettingsTab::Sources => {
                            mutated = tabs::sources::ui(
                                ui,
                                &mut self.state,
                                &self.i18n,
                                dark,
                                backdrop,
                            );
                        }
                        SettingsTab::LayoutAndSurface => {
                            mutated = tabs::layout_surface::ui(
                                ui,
                                &mut self.state,
                                &self.i18n,
                                dark,
                                backdrop,
                                preview_size_px,
                                monitor_dpi,
                            );
                        }
                        SettingsTab::TypographyAndFormat => {
                            mutated = tabs::typography_format::ui(
                                ui,
                                &mut self.state,
                                &self.i18n,
                                dark,
                                backdrop,
                            );
                        }
                        SettingsTab::TaskbarAndInteractions => {
                            mutated = tabs::taskbar_interactions::ui(
                                ui,
                                &mut self.state,
                                &self.i18n,
                                dark,
                                backdrop,
                            );
                        }
                        SettingsTab::FlyoutAndGameMode => {
                            mutated = tabs::flyout_gamemode::ui(
                                ui,
                                &mut self.state,
                                &mut self.flyout_tab,
                                &self.i18n,
                                dark,
                                backdrop,
                            );
                        }
                    });

                if mutated {
                    self.state.mark_draft_changed();
                    self.preview.invalidate();
                    self.sync_theme_and_lang(&ctx);
                    ctx.request_repaint();
                }

                if !self.first_frame_done {
                    self.first_frame_done = true;
                    let first_frame_ms = self.start.elapsed().as_secs_f64() * 1000.0;
                    if let Some(path) = &self.spike_probe {
                        let report = format!(
                            "init_ms={:.2}\nfirst_frame_ms={:.2}\nscroll_avail_h={:.1}\ndwm={}\nadapter={}\n",
                            self.init_ms,
                            first_frame_ms,
                            scroll_avail_h,
                            self.backdrop.as_str(),
                            self.adapter_info
                        );
                        let _ = std::fs::write(path, report);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            });

        // Forward any PawnIO helper click from this frame right away (no per-frame sends).
        self.flush_pawnio_control();
    }

    fn on_exit(&mut self) {
        if let Some(worker) = self.worker.take() {
            if self.state.preview_active_on_monitor && self.state.is_dirty() {
                worker.send(IpcUiRequest::Revert);
            }
            worker.shutdown();
        }
    }
}

fn banner_frame(fill: Color32, stroke: Color32) -> egui::Frame {
    egui::Frame::NONE
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(10, 7))
}

fn resolve_is_dark(theme: &str) -> bool {
    match theme.trim().to_ascii_lowercase().as_str() {
        "light" => false,
        "dark" => true,
        _ => !SystemThemeContext::query_windows().system_is_light,
    }
}

fn extract_hwnd(cc: &eframe::CreationContext<'_>) -> Option<HWND> {
    let wh = cc.window_handle().ok()?;
    let RawWindowHandle::Win32(win32) = wh.as_raw() else {
        return None;
    };
    Some(HWND(win32.hwnd.get() as *mut c_void))
}
