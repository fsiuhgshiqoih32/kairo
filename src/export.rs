//! Exports from the editor: a trimmed copy, or a copy small enough for Discord. Each runs ffmpeg
//! on a worker thread and reports progress.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kairo::args::{self, Encoder};
use kairo::library;
use kairo::media::{self, Effects, Info};

use crate::ffmpeg;
use crate::log::{info, warning};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Trim,
    /// Fit under this many bytes.
    Share(u64),
    Gif,
    /// 9:16 for phones.
    Vertical,
    /// 1:1 for feeds.
    Square,
    /// 4:5 for feeds.
    Portrait,
    /// Slowed down this many times.
    Slow(u32),
}

#[derive(Default)]
pub struct Status {
    /// 0 to 1.
    pub progress: f32,
    pub stage: String,
    pub result: Option<Result<PathBuf, String>>,
}

pub struct Job {
    pub kind: Kind,
    pub status: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
}

impl Job {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn finished(&self) -> bool {
        self.status.lock().unwrap().result.is_some()
    }
}

/// The encoder exports use: the best one that works here (probed once per editor).
pub fn best_encoder(ff: &Path, found: &Mutex<Option<Vec<Encoder>>>) -> Encoder {
    let mut f = found.lock().unwrap();
    let list = f.get_or_insert_with(|| ffmpeg::working_encoders(ff));
    list.first().copied().unwrap_or(Encoder::X264)
}

#[allow(clippy::too_many_arguments)]
pub fn start(
    kind: Kind,
    ff: PathBuf,
    clip: PathBuf,
    info: Info,
    range: (f64, f64),
    encoders: Arc<Mutex<Option<Vec<Encoder>>>>,
    eff: Effects,
    caps: Vec<kairo::captions::Caption>,
    keys: Vec<kairo::keys::KeyEvent>,
    cuts: Vec<(f64, f64)>,
) -> Job {
    let status = Arc::new(Mutex::new(Status {
        stage: match kind {
            Kind::Trim => "Saving the trimmed copy".into(),
            Kind::Share(_) => "Making a copy for Discord".into(),
            Kind::Gif => "Making a GIF".into(),
            Kind::Vertical => "Making a vertical copy".into(),
            Kind::Square => "Making a square copy".into(),
            Kind::Portrait => "Making a 4:5 copy".into(),
            Kind::Slow(f) => format!("Slowing it down {f}x"),
        },
        ..Default::default()
    }));
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let status = status.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let job = Export {
                kind,
                ff: &ff,
                clip: &clip,
                named: &clip,
                info: &info,
                range,
                caps: &caps,
                keys: &keys,
                burns_dir: kairo::config::config_dir().join("captions").join(format!(
                    "{}-{}",
                    std::process::id(),
                    crate::recorder::now_us()
                )),
                progress: (0.0, 1.0),
            };
            let result = if kairo::cuts::cuts_into(range, &cuts) {
                run_cut(&job, &cuts, &encoders, &eff, &status, &cancel)
            } else {
                run(&job, &encoders, &eff, &status, &cancel)
            };
            let _ = std::fs::remove_dir_all(&job.burns_dir);
            if let Err(e) = &result {
                warning!("export failed: {e}");
            }
            status.lock().unwrap().result = Some(result);
        });
    }
    Job {
        kind,
        status,
        cancel,
    }
}

/// What an export works on.
struct Export<'a> {
    kind: Kind,
    ff: &'a Path,
    /// What's read: the clip, or its kept parts joined.
    clip: &'a Path,
    /// The clip copies are named after.
    named: &'a Path,
    info: &'a Info,
    range: (f64, f64),
    caps: &'a [kairo::captions::Caption],
    /// Keys and clicks to overlay (empty for none).
    keys: &'a [kairo::keys::KeyEvent],
    /// Where caption pictures and overlay frames go while it runs.
    burns_dir: PathBuf,
    /// The share of the progress bar this pass fills.
    progress: (f32, f32),
}

