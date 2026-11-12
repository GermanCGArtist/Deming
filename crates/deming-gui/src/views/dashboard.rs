//! Dashboard tab: STOP / ABORT ALL, live run cards, per-instance controls, and live log drawer.

use chrono::Utc;
use deming_core::{InstanceState, RunStatus};
use egui::{Color32, RichText, ScrollArea};

use crate::runner_bridge::GuiBridge;
use crate::state::{ActiveTab, LiveRunCard, UiState};
use crate::theme::{
    card_frame, danger_button, instance_state_badge, primary_button, run_status_badge,
    status_badge, sub_card_frame, ACCENT_EMERALD, BG_INPUT, DANGER_RED, INFO_BLUE, TEXT_MUTED,
    TEXT_PRIMARY,
};

pub fn render(ui: &mut egui::Ui, state: &mut UiState, bridge: &GuiBridge) {
    // Top bar: Prominent red STOP / ABORT ALL button + IPC status
    card_frame().show(ui, |ui| {
        ui.horizontal(|ui| {
            let active_count = state
                .dashboard
                .runs
                .iter()
                .filter(|r| matches!(r.status, RunStatus::Running | RunStatus::Pending))
                .count();

            let stop_btn = egui::Button::new(
                RichText::new("  STOP / ABORT ALL  ")
                    .color(TEXT_PRIMARY)
                    .size(15.0)
                    .strong(),
            )
            .fill(DANGER_RED)
            .min_size(egui::vec2(210.0, 38.0));

            if ui
                .add(stop_btn)
                .on_hover_text("Immediately terminates all active local Job Objects and broadcasts AbortAll over IPC")
                .clicked()
            {
                bridge.abort_all();
                state.toast = Some(crate::state::StatusToast {
                    is_error: false,
                    text: "Abort All triggered (local Job Objects terminated + IPC AbortAll broadcast)".to_string(),
                    created_at: Utc::now(),
                });
            }

            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("Active Runs: {active_count}"))
                            .strong()
                            .color(if active_count > 0 { INFO_BLUE } else { TEXT_MUTED }),
                    );
                    ui.separator();
                    if let Some(port) = state.ipc_port {
                        status_badge(ui, &format!("IPC 127.0.0.1:{port}"), ACCENT_EMERALD);
                    } else if let Some(ref err) = state.ipc_error {
                        status_badge(ui, &format!("IPC Error: {err}"), DANGER_RED);
                    } else {
                        status_badge(ui, "IPC Starting...", TEXT_MUTED);
                    }
                });
                ui.label(
                    RichText::new(
                        "Runs launched locally or by AI agents via CLI/MCP appear here in real time.",
                    )
                    .size(11.5)
                    .color(TEXT_MUTED),
                );
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Clear Finished").clicked() {
                    state
                        .dashboard
                        .runs
                        .retain(|r| matches!(r.status, RunStatus::Running | RunStatus::Pending));
                }
                if ui.add(primary_button("+ New Test Run")).clicked() {
                    state.active_tab = ActiveTab::TestBuilder;
                }
            });
        });
    });

    ui.add_space(10.0);

    if state.dashboard.runs.is_empty() {
        card_frame().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(32.0);
                ui.label(
                    RichText::new("No active or recent runs yet")
                        .size(16.0)
                        .strong()
                        .color(TEXT_PRIMARY),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Configure a job in Test Builder or launch a run from `deming run submit` / MCP.",
                    )
                    .color(TEXT_MUTED),
                );
                ui.add_space(14.0);
                if ui.add(primary_button("Open Test Builder")).clicked() {
                    state.active_tab = ActiveTab::TestBuilder;
                }
                ui.add_space(32.0);
            });
        });
        return;
    }

    // Main content: Run cards on top/left, live log viewer drawer below if selected
    let has_log_drawer = state.dashboard.selected_log_target.is_some();
    let list_height = if has_log_drawer {
        (ui.available_height() * 0.56).max(200.0)
    } else {
        ui.available_height()
    };

    ScrollArea::vertical()
        .id_salt("dashboard_runs_scroll")
        .max_height(list_height)
        .show(ui, |ui| {
            let runs_snapshot: Vec<LiveRunCard> = state.dashboard.runs.clone();
            for run in &runs_snapshot {
                render_run_card(ui, run, state, bridge);
                ui.add_space(10.0);
            }
        });

    if let Some((run_id, source)) = state.dashboard.selected_log_target.clone() {
        ui.add_space(8.0);
        render_live_log_drawer(ui, state, &run_id, &source);
    }
}

