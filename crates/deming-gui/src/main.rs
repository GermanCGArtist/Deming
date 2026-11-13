//! Entry point for the Deming native desktop GUI.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result<()> {
    deming_gui::run_gui(None)
}