impl Export<'_> {
    /// `eff` with the captions and the keys overlay drawn for a picture of `size`.
    fn with_captions(&self, eff: &Effects, size: (u32, u32)) -> Result<Effects, String> {
        let mut eff = eff.clone();
        if !self.caps.is_empty() {
            eff.burns = render_burns(self.caps, self.range, eff.speed, size, &self.burns_dir)?;
        }
        if !eff.stamps.is_empty() {
            let stamps = render_stamps(&eff.stamps, self.range, eff.speed, size, &self.burns_dir)?;
            eff.burns.extend(stamps);
        }
        if !self.keys.is_empty() {
            const FPS: u32 = 15;
            let overlay = kairo::keys::size_for(size.1);
            let path = self.burns_dir.join("keys.rgba");
            std::fs::create_dir_all(&self.burns_dir).map_err(|e| e.to_string())?;
            let accent = crate::gui::theme::accent_rgb();
            kairo::keys::write_stream(
                self.keys,
                self.range,
                eff.speed,
                FPS,
                overlay,
                accent,
                include_bytes!("../assets/fonts/Inter-Bold.ttf"),
                &path,
            )
            .map_err(|e| e.to_string())?;
            eff.keys = Some(media::OverlayVideo {
                path,
                width: overlay.0,
                height: overlay.1,
                fps: FPS,
            });
        }
        Ok(eff)
    }
}

fn run(
    job: &Export,
    encoders: &Mutex<Option<Vec<Encoder>>>,
    eff: &Effects,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    let (kind, ff, clip, info) = (job.kind, job.ff, job.clip, job.info);
    let (start, end) = job.range;
    let size = (info.width, info.height);
    let encoder = best_encoder(ff, encoders);
    // How long the export runs once sped up or slowed down.
    let length = eff.length(start, end);
    match kind {
        Kind::Trim => {
            let eff = job.with_captions(eff, size)?;
            let what = if eff.is_plain() { "trim" } else { "edit" };
            let out = library::sibling(job.named, what);
            let part = out.with_extension("mp4.part");
            let a = media::trim_args(clip, start, end, info, &eff, encoder, &part);
            encode(ff, &a, length, &part, status, cancel, job.progress)?;
            finish(ff, &part, &out)
        }
        Kind::Share(limit) => {
            let out = library::sibling(job.named, "Discord");
            let part = out.with_extension("mp4.part");
            // Encoders overshoot now and then: if the copy comes out too big, go again smaller.
            for (attempt, scale) in [1.0, 0.85, 0.7].into_iter().enumerate() {
                let plan = media::share_plan(limit, length, info, scale)?;
                if attempt > 0 {
                    status.lock().unwrap().stage = "Too big, trying smaller".into();
                }
                let shown = match plan.height {
                    Some(h) => (
                        (info.width as u64 * h as u64 / info.height.max(1) as u64) as u32 & !1,
                        h,
                    ),
                    None => size,
                };
                let eff = job.with_captions(eff, shown)?;
                let a = media::share_args(clip, start, end, info, &eff, &plan, encoder, &part);
                encode(ff, &a, length, &part, status, cancel, job.progress)?;
                let size = std::fs::metadata(&part)
                    .map(|m| m.len())
                    .unwrap_or(u64::MAX);
                info!("Discord copy: {size} bytes for a {limit}-byte limit ({plan:?})");
                if size <= limit {
                    return finish(ff, &part, &out);
                }
            }
            let _ = std::fs::remove_file(&part);
            Err(format!(
                "Couldn't get it under {} MB. Trim it shorter and try again.",
                limit / 1_000_000
            ))
        }
        Kind::Gif => {
            if length > GIF_MAX_SECONDS + 0.05 {
                return Err(format!(
                    "GIFs get huge: trim it to {GIF_MAX_SECONDS:.0} seconds or less first."
                ));
            }
            let out = library::sibling_with(job.named, "GIF", "gif");
            let part = out.with_extension("gif.part");
            let a = media::gif_args(clip, start, end, info, eff, 480, 15, &part);
            encode(ff, &a, length, &part, status, cancel, job.progress)?;
            std::fs::rename(&part, &out).map_err(|e| e.to_string())?;
            info!("exported {}", out.display());
            Ok(out)
        }
        Kind::Vertical | Kind::Square | Kind::Portrait => {
            let (what, frame) = match kind {
                Kind::Vertical => ("vertical", (1080, 1920)),
                Kind::Square => ("square", (1080, 1080)),
                _ => ("4x5", (1080, 1350)),
            };
            let eff = job.with_captions(eff, frame)?;
            let out = library::sibling(job.named, what);
            let part = out.with_extension("mp4.part");
            let a = media::framed_args(clip, start, end, info, &eff, frame, encoder, &part);
            encode(ff, &a, length, &part, status, cancel, job.progress)?;
            finish(ff, &part, &out)
        }
        Kind::Slow(factor) => {
            let out = library::sibling(job.named, &format!("slow {factor}x"));
            let part = out.with_extension("mp4.part");
            let slow = job.with_captions(&eff.at_speed(eff.speed / factor.max(1) as f64), size)?;
            let a = media::trim_args(clip, start, end, info, &slow, encoder, &part);
            encode(
                ff,
                &a,
                slow.length(start, end),
                &part,
                status,
                cancel,
                job.progress,
            )?;
            finish(ff, &part, &out)
        }
    }
}

