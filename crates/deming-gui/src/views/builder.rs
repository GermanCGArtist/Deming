//! Test Builder tab: construct, save/load, and launch multi-instance + local server TestJobSpecs.

use chrono::Utc;
use egui::{RichText, ScrollArea};

use crate::preset::{
    mod_loader_label, AccountDraft, CompletionRuleDraft, CompletionRuleKind, InstanceDraft,
    ModEntryDraft, ALL_MOD_LOADERS,
};
use crate::runner_bridge::GuiBridge;
use crate::state::{ActiveTab, StatusToast, UiState};
use crate::theme::{
    card_frame, danger_button, primary_button, status_badge, sub_card_frame, ACCENT_EMERALD,
    TEXT_MUTED, TEXT_PRIMARY,
};

pub fn render(ui: &mut egui::Ui, state: &mut UiState, bridge: &GuiBridge) {
    // Top action bar: Job Name, Timeout, Save/Load Preset, Launch Test Run
    card_frame().show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Job Name:").strong());
            ui.add(
                egui::TextEdit::singleline(&mut state.builder.job_name)
                    .desired_width(220.0)
                    .hint_text("e.g. fabric-multiplayer-smoke"),
            );

            ui.add_space(10.0);
            ui.label(RichText::new("Timeout (s):").strong());
            ui.add(
                egui::DragValue::new(&mut state.builder.timeout_secs)
                    .range(0..=86400)
                    .speed(5),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(primary_button("  Launch Test Run  ")).clicked() {
                    match state.builder.to_test_job_spec() {
                        Ok(spec) => {
                            bridge.launch_job(state.config.clone(), spec);
                            state.active_tab = ActiveTab::Dashboard;
                            state.toast = Some(StatusToast {
                                is_error: false,
                                text: "Test run launched".to_string(),
                                created_at: Utc::now(),
                            });
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

                if ui.button("Save Preset JSON").clicked() {
                    match state.builder.to_preset_json() {
                        Ok(json) => {
                            let default_name = format!("{}.json", state.builder.job_name.trim());
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("JSON Preset", &["json"])
                                .set_file_name(&default_name)
                                .save_file()
                            {
                                match std::fs::write(&path, json) {
                                    Ok(()) => {
                                        state.toast = Some(StatusToast {
                                            is_error: false,
                                            text: format!("Saved preset to {}", path.display()),
                                            created_at: Utc::now(),
                                        });
                                    }
                                    Err(e) => {
                                        state.toast = Some(StatusToast {
                                            is_error: true,
                                            text: format!("Failed to save preset: {e}"),
                                            created_at: Utc::now(),
                                        });
                                    }
                                }
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

                if ui.button("Load Preset JSON").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("JSON Preset", &["json"])
                        .pick_file()
                    {
                        match std::fs::read_to_string(&path)
                            .map_err(|e| format!("Failed to read file: {e}"))
                            .and_then(|raw| crate::preset::BuilderState::from_preset_json(&raw))
                        {
                            Ok(loaded) => {
                                state.builder = loaded;
                                state.toast = Some(StatusToast {
                                    is_error: false,
                                    text: format!("Loaded preset from {}", path.display()),
                                    created_at: Utc::now(),
                                });
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
                }
            });
        });
    });

    ui.add_space(10.0);

    ScrollArea::vertical()
        .id_salt("builder_main_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Client Instances Section
            card_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "Client Instances ({})",
                            state.builder.instances.len()
                        ))
                        .size(15.5)
                        .strong()
                        .color(ACCENT_EMERALD),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(primary_button("+ Add Instance")).clicked() {
                            let next_idx = state.builder.instances.len() + 1;
                            let mut inst = InstanceDraft::new_with_index(next_idx);
                            if let Some(first_acc) = state.accounts.accounts.first() {
                                inst.account = AccountDraft::from_stored(first_acc);
                            }
                            state.builder.instances.push(inst);
                        }
                    });
                });

                ui.add_space(8.0);

                let stored_accounts = state.accounts.accounts.clone();
                let mut remove_idx: Option<usize> = None;
                let total_instances = state.builder.instances.len();

                for (idx, inst) in state.builder.instances.iter_mut().enumerate() {
                    let keep_open = render_instance_editor(
                        ui,
                        idx,
                        total_instances,
                        inst,
                        &stored_accounts,
                    );
                    if !keep_open {
                        remove_idx = Some(idx);
                    }
                    ui.add_space(8.0);
                }

                if let Some(idx) = remove_idx {
                    state.builder.instances.remove(idx);
                }
            });

            ui.add_space(10.0);

            // Optional Local Server Section
            render_server_editor(ui, &mut state.builder.server);
        });
}

