//! The icon, drawn in code so every size is crisp: a rewind symbol on a warm tile.

use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point, Shader,
    SpreadMode, Transform,
};

/// Straight (non-premultiplied) RGBA, `size` x `size`.
pub fn app_icon(size: usize) -> Vec<u8> {
    draw(size, false, true)
}

/// The tray icon: coloured while the replay buffer runs, grey while paused or failing. On macOS a
/// black template image the menu bar tints (faint while inactive).
pub fn tray_icon(size: usize, active: bool) -> Vec<u8> {
    draw(size, cfg!(target_os = "macos"), active)
}

fn draw(size: usize, template: bool, active: bool) -> Vec<u8> {
    let s = size as f32;
    let mut pm = Pixmap::new(size as u32, size as u32).expect("icon size");
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    if !template {
        let inset = s * 0.03;
        if let Some(path) = rounded_rect(inset, inset, s - 2.0 * inset, s - 2.0 * inset, s * 0.22) {
            let (a, b) = if active {
                (
                    Color::from_rgba8(0xFF, 0x6A, 0x3D, 0xFF),
                    Color::from_rgba8(0xE0, 0x2D, 0x4A, 0xFF),
                )
            } else {
                (
                    Color::from_rgba8(0x80, 0x84, 0x8C, 0xFF),
                    Color::from_rgba8(0x55, 0x58, 0x60, 0xFF),
                )
            };
            paint.shader = LinearGradient::new(
                Point::from_xy(0.0, 0.0),
                Point::from_xy(s, s),
                vec![GradientStop::new(0.0, a), GradientStop::new(1.0, b)],
                SpreadMode::Pad,
                Transform::identity(),
            )
            .unwrap_or(Shader::SolidColor(a));
            pm.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
    let alpha = if template && !active { 110 } else { 255 };
    let fg = if template {
        Color::from_rgba8(0, 0, 0, alpha)
    } else {
        Color::from_rgba8(255, 255, 255, 255)
    };
    paint.shader = Shader::SolidColor(fg);
    // Two left-pointing triangles (rewind), slightly overlapping.
    let (h, w) = if template {
        (s * 0.56, s * 0.42)
    } else {
        (s * 0.42, s * 0.31)
    };
    let cy = s / 2.0;
    let total = w * 2.0 - w * 0.18;
    let left = (s - total) / 2.0;
    for i in 0..2 {
        let x0 = left + i as f32 * (w - w * 0.18);
        let mut pb = PathBuilder::new();
        pb.move_to(x0, cy);
        pb.line_to(x0 + w, cy - h / 2.0);
        pb.line_to(x0 + w, cy + h / 2.0);
        pb.close();
        if let Some(path) = pb.finish() {
            pm.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
    let mut data = pm.take();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    data
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let k = 0.552_284_8 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

#[cfg(test)]
mod tests {
    #[test]
    fn icons_have_content() {
        for size in [16, 32, 256] {
            let px = super::app_icon(size);
            assert_eq!(px.len(), size * size * 4);
            // Opaque tile in the middle, transparent corner.
            let mid = (size / 2 * size + size / 4) * 4;
            assert_eq!(px[mid + 3], 255);
            assert_eq!(px[3], 0);
        }
    }
}