/// A copy without the parts cut out: the kept parts are joined first (or, for a plain trimmed
/// copy, that join is the copy), then the export runs on the joined file, with captions, text and
/// keys moved to their new times.
fn run_cut(
    job: &Export,
    cuts: &[(f64, f64)],
    encoders: &Mutex<Option<Vec<Encoder>>>,
    eff: &Effects,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    let parts = kairo::cuts::kept(job.range, cuts);
    let length: f64 = parts.iter().map(|(a, b)| b - a).sum();
    let encoder = best_encoder(job.ff, encoders);
    let plain = eff.is_plain() && job.caps.is_empty() && job.keys.is_empty();
    if job.kind == Kind::Trim && plain {
        let out = library::sibling(job.named, "edit");
        let part = out.with_extension("mp4.part");
        let a = media::join_args(job.clip, &parts, job.info, encoder, true, &part);
        encode(job.ff, &a, length, &part, status, cancel, (0.0, 1.0))?;
        return finish(job.ff, &part, &out);
    }
    status.lock().unwrap().stage = "Joining the parts".into();
    std::fs::create_dir_all(&job.burns_dir).map_err(|e| e.to_string())?;
    let joined = job.burns_dir.join("joined.mkv");
    let a = media::join_args(job.clip, &parts, job.info, encoder, false, &joined);
    encode(job.ff, &a, length, &joined, status, cancel, (0.0, 0.35))?;
    let mut info = ffmpeg::probe_clip(job.ff, &joined)?;
    // The finished copy keeps the clip's quality, not the joined file's higher bitrate.
    info.video_bps = job.info.video_bps;
    let window = |a: f64, b: f64| kairo::cuts::window(job.range, cuts, a, b);
    let caps: Vec<kairo::captions::Caption> = job
        .caps
        .iter()
        .filter_map(|c| {
            window(c.start, c.end).map(|(start, end)| kairo::captions::Caption {
                start,
                end,
                text: c.text.clone(),
            })
        })
        .collect();
    let keys: Vec<kairo::keys::KeyEvent> = job
        .keys
        .iter()
        .filter_map(|k| {
            kairo::cuts::to_copy(job.range, cuts, k.t).map(|t| kairo::keys::KeyEvent { t, ..*k })
        })
        .collect();
    let mut eff = eff.clone();
    eff.stamps = eff
        .stamps
        .iter()
        .filter_map(|s| {
            window(s.start, s.end).map(|(start, end)| kairo::overlays::Item {
                start,
                end,
                ..s.clone()
            })
        })
        .collect();
    status.lock().unwrap().stage = match job.kind {
        Kind::Share(_) => "Making a copy for Discord".into(),
        Kind::Gif => "Making a GIF".into(),
        _ => "Saving the edited copy".into(),
    };
    let next = Export {
        kind: job.kind,
        ff: job.ff,
        clip: &joined,
        named: job.named,
        info: &info,
        range: (0.0, length),
        caps: &caps,
        keys: &keys,
        burns_dir: job.burns_dir.clone(),
        progress: (0.35, 1.0),
    };
    run(&next, encoders, &eff, status, cancel)
}

