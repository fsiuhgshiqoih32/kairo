//! Keys and clicks for the input overlay: which ones are kept (gaming keys only, so typed text
//! can't be rebuilt from them), events stored beside a clip, and the overlay picture drawn into
//! exports as a stream of frames.

use std::io::Write;
use std::path::Path;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use serde::{Deserialize, Serialize};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};

/// The keys the overlay shows, by their place in this list (the id stored in events).
pub const KEYS: [&str; 19] = [
    "Q",
    "W",
    "E",
    "R",
    "A",
    "S",
    "D",
    "F",
    "Shift",
    "Ctrl",
    "Space",
    "Tab",
    "1",
    "2",
    "3",
    "4",
    "Left click",
    "Right click",
    "Middle click",
];

/// Ids of the mouse buttons in [`KEYS`].
pub const MOUSE_LEFT: u8 = 16;
pub const MOUSE_RIGHT: u8 = 17;
pub const MOUSE_MIDDLE: u8 = 18;

/// A key or button going down or up, `t` seconds into the clip.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyEvent {
    pub t: f64,
    pub key: u8,
    pub down: bool,
}

/// Which keys are down at `t`, as a bit per key.
pub fn state_at(events: &[KeyEvent], t: f64) -> u32 {
    let mut mask = 0u32;
    for e in events {
        if e.t > t {
            break;
        }
        if (e.key as usize) < KEYS.len() {
            if e.down {
                mask |= 1 << e.key;
            } else {
                mask &= !(1 << e.key);
            }
        }
    }
    mask
}

/// The overlay's size in pixels for an export `out_height` tall.
pub fn size_for(out_height: u32) -> (u32, u32) {
    let h = ((out_height as f32 * 0.16) as u32).clamp(80, 400) & !1;
    ((h * 2) & !1, h)
}

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut p = PathBuilder::new();
    p.move_to(x + r, y);
    p.line_to(x + w - r, y);
    p.quad_to(x + w, y, x + w, y + r);
    p.line_to(x + w, y + h - r);
    p.quad_to(x + w, y + h, x + w - r, y + h);
    p.line_to(x + r, y + h);
    p.quad_to(x, y + h, x, y + h - r);
    p.line_to(x, y + r);
    p.quad_to(x, y, x + r, y);
    p.close();
    p.finish()
}

