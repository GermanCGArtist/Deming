//! Test Builder tab: construct, save/load, and launch multi-instance + local server TestJobSpecs.

mod accounts_panel;
mod clients_panel;
mod server_panel;

use chrono::Utc;
use deming_core::{DemingLockfile, ModsManifest};
use egui::{RichText, ScrollArea};

use crate::preset::ModEntryDraft;
use crate::runner_bridge::GuiBridge;
use crate::state::{ActiveTab, StatusToast, UiState};
use crate::theme::{
    card_frame, primary_button, status_badge, sub_card_frame, ACCENT_EMERALD, DANGER_RED,
    INFO_BLUE, TEXT_MUTED, TEXT_PRIMARY, WARN_AMBER,
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

                if ui
                    .add_enabled(!state.diagnostics.in_flight, egui::Button::new("Export .mrpack"))
                    .clicked()
                {
                    match build_manifest_for_instance(state, None) {
                        Ok(manifest) => {
                            let default_name = format!(
                                "{}.mrpack",
                                state
                                    .builder
                                    .job_name
                                    .trim()
                                    .chars()
                                    .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                                        c
                                    } else {
                                        '_'
                                    })
                                    .collect::<String>()
                            );
                            if let Some(out_path) = rfd::FileDialog::new()
                                .add_filter("Modrinth Modpack", &["mrpack"])
                                .set_file_name(&default_name)
                                .save_file()
                            {
                                state.diagnostics.in_flight = true;
                                state.diagnostics.last_error = None;
                                bridge.export_mrpack_async(
                                    state.config.clone(),
                                    manifest,
                                    state.diagnostics.last_lockfile.clone(),
                                    out_path,
                                );
                            }
                        }
                        Err(err) => {
                            state.diagnostics.last_error = Some(err.clone());
                            state.toast = Some(StatusToast {
                                is_error: true,
                                text: err,
                                created_at: Utc::now(),
                            });
                        }
                    }
                }

                if ui
                    .add_enabled(
                        !state.diagnostics.in_flight,
                        egui::Button::new("Import .mrpack / .zip"),
                    )
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Modpacks (.mrpack, .zip)", &["mrpack", "zip"])
                        .pick_file()
                    {
                        state.diagnostics.in_flight = true;
                        state.diagnostics.last_error = None;
                        bridge.import_modpack(state.config.clone(), path);
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
            clients_panel::render_clients_section(ui, state);

            ui.add_space(10.0);

            server_panel::render_server_editor(ui, &mut state.builder.server);

            ui.add_space(10.0);

            render_diagnostics_and_sync_drawer(ui, state, bridge);
        });
}