fn render_instance_editor(
    ui: &mut egui::Ui,
    idx: usize,
    total_instances: usize,
    inst: &mut InstanceDraft,
    stored_accounts: &[deming_auth::StoredAccount],
) -> bool {
    let mut keep = true;

    sub_card_frame().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Instance #{}", idx + 1))
                    .strong()
                    .color(TEXT_PRIMARY),
            );
            ui.add(
                egui::TextEdit::singleline(&mut inst.id)
                    .desired_width(140.0)
                    .hint_text("instance id"),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if total_instances > 1 && ui.add(danger_button("Remove")).clicked() {
                    keep = false;
                }
            });
        });

        ui.add_space(6.0);

        // Version, Loader, Loader Version
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("MC Version:").color(TEXT_MUTED));
            ui.add(
                egui::TextEdit::singleline(&mut inst.mc_version)
                    .desired_width(90.0)
                    .hint_text("1.21.4"),
            );

            ui.add_space(6.0);
            ui.label(RichText::new("Loader:").color(TEXT_MUTED));
            egui::ComboBox::from_id_salt(format!("inst_loader_{idx}"))
                .selected_text(mod_loader_label(inst.loader))
                .show_ui(ui, |ui| {
                    for loader in ALL_MOD_LOADERS {
                        ui.selectable_value(&mut inst.loader, loader, mod_loader_label(loader));
                    }
                });

            ui.add_space(6.0);
            ui.label(RichText::new("Loader Version:").color(TEXT_MUTED));
            ui.add(
                egui::TextEdit::singleline(&mut inst.loader_version)
                    .desired_width(120.0)
                    .hint_text("auto (latest)"),
            );
        });

        ui.add_space(4.0);

        // RAM, CPU, Account
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut inst.use_custom_ram, "Custom RAM (MiB):");
            if inst.use_custom_ram {
                ui.add(
                    egui::DragValue::new(&mut inst.ram_mb)
                        .range(256..=65536)
                        .speed(128),
                );
            } else {
                ui.label(RichText::new("(default)").size(11.5).color(TEXT_MUTED));
            }

            ui.add_space(8.0);
            ui.checkbox(&mut inst.use_custom_cpu, "Custom CPU Cores:");
            if inst.use_custom_cpu {
                ui.add(egui::DragValue::new(&mut inst.cpu_cores).range(1..=64));
            } else {
                ui.label(RichText::new("(default)").size(11.5).color(TEXT_MUTED));
            }

            ui.add_space(8.0);
            ui.label(RichText::new("Account:").color(TEXT_MUTED));
            egui::ComboBox::from_id_salt(format!("inst_account_{idx}"))
                .selected_text(inst.account.display_label())
                .show_ui(ui, |ui| {
                    for acc in stored_accounts {
                        let draft = AccountDraft::from_stored(acc);
                        let label = draft.display_label();
                        if ui
                            .selectable_label(inst.account == draft, label)
                            .clicked()
                        {
                            inst.account = draft;
                        }
                    }
                    if stored_accounts.is_empty() {
                        ui.label(
                            RichText::new("No saved accounts (add in Accounts tab)")
                                .size(11.5)
                                .color(TEXT_MUTED),
                        );
                    }
                });
            if !inst.account.is_microsoft {
                ui.add(
                    egui::TextEdit::singleline(&mut inst.account.username)
                        .desired_width(110.0)
                        .hint_text("Offline username"),
                );
            }
        });

        ui.add_space(6.0);

        // Mods list + buttons
        render_mods_editor(ui, format!("inst_mods_{idx}"), &mut inst.mods);

        ui.add_space(6.0);

        // Test Completion Rule
        ui.label(RichText::new("Test Completion Rule").strong().size(12.5));
        render_rule_editor(ui, format!("inst_rule_{idx}"), &mut inst.test_rule, true);

        ui.add_space(4.0);
        ui.collapsing(
            RichText::new("Extra JVM & Game Arguments").size(12.0).color(TEXT_MUTED),
            |ui| {
                ui.horizontal(|ui| {
                    ui.label("JVM Args:");
                    ui.add(
                        egui::TextEdit::singleline(&mut inst.jvm_args_text)
                            .desired_width(360.0)
                            .hint_text("-XX:+UseG1GC -Dfoo=bar"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Game Args:");
                    ui.add(
                        egui::TextEdit::singleline(&mut inst.game_args_text)
                            .desired_width(360.0)
                            .hint_text("--width 1280 --height 720"),
                    );
                });
            },
        );
    });

    keep
}

