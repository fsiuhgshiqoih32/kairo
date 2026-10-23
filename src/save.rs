//! Saving a clip: the last N seconds of video from the buffer, the matching stretch of sound,
//! joined into an MP4 by ffmpeg (video copied, not re-encoded, so it takes about a second).

use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Makes the gallery picture for a clip that came from elsewhere, the way saving does; if ffmpeg
/// can't, from `picture` (the other app's own picture of it).
pub fn make_thumb(clip: &Path, picture: Option<&Path>) {
    let thumb = thumb_for(clip);
    let _ = std::fs::create_dir_all(thumbs_dir());
    let made = ffmpeg::path().is_ok_and(|ff| {
        ffmpeg::run(
            &ff,
            &args::thumbnail_args(clip, &thumb),
            Duration::from_secs(20),
        )
        .is_ok()
    });
    let Some(picture) = picture.filter(|_| !made || !thumb.is_file()) else {
        return;
    };
    // As wide as the ones ffmpeg makes.
    let small =
        image::open(picture).map(|p| p.resize(320, 320 * 4, image::imageops::FilterType::Triangle));
    if let Err(e) = small.and_then(|p| p.to_rgb8().save(&thumb)) {
        warning!("couldn't make a picture for {}: {e}", clip.display());
    }
}

/// Where a clip's keys and clicks are kept (for the overlay).
pub fn keys_for(clip: &Path) -> PathBuf {
    let name = clip
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    thumbs_dir().join(format!("{name}.keys.json"))
}

/// Where a clip's text overlays are kept.
pub fn stamps_for(clip: &Path) -> PathBuf {
    keys_for(clip)
        .with_extension("")
        .with_extension("stamps.json")
}

/// The files that belong with a clip: its thumbnail, banner picture, keys, captions and text.
pub fn sidecars(clip: &Path) -> Vec<PathBuf> {
    let mut v = vec![
        thumb_for(clip),
        keys_for(clip),
        clip.with_extension("srt"),
        stamps_for(clip),
    ];
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
    let lib = library::Library::load();
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
        info!("deleted {} for good after 30 days", t.from.display());
    }
    if !gone.is_empty()
        && let Err(e) = library::Library::edit(|lib| {
            for t in &gone {
                lib.forget(&t.from);
            }
        })
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
    // Count the frames without decoding, then decode only up to the last one. With a time
    // limit like every other run: a stuck ffmpeg would hold this thread and its folder for good.
    let count_args: Vec<String> = ["-hide_banner", "-nostdin", "-i"]
        .iter()
        .map(|s| s.to_string())
        .chain([video.display().to_string()])
        .chain(["-map", "0:v:0", "-c", "copy", "-f", "null", "-"].map(String::from))
        .collect();
    let frames = match ffmpeg::run_logged(&ffmpeg, &count_args, Duration::from_secs(20)) {
        Ok(log) => frame_count(&log).unwrap_or(1).max(1),
        Err(e) if e == ffmpeg::TOO_LONG => {
            return Err(format!("could not save the screenshot: {e}"));
        }
        // Counting can complain where decoding the keyframe still works.
        Err(_) => 1,
    };
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
    let _done = SavingFlag(&shared.saving);
    save_inner(shared, seconds, title)
}

/// Clears `shared.saving` however a save ends, a panic included: a flag left set would turn
/// every later save away.
pub struct SavingFlag<'a>(pub &'a AtomicBool);

