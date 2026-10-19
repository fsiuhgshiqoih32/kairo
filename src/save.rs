//! Saving a clip: the last N seconds of video from the buffer, the matching stretch of sound,
//! joined into an MP4 by ffmpeg (video copied, not re-encoded, so it takes about a second).

use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use kairo::args;
use kairo::{library, pcm};

use crate::ffmpeg;
use crate::log::{info, warning};
use crate::recorder::{Shared, now_us};

/// Shorter than this, saving reports "nothing yet" instead of writing a clip.
const MIN_CLIP_US: i64 = 1_500_000;

pub struct Clip {
    pub path: PathBuf,
    pub seconds: f64,
    pub thumbnail: Option<PathBuf>,
}

/// Where clips' small preview pictures live.
pub fn thumbs_dir() -> PathBuf {
    kairo::config::config_dir().join("thumbs")
}

pub fn thumb_for(clip: &Path) -> PathBuf {
    let name = clip
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    thumbs_dir().join(format!("{name}.jpg"))
}

/// Where a clip's keys and clicks are kept (for the overlay).
pub fn keys_for(clip: &Path) -> PathBuf {
    let name = clip
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    thumbs_dir().join(format!("{name}.keys.json"))
}

/// The files that belong with a clip: its thumbnail, banner picture, keys and captions.
pub fn sidecars(clip: &Path) -> Vec<PathBuf> {
    let mut v = vec![thumb_for(clip), keys_for(clip), clip.with_extension("srt")];
    if let Some(stem) = clip.file_stem() {
        v.push(thumbs_dir().join(format!("hero-{}.jpg", stem.to_string_lossy())));
    }
    v
}

/// Moves a clip's companion files along with it.
pub fn move_sidecars(from: &Path, to: &Path) {
    for (a, b) in sidecars(from).into_iter().zip(sidecars(to)) {
        if a.exists() {
            let _ = std::fs::rename(a, b);
        }
    }
}

/// Removes a clip's companion files.
pub fn remove_sidecars(clip: &Path) {
    for p in sidecars(clip) {
        let _ = std::fs::remove_file(p);
    }
}

/// Writes the overlay events for a clip, if there are any.
pub fn write_keys(clip: &Path, events: &[kairo::keys::KeyEvent]) {
    if events.is_empty() {
        return;
    }
    let _ = std::fs::create_dir_all(thumbs_dir());
    if let Ok(text) = serde_json::to_string(events) {
        let _ = std::fs::write(keys_for(clip), text);
    }
}