/// A montage being made from several clips.
pub struct Montage {
    pub status: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
}

impl Montage {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn finished(&self) -> bool {
        self.status.lock().unwrap().result.is_some()
    }
}

/// Joins `clips` (in that order) into a new clip in `dir`, on a worker thread.
pub fn start_montage(ff: PathBuf, clips: Vec<PathBuf>, dir: PathBuf) -> Montage {
    let status = Arc::new(Mutex::new(Status {
        stage: format!("Making a montage of {} clips", clips.len()),
        ..Default::default()
    }));
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let (status, cancel) = (status.clone(), cancel.clone());
        std::thread::spawn(move || {
            let result = montage(&ff, &clips, &dir, &status, &cancel);
            if let Err(e) = &result {
                warning!("montage failed: {e}");
            }
            status.lock().unwrap().result = Some(result);
        });
    }
    Montage { status, cancel }
}

fn montage(
    ff: &Path,
    clips: &[PathBuf],
    dir: &Path,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    let mut pieces = Vec::new();
    let mut first: Option<Info> = None;
    for c in clips {
        let info = ffmpeg::probe_clip(ff, c)?;
        pieces.push(media::Piece {
            path: c.clone(),
            duration: info.duration,
            has_audio: !info.audio_tracks.is_empty(),
        });
        first.get_or_insert(info);
    }
    let first = first.ok_or("Pick some clips first.")?;
    // The first clip's shape, at most 1080p and 60 fps: montages are for sharing.
    let size = args::output_size((first.width, first.height), Some(1080));
    let fps = (first.fps.round() as u32).clamp(24, 60);
    let encoder = best_encoder(ff, &Mutex::new(None));
    let out = crate::save::clip_path(dir, Some("Montage"));
    let part = out.with_extension("mp4.part");
    let a = media::montage_args(&pieces, size, fps, encoder, &part);
    encode(
        ff,
        &a,
        media::montage_length(&pieces),
        &part,
        status,
        cancel,
        (0.0, 1.0),
    )?;
    finish(ff, &part, &out)
}

/// Caption pictures for an export: drawn for `out_size` (the export's picture), written as raw
/// RGBA into `dir`, placed in the export's timeline.
pub fn render_burns(
    caps: &[kairo::captions::Caption],
    range: (f64, f64),
    speed: f64,
    out_size: (u32, u32),
    dir: &Path,
) -> Result<Vec<media::Burn>, String> {
    let font = include_bytes!("../assets/fonts/Inter-Bold.ttf");
    let (w, h) = out_size;
    let size = (w.min(h) as f32 * 0.058).max(14.0);
    let mut out = Vec::new();
    for (i, c) in caps.iter().enumerate() {
        let Some((start, end)) = media::burn_window(c.start, c.end, range, speed) else {
            continue;
        };
        let Some(p) = kairo::captions::render(&c.text, w * 88 / 100, size, font) else {
            continue;
        };
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{i}.rgba"));
        std::fs::write(&path, &p.rgba).map_err(|e| e.to_string())?;
        out.push(media::Burn {
            path,
            width: p.width,
            height: p.height,
            start,
            end,
            at: None,
            fps: None,
        });
    }
    Ok(out)
}