/// Draws the overlay for the keys in `mask`: key caps (lit in `accent` when down) and a mouse
/// with its buttons. Straight-alpha RGBA, `size` pixels.
pub fn render(mask: u32, size: (u32, u32), accent: [u8; 3], font: &[u8]) -> Vec<u8> {
    let (w, h) = size;
    let Some(mut pm) = Pixmap::new(w.max(1), h.max(1)) else {
        return vec![0; (w * h * 4) as usize];
    };
    let u = h as f32 / 4.6; // one key's size
    let gap = u * 0.12;
    let down = |k: usize| mask & (1 << k) != 0;
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    let font = FontRef::try_from_slice(font).ok();
    let mut labels: Vec<(f32, f32, f32, &str, bool)> = Vec::new();
    let mut cap = |pm: &mut Pixmap, x: f32, y: f32, kw: f32, k: usize, label: &'static str| {
        let on = down(k);
        if let Some(p) = rounded(x, y, kw - gap, u - gap, u * 0.18) {
            if on {
                paint.set_color_rgba8(accent[0], accent[1], accent[2], 235);
            } else {
                paint.set_color_rgba8(16, 17, 20, 170);
            }
            pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
            let edge = tiny_skia::Stroke {
                width: (u * 0.05).max(1.0),
                ..Default::default()
            };
            paint.set_color_rgba8(255, 255, 255, if on { 230 } else { 110 });
            pm.stroke_path(&p, &paint, &edge, Transform::identity(), None);
        }
        labels.push((
            x + (kw - gap) / 2.0,
            y + (u - gap) / 2.0,
            kw - gap,
            label,
            on,
        ));
    };
    let x0 = gap;
    let y0 = gap;
    // Rows: Q W E R / A S D F, then Shift and Ctrl beside Space, numbers above.
    for (i, k) in [(12usize, "1"), (13, "2"), (14, "3"), (15, "4")]
        .iter()
        .enumerate()
    {
        cap(&mut pm, x0 + i as f32 * u + u * 0.5, y0, u, k.0, k.1);
    }
    for (i, k) in [(11usize, "Tab")].iter().enumerate() {
        cap(&mut pm, x0 + i as f32 * u, y0 + u, u * 1.0, k.0, k.1);
    }
    for (i, k) in [(0usize, "Q"), (1, "W"), (2, "E"), (3, "R")]
        .iter()
        .enumerate()
    {
        cap(&mut pm, x0 + u + i as f32 * u, y0 + u, u, k.0, k.1);
    }
    cap(&mut pm, x0, y0 + 2.0 * u, u * 1.25, 8, "Shift");
    for (i, k) in [(4usize, "A"), (5, "S"), (6, "D"), (7, "F")]
        .iter()
        .enumerate()
    {
        cap(
            &mut pm,
            x0 + u * 1.25 + i as f32 * u,
            y0 + 2.0 * u,
            u,
            k.0,
            k.1,
        );
    }
    cap(&mut pm, x0, y0 + 3.0 * u, u * 1.25, 9, "Ctrl");
    cap(&mut pm, x0 + u * 1.25, y0 + 3.0 * u, u * 4.0, 10, "Space");
    // The mouse: a body, its left and right buttons outlined on top, the wheel between.
    let mx = x0 + u * 5.55;
    let mw = u * 1.75;
    let my = y0 + u * 0.35;
    let mh = u * 3.55;
    let edge = tiny_skia::Stroke {
        width: (u * 0.05).max(1.0),
        ..Default::default()
    };
    if let Some(body) = rounded(mx, my, mw, mh, mw * 0.48) {
        paint.set_color_rgba8(16, 17, 20, 170);
        pm.fill_path(
            &body,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        paint.set_color_rgba8(255, 255, 255, 110);
        pm.stroke_path(&body, &paint, &edge, Transform::identity(), None);
    }
    let inset = mw * 0.09;
    let bw = (mw - inset * 3.0) / 2.0;
    let bh = mh * 0.38;
    for (k, x) in [
        (MOUSE_LEFT as usize, mx + inset),
        (MOUSE_RIGHT as usize, mx + inset * 2.0 + bw),
    ] {
        if let Some(p) = rounded(x, my + inset, bw, bh, bw * 0.45) {
            if down(k) {
                paint.set_color_rgba8(accent[0], accent[1], accent[2], 235);
                pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
            }
            paint.set_color_rgba8(255, 255, 255, if down(k) { 230 } else { 90 });
            pm.stroke_path(&p, &paint, &edge, Transform::identity(), None);
        }
    }
    let ww = mw * 0.13;
    if let Some(p) = rounded(
        mx + mw / 2.0 - ww / 2.0,
        my + inset + bh * 0.25,
        ww,
        bh * 0.5,
        ww / 2.0,
    ) {
        if down(MOUSE_MIDDLE as usize) {
            paint.set_color_rgba8(accent[0], accent[1], accent[2], 240);
        } else {
            paint.set_color_rgba8(255, 255, 255, 150);
        }
        pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
    }
    // tiny-skia stores premultiplied colour; make it straight.
    let mut rgba = pm.data().to_vec();
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as f32 / 255.0;
        if a > 0.0 {
            for c in &mut px[..3] {
                *c = ((*c as f32 / a).round()).min(255.0) as u8;
            }
        }
    }
    // Labels on top, white (dark on lit keys for contrast).
    if let Some(font) = font {
        let size = u * 0.36;
        let sf = font.as_scaled(PxScale::from(size));
        for (cx, cy, room, label, on) in labels {
            let width: f32 = label.chars().map(|c| sf.h_advance(sf.glyph_id(c))).sum();
            let scale = if width > room * 0.86 {
                room * 0.86 / width
            } else {
                1.0
            };
            let sf = font.as_scaled(PxScale::from(size * scale));
            let width = width * scale;
            let mut x = cx - width / 2.0;
            let base = cy + (sf.ascent() + sf.descent()) / 2.0;
            for c in label.chars() {
                let id = sf.glyph_id(c);
                let g = id.with_scale_and_position(sf.scale(), point(x, base));
                if let Some(o) = font.outline_glyph(g) {
                    let b = o.px_bounds();
                    o.draw(|gx, gy, cov| {
                        let px = b.min.x as i32 + gx as i32;
                        let py = b.min.y as i32 + gy as i32;
                        if (0..w as i32).contains(&px) && (0..h as i32).contains(&py) {
                            let i = ((py as u32 * w + px as u32) * 4) as usize;
                            let ink = if on { 20.0 } else { 255.0 };
                            for ch in 0..3 {
                                let v = rgba[i + ch] as f32;
                                rgba[i + ch] = (v + (ink - v) * cov) as u8;
                            }
                            rgba[i + 3] = rgba[i + 3].max((cov * 255.0) as u8);
                        }
                    });
                }
                x += sf.h_advance(id);
            }
        }
    }
    rgba
}

