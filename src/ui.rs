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

/// A play triangle or pause bars centred on `c`, `size` across.
pub fn paint_play(
    painter: &egui::Painter,
    c: egui::Pos2,
    size: f32,
    playing: bool,
    color: Color32,
) {
    let h = size / 2.0;
    if playing {
        let w = size * 0.28;
        for dx in [-size * 0.2, size * 0.2] {
            let r = egui::Rect::from_center_size(c + egui::vec2(dx, 0.0), egui::vec2(w, size));
            painter.rect_filled(r, CornerRadius::same(1), color);
        }
    } else {
        painter.add(egui::Shape::convex_polygon(
            vec![
                c + egui::vec2(-h * 0.7, -h),
                c + egui::vec2(h, 0.0),
                c + egui::vec2(-h * 0.7, h),
            ],
            color,
            Stroke::NONE,
        ));
    }
}

/// A five-pointed star centred on `c`; filled, or just its outline.
pub fn paint_star(painter: &egui::Painter, c: egui::Pos2, r: f32, filled: bool, color: Color32) {
    let point = |i: usize, radius: f32| {
        let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
        c + egui::vec2(a.cos(), a.sin()) * radius
    };
    let inner = r * 0.45;
    let outline: Vec<egui::Pos2> = (0..10)
        .map(|i| point(i, if i % 2 == 0 { r } else { inner }))
        .collect();
    if filled {
        // A star isn't convex: fill the inner pentagon and the five points separately.
        let pentagon: Vec<egui::Pos2> = (0..5).map(|k| point(2 * k + 1, inner)).collect();
        painter.add(egui::Shape::convex_polygon(pentagon, color, Stroke::NONE));
        for k in 0..5 {
            let tip = point(2 * k, r);
            let left = point((2 * k + 9) % 10, inner);
            let right = point(2 * k + 1, inner);
            painter.add(egui::Shape::convex_polygon(
                vec![left, tip, right],
                color,
                Stroke::NONE,
            ));
        }
    }
    painter.add(egui::Shape::closed_line(outline, Stroke::new(1.4, color)));
}

/// A small round button with a drawn X, for closing.
pub fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter()
            .circle_filled(r.center(), 10.0, Color32::from_white_alpha(25));
    }
    let c = r.center();
    let d = 4.0;
    let stroke = Stroke::new(1.5, if resp.hovered() { TEXT } else { MUTED });
    ui.painter()
        .line_segment([c + egui::vec2(-d, -d), c + egui::vec2(d, d)], stroke);
    ui.painter()
        .line_segment([c + egui::vec2(-d, d), c + egui::vec2(d, -d)], stroke);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Close"));
    resp.on_hover_text("Close")
}
