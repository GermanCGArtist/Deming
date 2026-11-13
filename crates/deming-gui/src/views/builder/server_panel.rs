//! Optional Local Dedicated Server panel inside the Test Builder.

use egui::RichText;

use crate::preset::{mod_loader_label, ServerDraft, ALL_MOD_LOADERS};
use crate::theme::{card_frame, ACCENT_EMERALD, TEXT_MUTED, TEXT_PRIMARY};

use super::render_mods_editor;

pub(super) fn render_server_editor(ui: &mut egui::Ui, server: &mut ServerDraft) {
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
        ui.label(
            RichText::new("server.properties overrides (key=value per line):")
                .color(TEXT_MUTED),
        );
        ui.add(
            egui::TextEdit::multiline(&mut server.properties_text)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .code_editor(),
        );
    });
}
