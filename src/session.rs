//! Recording a whole session: from when it starts until it's stopped, the encoder's stream and
//! both sound tracks go to files as they arrive (so a long session costs no extra memory), then
//! they're joined into one clip in the Sessions folder, with saves and marks as bookmarks.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rewindr::{args, library};

use crate::ffmpeg;
use crate::log::{info, warning};
use crate::recorder::{Shared, now_us};

/// A session being recorded.
pub struct Session {
    pub start_us: i64,
    /// Where its files grow until it's joined.
    work: PathBuf,
    title: Option<String>,
}

/// Sessions are split at this length (a 16-bit WAV can't hold much more of stereo sound).
const MAX_SESSION_US: i64 = 5 * 3600 * 1_000_000;

/// The folder sessions are saved in.
pub fn dir(settings: &rewindr::config::Settings) -> PathBuf {
    settings.save_dir().join("Sessions")
}

/// Starts recording a session, if one isn't running. Needs the recorder running.
pub fn start(shared: &Arc<Shared>, title: Option<String>) -> Result<(), String> {
    let mut slot = shared.session.lock().unwrap();
    if slot.is_some() {
        return Ok(());
    }
    let work = rewindr::config::config_dir().join("sessions").join(format!(
        "{}-{}",
        std::process::id(),
        now_us()
    ));
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let start_us = match shared
        .ring
        .lock()
        .unwrap()
        .start_tape(&work.join("video.ts"))
    {
        Ok(t) => t,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&work);
            return Err(e);
        }
    };
    for (name, ring) in [
        ("Game", &shared.desktop),
        ("Microphone", &shared.mic),
        ("Voice chat", &shared.chat),
    ] {
        if let Some(r) = ring.lock().unwrap().as_mut()
            && let Err(e) = r.start_tape(&work.join(format!("{name}.wav")), start_us)
        {
            warning!("session sound ({name}) won't be kept: {e}");
        }
    }
    info!("session recording started");
    *slot = Some(Session {
        start_us,
        work,
        title,
    });
    Ok(())
}

/// Stops the session, if one runs, and joins its files into a clip on a worker thread. The
/// clip's path comes back through `done` (or why it failed).
pub fn stop(
    shared: &Arc<Shared>,
    done: impl FnOnce(Result<(PathBuf, f64), String>) + Send + 'static,
) {
    let Some(s) = shared.session.lock().unwrap().take() else {
        return;
    };
    let (video, hevc) = {
        let mut ring = shared.ring.lock().unwrap();
        (
            ring.take_tape(),
            ring.codec() == Some(rewindr::ts::Codec::Hevc),
        )
    };
    let mut sounds = Vec::new();
    for (name, ring) in [
        ("Game", &shared.desktop),
        ("Microphone", &shared.mic),
        ("Voice chat", &shared.chat),
    ] {
        if let Some(t) = ring.lock().unwrap().as_mut().and_then(|r| r.take_tape()) {
            sounds.push((t, name));
        }
    }
    let end_us = now_us();
    let marks: Vec<i64> = shared.marks.lock().unwrap().iter().copied().collect();
    let settings = shared.settings();
    std::thread::spawn(move || {
        let result = (|| {
            let video = video
                .ok_or("the recording stopped before the session had any video")?
                .finish()
                .map_err(|e| e.to_string())?;
            let mut audio: Vec<(PathBuf, &str)> = Vec::new();
            for (t, name) in sounds {
                match t.finish() {
                    Ok(p) => audio.push((p, name)),
                    Err(e) => warning!("session sound ({name}): {e}"),
                }
            }
            let out_dir = dir(&settings);
            std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
            let label = match &s.title {
                Some(t) => format!("{t} session"),
                None => "Session".to_string(),
            };
            let out = crate::save::clip_path(&out_dir, Some(&label));
            let part = out.with_extension("mp4.part");
            let refs: Vec<(&std::path::Path, &str)> =
                audio.iter().map(|(p, n)| (p.as_path(), *n)).collect();
            let ff = ffmpeg::path()?;
            // Joining copies the video: minutes, not hours, even for long sessions.
            let seconds = (end_us - s.start_us) as f64 / 1e6;
            let limit = Duration::from_secs(120 + seconds as u64 / 10);
            ffmpeg::run(&ff, &args::save_args(&video, &refs, hevc, &part), limit)
                .map_err(|e| format!("could not join the session: {e}"))?;
            std::fs::rename(&part, &out).map_err(|e| e.to_string())?;
            let thumb = crate::save::thumb_for(&out);
            let _ = std::fs::create_dir_all(crate::save::thumbs_dir());
            let _ = ffmpeg::run(
                &ff,
                &args::thumbnail_args(&out, &thumb),
                Duration::from_secs(30),
            );
            let bookmarks = library::marks_within(&marks, s.start_us, end_us);
            if !bookmarks.is_empty() {
                let mut lib = library::Library::load();
                lib.set_markers(&out, bookmarks);
                let _ = lib.save();
            }
            info!("session saved: {} ({seconds:.0} s)", out.display());
            Ok((out, seconds))
        })();
        let _ = std::fs::remove_dir_all(&s.work);
        done(result);
    });
}

/// Keeps sessions going while one is wanted: starts one when recording runs, joins one whose
/// stream ended (new settings, a pause) or that got too long, then starts the next. Called
/// from the background process's tick.
pub fn tend(shared: &Arc<Shared>, title: impl Fn() -> Option<String>) -> bool {
    let wanted = shared.session_wanted.load(Ordering::SeqCst);
    let (ended, running_since) = {
        let ring = shared.ring.lock().unwrap();
        (ring.tape_ended(), ring.tape_progress().map(|(s, _)| s))
    };
    let too_long = running_since.is_some_and(|s| now_us() - s > MAX_SESSION_US);
    let active = shared.session.lock().unwrap().is_some();
    if active && (ended || too_long || !wanted) {
        return true;
    }
    if wanted && !active && shared.status.lock().unwrap().recording {
        let _ = start(shared, title());
    }
    false
}