/// Frames per second of text coming in and going out.
const STAMP_FPS: u32 = 60;

/// Text for an export: each item drawn for `out_size`, written as raw RGBA into `dir`
/// and placed in the export's timeline, with its animation in and out (when it starts and ends
/// inside the export) as frames of their own.
pub fn render_stamps(
    items: &[kairo::overlays::Item],
    range: (f64, f64),
    speed: f64,
    out_size: (u32, u32),
    dir: &Path,
) -> Result<Vec<media::Burn>, String> {
    let font = include_bytes!("../assets/fonts/Inter-Bold.ttf");
    let (w, h) = out_size;
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let Some((a, b)) = media::burn_window(item.start, item.end, range, speed) else {
            continue;
        };
        let Some(pic) = item.picture(h, w * 9 / 10, font) else {
            continue;
        };
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let centre = (item.x * w as f32, item.y * h as f32);
        let (inn, outro) = item.anim_lengths(b - a);
        let mut still = (a, b);
        let parts = [
            (false, inn, item.start >= range.0 - 1e-6, a),
            (true, outro, item.end <= range.1 + 1e-6, b - outro),
        ];
        for (leaving, len, whole, at) in parts {
            if len <= 0.0 || !whole {
                continue;
            }
            let Some(m) = kairo::overlays::animate(
                &pic, &item.anim, leaving, centre, out_size, len, STAMP_FPS,
            ) else {
                continue;
            };
            let path = dir.join(format!(
                "stamp-{i}-{}.rgba",
                if leaving { "out" } else { "in" }
            ));
            std::fs::write(&path, &m.rgba).map_err(|e| e.to_string())?;
            out.push(media::Burn {
                path,
                width: m.width,
                height: m.height,
                start: at,
                // The last frame ends where the still picture takes over.
                end: at + len - 0.5 / STAMP_FPS as f64,
                at: Some((m.x, m.y)),
                fps: Some(STAMP_FPS),
            });
            if leaving {
                still.1 = at;
            } else {
                still.0 = at + len;
            }
        }
        if still.1 > still.0 {
            let path = dir.join(format!("stamp-{i}.rgba"));
            std::fs::write(&path, &pic.rgba).map_err(|e| e.to_string())?;
            out.push(media::Burn {
                path,
                width: pic.width,
                height: pic.height,
                start: still.0,
                end: still.1,
                at: Some((
                    (centre.0 - pic.width as f32 / 2.0).round() as i32,
                    (centre.1 - pic.height as f32 / 2.0).round() as i32,
                )),
                fps: None,
            });
        }
    }
    Ok(out)
}

