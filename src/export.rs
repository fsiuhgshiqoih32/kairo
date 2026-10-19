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
                info: &info,
                range,
                caps: &caps,
                keys: &keys,
                burns_dir: kairo::config::config_dir().join("captions").join(format!(
                    "{}-{}",
                    std::process::id(),
                    crate::recorder::now_us()
                )),
            };
            let result = run(&job, &encoders, &eff, &status, &cancel);
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
    clip: &'a Path,
    info: &'a Info,
    range: (f64, f64),
    caps: &'a [kairo::captions::Caption],
    /// Keys and clicks to overlay (empty for none).
    keys: &'a [kairo::keys::KeyEvent],
    /// Where caption pictures and overlay frames go while it runs.
    burns_dir: PathBuf,
}

impl Export<'_> {
    /// `eff` with the captions and the keys overlay drawn for a picture of `size`.
    fn with_captions(&self, eff: &Effects, size: (u32, u32)) -> Result<Effects, String> {
        let mut eff = eff.clone();
        if !self.caps.is_empty() {
            eff.burns = render_burns(self.caps, self.range, eff.speed, size, &self.burns_dir)?;
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
            let out = library::sibling(clip, what);
            let part = out.with_extension("mp4.part");
            let a = media::trim_args(clip, start, end, info, &eff, encoder, &part);
            encode(ff, &a, length, &part, status, cancel, (0.0, 1.0))?;
            finish(ff, &part, &out)
        }
        Kind::Share(limit) => {
            let out = library::sibling(clip, "Discord");
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
                encode(ff, &a, length, &part, status, cancel, (0.0, 1.0))?;
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
            let out = library::sibling_with(clip, "GIF", "gif");
            let part = out.with_extension("gif.part");
            let a = media::gif_args(clip, start, end, info, eff, 480, 15, &part);
            encode(ff, &a, length, &part, status, cancel, (0.0, 1.0))?;
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
            let out = library::sibling(clip, what);
            let part = out.with_extension("mp4.part");
            let a = media::framed_args(clip, start, end, info, &eff, frame, encoder, &part);
            encode(ff, &a, length, &part, status, cancel, (0.0, 1.0))?;
            finish(ff, &part, &out)
        }
        Kind::Slow(factor) => {
            let out = library::sibling(clip, &format!("slow {factor}x"));
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
                (0.0, 1.0),
            )?;
            finish(ff, &part, &out)
        }
    }
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
        });
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
