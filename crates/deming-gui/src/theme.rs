//! Dark obsidian theme (`#181A20` base, `#2EE59D` emerald accents) and UI styling helpers.

use deming_core::{InstanceState, RunStatus};
use egui::{Color32, CornerRadius, Frame, Margin, Stroke, Style, Visuals};

/// Primary obsidian window background (`#181A20`).
pub const BG_BASE: Color32 = Color32::from_rgb(0x18, 0x1A, 0x20);
/// Card / elevated panel background (`#1E2128`).
pub const BG_SURFACE: Color32 = Color32::from_rgb(0x1E, 0x21, 0x28);
/// Secondary widget / hover surface (`#252932`).
pub const BG_ELEVATED: Color32 = Color32::from_rgb(0x25, 0x29, 0x32);
/// Recessed input / code / log background (`#12141A`).
pub const BG_INPUT: Color32 = Color32::from_rgb(0x12, 0x14, 0x1A);
/// Subtle divider and card border (`#2E3340`).
pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(0x2E, 0x33, 0x40);

/// Signature Deming emerald accent (`#2EE59D`).
pub const ACCENT_EMERALD: Color32 = Color32::from_rgb(0x2E, 0xE5, 0x9D);
/// Dimmed emerald for subtle borders and pressed states.
pub const ACCENT_EMERALD_DIM: Color32 = Color32::from_rgb(0x1D, 0x9E, 0x6B);

/// Prominent danger red for STOP / ABORT ALL and Kill actions.
pub const DANGER_RED: Color32 = Color32::from_rgb(0xEF, 0x44, 0x44);
/// Amber for queued/preparing/warnings.
pub const WARN_AMBER: Color32 = Color32::from_rgb(0xF5, 0x9E, 0x0B);
/// Blue for running / active indicators.
pub const INFO_BLUE: Color32 = Color32::from_rgb(0x3B, 0x82, 0xF6);

/// Primary crisp light text.
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xF3, 0xF4, 0xF6);
/// Muted secondary text.
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x9C, 0xA3, 0xAF);

/// Configures `egui::Context` with the dark obsidian palette and clean spacing.
pub fn apply_obsidian_theme(ctx: &egui::Context) {
    let mut style: Style = (*ctx.style()).clone();
    let mut visuals = Visuals::dark();

    visuals.dark_mode = true;
    visuals.override_text_color = Some(TEXT_PRIMARY);
    visuals.panel_fill = BG_BASE;
    visuals.window_fill = BG_SURFACE;
    visuals.extreme_bg_color = BG_INPUT;
    visuals.faint_bg_color = BG_ELEVATED;
    visuals.code_bg_color = BG_INPUT;

    visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(0x2E, 0xE5, 0x9D, 48);
    visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT_EMERALD);

    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.window_stroke = Stroke::new(1.0_f32, BORDER_SUBTLE);

    visuals.widgets.noninteractive.bg_fill = BG_SURFACE;
    visuals.widgets.noninteractive.weak_bg_fill = BG_SURFACE;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER_SUBTLE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT_PRIMARY);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);

    visuals.widgets.inactive.bg_fill = BG_ELEVATED;
    visuals.widgets.inactive.weak_bg_fill = BG_ELEVATED;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER_SUBTLE);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT_PRIMARY);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(6);

    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x2E, 0x33, 0x40);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2E, 0x33, 0x40);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT_EMERALD_DIM);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, TEXT_PRIMARY);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(6);

    visuals.widgets.active.bg_fill = Color32::from_rgb(0x1D, 0x4D, 0x3C);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(0x1D, 0x4D, 0x3C);
    visuals.widgets.active.bg_stroke = Stroke::new(1.5_f32, ACCENT_EMERALD);
    visuals.widgets.active.fg_stroke = Stroke::new(1.5_f32, TEXT_PRIMARY);
    visuals.widgets.active.corner_radius = CornerRadius::same(6);

    visuals.widgets.open.bg_fill = BG_ELEVATED;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, ACCENT_EMERALD);
    visuals.widgets.open.corner_radius = CornerRadius::same(6);

    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.window_margin = Margin::same(14);
    ctx.set_style(style);
}

/// Renders a dark elevated card container with consistent padding and border.
pub fn card_frame() -> Frame {
    Frame::new()
        .fill(BG_SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
}

/// Renders a recessed sub-card container (e.g. for an instance row or log box).
pub fn sub_card_frame() -> Frame {
    Frame::new()
        .fill(BG_INPUT)
        .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
}

/// Renders a pill-shaped status badge.
pub fn status_badge(ui: &mut egui::Ui, label: &str, fg: Color32) {
    let bg = Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), 32);
    Frame::new()
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, fg))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(label)
                    .color(fg)
                    .size(11.5)
                    .strong(),
            );
        });
}

/// Returns display label and color for an [`InstanceState`].
pub fn instance_state_badge(state: InstanceState) -> (&'static str, Color32) {
    match state {
        InstanceState::Queued => ("Queued", TEXT_MUTED),
        InstanceState::Preparing => ("Preparing", WARN_AMBER),
        InstanceState::Starting => ("Starting", INFO_BLUE),
        InstanceState::Running => ("Running", INFO_BLUE),
        InstanceState::Passed => ("Passed", ACCENT_EMERALD),
        InstanceState::Failed => ("Failed", DANGER_RED),
        InstanceState::Aborted => ("Aborted", WARN_AMBER),
        InstanceState::TimedOut => ("TimedOut", DANGER_RED),
    }
}

/// Returns display label and color for a [`RunStatus`].
pub fn run_status_badge(status: RunStatus) -> (&'static str, Color32) {
    match status {
        RunStatus::Pending => ("Pending", TEXT_MUTED),
        RunStatus::Running => ("Running", INFO_BLUE),
        RunStatus::Passed => ("Passed", ACCENT_EMERALD),
        RunStatus::Failed => ("Failed", DANGER_RED),
        RunStatus::Aborted => ("Aborted", WARN_AMBER),
    }
}

/// Styled emerald primary action button.
pub fn primary_button(label: &str) -> egui::Button<'_> {
    egui::Button::new(
        egui::RichText::new(label)
            .color(Color32::from_rgb(0x09, 0x16, 0x10))
            .strong(),
    )
    .fill(ACCENT_EMERALD)
    .corner_radius(CornerRadius::same(6))
}

/// Styled danger button (red).
pub fn danger_button(label: &str) -> egui::Button<'_> {
    egui::Button::new(egui::RichText::new(label).color(TEXT_PRIMARY).strong())
        .fill(DANGER_RED)
        .corner_radius(CornerRadius::same(6))
}
