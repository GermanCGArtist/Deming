//! Client instances section, instance editor, and completion rule editor.

use egui::RichText;

use crate::preset::{
    mod_loader_label, AccountDraft, CompletionRuleDraft, CompletionRuleKind, InstanceDraft,
    ALL_MOD_LOADERS,
};
use crate::state::UiState;
use crate::theme::{
    card_frame, danger_button, primary_button, sub_card_frame, ACCENT_EMERALD, TEXT_MUTED,
    TEXT_PRIMARY,
};

use super::accounts_panel::render_instance_account_selector;
use super::render_mods_editor;

pub(super) fn render_clients_section(ui: &mut egui::Ui, state: &mut UiState) {
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
            let keep_open =
                render_instance_editor(ui, idx, total_instances, inst, &stored_accounts);
            if !keep_open {
                remove_idx = Some(idx);
            }
            ui.add_space(8.0);
        }

        if let Some(idx) = remove_idx {
            state.builder.instances.remove(idx);
        }
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
            render_instance_account_selector(ui, idx, &mut inst.account, stored_accounts);
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
            RichText::new("Extra JVM & Game Arguments")
                .size(12.0)
                .color(TEXT_MUTED),
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
                        ui.label(
                            RichText::new(format!("Sub-rule #{}:", s_idx + 1)).color(TEXT_MUTED),
                        );
                        if ui.button("Remove").clicked() {
                            remove_sub = Some(s_idx);
                        }
                    });
                    render_rule_editor(ui, format!("{id_salt}_sub_{s_idx}"), sub, false);
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
