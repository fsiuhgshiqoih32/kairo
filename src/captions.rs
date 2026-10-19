//! Captions from recognised speech: words grouped into lines, SubRip text, and the pictures
//! burned into exported copies.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};

/// One recognised word and when it was said (seconds into the clip).
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// One caption on screen.
#[derive(Clone, Debug, PartialEq)]
pub struct Caption {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// A line breaks before it gets longer than this many characters...
const MAX_CHARS: usize = 34;
/// ...or than this many seconds...
const MAX_SECONDS: f64 = 3.2;
/// ...or at a pause this long.
const PAUSE: f64 = 0.7;
/// Every caption stays up at least this long (unless the next one starts sooner).
const MIN_SHOWN: f64 = 0.9;

/// The words of a Vosk result (`{"result": [{"word", "start", "end", "conf"}], "text"}`).
pub fn words_from_vosk(json: &str) -> Vec<Word> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    v["result"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|w| {
                    Some(Word {
                        start: w["start"].as_f64()?,
                        end: w["end"].as_f64()?,
                        text: w["word"].as_str()?.to_string(),
                    })
                })
                .filter(|w| !w.text.is_empty() && w.text != "[unk]")
                .collect()
        })
        .unwrap_or_default()
}

/// Words grouped into readable lines, each shown from its first word to a little after its
/// last.
pub fn lines(words: &[Word]) -> Vec<Caption> {
    let mut out: Vec<Caption> = Vec::new();
    let mut cur: Option<Caption> = None;
    let mut last_end = f64::NEG_INFINITY;
    for w in words {
        let breaks = cur.as_ref().is_some_and(|c| {
            c.text.len() + 1 + w.text.len() > MAX_CHARS
                || w.end - c.start > MAX_SECONDS
                || w.start - last_end > PAUSE
        });
        if breaks {
            out.extend(cur.take());
        }
        match &mut cur {
            Some(c) => {
                c.text.push(' ');
                c.text.push_str(&w.text);
                c.end = w.end;
            }
            None => {
                cur = Some(Caption {
                    start: w.start,
                    end: w.end,
                    text: w.text.clone(),
                })
            }
        }
        last_end = w.end;
    }
    out.extend(cur);
    // Hold short lines a little, without running into the next.
    for i in 0..out.len() {
        let next = out.get(i + 1).map_or(f64::INFINITY, |n| n.start);
        let c = &mut out[i];
        c.end = c.end.max(c.start + MIN_SHOWN).min(next);
        if let Some(first) = c.text.get(..1) {
            c.text = first.to_uppercase() + &c.text[1..];
        }
    }
    out
}

