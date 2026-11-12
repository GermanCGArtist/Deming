//! Root `eframe::App` implementation for Deming GUI.

use chrono::Utc;
use deming_auth::AccountStore;
use deming_core::{DemingConfig, RunStatus};
use egui::{Color32, RichText};

use crate::preset::BuilderState;
use crate::runner_bridge::{default_config_path, BridgeMessage, GuiBridge};
use crate::state::{
    AccountsState, ActiveTab, DashboardState, HistoryState, LiveRunCard, SettingsDraft,
    StatusToast, UiState,
};
use crate::theme::{
    apply_obsidian_theme, ACCENT_EMERALD, BG_BASE, BG_SURFACE, BORDER_SUBTLE, DANGER_RED,
    TEXT_MUTED, TEXT_PRIMARY,
};
use crate::views;

pub struct DemingApp {
    state: UiState,
    bridge: GuiBridge,
    theme_applied: bool,
}

impl DemingApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let config_path = default_config_path();
        let config = DemingConfig::load_or_default(&config_path).unwrap_or_default();
        let accounts = AccountStore::from_config(&config).load().unwrap_or_default();

        let bridge = GuiBridge::new();
        bridge.start_ipc_server(config.workspace_dir.clone());

        let mut state = UiState {
            active_tab: ActiveTab::Dashboard,
            config_path,
            settings: SettingsDraft::from_config(&config),
            config,
            ipc_port: None,
            ipc_error: None,
            toast: None,
            dashboard: DashboardState::default(),
            builder: BuilderState::default(),
            history: HistoryState::default(),
            accounts: AccountsState {
                accounts,
                ..Default::default()
            },
        };

        views::history::refresh_runs(&mut state);

        Self {
            state,
            bridge,
            theme_applied: false,
        }
    }

    fn process_bridge_messages(&mut self) {
        let messages = self.bridge.drain_messages();
        for msg in messages {
            match msg {
                BridgeMessage::IpcServerStarted { port, .. } => {
                    self.state.ipc_port = Some(port);
                    self.state.ipc_error = None;
                }
                BridgeMessage::IpcServerError(err) => {
                    self.state.ipc_port = None;
                    self.state.ipc_error = Some(err);
                }
                BridgeMessage::IpcRunStarted { run_id, name } => {
                    if !self.state.dashboard.runs.iter().any(|r| r.run_id == run_id) {
                        self.state
                            .dashboard
                            .runs
                            .insert(0, LiveRunCard::from_ipc(run_id, name));
                    }
                }
                BridgeMessage::LocalRunInitialized {
                    run_id,
                    job_name,
                    spec,
                    ..
                } => {
                    let id_str = run_id.to_string();
                    if !self.state.dashboard.runs.iter().any(|r| r.run_id == id_str) {
                        self.state.dashboard.runs.insert(
                            0,
                            LiveRunCard::from_local_spec(id_str, job_name, &spec, &self.state.config),
                        );
                    }
                }
                BridgeMessage::RunEventReceived { run_id, event } => {
                    let defaults = &self.state.config.global_limits;
                    if let Some(card) = self
                        .state
                        .dashboard
                        .runs
                        .iter_mut()
                        .find(|r| r.run_id == run_id)
                    {
                        card.apply_event(&event, defaults);
                    } else {
                        let mut card = LiveRunCard::from_ipc(run_id, "External Run".to_string());
                        card.apply_event(&event, defaults);
                        self.state.dashboard.runs.insert(0, card);
                    }
                }
                BridgeMessage::ScreenshotResult {
                    run_id,
                    instance_id,
                    path,
                    error,
                } => {
                    if let Some(p) = path {
                        self.state.toast = Some(StatusToast {
                            is_error: false,
                            text: format!("Screenshot saved for {instance_id}: {p}"),
                            created_at: Utc::now(),
                        });
                    } else if let Some(err) = error {
                        self.state.toast = Some(StatusToast {
                            is_error: true,
                            text: format!(
                                "Screenshot failed ({}/{instance_id}): {err}",
                                &run_id[..run_id.len().min(8)]
                            ),
                            created_at: Utc::now(),
                        });
                    }
                }
                BridgeMessage::LocalRunFinished { run_id, result } => {
                    let id_str = run_id.to_string();
                    match result {
                        Ok(manifest) => {
                            if let Some(card) = self
                                .state
                                .dashboard
                                .runs
                                .iter_mut()
                                .find(|r| r.run_id == id_str)
                            {
                                card.status = manifest.status;
                                card.finished_at = manifest.finished_at;
                            }
                            self.state.toast = Some(StatusToast {
                                is_error: manifest.status == RunStatus::Failed,
                                text: format!(
                                    "Run '{}' finished: {:?}",
                                    manifest.job_name, manifest.status
                                ),
                                created_at: Utc::now(),
                            });
                        }
                        Err(err) => {
                            if let Some(card) = self
                                .state
                                .dashboard
                                .runs
                                .iter_mut()
                                .find(|r| r.run_id == id_str)
                            {
                                if matches!(card.status, RunStatus::Running | RunStatus::Pending) {
                                    card.status = RunStatus::Failed;
                                    card.finished_at = Some(Utc::now());
                                }
                            }
                            self.state.toast = Some(StatusToast {
                                is_error: true,
                                text: err,
                                created_at: Utc::now(),
                            });
                        }
                    }
                    views::history::refresh_runs(&mut self.state);
                }
                BridgeMessage::MsaDeviceCodeReady(res) => match res {
                    Ok(prompt) => {
                        self.state.accounts.msa_prompt = Some(prompt);
                        self.state.accounts.msa_is_error = false;
                        self.state.accounts.msa_status_msg =
                            Some("Waiting for browser authorization...".to_string());
                    }
                    Err(err) => {
                        self.state.accounts.msa_polling = false;
                        self.state.accounts.msa_prompt = None;
                        self.state.accounts.msa_is_error = true;
                        self.state.accounts.msa_status_msg = Some(err);
                    }
                },
                BridgeMessage::MsaLoginFinished(res) => {
                    self.state.accounts.msa_polling = false;
                    self.state.accounts.msa_prompt = None;
                    match res {
                        Ok(session) => {
                            self.state.accounts.msa_is_error = false;
                            self.state.accounts.msa_status_msg = Some(format!(
                                "Signed in as {} ({})",
                                session.username, session.uuid
                            ));
                            views::accounts::refresh_accounts(&mut self.state);
                            self.state.toast = Some(StatusToast {
                                is_error: false,
                                text: format!("Signed in with Microsoft as {}", session.username),
                                created_at: Utc::now(),
                            });
                        }
                        Err(err) => {
                            self.state.accounts.msa_is_error = true;
                            self.state.accounts.msa_status_msg = Some(err);
                        }
                    }
                }
            }
        }
    }
}