fn render_mods_editor(ui: &mut egui::Ui, id_prefix: String, mods: &mut Vec<ModEntryDraft>) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(format!("Mods ({})", mods.len())).strong().size(12.5));
        if ui.button("+ Browse .jar(s)").clicked() {
            if let Some(files) = rfd::FileDialog::new()
                .add_filter("Mod JARs", &["jar"])
                .pick_files()
            {
                for f in files {
                    mods.push(ModEntryDraft::local(f.to_string_lossy().to_string()));
                }
            }
        }
        if ui.button("+ Local Path").clicked() {
            mods.push(ModEntryDraft::local(""));
        }
        if ui.button("+ Modrinth Mod").clicked() {
            mods.push(ModEntryDraft::modrinth("", None));
        }
    });

    let mut remove_mod: Option<usize> = None;
    for (m_idx, m) in mods.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            if m.is_modrinth {
                status_badge(ui, "Modrinth", ACCENT_EMERALD);
                ui.add(
                    egui::TextEdit::singleline(&mut m.modrinth_slug)
                        .desired_width(180.0)
                        .hint_text("project id or slug (e.g. fabric-api)"),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut m.modrinth_version_id)
                        .desired_width(130.0)
                        .hint_text("version id (optional)"),
                );
            } else {
                status_badge(ui, "Local JAR", egui::Color32::from_rgb(0x60, 0xA5, 0xFA));
                ui.add(
                    egui::TextEdit::singleline(&mut m.local_path)
                        .desired_width(300.0)
                        .hint_text("path/to/mod.jar"),
                );
                if ui
                    .button("Browse")
                    .on_hover_text("Pick .jar file")
                    .clicked()
                {
                    if let Some(file) = rfd::FileDialog::new()
                        .add_filter("Mod JAR", &["jar"])
                        .pick_file()
                    {
                        m.local_path = file.to_string_lossy().to_string();
                    }
                }
            }
            if ui.button("✕").clicked() {
                remove_mod = Some(m_idx);
            }
        });
    }
    if let Some(idx) = remove_mod {
        mods.remove(idx);
    }
    let _ = id_prefix;
}