/// Writes the overlay for `range` of the clip at `speed`, `fps` frames a second of the export,
/// as raw RGBA to `out`. Returns the frame count. Each state is drawn once and reused.
#[allow(clippy::too_many_arguments)]
pub fn write_stream(
    events: &[KeyEvent],
    range: (f64, f64),
    speed: f64,
    fps: u32,
    size: (u32, u32),
    accent: [u8; 3],
    font: &[u8],
    out: &Path,
) -> std::io::Result<u64> {
    let length = (range.1 - range.0).max(0.0) / speed.max(0.01);
    let frames = (length * fps as f64).ceil() as u64;
    let mut file = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(out)?);
    let mut cache: std::collections::HashMap<u32, Vec<u8>> = std::collections::HashMap::new();
    for i in 0..frames {
        let t = range.0 + i as f64 / fps as f64 * speed;
        let mask = state_at(events, t);
        let pic = cache
            .entry(mask)
            .or_insert_with(|| render(mask, size, accent, font));
        file.write_all(pic)?;
    }
    file.flush()?;
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(t: f64, key: u8, down: bool) -> KeyEvent {
        KeyEvent { t, key, down }
    }

    #[test]
    fn state_follows_presses_and_releases() {
        let ev = [
            e(0.5, 1, true),
            e(0.7, 16, true),
            e(1.0, 1, false),
            e(1.2, 16, false),
            e(1.5, 99, true),
        ];
        assert_eq!(state_at(&ev, 0.0), 0);
        assert_eq!(state_at(&ev, 0.8), (1 << 1) | (1 << 16));
        assert_eq!(state_at(&ev, 1.1), 1 << 16);
        assert_eq!(state_at(&ev, 2.0), 0);
    }

    #[test]
    fn draws_keys_lit_when_down() {
        let font = include_bytes!("../assets/fonts/Inter-Bold.ttf");
        let size = size_for(1080);
        assert_eq!(size, (344, 172));
        let idle = render(0, size, [255, 79, 70], font);
        let w_down = render(1 << 1, size, [255, 79, 70], font);
        assert_eq!(idle.len(), (size.0 * size.1 * 4) as usize);
        let reddish = |px: &[u8]| {
            px.chunks(4)
                .filter(|p| p[0] > 200 && p[1] < 120 && p[3] > 200)
                .count()
        };
        assert_eq!(reddish(&idle), 0);
        assert!(reddish(&w_down) > 200);
    }

    #[test]
    fn a_stream_has_a_frame_per_tick_of_the_export() {
        let font = include_bytes!("../assets/fonts/Inter-Bold.ttf");
        let dir = std::env::temp_dir().join(format!("kairo-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("k.rgba");
        let ev = [e(1.0, 4, true), e(1.5, 4, false)];
        // 2 s of the clip at double speed, 10 frames a second: 10 frames.
        let n = write_stream(&ev, (0.5, 2.5), 2.0, 10, (100, 50), [255, 0, 0], font, &out).unwrap();
        assert_eq!(n, 10);
        assert_eq!(std::fs::metadata(&out).unwrap().len(), 10 * 100 * 50 * 4);
        let _ = std::fs::remove_dir_all(dir);
    }
}
