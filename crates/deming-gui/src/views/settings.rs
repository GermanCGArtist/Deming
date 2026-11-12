//! Settings tab: workspace directory, global resource limits, process priority, and retention policy.

use chrono::Utc;
use deming_core::{DemingConfig, ProcessPriority};
use egui::{RichText, ScrollArea};

use crate::runner_bridge::GuiBridge;
use crate::state::{SettingsDraft, StatusToast, UiState};
use crate::theme::{card_frame, primary_button, sub_card_frame, ACCENT_EMERALD, TEXT_MUTED};

pub fn render(ui: &mut egui::Ui, state: &mut UiState, bridge: &GuiBridge) {
    ScrollArea::vertical()
        .id_salt("settings_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            card_frame().show(ui, |ui| {
                ui.label(
                    RichText::new("Workspace & Paths")
                        .size(15.5)
                        .strong()
                        .color(ACCENT_EMERALD),
                );
                ui.label(
                    RichText::new(format!("Config file: {}", state.config_path.display()))
                        .size(11.5)
                        .color(TEXT_MUTED),
                );
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.label("Workspace Directory:");
                    ui.add(
                        egui::TextEdit::singleline(&mut state.settings.workspace_dir)
                            .desired_width(380.0),
                    );
                    if ui.button("Browse...").clicked() {
                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                            state.settings.workspace_dir =
                                folder.to_string_lossy().to_string();
                        }
                    }
                });
            });

            ui.add_space(10.0);

            card_frame().show(ui, |ui| {
                ui.label(
                    RichText::new("Global Resource Limits")
                        .size(15.5)
                        .strong()
                        .color(ACCENT_EMERALD),
                );
                ui.add_space(6.0);

                egui::Grid::new("settings_limits_grid")
                    .num_columns(2)
                    .spacing([18.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Max Total RAM Budget (MiB):");
                        ui.add(
                            egui::DragValue::new(&mut state.settings.max_total_ram_mb)
                                .range(512..=524_288)
                                .speed(256),
                        );
                        ui.end_row();

                        ui.label("Max Concurrent Instances:");
                        ui.add(
                            egui::DragValue::new(&mut state.settings.max_concurrent_instances)
                                .range(1..=64),
                        );
                        ui.end_row();

                        ui.label("Default Instance RAM (MiB):");
                        ui.add(
                            egui::DragValue::new(&mut state.settings.default_instance_ram_mb)
                                .range(256..=65_536)
                                .speed(128),
                        );
                        ui.end_row();

                        ui.label("Default CPU Cores per Instance:");
                        ui.add(
                            egui::DragValue::new(
                                &mut state.settings.default_cpu_cores_per_instance,
                            )
                            .range(1..=128),
                        );
                        ui.end_row();

                        ui.label("JVM Process Priority:");
                        egui::ComboBox::from_id_salt("settings_priority")
                            .selected_text(priority_label(state.settings.process_priority))
                            .show_ui(ui, |ui| {
                                for p in [
                                    ProcessPriority::BelowNormal,
                                    ProcessPriority::Normal,
                                    ProcessPriority::AboveNormal,
                                    ProcessPriority::High,
                                ] {
                                    ui.selectable_value(
                                        &mut state.settings.process_priority,
                                        p,
                                        priority_label(p),
                                    );
                                }
                            });
                        ui.end_row();
                    });
            });

            ui.add_space(10.0);

            card_frame().show(ui, |ui| {
                ui.label(
                    RichText::new("Artifact Retention & Pruning")
                        .size(15.5)
                        .strong()
                        .color(ACCENT_EMERALD),
                );
                ui.add_space(6.0);

                egui::Grid::new("settings_retention_grid")
                    .num_columns(2)
                    .spacing([18.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Max Retained Runs:");
                        ui.add(egui::DragValue::new(&mut state.settings.max_runs).range(1..=10000));
                        ui.end_row();

                        ui.label("Compress to .zip After Runs:");
                        ui.add(
                            egui::DragValue::new(&mut state.settings.compress_after_runs)
                                .range(0..=10000),
                        );
                        ui.end_row();

                        ui.label("Auto-Prune After Each Run:");
                        ui.checkbox(&mut state.settings.auto_prune, "Enabled");
                        ui.end_row();
                    });
            });

            ui.add_space(12.0);

            sub_card_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.add(primary_button("  Save Settings  ")).clicked() {
                        match state.settings.to_config() {
                            Ok(new_cfg) => {
                                if let Err(e) = new_cfg.save(&state.config_path) {
                                    state.toast = Some(StatusToast {
                                        is_error: true,
                                        text: format!("Failed to save config: {e}"),
                                        created_at: Utc::now(),
                                    });
                                } else {
                                    let ws_changed =
                                        new_cfg.workspace_dir != state.config.workspace_dir;
                                    state.config = new_cfg;
                                    if ws_changed {
                                        bridge.start_ipc_server(state.config.workspace_dir.clone());
                                        crate::views::accounts::refresh_accounts(state);
                                        crate::views::history::refresh_runs(state);
                                    }
                                    state.toast = Some(StatusToast {
                                        is_error: false,
                                        text: format!(
                                            "Saved settings to {}",
                                            state.config_path.display()
                                        ),
                                        created_at: Utc::now(),
                                    });
                                }
                            }
                            Err(err) => {
                                state.toast = Some(StatusToast {
                                    is_error: true,
                                    text: err,
                                    created_at: Utc::now(),
                                });
                            }
                        }
                    }

                    if ui.button("Reset to Defaults").clicked() {
                        state.settings = SettingsDraft::from_config(&DemingConfig::default());
                    }
                });
            });
        });
}

fn priority_label(p: ProcessPriority) -> &'static str {
    match p {
        ProcessPriority::BelowNormal => "Below Normal",
        ProcessPriority::Normal => "Normal",
        ProcessPriority::AboveNormal => "Above Normal",
        ProcessPriority::High => "High",
    }
}