pub(super) fn render_mods_editor(
    ui: &mut egui::Ui,
    id_prefix: String,
    mods: &mut Vec<ModEntryDraft>,
) {
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

fn build_manifest_for_instance(
    state: &UiState,
    target_id: Option<&str>,
) -> Result<ModsManifest, String> {
    let inst = match target_id {
        Some(id) if !id.trim().is_empty() => state
            .builder
            .instances
            .iter()
            .find(|i| i.id == id)
            .or_else(|| state.builder.instances.first()),
        _ => state.builder.instances.first(),
    }
    .ok_or_else(|| "At least one client instance is required".to_string())?;

    let mc = inst.mc_version.trim();
    if mc.is_empty() {
        return Err("Instance MC version cannot be empty".to_string());
    }
    let mut mods = Vec::with_capacity(inst.mods.len());
    for m in &inst.mods {
        mods.push(m.to_mod_source()?);
    }
    let loader_ver = inst.loader_version.trim();
    Ok(ModsManifest {
        mc_version: mc.to_string(),
        loader: inst.loader,
        loader_version: if loader_ver.is_empty() {
            None
        } else {
            Some(loader_ver.to_string())
        },
        mods,
    })
}

/// Renders a collapsible Diagnostics & Mod Sync drawer surfacing sync diffs, errors, and `deming.lock` summary.
pub fn render_diagnostics_and_sync_drawer(
    ui: &mut egui::Ui,
    state: &mut UiState,
    bridge: &GuiBridge,
) {
    card_frame().show(ui, |ui| {
        let has_error = state.diagnostics.last_error.is_some();
        let header_title = if has_error {
            "Diagnostics & Mod Sync Drawer (Warnings / Errors)"
        } else {
            "Diagnostics & Mod Sync Drawer"
        };

        egui::CollapsingHeader::new(
            RichText::new(header_title)
                .size(14.5)
                .strong()
                .color(if has_error { DANGER_RED } else { ACCENT_EMERALD }),
        )
        .default_open(has_error || state.diagnostics.last_sync_plan.is_some())
        .show(ui, |ui| {
            let instance_ids: Vec<String> = state
                .builder
                .instances
                .iter()
                .map(|i| i.id.clone())
                .collect();
            if !instance_ids.is_empty()
                && !instance_ids.contains(&state.diagnostics.target_instance_id)
            {
                state.diagnostics.target_instance_id = instance_ids[0].clone();
            }

            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Target Instance:").color(TEXT_MUTED));
                if !instance_ids.is_empty() {
                    egui::ComboBox::from_id_salt("diag_sync_target_instance")
                        .selected_text(&state.diagnostics.target_instance_id)
                        .show_ui(ui, |ui| {
                            for id in &instance_ids {
                                ui.selectable_value(
                                    &mut state.diagnostics.target_instance_id,
                                    id.clone(),
                                    id,
                                );
                            }
                        });
                } else {
                    ui.add(
                        egui::TextEdit::singleline(&mut state.diagnostics.target_instance_id)
                            .desired_width(140.0),
                    );
                }

                let busy = state.diagnostics.in_flight;
                if ui
                    .add_enabled(!busy, egui::Button::new("Dry-Run Sync"))
                    .clicked()
                {
                    trigger_sync(state, bridge, true);
                }
                if ui
                    .add_enabled(!busy, primary_button("Apply Sync"))
                    .clicked()
                {
                    trigger_sync(state, bridge, false);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Load deming.lock"))
                    .clicked()
                {
                    let lock_path = state
                        .config
                        .instances_dir()
                        .join(&state.diagnostics.target_instance_id)
                        .join("deming.lock");
                    match DemingLockfile::load(&lock_path) {
                        Ok(lf) => {
                            state.diagnostics.last_error = None;
                            state.diagnostics.last_lockfile = Some(lf);
                        }
                        Err(e) => {
                            state.diagnostics.last_error = Some(format!(
                                "Failed to load {}: {e}",
                                lock_path.display()
                            ));
                        }
                    }
                }

                if busy {
                    ui.spinner();
                    ui.label(RichText::new("Syncing / importing...").color(INFO_BLUE));
                }
            });

            if let Some(ref pack_summary) = state.diagnostics.last_pack_summary {
                ui.add_space(6.0);
                status_badge(ui, pack_summary, INFO_BLUE);
            }

            if let Some(ref err) = state.diagnostics.last_error.clone() {
                ui.add_space(6.0);
                sub_card_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Sync / Dependency Diagnostics")
                                .strong()
                                .color(DANGER_RED),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Dismiss").clicked() {
                                state.diagnostics.last_error = None;
                            }
                        });
                    });
                    ui.label(RichText::new(err).monospace().size(11.5).color(DANGER_RED));
                });
            }

            if let Some(ref plan) = state.diagnostics.last_sync_plan {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Last Sync Diff:").strong().color(TEXT_PRIMARY));
                    status_badge(ui, &format!("+{} added", plan.added.len()), ACCENT_EMERALD);
                    status_badge(ui, &format!("~{} updated", plan.updated.len()), WARN_AMBER);
                    status_badge(ui, &format!("-{} removed", plan.removed.len()), DANGER_RED);
                    status_badge(ui, &format!("={} unchanged", plan.unchanged.len()), TEXT_MUTED);
                });
                for m in &plan.added {
                    ui.label(
                        RichText::new(format!("  + {} ({})", m.filename, m.version_id))
                            .monospace()
                            .size(11.0)
                            .color(ACCENT_EMERALD),
                    );
                }
                for (old, new) in &plan.updated {
                    ui.label(
                        RichText::new(format!(
                            "  ~ {} ({} -> {})",
                            new.filename, old.version_id, new.version_id
                        ))
                        .monospace()
                        .size(11.0)
                        .color(WARN_AMBER),
                    );
                }
                for m in &plan.removed {
                    ui.label(
                        RichText::new(format!("  - {}", m.filename))
                            .monospace()
                            .size(11.0)
                            .color(DANGER_RED),
                    );
                }
            }

            if let Some(ref lf) = state.diagnostics.last_lockfile {
                ui.add_space(6.0);
                sub_card_frame().show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "Lockfile Summary (v{}, MC {}, {:?}{}, {} mods)",
                                lf.schema_version,
                                lf.mc_version,
                                lf.loader,
                                lf.loader_version
                                    .as_deref()
                                    .map(|v| format!(" {v}"))
                                    .unwrap_or_default(),
                                lf.mods.len()
                            ))
                            .strong()
                            .size(12.5)
                            .color(ACCENT_EMERALD),
                        );
                    });

                    if !lf.mods.is_empty() {
                        ui.add_space(4.0);
                        ScrollArea::vertical()
                            .id_salt("diag_lockfile_scroll")
                            .max_height(140.0)
                            .show(ui, |ui| {
                                for m in &lf.mods {
                                    let short_hash = &m.sha256[..m.sha256.len().min(12)];
                                    ui.label(
                                        RichText::new(format!(
                                            "{}  [{} · {}]  sha256:{}",
                                            m.filename, m.source, m.version_id, short_hash
                                        ))
                                        .monospace()
                                        .size(11.0)
                                        .color(TEXT_PRIMARY),
                                    );
                                }
                            });
                    }
                });
            }
        });
    });
}

fn trigger_sync(state: &mut UiState, bridge: &GuiBridge, dry_run: bool) {
    let target = state.diagnostics.target_instance_id.clone();
    match build_manifest_for_instance(state, Some(&target)) {
        Ok(manifest) => {
            let inst_dir = state.config.instances_dir().join(&target);
            state.diagnostics.in_flight = true;
            state.diagnostics.last_error = None;
            bridge.sync_instance_mods_async(state.config.clone(), manifest, inst_dir, dry_run);
        }
        Err(err) => {
            state.diagnostics.last_error = Some(err.clone());
            state.toast = Some(StatusToast {
                is_error: true,
                text: err,
                created_at: Utc::now(),
            });
        }
    }
}