impl eframe::App for DemingApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.theme_applied {
            apply_obsidian_theme(ctx);
            self.bridge.set_egui_ctx(ctx);
            self.theme_applied = true;
        }

        self.process_bridge_messages();

        // Top Navigation Bar
        egui::TopBottomPanel::top("deming_top_nav")
            .frame(
                egui::Frame::new()
                    .fill(BG_SURFACE)
                    .stroke(egui::Stroke::new(1.0_f32, BORDER_SUBTLE))
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("◆ DEMING")
                            .size(18.0)
                            .strong()
                            .color(ACCENT_EMERALD),
                    );
                    ui.add_space(16.0);

                    for tab in ActiveTab::ALL {
                        let selected = self.state.active_tab == tab;
                        let text = RichText::new(tab.label())
                            .size(13.5)
                            .strong()
                            .color(if selected {
                                Color32::from_rgb(0x09, 0x16, 0x10)
                            } else {
                                TEXT_PRIMARY
                            });
                        let btn = egui::Button::new(text)
                            .fill(if selected {
                                ACCENT_EMERALD
                            } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(egui::CornerRadius::same(6));

                        if ui.add(btn).clicked() {
                            self.state.active_tab = tab;
                            match tab {
                                ActiveTab::RunHistory => {
                                    views::history::refresh_runs(&mut self.state);
                                }
                                ActiveTab::Accounts | ActiveTab::TestBuilder => {
                                    views::accounts::refresh_accounts(&mut self.state);
                                }
                                _ => {}
                            }
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Workspace: {}",
                                self.state.config.workspace_dir.display()
                            ))
                            .size(11.0)
                            .color(TEXT_MUTED),
                        );
                    });
                });
            });

        // Optional Bottom Toast Bar
        if let Some(toast) = self.state.toast.clone() {
            let age_secs = (Utc::now() - toast.created_at).num_seconds();
            if age_secs > 12 {
                self.state.toast = None;
            } else {
                egui::TopBottomPanel::bottom("deming_toast_bar")
                    .frame(
                        egui::Frame::new()
                            .fill(BG_SURFACE)
                            .stroke(egui::Stroke::new(
                                1.0_f32,
                                if toast.is_error {
                                    DANGER_RED
                                } else {
                                    ACCENT_EMERALD
                                },
                            ))
                            .inner_margin(egui::Margin::symmetric(14, 6)),
                    )
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&toast.text).color(if toast.is_error {
                                    DANGER_RED
                                } else {
                                    ACCENT_EMERALD
                                }),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Dismiss").clicked() {
                                        self.state.toast = None;
                                    }
                                },
                            );
                        });
                    });
            }
        }

        // Main Workspace Area
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG_BASE)
                    .inner_margin(egui::Margin::same(14)),
            )
            .show(ctx, |ui| match self.state.active_tab {
                ActiveTab::Dashboard => views::dashboard::render(ui, &mut self.state, &self.bridge),
                ActiveTab::TestBuilder => {
                    views::builder::render(ui, &mut self.state, &self.bridge);
                }
                ActiveTab::RunHistory => views::history::render(ui, &mut self.state),
                ActiveTab::Accounts => views::accounts::render(ui, &mut self.state, &self.bridge),
                ActiveTab::Settings => views::settings::render(ui, &mut self.state, &self.bridge),
            });
    }
}
