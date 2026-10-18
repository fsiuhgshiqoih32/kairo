//! Shared look for Rewindr's windows.

use eframe::egui::{self, Color32, CornerRadius, Stroke};

pub const ACCENT: Color32 = Color32::from_rgb(0xFF, 0x5A, 0x45);
pub const BG: Color32 = Color32::from_rgb(0x1C, 0x1D, 0x21);
pub const PANEL: Color32 = Color32::from_rgb(0x25, 0x27, 0x2C);
pub const TEXT: Color32 = Color32::from_rgb(0xE6, 0xE7, 0xEA);
pub const MUTED: Color32 = Color32::from_rgb(0x9A, 0x9D, 0xA6);
pub const DANGER: Color32 = Color32::from_rgb(0xF2, 0x5C, 0x54);
pub const OK: Color32 = Color32::from_rgb(0x4C, 0xC3, 0x8A);

/// A dark, flat theme with one accent colour.
pub fn apply_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = Color32::from_rgb(0x15, 0x16, 0x19);
    v.faint_bg_color = Color32::from_rgb(0x22, 0x23, 0x28);
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.55);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(0x3A, 0x3C, 0x43));
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(5);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x34, 0x36, 0x3C));
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x2E, 0x30, 0x36);
    v.widgets.inactive.bg_fill = Color32::from_rgb(0x2E, 0x30, 0x36);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x3A, 0x3D, 0x45);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x3A, 0x3D, 0x45);
    v.widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.7);
    v.widgets.active.bg_fill = ACCENT.gamma_multiply(0.7);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        s.interaction.tooltip_delay = 0.35;
    });
}

pub fn app_icon() -> egui::IconData {
    let size = 64;
    egui::IconData {
        rgba: rewindr::art::app_icon(size),
        width: size as u32,
        height: size as u32,
    }
}

pub fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).color(MUTED).size(12.0));
}
