//! Text and stickers laid over a clip in the editor and burned into copies, with how they come
//! in and go out: a pop, a slide or a blur, drawn with motion blur.
//!
//! The sticker shapes are Lucide icons (ISC; see THIRD-PARTY-NOTICES.txt), filled and outlined.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use serde::{Deserialize, Serialize};

use crate::captions::{Picture, render_colored};

/// The colours text and stickers come in.
pub const COLORS: [(&str, [u8; 3]); 7] = [
    ("White", [255, 255, 255]),
    ("Yellow", [255, 214, 10]),
    ("Orange", [255, 122, 26]),
    ("Red", [255, 69, 58]),
    ("Green", [48, 209, 88]),
    ("Blue", [10, 132, 255]),
    ("Purple", [191, 90, 242]),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Look {
    /// Words on a coloured pill.
    Badge(&'static str),
    /// A shape filled with the colour, outlined in white.
    Fill(&'static str),
    /// A ring, for circling something.
    Ring(&'static str),
}

pub struct Sticker {
    pub id: &'static str,
    pub name: &'static str,
    /// Its colour when added (`COLORS` index).
    pub color: usize,
    look: Look,
}

const STAR: &str = r#"<path d="M11.525 2.295a.53.53 0 0 1 .95 0l2.31 4.679a2.123 2.123 0 0 0 1.595 1.16l5.166.756a.53.53 0 0 1 .294.904l-3.736 3.638a2.123 2.123 0 0 0-.611 1.878l.882 5.14a.53.53 0 0 1-.771.56l-4.618-2.428a2.122 2.122 0 0 0-1.973 0L6.396 21.01a.53.53 0 0 1-.77-.56l.881-5.139a2.122 2.122 0 0 0-.611-1.879L2.16 9.795a.53.53 0 0 1 .294-.906l5.165-.755a2.122 2.122 0 0 0 1.597-1.16z"/>"#;
const HEART: &str = r#"<path d="M2 9.5a5.5 5.5 0 0 1 9.591-3.676.56.56 0 0 0 .818 0A5.49 5.49 0 0 1 22 9.5c0 2.29-1.5 4-3 5.5l-5.492 5.313a2 2 0 0 1-3 .019L5 15c-1.5-1.5-3-3.2-3-5.5"/>"#;
const FLAME: &str = r#"<path d="M12 3q1 4 4 6.5t3 5.5a1 1 0 0 1-14 0 5 5 0 0 1 1-3 1 1 0 0 0 5 0c0-2-1.5-3-1.5-5q0-2 2.5-4"/>"#;
const ZAP: &str = r#"<path d="M15.914 4a1.5 1.5 0 00-2.474-1.561l-9 9A1.5 1.5 0 005.5 14h4.002a.5.5 0 01.471.666L8.086 20a1.5 1.5 0 002.475 1.56l9-9A1.5 1.5 0 0018.5 10h-3.997a.5.5 0 01-.472-.667z"/>"#;
const CROWN: &str = r#"<path d="M11.562 3.266a.5.5 0 0 1 .876 0L15.39 8.87a1 1 0 0 0 1.516.294L21.183 5.5a.5.5 0 0 1 .798.519l-2.834 10.246a1 1 0 0 1-.956.734H5.81a1 1 0 0 1-.957-.734L2.02 6.02a.5.5 0 0 1 .798-.519l4.276 3.664a1 1 0 0 0 1.516-.294z"/><path d="M5 21h14"/>"#;
const THUMBS_UP: &str = r#"<path d="M15 5.88 14 10h5.83a2 2 0 0 1 1.92 2.56l-2.33 8A2 2 0 0 1 17.5 22H4a2 2 0 0 1-2-2v-8a2 2 0 0 1 2-2h2.76a2 2 0 0 0 1.79-1.11L12 2a3.13 3.13 0 0 1 3 3.88Z"/>"#;
const ARROW: &str = r#"<path d="M13.207 19.793a.707.707 0 0 1-1.207-.5V16a1 1 0 0 0-1-1H5a1 1 0 0 1-1-1v-4a1 1 0 0 1 1-1h6a1 1 0 0 0 1-1V4.707a.707.707 0 0 1 1.207-.5l6.94 6.94a1.207 1.207 0 0 1 0 1.707z"/>"#;
const CIRCLE: &str = r#"<circle cx="12" cy="12" r="10"/>"#;

pub const STICKERS: [Sticker; 14] = [
    Sticker {
        id: "gg",
        name: "GG",
        color: 2,
        look: Look::Badge("GG"),
    },
    Sticker {
        id: "clutch",
        name: "Clutch",
        color: 3,
        look: Look::Badge("CLUTCH"),
    },
    Sticker {
        id: "ace",
        name: "Ace",
        color: 6,
        look: Look::Badge("ACE"),
    },
    Sticker {
        id: "no-way",
        name: "No way",
        color: 5,
        look: Look::Badge("NO WAY"),
    },
    Sticker {
        id: "ez",
        name: "EZ",
        color: 4,
        look: Look::Badge("EZ"),
    },
    Sticker {
        id: "wow",
        name: "Wow",
        color: 1,
        look: Look::Badge("WOW"),
    },
    Sticker {
        id: "star",
        name: "Star",
        color: 1,
        look: Look::Fill(STAR),
    },
    Sticker {
        id: "heart",
        name: "Heart",
        color: 3,
        look: Look::Fill(HEART),
    },
    Sticker {
        id: "flame",
        name: "Flame",
        color: 2,
        look: Look::Fill(FLAME),
    },
    Sticker {
        id: "zap",
        name: "Lightning",
        color: 1,
        look: Look::Fill(ZAP),
    },
    Sticker {
        id: "crown",
        name: "Crown",
        color: 1,
        look: Look::Fill(CROWN),
    },
    Sticker {
        id: "thumbs-up",
        name: "Thumbs up",
        color: 5,
        look: Look::Fill(THUMBS_UP),
    },
    Sticker {
        id: "arrow",
        name: "Arrow",
        color: 3,
        look: Look::Fill(ARROW),
    },
    Sticker {
        id: "circle",
        name: "Circle",
        color: 3,
        look: Look::Ring(CIRCLE),
    },
];

pub fn sticker(id: &str) -> Option<&'static Sticker> {
    STICKERS.iter().find(|s| s.id == id)
}

/// How an item comes in and goes out: id and name.
pub const ANIMS: [(&str, &str); 4] = [
    ("pop", "Pop"),
    ("slide", "Slide"),
    ("blur", "Blur"),
    ("none", "None"),
];

/// How long coming in and going out take, at most (seconds).
pub const IN_SECONDS: f64 = 0.4;
pub const OUT_SECONDS: f64 = 0.3;

/// Text or a sticker over part of a clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Item {
    /// The words, for text.
    pub text: String,
    /// The sticker (`STICKERS` id), for a sticker.
    pub sticker: String,
    /// When it shows, in seconds of the clip.
    pub start: f64,
    pub end: f64,
    /// Its middle, as a share of the picture's width and height.
    pub x: f32,
    pub y: f32,
    /// Its height (a line, for text) as a share of the picture's.
    pub size: f32,
    /// `COLORS` index.
    pub color: usize,
    /// `ANIMS` id.
    pub anim: String,
}

impl Default for Item {
    fn default() -> Self {
        Item {
            text: String::new(),
            sticker: String::new(),
            start: 0.0,
            end: 3.0,
            x: 0.5,
            y: 0.22,
            size: 0.08,
            color: 0,
            anim: "pop".into(),
        }
    }
}

impl Item {
    pub fn text(text: &str, start: f64, end: f64) -> Item {
        Item {
            text: text.trim().to_string(),
            start,
            end,
            ..Default::default()
        }
    }

    pub fn sticker(id: &str, start: f64, end: f64) -> Item {
        let s = sticker(id);
        let badge = s.is_some_and(|s| matches!(s.look, Look::Badge(_)));
        Item {
            sticker: id.to_string(),
            start,
            end,
            x: 0.78,
            y: 0.26,
            size: if badge { 0.1 } else { 0.16 },
            color: s.map_or(0, |s| s.color),
            ..Default::default()
        }
    }

    pub fn is_text(&self) -> bool {
        self.sticker.is_empty()
    }

    /// What the editor's list calls it.
    pub fn label(&self) -> String {
        if self.is_text() {
            self.text.clone()
        } else {
            sticker(&self.sticker)
                .map_or("Sticker", |s| s.name)
                .to_string()
        }
    }

    pub fn shows_at(&self, t: f64) -> bool {
        self.start <= t && t < self.end
    }

    /// Its picture for a frame `frame_h` pixels tall (text wraps at `max_w`).
    pub fn picture(&self, frame_h: u32, max_w: u32, font: &[u8]) -> Option<Picture> {
        let px = (self.size.clamp(0.02, 0.5) * frame_h as f32).max(8.0);
        let rgb = COLORS.get(self.color).map_or([255; 3], |c| c.1);
        if self.is_text() {
            return render_colored(&self.text, max_w, px, font, rgb);
        }
        match sticker(&self.sticker)?.look {
            Look::Badge(words) => badge(words, px.round() as u32, rgb, font),
            Look::Fill(shape) => shape_picture(shape, px.round() as u32, rgb, true),
            Look::Ring(shape) => shape_picture(shape, px.round() as u32, rgb, false),
        }
    }

    /// How long its coming in and going out last when it shows for `shown` seconds.
    pub fn anim_lengths(&self, shown: f64) -> (f64, f64) {
        if self.anim == "none" || !ANIMS.iter().any(|(id, _)| *id == self.anim) {
            return (0.0, 0.0);
        }
        let k = (shown * 0.8 / (IN_SECONDS + OUT_SECONDS)).min(1.0);
        (IN_SECONDS * k, OUT_SECONDS * k)
    }

    /// Its look `t` seconds into the clip: in the middle of coming in or going out, or settled.
    pub fn pose_at(&self, t: f64) -> Pose {
        let (inn, out) = self.anim_lengths(self.end - self.start);
        if inn > 0.0 && t < self.start + inn {
            pose(
                &self.anim,
                ((t - self.start) / inn).clamp(0.0, 1.0) as f32,
                false,
            )
        } else if out > 0.0 && t > self.end - out {
            pose(
                &self.anim,
                ((self.end - t) / out).clamp(0.0, 1.0) as f32,
                true,
            )
        } else {
            Pose::SETTLED
        }
    }
}

/// An item's look during an animation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub scale: f32,
    /// Sideways, as a share of the picture's width.
    pub shift: f32,
    pub alpha: f32,
    /// Blur, as a share of the item's height.
    pub blur: f32,
}

impl Pose {
    pub const SETTLED: Pose = Pose {
        scale: 1.0,
        shift: 0.0,
        alpha: 1.0,
        blur: 0.0,
    };
}

fn ease_out_cubic(u: f32) -> f32 {
    1.0 - (1.0 - u).powi(3)
}

fn ease_out_back(u: f32) -> f32 {
    let (c1, c3) = (1.70158f32, 2.70158f32);
    1.0 + c3 * (u - 1.0).powi(3) + c1 * (u - 1.0).powi(2)
}

/// The look of `anim` at `u`, from 0 (not there) to 1 (settled); going out runs it backwards,
/// a little quicker and without the bounce.
pub fn pose(anim: &str, u: f32, leaving: bool) -> Pose {
    let u = u.clamp(0.0, 1.0);
    match anim {
        "pop" => Pose {
            scale: if leaving {
                ease_out_cubic(u)
            } else {
                ease_out_back(u)
            }
            .max(0.0),
            alpha: (u * 3.0).min(1.0),
            ..Pose::SETTLED
        },
        "slide" => Pose {
            shift: (1.0 - ease_out_cubic(u)) * if leaving { 1.1 } else { -1.1 },
            alpha: (u * 2.0).min(1.0),
            ..Pose::SETTLED
        },
        "blur" => Pose {
            scale: 1.0 + 0.1 * (1.0 - ease_out_cubic(u)),
            alpha: ease_out_cubic(u),
            blur: 0.12 * (1.0 - ease_out_cubic(u)),
            ..Pose::SETTLED
        },
        _ => Pose::SETTLED,
    }
}

/// Words on a pill of `rgb` with a white edge, `h` pixels tall.
fn badge(words: &str, h: u32, rgb: [u8; 3], font: &[u8]) -> Option<Picture> {
    let font = FontRef::try_from_slice(font).ok()?;
    let h = h.max(12) as f32;
    let sf = font.as_scaled(PxScale::from(h * 0.56));
    let mut text_w = 0.0;
    let mut prev = None;
    for c in words.chars() {
        let id = sf.glyph_id(c);
        if let Some(p) = prev {
            text_w += sf.kern(p, id);
        }
        text_w += sf.h_advance(id);
        prev = Some(id);
    }
    let w = (text_w + h * 0.9).ceil() as usize;
    let hh = h.ceil() as usize;
    let edge = (h * 0.07).max(2.0);
    let r = h / 2.0;
    let mut rgba = vec![0u8; w * hh * 4];
    // The pill: white out to the edge, the colour inside it.
    for y in 0..hh {
        for x in 0..w {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = px.clamp(r, w as f32 - r);
            let d = ((px - cx).powi(2) + (py - r).powi(2)).sqrt() - r;
            let outer = (0.5 - d).clamp(0.0, 1.0);
            if outer <= 0.0 {
                continue;
            }
            let inner = (0.5 - (d + edge)).clamp(0.0, 1.0);
            let k = (y * w + x) * 4;
            for (i, c) in rgb.iter().enumerate() {
                rgba[k + i] = (255.0 * (1.0 - inner) + *c as f32 * inner).round() as u8;
            }
            rgba[k + 3] = (outer * 255.0).round() as u8;
        }
    }
    // The words in white.
    let mut x = (w as f32 - text_w) / 2.0;
    let baseline = (h + sf.ascent() + sf.descent()) / 2.0;
    let mut prev = None;
    for c in words.chars() {
        let id = sf.glyph_id(c);
        if let Some(p) = prev {
            x += sf.kern(p, id);
        }
        let glyph = id.with_scale_and_position(sf.scale(), point(x, baseline));
        if let Some(o) = font.outline_glyph(glyph) {
            let b = o.px_bounds();
            o.draw(|gx, gy, cov| {
                let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                if (0..w as i32).contains(&px) && (0..hh as i32).contains(&py) {
                    let k = (py as usize * w + px as usize) * 4;
                    for i in 0..3 {
                        let c = rgba[k + i] as f32;
                        rgba[k + i] = (c + (255.0 - c) * cov).round() as u8;
                    }
                }
            });
        }
        x += sf.h_advance(id);
        prev = Some(id);
    }
    Some(Picture {
        width: w as u32,
        height: hh as u32,
        rgba,
    })
}

/// A Lucide shape `h` pixels tall: filled with `rgb` and outlined in white, or (`filled` false)
/// a coloured ring with a white edge.
fn shape_picture(shape: &str, h: u32, rgb: [u8; 3], filled: bool) -> Option<Picture> {
    let c = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
    let (under, over) = if filled {
        (
            r##"fill="#fff" stroke="#fff" stroke-width="4.2""##.to_string(),
            format!(r#"fill="{c}" stroke="{c}" stroke-width="1.8""#),
        )
    } else {
        (
            r##"fill="none" stroke="#fff" stroke-width="4.6""##.to_string(),
            format!(r#"fill="none" stroke="{c}" stroke-width="2.4""#),
        )
    };
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="-2.5 -2.5 29 29" width="29" height="29"><g {under} stroke-linecap="round" stroke-linejoin="round">{shape}</g><g {over} stroke-linecap="round" stroke-linejoin="round">{shape}</g></svg>"#
    );
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok()?;
    let side = h.max(8);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(side, side)?;
    let k = side as f32 / 29.0;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(k, k),
        &mut pixmap.as_mut(),
    );
    Some(Picture {
        width: side,
        height: side,
        rgba: unpremultiply(pixmap.take()),
    })
}

fn unpremultiply(mut rgba: Vec<u8>) -> Vec<u8> {
    for p in rgba.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    rgba
}

/// A picture's colours premultiplied by alpha, as floats, blurred by `radius` pixels (a box
/// blur run three times, close to a Gaussian). Grown by the radius on each side.
fn premultiplied(pic: &Picture, radius: usize) -> (usize, usize, Vec<[f32; 4]>) {
    let (w0, h0) = (pic.width as usize, pic.height as usize);
    let pad = radius * 2;
    let (w, h) = (w0 + 2 * pad, h0 + 2 * pad);
    let mut buf = vec![[0f32; 4]; w * h];
    for y in 0..h0 {
        for x in 0..w0 {
            let k = (y * w0 + x) * 4;
            let a = pic.rgba[k + 3] as f32 / 255.0;
            buf[(y + pad) * w + x + pad] = [
                pic.rgba[k] as f32 / 255.0 * a,
                pic.rgba[k + 1] as f32 / 255.0 * a,
                pic.rgba[k + 2] as f32 / 255.0 * a,
                a,
            ];
        }
    }
    if radius > 0 {
        for _ in 0..3 {
            box_blur(&mut buf, w, h, radius, true);
            box_blur(&mut buf, w, h, radius, false);
        }
    }
    (w, h, buf)
}

fn box_blur(buf: &mut [[f32; 4]], w: usize, h: usize, r: usize, across: bool) {
    let (lines, len) = if across { (h, w) } else { (w, h) };
    let at = |line: usize, i: usize| if across { line * w + i } else { i * w + line };
    let mut row = vec![[0f32; 4]; len];
    let n = (2 * r + 1) as f32;
    for line in 0..lines {
        for (i, v) in row.iter_mut().enumerate() {
            *v = buf[at(line, i)];
        }
        let mut sum = [0f32; 4];
        for px in &row[..=r.min(len - 1)] {
            for (total, v) in sum.iter_mut().zip(px) {
                *total += v;
            }
        }
        for i in 0..len {
            let out = at(line, i);
            for c in 0..4 {
                buf[out][c] = sum[c] / n;
            }
            if i + r + 1 < len {
                for c in 0..4 {
                    sum[c] += row[i + r + 1][c];
                }
            }
            if i >= r {
                for c in 0..4 {
                    sum[c] -= row[i - r][c];
                }
            }
        }
    }
}

/// Frames of an item coming in or going out, for laying over the picture at (`x`, `y`).
#[derive(Clone, Debug)]
pub struct Moving {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub frames: usize,
    /// Every frame's straight-alpha RGBA, one after another.
    pub rgba: Vec<u8>,
}

/// Samples per frame for the motion blur: enough that fast movement smears smoothly (one about
/// every 3 pixels it travels), within these bounds.
const MIN_SAMPLES: usize = 6;
const MAX_SAMPLES: usize = 48;

/// `pic` coming in (or, `leaving`, going out) over `seconds` at `fps`, centred at `centre` in a
/// `frame`-sized picture. Each frame averages several moments across its time, which blurs fast
/// movement the way a camera would. `None` without an animation.
pub fn animate(
    pic: &Picture,
    anim: &str,
    leaving: bool,
    centre: (f32, f32),
    frame: (u32, u32),
    seconds: f64,
    fps: u32,
) -> Option<Moving> {
    if anim == "none" || seconds <= 0.0 || pic.width == 0 || pic.height == 0 {
        return None;
    }
    let frames = ((seconds * fps as f64).ceil() as usize).max(1);
    let fw = frame.0 as f32;
    let (pw, ph) = (pic.width as f32, pic.height as f32);
    // Each sample's pose (progress runs 0 to 1 coming in, 1 to 0 going out).
    let at = |t: f64| {
        let p = (t / seconds).clamp(0.0, 1.0) as f32;
        pose(anim, if leaving { 1.0 - p } else { p }, leaving)
    };
    let poses: Vec<Vec<Pose>> = (0..frames)
        .map(|f| {
            let (a, b) = (at(f as f64 / fps as f64), at((f + 1) as f64 / fps as f64));
            let travel = (b.shift - a.shift).abs() * fw + (b.scale - a.scale).abs() * pw.max(ph);
            let n = ((travel / 3.0).ceil() as usize).clamp(MIN_SAMPLES, MAX_SAMPLES);
            (0..n)
                .map(|s| at((f as f64 + s as f64 / n as f64) / fps as f64))
                .collect()
        })
        .collect();
    let max_blur = poses
        .iter()
        .flatten()
        .map(|p| p.blur)
        .fold(0.0f32, f32::max);
    let blur_px = (max_blur * ph).round() as usize;
    // The area anything is drawn in, kept inside the picture.
    let (mut l, mut t, mut r, mut b) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in poses.iter().flatten() {
        let cx = centre.0 + p.shift * fw;
        let (hw, hh) = (pw * p.scale / 2.0, ph * p.scale / 2.0);
        let pad = 2.0 * p.blur * ph * p.scale + 2.0;
        l = l.min(cx - hw - pad);
        r = r.max(cx + hw + pad);
        t = t.min(centre.1 - hh - pad);
        b = b.max(centre.1 + hh + pad);
    }
    let l = l.floor().max(0.0) as i32;
    let t = t.floor().max(0.0) as i32;
    let r = (r.ceil() as i32).min(frame.0 as i32);
    let b = (b.ceil() as i32).min(frame.1 as i32);
    if r <= l || b <= t {
        return None;
    }
    let (w, h) = ((r - l) as usize, (b - t) as usize);
    // Blurred copies, made once for each radius used.
    let mut blurred: std::collections::HashMap<usize, (usize, usize, Vec<[f32; 4]>)> =
        Default::default();
    let mut rgba = Vec::with_capacity(w * h * 4 * frames);
    let mut acc = vec![[0f32; 4]; w * h];
    for samples in &poses {
        acc.iter_mut().for_each(|v| *v = [0.0; 4]);
        for p in samples {
            let radius = if blur_px == 0 {
                0
            } else {
                (p.blur * ph).round() as usize
            };
            let (sw, sh, src) = blurred
                .entry(radius)
                .or_insert_with(|| premultiplied(pic, radius));
            let (sw, sh) = (*sw, *sh);
            if p.scale <= 0.001 || p.alpha <= 0.0 {
                continue;
            }
            // Where the (grown) source lands, in the area's pixels.
            let (dw, dh) = (sw as f32 * p.scale, sh as f32 * p.scale);
            let ox = centre.0 + p.shift * fw - dw / 2.0 - l as f32;
            let oy = centre.1 - dh / 2.0 - t as f32;
            let x0 = ox.floor().max(0.0) as usize;
            let y0 = oy.floor().max(0.0) as usize;
            let x1 = ((ox + dw).ceil() as usize).min(w);
            let y1 = ((oy + dh).ceil() as usize).min(h);
            for y in y0..y1 {
                let sy = (y as f32 + 0.5 - oy) / p.scale - 0.5;
                for x in x0..x1 {
                    let sx = (x as f32 + 0.5 - ox) / p.scale - 0.5;
                    let v = bilinear(src, sw, sh, sx, sy);
                    let a = &mut acc[y * w + x];
                    for c in 0..4 {
                        a[c] += v[c] * p.alpha;
                    }
                }
            }
        }
        let n = samples.len() as f32;
        for v in &acc {
            let a = (v[3] / n).clamp(0.0, 1.0);
            if a <= 0.0 {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            for c in &v[..3] {
                let straight = c / n / a;
                rgba.push((straight.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
            rgba.push((a * 255.0).round() as u8);
        }
    }
    Some(Moving {
        x: l,
        y: t,
        width: w as u32,
        height: h as u32,
        fps,
        frames,
        rgba,
    })
}

fn bilinear(src: &[[f32; 4]], w: usize, h: usize, x: f32, y: f32) -> [f32; 4] {
    if x < -1.0 || y < -1.0 || x > w as f32 || y > h as f32 {
        return [0.0; 4];
    }
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let get = |xi: f32, yi: f32| -> [f32; 4] {
        if xi < 0.0 || yi < 0.0 || xi >= w as f32 || yi >= h as f32 {
            [0.0; 4]
        } else {
            src[yi as usize * w + xi as usize]
        }
    };
    let (a, b, c, d) = (
        get(x0, y0),
        get(x0 + 1.0, y0),
        get(x0, y0 + 1.0),
        get(x0 + 1.0, y0 + 1.0),
    );
    let mut out = [0f32; 4];
    for i in 0..4 {
        let top = a[i] + (b[i] - a[i]) * fx;
        let bottom = c[i] + (d[i] - c[i]) * fx;
        out[i] = top + (bottom - top) * fy;
    }
    out
}

/// Items kept with a clip (a JSON file).
pub fn load(path: &std::path::Path) -> Vec<Item> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(path: &std::path::Path, items: &[Item]) -> std::io::Result<()> {
    if items.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(items).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FONT: &[u8] = include_bytes!("../assets/fonts/Inter-Bold.ttf");

    /// Every sticker, and the frames of each animation, on one sheet:
    /// `KAIRO_SNAPSHOTS=<dir> cargo test --lib -- --ignored overlays::tests::sheet`.
    #[test]
    #[ignore]
    fn sheet() {
        let dir = std::path::PathBuf::from(std::env::var_os("KAIRO_SNAPSHOTS").expect("dir"));
        let (w, h) = (1280u32, 1100u32);
        let mut sheet = image::RgbaImage::from_pixel(w, h, image::Rgba([40, 44, 52, 255]));
        let mut paste = |p: &Picture, x: i64, y: i64| {
            let img = image::RgbaImage::from_raw(p.width, p.height, p.rgba.clone()).expect("rgba");
            image::imageops::overlay(&mut sheet, &img, x, y);
        };
        let (mut x, mut y) = (20i64, 20i64);
        for s in &STICKERS {
            let p = Item::sticker(s.id, 0.0, 3.0)
                .picture(720, 1200, FONT)
                .expect(s.id);
            if x + p.width as i64 > w as i64 - 20 {
                x = 20;
                y += 140;
            }
            paste(&p, x, y);
            x += p.width as i64 + 24;
        }
        let mut text = Item::text("What a clutch", 0.0, 3.0);
        text.color = 1;
        paste(&text.picture(720, 1200, FONT).expect("text"), 20, y + 150);
        y += 260;
        let star = Item::sticker("star", 0.0, 3.0)
            .picture(720, 1200, FONT)
            .expect("star");
        for (row, anim) in ["pop", "slide", "blur"].into_iter().enumerate() {
            let m = animate(&star, anim, false, (640.0, 60.0), (1280, 120), 0.4, 60).expect(anim);
            let n = (m.width * m.height * 4) as usize;
            for (k, f) in [3usize, 6, 9, 12, 23].into_iter().enumerate() {
                let frame = Picture {
                    width: m.width,
                    height: m.height,
                    rgba: m.rgba[f * n..(f + 1) * n].to_vec(),
                };
                let fx = if anim == "slide" { m.x as i64 / 3 } else { 0 };
                paste(&frame, 20 + k as i64 * 250 + fx, y + row as i64 * 260);
            }
        }
        sheet.save(dir.join("stickers.png")).expect("save");
    }

    #[test]
    fn every_sticker_draws() {
        for s in &STICKERS {
            let item = Item::sticker(s.id, 0.0, 3.0);
            let p = item.picture(720, 1200, FONT).expect(s.id);
            assert!(p.width > 10 && p.height > 10, "{}", s.id);
            assert_eq!(p.rgba.len(), (p.width * p.height * 4) as usize);
            let opaque = p.rgba.chunks(4).filter(|px| px[3] > 200).count();
            assert!(
                opaque > (p.width * p.height / 10) as usize,
                "{} is mostly empty",
                s.id
            );
        }
        let mut ids: Vec<_> = STICKERS.iter().map(|s| s.id).collect();
        ids.dedup();
        assert_eq!(ids.len(), STICKERS.len());
    }

    #[test]
    fn text_comes_in_the_chosen_colour() {
        let mut item = Item::text("Nice shot", 1.0, 4.0);
        item.color = 3;
        let p = item.picture(720, 1200, FONT).expect("text");
        // The middle of a letter is red, not white.
        let reddest = p
            .rgba
            .chunks(4)
            .filter(|px| px[3] == 255)
            .map(|px| px[0] as i32 - px[1] as i32)
            .max()
            .unwrap_or(0);
        assert!(reddest > 150, "{reddest}");
    }

    #[test]
    fn animations_settle_where_the_item_rests() {
        for (anim, _) in ANIMS {
            assert_eq!(pose(anim, 1.0, false), Pose::SETTLED, "{anim}");
        }
        assert_eq!(pose("pop", 0.0, false).scale, 0.0);
        assert!(pose("slide", 0.0, false).shift < -1.0);
        assert!(pose("blur", 0.0, false).blur > 0.1);

        let item = Item {
            anim: "pop".into(),
            ..Item::text("GG", 2.0, 5.0)
        };
        assert_eq!(item.anim_lengths(3.0), (IN_SECONDS, OUT_SECONDS));
        assert!(item.pose_at(2.05).scale < 0.9);
        assert_eq!(item.pose_at(3.5), Pose::SETTLED);
        assert!(item.pose_at(4.95).alpha < 1.0);
        // A short one animates faster rather than never settling.
        let (i, o) = item.anim_lengths(0.5);
        assert!(i + o <= 0.41);
    }

    #[test]
    fn coming_in_is_drawn_with_motion_blur() {
        let item = Item::sticker("star", 0.0, 3.0);
        let pic = item.picture(720, 1200, FONT).expect("star");
        let m =
            animate(&pic, "slide", false, (900.0, 200.0), (1280, 720), 0.4, 60).expect("frames");
        assert_eq!(m.frames, 24);
        assert_eq!(m.rgba.len(), (m.width * m.height * 4) as usize * m.frames);
        assert!(m.x >= 0 && m.x as u32 + m.width <= 1280);
        // A third of the way in it's moving fast: smeared, so mostly see-through, not solid.
        let frame = |i: usize| {
            let n = (m.width * m.height * 4) as usize;
            &m.rgba[i * n..(i + 1) * n]
        };
        let partly = |f: &[u8]| f.chunks(4).filter(|p| p[3] > 20 && p[3] < 200).count();
        let solid = |f: &[u8]| f.chunks(4).filter(|p| p[3] >= 250).count();
        assert!(partly(frame(8)) > 500 && partly(frame(8)) > solid(frame(8)));
        // At the end it has settled: solid, like the still picture.
        assert!(solid(frame(23)) > partly(frame(23)));

        let blur =
            animate(&pic, "blur", false, (640.0, 360.0), (1280, 720), 0.4, 60).expect("frames");
        assert!(blur.width > pic.width, "grown for the blur");
        assert!(animate(&pic, "none", false, (640.0, 360.0), (1280, 720), 0.4, 60).is_none());
    }
}
