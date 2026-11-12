//! Run History & Archive tab: manifest summary, regex log viewer, inline PNG viewer, and retention trigger.

use std::io::Read;
use std::path::Path;

use chrono::Utc;
use deming_archive::{apply_retention, LogSearchFilter, RunArchiveReader, ScreenshotInfo};
use egui::{ColorImage, RichText, ScrollArea};
use zip::ZipArchive;

use crate::state::{StatusToast, UiState};
use crate::theme::{
    card_frame, instance_state_badge, primary_button, run_status_badge, status_badge,
    sub_card_frame, ACCENT_EMERALD, BG_INPUT, DANGER_RED, TEXT_MUTED, TEXT_PRIMARY, WARN_AMBER,
};

pub fn refresh_runs(state: &mut UiState) {
    let reader = RunArchiveReader::new(state.config.runs_dir());
    match reader.list_runs() {
        Ok(runs) => {
            state.history.runs = runs;
            state.history.load_error = None;
            if let Some(idx) = state.history.selected_run_idx {
                if idx >= state.history.runs.len() {
                    state.history.selected_run_idx = None;
                    state.history.selected_manifest = None;
                    state.history.screenshots.clear();
                    state.history.log_matches.clear();
                }
            }
        }
        Err(e) => {
            state.history.load_error = Some(e.to_string());
        }
    }
}

fn select_run(ui_ctx: &egui::Context, state: &mut UiState, idx: usize) {
    state.history.selected_run_idx = Some(idx);
    state.history.selected_manifest = None;
    state.history.log_matches.clear();
    state.history.log_search_error = None;
    state.history.screenshots.clear();
    state.history.selected_screenshot_idx = None;
    state.history.screenshot_load_error = None;

    let Some(run) = state.history.runs.get(idx).cloned() else {
        return;
    };
    let reader = RunArchiveReader::new(state.config.runs_dir());

    match reader.load_manifest(&run.path) {
        Ok(m) => state.history.selected_manifest = Some(m),
        Err(e) => state.history.load_error = Some(format!("Failed to load manifest: {e}")),
    }

    if let Ok(shots) = reader.list_screenshots(&run.path, None) {
        state.history.screenshots = shots;
        if !state.history.screenshots.is_empty() {
            state.history.selected_screenshot_idx = Some(0);
            ensure_screenshot_texture(ui_ctx, state, &run.path, 0);
        }
    }

    run_log_search(state, &run.path);
}

fn run_log_search(state: &mut UiState, run_path: &Path) {
    let reader = RunArchiveReader::new(state.config.runs_dir());
    let inst_filter = state.history.log_instance_filter.trim();
    let file_filter = state.history.log_file_filter.trim();
    let regex_pat = state.history.log_regex.trim();

    let filter = LogSearchFilter {
        instance_id: if inst_filter.is_empty() {
            None
        } else {
            Some(inst_filter.to_string())
        },
        log_file: if file_filter.is_empty() {
            None
        } else {
            Some(file_filter.to_string())
        },
        regex: if regex_pat.is_empty() {
            None
        } else {
            Some(regex_pat.to_string())
        },
        case_sensitive: state.history.log_case_sensitive,
        include_server: true,
        limit: Some(500),
    };

    match reader.search_logs(run_path, &filter) {
        Ok(matches) => {
            state.history.log_matches = matches;
            state.history.log_search_error = None;
        }
        Err(e) => {
            state.history.log_matches.clear();
            state.history.log_search_error = Some(e.to_string());
        }
    }
}

fn ensure_screenshot_texture(
    ctx: &egui::Context,
    state: &mut UiState,
    run_path: &Path,
    shot_idx: usize,
) {
    let Some(shot) = state.history.screenshots.get(shot_idx).cloned() else {
        return;
    };
    let cache_key = format!("{}::{}", run_path.display(), shot.relative_path);
    if state.history.loaded_textures.contains_key(&cache_key) {
        state.history.screenshot_load_error = None;
        return;
    }

    match load_screenshot_bytes(run_path, &shot) {
        Ok(bytes) => match image::load_from_memory(&bytes) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                let color_img = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                let tex = ctx.load_texture(
                    cache_key.clone(),
                    color_img,
                    egui::TextureOptions::LINEAR,
                );
                state.history.loaded_textures.insert(cache_key, tex);
                state.history.screenshot_load_error = None;
            }
            Err(e) => {
                state.history.screenshot_load_error = Some(format!("Failed to decode PNG: {e}"));
            }
        },
        Err(e) => {
            state.history.screenshot_load_error = Some(e);
        }
    }
}