fn render_run_card(
    ui: &mut egui::Ui,
    run: &LiveRunCard,
    state: &mut UiState,
    bridge: &GuiBridge,
) {
    card_frame().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&run.job_name)
                    .size(16.0)
                    .strong()
                    .color(TEXT_PRIMARY),
            );
            let (status_label, status_color) = run_status_badge(run.status);
            status_badge(ui, status_label, status_color);
            status_badge(ui, &run.source_label, TEXT_MUTED);

            if let Some(port) = run.server_port {
                status_badge(ui, &format!("Server :{}", port), ACCENT_EMERALD);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !run.server_logs.is_empty() && ui.button("View Server Log").clicked() {
                    state.dashboard.selected_log_target =
                        Some((run.run_id.clone(), "server".to_string()));
                }
                let elapsed = match run.finished_at {
                    Some(end) => (end - run.started_at).num_seconds().max(0),
                    None => (Utc::now() - run.started_at).num_seconds().max(0),
                };
                ui.label(
                    RichText::new(format!("{} · {}s", short_id(&run.run_id), elapsed))
                        .monospace()
                        .size(11.5)
                        .color(TEXT_MUTED),
                );
            });
        });

        ui.add_space(8.0);

        if run.instances.is_empty() {
            ui.label(
                RichText::new("Waiting for instance events...")
                    .italics()
                    .color(TEXT_MUTED),
            );
            return;
        }

        for inst in &run.instances {
            sub_card_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&inst.instance_id)
                            .strong()
                            .size(13.5)
                            .color(TEXT_PRIMARY),
                    );
                    let (badge_txt, badge_col) = instance_state_badge(inst.state);
                    status_badge(ui, badge_txt, badge_col);

                    ui.label(
                        RichText::new(format!(
                            "{} MiB · {} CPU{}",
                            inst.ram_mb,
                            inst.cpu_cores,
                            inst.pid
                                .map(|p| format!(" · PID {p}"))
                                .unwrap_or_default()
                        ))
                        .size(12.0)
                        .color(TEXT_MUTED),
                    );

                    if !inst.screenshots.is_empty() {
                        ui.label(
                            RichText::new(format!("📷 {}", inst.screenshots.len()))
                                .size(12.0)
                                .color(ACCENT_EMERALD),
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let is_active = matches!(
                            inst.state,
                            InstanceState::Queued
                                | InstanceState::Preparing
                                | InstanceState::Starting
                                | InstanceState::Running
                        );

                        if ui.button("View Log").clicked() {
                            state.dashboard.selected_log_target =
                                Some((run.run_id.clone(), inst.instance_id.clone()));
                        }

                        if ui
                            .add_enabled(
                                inst.state == InstanceState::Running || !is_active && inst.pid.is_some(),
                                egui::Button::new("Screenshot"),
                            )
                            .on_hover_text("Capture HWND screenshot of this running instance")
                            .clicked()
                        {
                            bridge.request_screenshot(&run.run_id, &inst.instance_id);
                        }

                        if ui
                            .add_enabled(is_active, danger_button("Kill"))
                            .on_hover_text("Terminate this instance immediately")
                            .clicked()
                        {
                            bridge.kill_instance(&run.run_id, &inst.instance_id);
                        }
                    });
                });

                if let Some(ref reason) = inst.reason {
                    ui.label(
                        RichText::new(reason)
                            .size(11.5)
                            .color(if inst.passed == Some(true) {
                                ACCENT_EMERALD
                            } else {
                                TEXT_MUTED
                            }),
                    );
                } else if let Some(ref msg) = inst.status_message {
                    ui.label(RichText::new(msg).size(11.5).color(TEXT_MUTED));
                }
            });
            ui.add_space(4.0);
        }
    });
}

fn render_live_log_drawer(ui: &mut egui::Ui, state: &mut UiState, run_id: &str, source: &str) {
    let lines: Vec<String> = state
        .dashboard
        .runs
        .iter()
        .find(|r| r.run_id == run_id)
        .map(|r| {
            if source == "server" {
                r.server_logs.clone()
            } else {
                r.instances
                    .iter()
                    .find(|i| i.instance_id == source)
                    .map(|i| i.logs.clone())
                    .unwrap_or_default()
            }
        })
        .unwrap_or_default();

    card_frame().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Live Log — {} / {}", short_id(run_id), source))
                    .strong()
                    .color(ACCENT_EMERALD),
            );
            ui.add_space(8.0);
            ui.label(RichText::new("Filter:").color(TEXT_MUTED));
            ui.add(
                egui::TextEdit::singleline(&mut state.dashboard.log_filter)
                    .desired_width(180.0)
                    .hint_text("substring filter..."),
            );
            ui.checkbox(&mut state.dashboard.auto_scroll_logs, "Auto-scroll");

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close Log").clicked() {
                    state.dashboard.selected_log_target = None;
                }
                ui.label(
                    RichText::new(format!("{} lines", lines.len()))
                        .size(11.5)
                        .color(TEXT_MUTED),
                );
            });
        });

        ui.add_space(6.0);

        let filter_lower = state.dashboard.log_filter.trim().to_ascii_lowercase();
        egui::Frame::new()
            .fill(BG_INPUT)
            .inner_margin(egui::Margin::same(8))
            .corner_radius(egui::CornerRadius::same(6))
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .id_salt("dashboard_live_log_scroll")
                    .auto_shrink([false, false])
                    .stick_to_bottom(state.dashboard.auto_scroll_logs)
                    .show(ui, |ui| {
                        for line in &lines {
                            if !filter_lower.is_empty()
                                && !line.to_ascii_lowercase().contains(&filter_lower)
                            {
                                continue;
                            }
                            let color = if line.contains("ERROR")
                                || line.contains("FATAL")
                                || line.contains("Exception")
                            {
                                DANGER_RED
                            } else if line.contains("WARN") {
                                Color32::from_rgb(0xF5, 0x9E, 0x0B)
                            } else {
                                TEXT_PRIMARY
                            };
                            ui.label(RichText::new(line).monospace().size(11.5).color(color));
                        }
                    });
            });
    });
}

fn short_id(id: &str) -> &str {
    if id.len() > 8 {
        &id[..8]
    } else {
        id
    }
}
