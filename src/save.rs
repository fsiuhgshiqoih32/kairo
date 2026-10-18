//! Saving a clip: the last N seconds of video from the buffer, the matching stretch of sound,
//! joined into an MP4 by ffmpeg (video copied, not re-encoded, so it takes about a second).

use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use rewindr::args;
use rewindr::pcm;

use crate::ffmpeg;
use crate::log::info;
use crate::recorder::{Shared, now_us};

pub struct Clip {
    pub path: PathBuf,
    pub seconds: f64,
    pub thumbnail: Option<PathBuf>,
}

/// Where clips' small preview pictures live.
pub fn thumbs_dir() -> PathBuf {
    rewindr::config::config_dir().join("thumbs")
}

pub fn thumb_for(clip: &Path) -> PathBuf {
    let name = clip
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    thumbs_dir().join(format!("{name}.jpg"))
}

/// `<title> 2026-10-18 14-30-05.mp4` in `dir`, not overwriting anything.
pub fn clip_path(dir: &Path, title: Option<&str>) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S");
    let base = match title {
        Some(t) => format!("{t} {stamp}"),
        None => format!("Rewindr {stamp}"),
    };
    let mut p = dir.join(format!("{base}.mp4"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{base} ({n}).mp4"));
        n += 1;
    }
    p
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

fn save_inner(shared: &Shared, seconds: Option<u32>, title: Option<&str>) -> Result<Clip, String> {
    let settings = shared.settings();
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
                "Nothing to save: Rewindr isn't recording.".to_string()
            }
        })?;
    let fps = settings.fps.max(1) as i64;
    let start = snap.start_us();
    let end = snap.end_us() + 1_000_000 / fps;

    let work = rewindr::config::config_dir()
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

    let mut audio: Vec<PathBuf> = Vec::new();
    for (name, ring) in [("desktop", &shared.desktop), ("mic", &shared.mic)] {
        let wav = {
            let r = ring.lock().unwrap();
            r.as_ref()
                .map(|r| pcm::wav_f32(r.rate, r.channels, &r.extract(start, end)))
        };
        if let Some(bytes) = wav {
            let p = work.join(format!("{name}.wav"));
            std::fs::write(&p, bytes).map_err(|e| e.to_string())?;
            audio.push(p);
        }
    }

    let dir = settings.save_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let out = clip_path(&dir, title);
    let partial = out.with_extension("mp4.part");
    let refs: Vec<&Path> = audio.iter().map(PathBuf::as_path).collect();
    // ffmpeg picks the format from the name; write to .part and rename, so a clip appears whole.
    let mut save_args = args::save_args(&video, &refs, &partial);
    let last = save_args.len() - 1;
    save_args.splice(last..last, ["-f".to_string(), "mp4".to_string()]);
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
    let seconds = (end - start) as f64 / 1e6;
    info!("saved {} ({seconds:.1} s)", out.display());
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