fn load_screenshot_bytes(run_path: &Path, shot: &ScreenshotInfo) -> Result<Vec<u8>, String> {
    if run_path.is_dir() {
        let full = run_path.join(&shot.relative_path);
        std::fs::read(&full).map_err(|e| format!("Failed to read {}: {e}", full.display()))
    } else if run_path.is_file() {
        let file = std::fs::File::open(run_path)
            .map_err(|e| format!("Failed to open zip {}: {e}", run_path.display()))?;
        let mut archive =
            ZipArchive::new(file).map_err(|e| format!("Invalid zip archive: {e}"))?;
        let target = shot.relative_path.replace('\\', "/");
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("Zip entry error: {e}"))?;
            if entry.name().replace('\\', "/") == target {
                let mut buf = Vec::with_capacity(entry.size() as usize);
                entry
                    .read_to_end(&mut buf)
                    .map_err(|e| format!("Failed reading zip screenshot: {e}"))?;
                return Ok(buf);
            }
        }
        Err(format!("Screenshot '{}' not found in zip archive", target))
    } else {
        Err(format!("Run path does not exist: {}", run_path.display()))
    }
}

pub fn render(ui: &mut egui::Ui, state: &mut UiState) {
    // Top controls: Refresh + Retention Policy Trigger
    card_frame().show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new("Archived Test Runs")
                    .size(16.0)
                    .strong()
                    .color(TEXT_PRIMARY),
            );
            ui.label(
                RichText::new(format!("({})", state.config.runs_dir().display()))
                    .size(11.5)
                    .color(TEXT_MUTED),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(primary_button("Refresh Runs")).clicked() {
                    refresh_runs(state);
                }

                if ui
                    .button("Apply Retention & Compress")
                    .on_hover_text(format!(
                        "max_runs={}, compress_after_runs={}, auto_prune={}",
                        state.config.retention.max_runs,
                        state.config.retention.compress_after_runs,
                        state.config.retention.auto_prune
                    ))
                    .clicked()
                {
                    match apply_retention(state.config.runs_dir(), &state.config.retention) {
                        Ok(report) => {
                            let msg = format!(
                                "Retention complete: {} compressed, {} pruned",
                                report.compressed.len(),
                                report.pruned.len()
                            );
                            state.history.last_retention_report = Some(report);
                            state.history.loaded_textures.clear();
                            refresh_runs(state);
                            state.toast = Some(StatusToast {
                                is_error: false,
                                text: msg,
                                created_at: Utc::now(),
                            });
                        }
                        Err(e) => {
                            state.toast = Some(StatusToast {
                                is_error: true,
                                text: format!("Retention error: {e}"),
                                created_at: Utc::now(),
                            });
                        }
                    }
                }
            });
        });

        if let Some(ref rep) = state.history.last_retention_report {
            ui.label(
                RichText::new(format!(
                    "Last retention run: {} archived to .zip, {} pruned",
                    rep.compressed.len(),
                    rep.pruned.len()
                ))
                .size(11.5)
                .color(ACCENT_EMERALD),
            );
        }
        if let Some(ref err) = state.history.load_error {
            ui.label(RichText::new(err).color(DANGER_RED));
        }
    });

    ui.add_space(10.0);

    ui.columns(2, |cols| {
        // Left column: List of past runs
        cols[0].set_min_width(280.0);
        card_frame().show(&mut cols[0], |ui| {
            ui.label(
                RichText::new(format!("Runs ({})", state.history.runs.len()))
                    .strong()
                    .color(ACCENT_EMERALD),
            );
            ui.add_space(6.0);

            if state.history.runs.is_empty() {
                ui.label(
                    RichText::new("No archived runs found in workspace.")
                        .italics()
                        .color(TEXT_MUTED),
                );
                return;
            }

            let runs_snapshot = state.history.runs.clone();
            let selected_idx = state.history.selected_run_idx;
            let mut clicked_idx: Option<usize> = None;

            ScrollArea::vertical()
                .id_salt("history_run_list")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (idx, run) in runs_snapshot.iter().enumerate() {
                        let is_selected = selected_idx == Some(idx);
                        let frame = if is_selected {
                            egui::Frame::new()
                                .fill(egui::Color32::from_rgb(0x1D, 0x35, 0x2C))
                                .stroke(egui::Stroke::new(1.2_f32, ACCENT_EMERALD))
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::same(10))
                        } else {
                            sub_card_frame()
                        };

                        let resp = frame
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let title = run
                                        .job_name
                                        .as_deref()
                                        .unwrap_or(&run.name);
                                    ui.label(RichText::new(title).strong().color(TEXT_PRIMARY));
                                    if let Some(status) = run.status {
                                        let (lbl, col) = run_status_badge(status);
                                        status_badge(ui, lbl, col);
                                    }
                                    if run.is_compressed {
                                        status_badge(ui, ".zip", WARN_AMBER);
                                    } else {
                                        status_badge(ui, "folder", TEXT_MUTED);
                                    }
                                });
                                ui.horizontal(|ui| {
                                    let ts = run
                                        .created_at
                                        .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                                        .unwrap_or_else(|| "Unknown date".to_string());
                                    ui.label(RichText::new(ts).size(11.0).color(TEXT_MUTED));
                                });
                            })
                            .response;

                        if resp.interact(egui::Sense::click()).clicked() {
                            clicked_idx = Some(idx);
                        }
                        ui.add_space(4.0);
                    }
                });

            if let Some(idx) = clicked_idx {
                select_run(ui.ctx(), state, idx);
            }
        });

        // Right column: Manifest details, Searchable Log Viewer, Inline Screenshot Viewer
        card_frame().show(&mut cols[1], |ui| {
            let Some(sel_idx) = state.history.selected_run_idx else {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.label(
                        RichText::new("Select a run on the left to inspect its manifest, logs, and screenshots.")
                            .color(TEXT_MUTED),
                    );
                    ui.add_space(40.0);
                });
                return;
            };

            let Some(run_path) = state.history.runs.get(sel_idx).map(|r| r.path.clone()) else {
                return;
            };

            ScrollArea::vertical()
                .id_salt("history_details_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Manifest Summary
                    if let Some(ref manifest) = state.history.selected_manifest {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&manifest.job_name)
                                    .size(16.0)
                                    .strong()
                                    .color(ACCENT_EMERALD),
                            );
                            let (lbl, col) = run_status_badge(manifest.status);
                            status_badge(ui, lbl, col);
                        });
                        ui.label(
                            RichText::new(format!(
                                "Run ID: {} · Created: {}",
                                manifest.run_id,
                                manifest.created_at.format("%Y-%m-%d %H:%M:%S UTC")
                            ))
                            .monospace()
                            .size(11.5)
                            .color(TEXT_MUTED),
                        );

                        ui.add_space(6.0);
                        for inst in &manifest.instances {
                            sub_card_frame().show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(&inst.instance_id).strong());
                                    let (lbl, col) = instance_state_badge(inst.state);
                                    status_badge(ui, lbl, col);
                                    if let Some(code) = inst.exit_code {
                                        ui.label(
                                            RichText::new(format!("exit={code}"))
                                                .monospace()
                                                .size(11.5)
                                                .color(TEXT_MUTED),
                                        );
                                    }
                                });
                                if let Some(ref reason) = inst.reason {
                                    ui.label(RichText::new(reason).size(11.5).color(TEXT_MUTED));
                                }
                            });
                            ui.add_space(3.0);
                        }
                    }

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);

                    // Searchable Log Viewer
                    ui.label(RichText::new("Log Search").strong().color(ACCENT_EMERALD));
                    let mut trigger_search = false;
                    ui.horizontal_wrapped(|ui| {
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut state.history.log_regex)
                                .desired_width(200.0)
                                .hint_text("Regex pattern (empty = all)"),
                        );
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            trigger_search = true;
                        }

                        ui.checkbox(&mut state.history.log_case_sensitive, "Case sensitive");

                        ui.label(RichText::new("Source:").color(TEXT_MUTED));
                        let mut sources = vec![String::new(), "server".to_string()];
                        if let Some(ref m) = state.history.selected_manifest {
                            for inst in &m.instances {
                                if !sources.contains(&inst.instance_id) {
                                    sources.push(inst.instance_id.clone());
                                }
                            }
                        }
                        let current_src_lbl = if state.history.log_instance_filter.is_empty() {
                            "All".to_string()
                        } else {
                            state.history.log_instance_filter.clone()
                        };
                        egui::ComboBox::from_id_salt("history_log_src")
                            .selected_text(current_src_lbl)
                            .show_ui(ui, |ui| {
                                for s in sources {
                                    let lbl = if s.is_empty() { "All" } else { s.as_str() };
                                    if ui
                                        .selectable_label(state.history.log_instance_filter == s, lbl)
                                        .clicked()
                                    {
                                        state.history.log_instance_filter = s;
                                        trigger_search = true;
                                    }
                                }
                            });

                        let current_file_lbl = if state.history.log_file_filter.is_empty() {
                            "All files".to_string()
                        } else {
                            state.history.log_file_filter.clone()
                        };
                        egui::ComboBox::from_id_salt("history_log_file")
                            .selected_text(current_file_lbl)
                            .show_ui(ui, |ui| {
                                for f in ["", "latest.log", "stdout.log", "stderr.log"] {
                                    let lbl = if f.is_empty() { "All files" } else { f };
                                    if ui
                                        .selectable_label(state.history.log_file_filter == f, lbl)
                                        .clicked()
                                    {
                                        state.history.log_file_filter = f.to_string();
                                        trigger_search = true;
                                    }
                                }
                            });

                        if ui.add(primary_button("Search")).clicked() {
                            trigger_search = true;
                        }
                    });

                    if trigger_search {
                        run_log_search(state, &run_path);
                    }

                    if let Some(ref err) = state.history.log_search_error {
                        ui.label(RichText::new(err).color(DANGER_RED));
                    }

                    egui::Frame::new()
                        .fill(BG_INPUT)
                        .inner_margin(egui::Margin::same(8))
                        .corner_radius(egui::CornerRadius::same(6))
                        .show(ui, |ui| {
                            ScrollArea::vertical()
                                .id_salt("history_log_results")
                                .max_height(210.0)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    if state.history.log_matches.is_empty() {
                                        ui.label(
                                            RichText::new("No matching log lines.")
                                                .italics()
                                                .color(TEXT_MUTED),
                                        );
                                    } else {
                                        for m in &state.history.log_matches {
                                            ui.label(
                                                RichText::new(format!(
                                                    "[{}/{}:{}] {}",
                                                    m.source, m.log_file, m.line_number, m.line
                                                ))
                                                .monospace()
                                                .size(11.0)
                                                .color(TEXT_PRIMARY),
                                            );
                                        }
                                    }
                                });
                        });

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);

                    // Inline PNG Screenshot Viewer
                    ui.label(
                        RichText::new(format!(
                            "Screenshots ({})",
                            state.history.screenshots.len()
                        ))
                        .strong()
                        .color(ACCENT_EMERALD),
                    );

                    if state.history.screenshots.is_empty() {
                        ui.label(
                            RichText::new("No screenshots recorded for this run.")
                                .italics()
                                .color(TEXT_MUTED),
                        );
                    } else {
                        let mut new_shot_sel: Option<usize> = None;
                        ui.horizontal_wrapped(|ui| {
                            for (s_idx, shot) in state.history.screenshots.iter().enumerate() {
                                let selected = state.history.selected_screenshot_idx == Some(s_idx);
                                if ui
                                    .selectable_label(
                                        selected,
                                        format!("{}/{}", shot.instance_id, shot.filename),
                                    )
                                    .clicked()
                                {
                                    new_shot_sel = Some(s_idx);
                                }
                            }
                        });
                        if let Some(s_idx) = new_shot_sel {
                            state.history.selected_screenshot_idx = Some(s_idx);
                            ensure_screenshot_texture(ui.ctx(), state, &run_path, s_idx);
                        }

                        if let Some(ref err) = state.history.screenshot_load_error {
                            ui.label(RichText::new(err).color(DANGER_RED));
                        }

                        if let Some(s_idx) = state.history.selected_screenshot_idx {
                            if let Some(shot) = state.history.screenshots.get(s_idx) {
                                let cache_key =
                                    format!("{}::{}", run_path.display(), shot.relative_path);
                                if let Some(tex) = state.history.loaded_textures.get(&cache_key) {
                                    let max_w = ui.available_width().max(240.0);
                                    let tex_size = tex.size_vec2();
                                    let scale = (max_w / tex_size.x).min(1.0).min(380.0 / tex_size.y);
                                    let display_size = tex_size * scale;
                                    ui.add_space(6.0);
                                    ui.image((tex.id(), display_size));
                                }
                            }
                        }
                    }
                });
        });
    });
}