/// Captions being made for a clip.
pub struct CaptionJob {
    pub status: Arc<Mutex<CaptionStatus>>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct CaptionStatus {
    pub stage: String,
    pub progress: f32,
    pub result: Option<Result<Vec<kairo::captions::Caption>, String>>,
}

impl CaptionJob {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn finished(&self) -> bool {
        self.status.lock().unwrap().result.is_some()
    }
}

/// Listens to sound track `track` of `clip` and writes down what's said, on a worker thread.
/// Downloads the speech files first if voice clipping never has.
pub fn start_captions(ff: PathBuf, clip: PathBuf, track: usize) -> CaptionJob {
    let status = Arc::new(Mutex::new(CaptionStatus::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let (status, cancel) = (status.clone(), cancel.clone());
        std::thread::spawn(move || {
            let result = captions(&ff, &clip, track, &status, &cancel);
            if let Err(e) = &result {
                warning!("captions failed: {e}");
            }
            status.lock().unwrap().result = Some(result);
        });
    }
    CaptionJob { status, cancel }
}

fn captions(
    ff: &Path,
    clip: &Path,
    track: usize,
    status: &Mutex<CaptionStatus>,
    cancel: &AtomicBool,
) -> Result<Vec<kairo::captions::Caption>, String> {
    let set = |stage: &str, p: f32| {
        let mut s = status.lock().unwrap();
        s.stage = stage.to_string();
        s.progress = p;
    };
    if !crate::voice::installed() {
        set("Downloading the speech files", 0.0);
        crate::voice::install(&|done, total| {
            set(
                "Downloading the speech files",
                done as f32 / total.max(1) as f32,
            )
        })?;
    }
    set("Listening", 0.0);
    let out = ffmpeg::command(ff)
        .args(media::speech_args(clip, track))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    let samples: Vec<i16> = out
        .stdout
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    if samples.is_empty() {
        return Err("This clip has no sound to caption.".into());
    }
    let words = crate::voice::transcribe(&samples, &|p| set("Listening", p), cancel)?;
    info!("captions: {} words from {}", words.len(), clip.display());
    Ok(kairo::captions::lines(&words))
}

/// Longest stretch a GIF export takes.
pub const GIF_MAX_SECONDS: f64 = 15.0;

/// Moves the finished file into place and makes its gallery picture.
fn finish(ff: &Path, part: &Path, out: &Path) -> Result<PathBuf, String> {
    std::fs::rename(part, out).map_err(|e| e.to_string())?;
    let thumb = crate::save::thumb_for(out);
    let _ = std::fs::create_dir_all(crate::save::thumbs_dir());
    let _ = ffmpeg::run(
        ff,
        &args::thumbnail_args(out, &thumb),
        Duration::from_secs(20),
    );
    info!("exported {}", out.display());
    Ok(out.to_path_buf())
}

/// Runs one ffmpeg encode, turning its `-progress` lines into `status.progress`.
fn encode(
    ff: &Path,
    a: &[String],
    length: f64,
    part: &Path,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
    span: (f32, f32),
) -> Result<(), String> {
    encode_with(ff, a, length, part, cancel, &|f| {
        status.lock().unwrap().progress = span.0 + (span.1 - span.0) * f;
    })
}

/// Runs one ffmpeg encode, reporting how far along it is (0 to 1) from its `-progress` lines.
pub fn encode_with(
    ff: &Path,
    a: &[String],
    length: f64,
    part: &Path,
    cancel: &AtomicBool,
    progress: &dyn Fn(f32),
) -> Result<(), String> {
    let mut child = ffmpeg::command(ff)
        .args(a)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    let err = child.stderr.take().expect("piped");
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut BufReader::new(err), &mut s);
        s
    });
    let out = BufReader::new(child.stdout.take().expect("piped"));
    for line in out.lines() {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(part);
            return Err("Cancelled".into());
        }
        let Ok(line) = line else { break };
        if let Some(t) = media::progress_secs(&line) {
            progress((t / length.max(0.01)).clamp(0.0, 1.0) as f32);
        }
    }
    let st = child.wait().map_err(|e| e.to_string())?;
    let text = errors.join().unwrap_or_default();
    if cancel.load(Ordering::SeqCst) {
        let _ = std::fs::remove_file(part);
        return Err("Cancelled".into());
    }
    if !st.success() {
        let _ = std::fs::remove_file(part);
        return Err(ffmpeg::last_lines(&text, 3).unwrap_or_else(|| format!("ffmpeg failed ({st})")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Burns animated text into a copy of a made-up clip, and keeps a few frames
    /// to look at: `KAIRO_SNAPSHOTS=<dir> cargo test -- --ignored export::tests::stamps`.
    /// Needs ffmpeg.
    #[test]
    #[ignore]
    fn stamps_burn_into_a_copy() {
        let dir = PathBuf::from(std::env::var_os("KAIRO_SNAPSHOTS").expect("dir"));
        let ff = ffmpeg::path().expect("ffmpeg");
        let clip = dir.join("stamps-source.mp4");
        let make = [
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=60",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330",
            "-t",
            "4",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ]
        .map(String::from)
        .into_iter()
        .chain([clip.display().to_string()])
        .collect::<Vec<_>>();
        ffmpeg::run(&ff, &make, Duration::from_secs(60)).expect("source clip");
        let info = ffmpeg::probe_clip(&ff, &clip).expect("probe");
        let mut words = kairo::overlays::Item::text("What a clutch", 0.5, 3.5);
        words.color = 1;
        let mut star = kairo::overlays::Item::text("GG", 1.0, 3.5);
        star.anim = "slide".into();
        (star.x, star.y, star.size) = (0.8, 0.3, 0.14);
        let burns_dir = dir.join("stamps-work");
        let mut eff = Effects {
            stamps: vec![words, star],
            ..Default::default()
        };
        eff.burns =
            render_stamps(&eff.stamps, (0.0, 4.0), 1.0, (1280, 720), &burns_dir).expect("stamps");
        assert_eq!(eff.burns.len(), 6, "in, still and out for each");
        let out = dir.join("stamps-out.mp4");
        let a = media::trim_args(&clip, 0.0, 4.0, &info, &eff, Encoder::X264, &out);
        ffmpeg::run(&ff, &a, Duration::from_secs(120)).expect("export");
        for t in ["0.55", "1.08", "2.00", "3.45"] {
            let shot = dir.join(format!("stamps-{t}.jpg"));
            let grab = ["-y", "-ss", t, "-i"]
                .map(String::from)
                .into_iter()
                .chain([out.display().to_string()])
                .chain(["-frames:v", "1", "-vf", "scale=640:-2"].map(String::from))
                .chain([shot.display().to_string()])
                .collect::<Vec<_>>();
            ffmpeg::run(&ff, &grab, Duration::from_secs(30)).expect("frame");
        }
        let _ = std::fs::remove_dir_all(&burns_dir);
    }

    /// Cuts a part out of a made-up clip, as a plain copy and with text, and checks the copies
    /// are as long as what was kept: `cargo test -- --ignored export::tests::cut`. Needs ffmpeg.
    #[test]
    #[ignore]
    fn cut_parts_close_up() {
        let dir = std::env::temp_dir().join(format!("kairo-cut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let ff = ffmpeg::path().expect("ffmpeg");
        let clip = dir.join("source.mp4");
        let make = [
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330",
            "-t",
            "6",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ]
        .map(String::from)
        .into_iter()
        .chain([clip.display().to_string()])
        .collect::<Vec<_>>();
        ffmpeg::run(&ff, &make, Duration::from_secs(60)).expect("source clip");
        let info = ffmpeg::probe_clip(&ff, &clip).expect("probe");
        let cuts = [(1.5, 3.0)];
        let encoders = Mutex::new(Some(vec![Encoder::X264]));
        for with_text in [false, true] {
            let mut eff = Effects::default();
            if with_text {
                eff.stamps = vec![kairo::overlays::Item::text("After the cut", 3.2, 5.0)];
            }
            let named = dir.join(format!("clip-{with_text}.mp4"));
            let job = Export {
                kind: Kind::Trim,
                ff: &ff,
                clip: &clip,
                named: &named,
                info: &info,
                range: (0.0, 6.0),
                caps: &[],
                keys: &[],
                burns_dir: dir.join(format!("work-{with_text}")),
                progress: (0.0, 1.0),
            };
            let status = Mutex::new(Status::default());
            let out = run_cut(
                &job,
                &cuts,
                &encoders,
                &eff,
                &status,
                &AtomicBool::new(false),
            )
            .expect("export");
            let got = ffmpeg::probe_clip(&ff, &out).expect("probe copy");
            assert!(
                (got.duration - 4.5).abs() < 0.2,
                "{with_text}: {}",
                got.duration
            );
            assert!(!got.audio_tracks.is_empty(), "the sound is kept");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
