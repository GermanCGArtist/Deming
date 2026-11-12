//! Entry point for the Deming native desktop GUI.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod preset;
mod runner_bridge;
mod state;
mod theme;
mod views;

use std::sync::Arc;

use app::DemingApp;

const EMBEDDED_ICON_PNG: &[u8] = include_bytes!("../../../assets/deming-icon.png");

/// Decodes the embedded `assets/deming-icon.png` into an [`egui::IconData`] for window/taskbar branding.
pub fn load_embedded_icon() -> Result<egui::IconData, String> {
    let img = image::load_from_memory(EMBEDDED_ICON_PNG)
        .map_err(|e| format!("failed to decode embedded deming-icon.png: {e}"))?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(egui::IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    })
}

fn main() -> eframe::Result<()> {
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("Deming — Minecraft Mod Test Orchestrator")
        .with_inner_size([1280.0, 820.0])
        .with_min_inner_size([960.0, 620.0]);

    if let Ok(icon) = load_embedded_icon() {
        viewport = viewport.with_icon(Arc::new(icon));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Deming",
        options,
        Box::new(|cc| Ok(Box::new(DemingApp::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icon_decodes_to_valid_rgba() {
        let icon = load_embedded_icon().expect("embedded icon should decode");
        assert!(icon.width > 0);
        assert!(icon.height > 0);
        assert_eq!(
            icon.rgba.len(),
            (icon.width as usize) * (icon.height as usize) * 4
        );
    }
}