fn render_rule_editor(
    ui: &mut egui::Ui,
    id_salt: String,
    rule: &mut CompletionRuleDraft,
    allow_composite: bool,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Rule Type:").color(TEXT_MUTED));
        egui::ComboBox::from_id_salt(&id_salt)
            .selected_text(rule.kind.label())
            .show_ui(ui, |ui| {
                for kind in CompletionRuleKind::ALL {
                    if !allow_composite && kind == CompletionRuleKind::Composite {
                        continue;
                    }
                    ui.selectable_value(&mut rule.kind, kind, kind.label());
                }
            });
    });

    match rule.kind {
        CompletionRuleKind::ExitCodeZero => {}
        CompletionRuleKind::LogPattern => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Success Regex:");
                ui.add(
                    egui::TextEdit::singleline(&mut rule.success_regex)
                        .desired_width(220.0)
                        .hint_text("All tests passed"),
                );
                ui.label("Failure Regex (opt):");
                ui.add(
                    egui::TextEdit::singleline(&mut rule.failure_regex)
                        .desired_width(200.0)
                        .hint_text("FATAL|Exception"),
                );
            });
        }
        CompletionRuleKind::GameTestReport => {
            ui.horizontal(|ui| {
                ui.label("Report Glob:");
                ui.add(
                    egui::TextEdit::singleline(&mut rule.report_path_glob)
                        .desired_width(260.0)
                        .hint_text("reports/*.xml"),
                );
            });
        }
        CompletionRuleKind::Composite => {
            if rule.sub_rules.is_empty() {
                rule.sub_rules.push(CompletionRuleDraft::exit_code_zero());
            }
            ui.indent(format!("{id_salt}_composite"), |ui| {
                let mut remove_sub: Option<usize> = None;
                for (s_idx, sub) in rule.sub_rules.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Sub-rule #{}:", s_idx + 1)).color(TEXT_MUTED));
                        if ui.button("Remove").clicked() {
                            remove_sub = Some(s_idx);
                        }
                    });
                    render_rule_editor(
                        ui,
                        format!("{id_salt}_sub_{s_idx}"),
                        sub,
                        false,
                    );
                }
                if let Some(idx) = remove_sub {
                    rule.sub_rules.remove(idx);
                }
                if ui.button("+ Add Sub-rule").clicked() {
                    rule.sub_rules.push(CompletionRuleDraft::exit_code_zero());
                }
            });
        }
    }
}

fn render_server_editor(ui: &mut egui::Ui, server: &mut crate::preset::ServerDraft) {
    card_frame().show(ui, |ui| {
        ui.horizontal(|ui| {
            let was_enabled = server.enabled;
            ui.checkbox(
                &mut server.enabled,
                RichText::new("Local Dedicated Server (Optional)")
                    .size(15.5)
                    .strong()
                    .color(if was_enabled {
                        ACCENT_EMERALD
                    } else {
                        TEXT_PRIMARY
                    }),
            );
        });

        if !server.enabled {
            ui.label(
                RichText::new("Enable to spin up a local Minecraft server and auto-connect clients.")
                    .size(12.0)
                    .color(TEXT_MUTED),
            );
            return;
        }

        ui.add_space(6.0);

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("MC Version:").color(TEXT_MUTED));
            ui.add(egui::TextEdit::singleline(&mut server.mc_version).desired_width(90.0));

            ui.label(RichText::new("Loader:").color(TEXT_MUTED));
            egui::ComboBox::from_id_salt("srv_loader")
                .selected_text(mod_loader_label(server.loader))
                .show_ui(ui, |ui| {
                    for loader in ALL_MOD_LOADERS {
                        ui.selectable_value(&mut server.loader, loader, mod_loader_label(loader));
                    }
                });

            ui.label(RichText::new("Loader Version:").color(TEXT_MUTED));
            ui.add(
                egui::TextEdit::singleline(&mut server.loader_version)
                    .desired_width(120.0)
                    .hint_text("auto"),
            );
        });

        ui.add_space(4.0);

        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut server.online_mode, "online-mode");
            ui.checkbox(&mut server.auto_connect_clients, "Auto-connect clients");
            ui.checkbox(&mut server.auto_port, "Auto ephemeral port");
            if !server.auto_port {
                ui.label("Port:");
                ui.add(egui::DragValue::new(&mut server.port).range(1..=65535));
            }
            ui.checkbox(&mut server.use_custom_ram, "Custom RAM (MiB):");
            if server.use_custom_ram {
                ui.add(
                    egui::DragValue::new(&mut server.ram_mb)
                        .range(512..=65536)
                        .speed(128),
                );
            }
        });

        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("Ready Pattern (Regex):");
            ui.add(
                egui::TextEdit::singleline(&mut server.ready_pattern)
                    .desired_width(340.0),
            );
        });

        ui.add_space(6.0);
        render_mods_editor(ui, "srv_mods".to_string(), &mut server.mods);

        ui.add_space(6.0);
        ui.label(RichText::new("server.properties overrides (key=value per line):").color(TEXT_MUTED));
        ui.add(
            egui::TextEdit::multiline(&mut server.properties_text)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .code_editor(),
        );
    });
}