impl Drop for SavingFlag<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
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

    let mut audio: Vec<(PathBuf, &str)> = Vec::new();
    for (name, ring) in [
        ("Game", &shared.desktop),
        ("Microphone", &shared.mic),
        ("Voice chat", &shared.chat),
    ] {
        // Only the copy happens under the lock: the sound card's thread waits on it.
        let samples = ring
            .lock()
            .unwrap()
            .as_ref()
            .map(|r| (r.rate, r.channels, r.extract(start, end)));
        if let Some((rate, channels, samples)) = samples {
            let p = work.join(format!("{name}.wav"));
            std::fs::write(&p, pcm::wav_f32(rate, channels, &samples))
                .map_err(|e| e.to_string())?;
            audio.push((p, name));
        }
    }

    // The webcam, from its keyframe at or before the clip's start.
    let cam_file = work.join("cam.ts");
    let cam_start = settings
        .webcam
        .then(|| shared.cam.lock().unwrap().snapshot(start))
        .flatten()
        .filter(|c| c.end_us() > start + 500_000)
        .and_then(|c| {
            let f = std::fs::File::create(&cam_file).ok()?;
            c.write_to(BufWriter::new(f)).ok()?;
            Some(c.start_us())
        });

    // A game changed the display mode during the clip: its pictures are fitted to one size.
    let fit = snap.fit_size();

    // The video goes straight from the replay buffer into ffmpeg: through a file first, a
    // save wrote (and the system cached) every byte twice, hundreds of MB at the moment of
    // saving, while a game was running. Not when it's encoded again (the webcam, or sizes to
    // fit): that's slower than the buffer may wait before newer video takes the place of what's
    // being read (and it's tried again as a plain copy if it fails), so the video is copied out
    // first, at disk speed.
    let video_file = work.join("video.ts");
    let (video, feed): (&Path, Option<ffmpeg::Feed>) = if cam_start.is_some() || fit.is_some() {
        let f = std::fs::File::create(&video_file).map_err(|e| e.to_string())?;
        snap.write_to(BufWriter::new(f))
            .map_err(|e| format!("could not write the video: {e}"))?;
        drop(snap);
        (&video_file, None)
    } else {
        let feed: ffmpeg::Feed =
            Box::new(move |w| snap.write_to(BufWriter::with_capacity(1 << 20, w)));
        (Path::new("pipe:0"), Some(feed))
    };

    let dir = settings.save_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let out = clip_path(&dir, title);
    let partial = out.with_extension("mp4.part");
    let refs: Vec<(&Path, &str)> = audio.iter().map(|(p, t)| (p.as_path(), *t)).collect();
    // ffmpeg picks the format from the name; write to .part and rename, so a clip appears whole.
    let save_args = args::save_args(video, &refs, hevc, &partial);
    let reencoded = (cam_start.is_some() || fit.is_some()).then(|| {
        let st = shared.status.lock().unwrap().clone();
        let encoder = st
            .encoder
            .as_deref()
            .and_then(|l| args::Encoder::ALL.into_iter().find(|e| e.label() == l))
            .map(kairo::media::export_encoder)
            .unwrap_or(args::Encoder::X264);
        let (w, h) = fit.or(st.size).unwrap_or((1920, 1080));
        let codec = if hevc {
            kairo::config::VideoCodec::Hevc
        } else {
            kairo::config::VideoCodec::H264
        };
        let bitrate = settings.quality.bitrate(w as u64 * h as u64, settings.fps) as f64
            * codec.bitrate_factor();
        let re = args::Reencode {
            encoder,
            bitrate: bitrate as u64,
            fps: settings.fps,
            cam: cam_start.map(|cs| args::CamOverlay {
                path: &cam_file,
                offset: (cs - start) as f64 / 1e6,
                width: w * settings.webcam_size / 100,
                corner: &settings.webcam_corner,
            }),
            fit,
        };
        args::save_reencoded_args(video, Some(&re), &refs, hevc, &partial)
    });
    if let Some((w, h)) = fit {
        info!("the screen changed size during the clip: fitting it to {w}x{h}");
    }
    let saved = match (reencoded, feed) {
        (_, Some(feed)) => ffmpeg::run_fed(&ffmpeg, &save_args, Duration::from_secs(120), feed),
        // Encoding again can fail where copying can't (a busy encoder): keep the clip anyway.
        (Some(a), None) => ffmpeg::run(&ffmpeg, &a, Duration::from_secs(300)).or_else(|e| {
            warning!("could not encode the clip again (webcam or sizes): {e}");
            ffmpeg::run(&ffmpeg, &save_args, Duration::from_secs(120))
        }),
        (None, None) => ffmpeg::run(&ffmpeg, &save_args, Duration::from_secs(120)),
    };
    saved.map_err(|e| format!("could not save the clip: {e}"))?;
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
    if !marks.is_empty()
        && let Err(e) = library::Library::edit(|lib| lib.set_markers(&out, marks))
    {
        warning!("could not keep the clip's marks: {e}");
    }
    let seconds = (end - start) as f64 / 1e6;
    info!("saved {} ({seconds:.1} s)", out.display());
    if settings.max_storage_gb > 0 {
        let limit = settings.max_storage_gb as u64 * 1_000_000_000;
        let _ = library::Library::edit(|lib| {
            for p in library::clean_up(&dir, limit, lib, &out) {
                remove_sidecars(&p);
                lib.forget(&p);
                info!(
                    "deleted {} to stay under {} GB",
                    p.display(),
                    settings.max_storage_gb
                );
            }
        });
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