/// A clip's overlay events, if it has any.
pub fn read_keys(clip: &Path) -> Vec<kairo::keys::KeyEvent> {
    std::fs::read_to_string(keys_for(clip))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Moves clips past the "keep clips" age to Recently deleted, and deletes for good what's
/// waited there for 30 days.
pub fn tidy_folder(settings: &kairo::config::Settings) {
    let dir = settings.save_dir();
    let now = crate::share::now_secs();
    let mut lib = library::Library::load();
    for p in library::expired_clips(&dir, settings.keep_days, &lib, now) {
        match library::trash(&dir, &p, now) {
            Ok(_) => info!(
                "moved {} to Recently deleted (older than {} days)",
                p.display(),
                settings.keep_days
            ),
            Err(e) => warning!("couldn't move {} to Recently deleted: {e}", p.display()),
        }
    }
    let gone = library::purge_expired(&dir, now);
    for t in &gone {
        remove_sidecars(&t.from);
        lib.forget(&t.from);
        info!("deleted {} for good after 30 days", t.from.display());
    }
    if !gone.is_empty()
        && let Err(e) = lib.save()
    {
        warning!("{e}");
    }
}

/// `<title> 2026-10-18 14-30-05.mp4` in `dir`, not overwriting anything.
pub fn clip_path(dir: &Path, title: Option<&str>) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S");
    let base = match title {
        Some(t) => format!("{t} {stamp}"),
        None => format!("Kairo {stamp}"),
    };
    let mut p = dir.join(format!("{base}.mp4"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{base} ({n}).mp4"));
        n += 1;
    }
    p
}

/// `<title> 2026-10-18 14-30-05.jpg` in `dir`, not overwriting anything.
pub fn shot_path(dir: &Path, title: Option<&str>) -> PathBuf {
    clip_path(dir, title).with_extension("jpg")
}

/// Saves the newest frame in the replay buffer as a full-size JPEG in the screenshots folder.
pub fn screenshot(shared: &Shared, title: Option<&str>) -> Result<PathBuf, String> {
    let settings = shared.settings();
    if shared.paused.load(Ordering::SeqCst) {
        return Err("Recording is paused. Resume it to take screenshots.".into());
    }
    let ffmpeg = ffmpeg::path()?;
    // The newest keyframe and everything after it.
    let snap = shared
        .ring
        .lock()
        .unwrap()
        .snapshot(now_us())
        .ok_or("Nothing to capture: Kairo isn't recording.")?;
    let work = kairo::config::config_dir().join("saving").join(format!(
        "{}-shot-{}",
        std::process::id(),
        now_us()
    ));
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let cleanup = Cleanup(work.clone());
    let video = work.join("video.ts");
    let f = std::fs::File::create(&video).map_err(|e| e.to_string())?;
    snap.write_to(BufWriter::new(f))
        .map_err(|e| format!("could not write the video: {e}"))?;
    drop(snap);
    // Count the frames without decoding, then decode only up to the last one.
    let out = ffmpeg::command(&ffmpeg)
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(&video)
        .args(["-map", "0:v:0", "-c", "copy", "-f", "null", "-"])
        .output()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    let frames = frame_count(&String::from_utf8_lossy(&out.stderr))
        .unwrap_or(1)
        .max(1);
    let dir = settings.screenshot_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = shot_path(&dir, title);
    let tmp = work.join("shot.jpg");
    let shot_args: Vec<String> = ["-y", "-hide_banner", "-nostdin", "-loglevel", "error", "-i"]
        .iter()
        .map(|s| s.to_string())
        .chain([
            video.display().to_string(),
            "-vf".into(),
            format!("select=eq(n\\,{})", frames - 1),
            "-frames:v".into(),
            "1".into(),
            "-q:v".into(),
            "2".into(),
            "-update".into(),
            "1".into(),
            tmp.display().to_string(),
        ])
        .collect();
    ffmpeg::run(&ffmpeg, &shot_args, Duration::from_secs(30))
        .map_err(|e| format!("could not save the screenshot: {e}"))?;
    std::fs::copy(&tmp, &path).map_err(|e| format!("could not save the screenshot: {e}"))?;
    drop(cleanup);
    info!("screenshot {}", path.display());
    shared.status.lock().unwrap().last_screenshot = Some(path.display().to_string());
    Ok(path)
}

/// The frame count from the last progress line of an ffmpeg run ("frame=  120 fps=...").
fn frame_count(stderr: &str) -> Option<u64> {
    stderr
        .rsplit("frame=")
        .next()
        .filter(|_| stderr.contains("frame="))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Saves the last `seconds` (the settings' clip length if `None`). `title` names the file (the
/// game or app in front).
pub fn save(shared: &Shared, seconds: Option<u32>, title: Option<&str>) -> Result<Clip, String> {
    if shared.saving.swap(true, Ordering::SeqCst) {
        return Err("Already saving a clip.".into());
    }
    let result = save_inner(shared, seconds, title);
    shared.saving.store(false, Ordering::SeqCst);
    result
}

/// Saves while the caller holds `shared.saving` (set it first; this doesn't clear it).
pub fn save_inner(
    shared: &Shared,
    seconds: Option<u32>,
    title: Option<&str>,
) -> Result<Clip, String> {
    let settings = shared.settings();
    if shared.paused.load(Ordering::SeqCst) {
        return Err("Recording is paused. Resume it from the tray icon to save clips.".into());
    }
    let ffmpeg = ffmpeg::path()?;
    let secs = seconds.unwrap_or(settings.clip_seconds) as i64;
    let now = now_us();
    let snap = shared
        .ring
        .lock()
        .unwrap()
        .snapshot(now - secs * 1_000_000)
        .ok_or_else(|| {
            if shared.status.lock().unwrap().recording {
                "Nothing to save yet: recording only just started.".to_string()
            } else {
                "Nothing to save: Kairo isn't recording.".to_string()
            }
        })?;
    // A clip of a split second is a recording that just (re)started, not a moment worth keeping.
    if snap.end_us() - snap.start_us() < MIN_CLIP_US {
        return Err("Nothing to save yet: recording only just started.".into());
    }
    let hevc = shared.ring.lock().unwrap().codec() == Some(kairo::ts::Codec::Hevc);
    let fps = settings.fps.max(1) as i64;
    let start = snap.start_us();
    let end = snap.end_us() + 1_000_000 / fps;

    let work = kairo::config::config_dir()
        .join("saving")
        .join(format!("{}-{now}", std::process::id()));
    std::fs::create_dir_all(&work)
        .map_err(|e| format!("could not create {}: {e}", work.display()))?;
    let cleanup = Cleanup(work.clone());

    let video = work.join("video.ts");
    let f = std::fs::File::create(&video).map_err(|e| e.to_string())?;
    snap.write_to(BufWriter::new(f))
        .map_err(|e| format!("could not write the video: {e}"))?;
    drop(snap);

    let mut audio: Vec<(PathBuf, &str)> = Vec::new();
    for (name, ring) in [
        ("Game", &shared.desktop),
        ("Microphone", &shared.mic),
        ("Voice chat", &shared.chat),
    ] {
        let wav = {
            let r = ring.lock().unwrap();
            r.as_ref()
                .map(|r| pcm::wav_f32(r.rate, r.channels, &r.extract(start, end)))
        };
        if let Some(bytes) = wav {
            let p = work.join(format!("{name}.wav"));
            std::fs::write(&p, bytes).map_err(|e| e.to_string())?;
            audio.push((p, name));
        }
    }

    let dir = settings.save_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let out = clip_path(&dir, title);
    let partial = out.with_extension("mp4.part");
    let refs: Vec<(&Path, &str)> = audio.iter().map(|(p, t)| (p.as_path(), *t)).collect();
    // ffmpeg picks the format from the name; write to .part and rename, so a clip appears whole.
    let save_args = args::save_args(&video, &refs, hevc, &partial);
    ffmpeg::run(&ffmpeg, &save_args, Duration::from_secs(120))
        .map_err(|e| format!("could not save the clip: {e}"))?;
    std::fs::rename(&partial, &out).map_err(|e| e.to_string())?;

    let thumb = thumb_for(&out);
    let _ = std::fs::create_dir_all(thumbs_dir());
    let thumbnail = ffmpeg::run(
        &ffmpeg,
        &args::thumbnail_args(&out, &thumb),
        Duration::from_secs(20),
    )
    .ok()
    .map(|_| thumb);
    write_keys(&out, &shared.inputs_within(start, end));
    let marks: Vec<i64> = shared.marks.lock().unwrap().iter().copied().collect();
    let marks = library::marks_within(&marks, start, end);
    if !marks.is_empty() {
        let mut lib = library::Library::load();
        lib.set_markers(&out, marks);
        if let Err(e) = lib.save() {
            warning!("could not keep the clip's marks: {e}");
        }
    }
    let seconds = (end - start) as f64 / 1e6;
    info!("saved {} ({seconds:.1} s)", out.display());
    if settings.max_storage_gb > 0 {
        let limit = settings.max_storage_gb as u64 * 1_000_000_000;
        let mut lib = library::Library::load();
        for p in library::clean_up(&dir, limit, &lib) {
            remove_sidecars(&p);
            lib.forget(&p);
            info!(
                "deleted {} to stay under {} GB",
                p.display(),
                settings.max_storage_gb
            );
        }
        let _ = lib.save();
    }
    drop(cleanup);
    shared.status.lock().unwrap().last_clip = Some(out.display().to_string());
    Ok(Clip {
        path: out,
        seconds,
        thumbnail,
    })
}

/// Removes the working folder however saving ends.
struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn frame_count_reads_the_last_progress_line() {
        let text = "Input #0...\nframe=   12 fps=0.0 q=-1.0 size=N/A\rframe=  119 fps=0.0 q=-1.0 Lsize=N/A time=00:00:00.99\n";
        assert_eq!(super::frame_count(text), Some(119));
        assert_eq!(super::frame_count("no progress"), None);
    }
}
