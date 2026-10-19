//! Exports from the editor: a trimmed copy, or a copy small enough for Discord. Each runs ffmpeg
//! on a worker thread and reports progress.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rewindr::args::{self, Encoder};
use rewindr::library;
use rewindr::media::{self, Info};

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

pub fn start(
    kind: Kind,
    ff: PathBuf,
    clip: PathBuf,
    info: Info,
    range: (f64, f64),
    encoders: Arc<Mutex<Option<Vec<Encoder>>>>,
) -> Job {
    let status = Arc::new(Mutex::new(Status {
        stage: match kind {
            Kind::Trim => "Saving the trimmed copy".into(),
            Kind::Share(_) => "Making a copy for Discord".into(),
            Kind::Gif => "Making a GIF".into(),
            Kind::Vertical => "Making a vertical copy".into(),
            Kind::Slow(f) => format!("Slowing it down {f}x"),
        },
        ..Default::default()
    }));
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let status = status.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let result = run(kind, &ff, &clip, &info, range, &encoders, &status, &cancel);
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

#[allow(clippy::too_many_arguments)]
fn run(
    kind: Kind,
    ff: &Path,
    clip: &Path,
    info: &Info,
    (start, end): (f64, f64),
    encoders: &Mutex<Option<Vec<Encoder>>>,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    let encoder = best_encoder(ff, encoders);
    let length = end - start;
    match kind {
        Kind::Trim => {
            let out = library::sibling(clip, "trim");
            let part = out.with_extension("mp4.part");
            let a = media::trim_args(clip, start, end, info, encoder, &part);
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
                let a = media::share_args(clip, start, end, &plan, encoder, &part);
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
            let a = media::gif_args(clip, start, end, 480, 15, &part);
            encode(ff, &a, length, &part, status, cancel, (0.0, 1.0))?;
            std::fs::rename(&part, &out).map_err(|e| e.to_string())?;
            info!("exported {}", out.display());
            Ok(out)
        }
        Kind::Vertical => {
            let out = library::sibling(clip, "vertical");
            let part = out.with_extension("mp4.part");
            let a = media::vertical_args(clip, start, end, info, encoder, &part);
            encode(ff, &a, length, &part, status, cancel, (0.0, 1.0))?;
            finish(ff, &part, &out)
        }
        Kind::Slow(factor) => {
            let out = library::sibling(clip, &format!("slow {factor}x"));
            let part = out.with_extension("mp4.part");
            let a = media::slow_args(clip, start, end, factor, info, encoder, &part);
            encode(
                ff,
                &a,
                length * factor as f64,
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