fn srt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// The captions as a SubRip (.srt) file.
pub fn srt(caps: &[Caption]) -> String {
    caps.iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                "{}\n{} --> {}\n{}\n",
                i + 1,
                srt_time(c.start),
                srt_time(c.end),
                c.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_srt_time(t: &str) -> Option<f64> {
    let (hms, ms) = t.trim().split_once([',', '.'])?;
    let mut p = hms.split(':');
    let (h, m, s) = (p.next()?, p.next()?, p.next()?);
    Some(
        h.parse::<f64>().ok()? * 3600.0
            + m.parse::<f64>().ok()? * 60.0
            + s.parse::<f64>().ok()?
            + ms.parse::<f64>().ok()? / 1000.0,
    )
}

/// Captions read back from a SubRip file (blocks that don't parse are skipped).
pub fn parse_srt(text: &str) -> Vec<Caption> {
    let text = text.replace("\r\n", "\n");
    text.split("\n\n")
        .filter_map(|block| {
            let mut lines = block.lines().map(str::trim).filter(|l| !l.is_empty());
            let first = lines.next()?;
            let times = if first.contains("-->") {
                first
            } else {
                lines.next()?
            };
            let (a, b) = times.split_once("-->")?;
            let text = lines.collect::<Vec<_>>().join(" ");
            (!text.is_empty()).then_some(Caption {
                start: parse_srt_time(a)?,
                end: parse_srt_time(b)?,
                text,
            })
        })
        .collect()
}

/// A caption picture: straight-alpha RGBA.
#[derive(Clone, Debug)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

fn text_width<F: Font>(sf: &ab_glyph::PxScaleFont<&F>, s: &str) -> f32 {
    let mut w = 0.0;
    let mut prev = None;
    for c in s.chars() {
        let id = sf.glyph_id(c);
        if let Some(p) = prev {
            w += sf.kern(p, id);
        }
        w += sf.h_advance(id);
        prev = Some(id);
    }
    w
}

/// Draws `text` in white with a black outline, wrapped to at most `max_width` pixels (two lines
/// at most), `size` pixels tall. `None` for an unusable font or empty text.
pub fn render(text: &str, max_width: u32, size: f32, font: &[u8]) -> Option<Picture> {
    render_colored(text, max_width, size, font, [255, 255, 255])
}

/// [`render`] in the colour `rgb`.
pub fn render_colored(
    text: &str,
    max_width: u32,
    size: f32,
    font: &[u8],
    rgb: [u8; 3],
) -> Option<Picture> {
    let font = FontRef::try_from_slice(font).ok()?;
    let sf = font.as_scaled(PxScale::from(size));
    let pad = (size * 0.12).ceil() as i32 + 2;
    let room = (max_width as f32 - 2.0 * pad as f32).max(size);
    // Greedy wrap.
    let mut rows: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match rows.last_mut() {
            Some(r) if text_width(&sf, &format!("{r} {word}")) <= room => {
                r.push(' ');
                r.push_str(word);
            }
            _ => rows.push(word.to_string()),
        }
    }
    if rows.is_empty() {
        return None;
    }
    if rows.len() > 2 {
        let rest = rows.split_off(1).join(" ");
        rows.push(rest);
    }
    let line_h = (sf.ascent() - sf.descent() + sf.line_gap()).ceil();
    let widest = rows
        .iter()
        .map(|r| text_width(&sf, r).min(room))
        .fold(0.0f32, f32::max);
    let w = (widest.ceil() as i32 + 2 * pad).max(1) as usize;
    let h = (line_h as i32 * rows.len() as i32 + 2 * pad).max(1) as usize;
    let mut fill = vec![0f32; w * h];
    for (i, r) in rows.iter().enumerate() {
        let mut x = (w as f32 - text_width(&sf, r).min(room)) / 2.0;
        let baseline = pad as f32 + i as f32 * line_h + sf.ascent();
        let mut prev = None;
        for c in r.chars() {
            let id = sf.glyph_id(c);
            if let Some(p) = prev {
                x += sf.kern(p, id);
            }
            let glyph = id.with_scale_and_position(sf.scale(), point(x, baseline));
            if let Some(outlined) = font.outline_glyph(glyph) {
                let b = outlined.px_bounds();
                outlined.draw(|gx, gy, cov| {
                    let px = b.min.x as i32 + gx as i32;
                    let py = b.min.y as i32 + gy as i32;
                    if (0..w as i32).contains(&px) && (0..h as i32).contains(&py) {
                        let k = py as usize * w + px as usize;
                        fill[k] = fill[k].max(cov);
                    }
                });
            }
            x += sf.h_advance(id);
            prev = Some(id);
        }
    }
    // The outline: the letters grown by a few pixels in every direction.
    let r = (size * 0.09).ceil().max(2.0) as i32;
    let mut edge = vec![0f32; w * h];
    let disc: Vec<(i32, i32)> = (-r..=r)
        .flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)))
        .filter(|(dx, dy)| dx * dx + dy * dy <= r * r)
        .collect();
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut m = 0f32;
            for (dx, dy) in &disc {
                let (sx, sy) = (x + dx, y + dy);
                if (0..w as i32).contains(&sx) && (0..h as i32).contains(&sy) {
                    m = m.max(fill[sy as usize * w + sx as usize]);
                    if m >= 1.0 {
                        break;
                    }
                }
            }
            edge[y as usize * w + x as usize] = m;
        }
    }
    let mut rgba = vec![0u8; w * h * 4];
    for k in 0..w * h {
        let a_in = fill[k];
        let a_edge = edge[k] * 0.92;
        let alpha = a_in + a_edge * (1.0 - a_in);
        if alpha <= 0.0 {
            continue;
        }
        let lit = a_in / alpha;
        let [r, g, b] = rgb.map(|c| (c as f32 * lit).round() as u8);
        rgba[k * 4..k * 4 + 4].copy_from_slice(&[r, g, b, (alpha * 255.0) as u8]);
    }
    Some(Picture {
        width: w as u32,
        height: h as u32,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(start: f64, end: f64, t: &str) -> Word {
        Word {
            start,
            end,
            text: t.into(),
        }
    }

    #[test]
    fn reads_vosk_results() {
        let j = r#"{"result":[{"conf":1.0,"end":1.11,"start":0.87,"word":"nice"},
            {"conf":0.6,"end":1.5,"start":1.2,"word":"[unk]"},
            {"conf":0.9,"end":1.9,"start":1.5,"word":"shot"}],"text":"nice shot"}"#;
        assert_eq!(
            words_from_vosk(j),
            [w(0.87, 1.11, "nice"), w(1.5, 1.9, "shot")]
        );
        assert!(words_from_vosk("{\"text\": \"\"}").is_empty());
        assert!(words_from_vosk("not json").is_empty());
    }

    #[test]
    fn lines_break_at_pauses_length_and_time() {
        let words = [
            w(0.0, 0.3, "nice"),
            w(0.35, 0.6, "shot"),
            // A long pause: a new line.
            w(2.0, 2.2, "go"),
            w(2.25, 2.5, "left"),
            w(2.55, 2.9, "there"),
            w(2.95, 3.3, "are"),
            w(3.35, 3.6, "two"),
            w(3.65, 4.0, "behind"),
            w(4.05, 4.4, "the"),
            w(4.45, 4.9, "wall"),
            w(4.95, 5.3, "watch"),
            w(5.35, 5.7, "out"),
        ];
        let l = lines(&words);
        assert_eq!(l[0].text, "Nice shot");
        assert_eq!((l[0].start, l[0].end), (0.0, 0.9));
        assert!(l[1].text.starts_with("Go left"));
        assert!(l.iter().all(|c| c.text.len() <= MAX_CHARS));
        assert!(l.iter().all(|c| c.end - c.start <= MAX_SECONDS + MIN_SHOWN));
        for p in l.windows(2) {
            assert!(p[0].end <= p[1].start);
        }
        assert_eq!(
            l.iter().map(|c| c.text.split(' ').count()).sum::<usize>(),
            words.len()
        );
    }

    #[test]
    fn subrip_text() {
        let s = srt(&[
            Caption {
                start: 0.5,
                end: 1.25,
                text: "Nice".into(),
            },
            Caption {
                start: 61.0,
                end: 3725.004,
                text: "Shot".into(),
            },
        ]);
        assert_eq!(
            s,
            "1\n00:00:00,500 --> 00:00:01,250\nNice\n\n2\n00:01:01,000 --> 01:02:05,004\nShot\n"
        );
    }

    #[test]
    fn subrip_reads_back() {
        let caps = vec![
            Caption {
                start: 0.5,
                end: 1.25,
                text: "Nice".into(),
            },
            Caption {
                start: 61.0,
                end: 62.5,
                text: "Two behind the wall".into(),
            },
        ];
        assert_eq!(parse_srt(&srt(&caps)), caps);
        let windows = "1\r\n00:00:00,500 --> 00:00:01,250\r\nNice\r\nshot\r\n\r\nbroken\r\n";
        assert_eq!(parse_srt(windows)[0].text, "Nice shot");
        assert_eq!(parse_srt(windows).len(), 1);
    }

    #[test]
    fn draws_outlined_text_that_fits() {
        let font = include_bytes!("../assets/fonts/Inter-Bold.ttf");
        let p = render(
            "Two behind the wall, watch out on the left",
            600,
            48.0,
            font,
        )
        .unwrap();
        assert!(p.width <= 600, "{}", p.width);
        // Wrapped onto two lines.
        assert!(p.height > 48 * 2, "{}", p.height);
        assert_eq!(p.rgba.len(), (p.width * p.height * 4) as usize);
        let px = |x: u32, y: u32| &p.rgba[((y * p.width + x) * 4) as usize..][..4];
        // White letters with a black edge somewhere, and clear corners.
        let mut white = false;
        let mut black = false;
        for y in 0..p.height {
            for x in 0..p.width {
                let c = px(x, y);
                white |= c[3] == 255 && c[0] > 240;
                black |= c[3] > 200 && c[0] < 15;
            }
        }
        assert!(white && black);
        assert_eq!(px(0, 0)[3], 0);
        assert!(render("   ", 600, 48.0, font).is_none());
    }
}
