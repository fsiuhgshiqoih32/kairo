//! Recording a whole session: from when it starts until it's stopped, the encoder's stream and
//! the sound tracks go to files as they arrive (so a long session costs no extra memory), then
//! they're joined into one clip in the Sessions folder, with saves and marks as bookmarks.
//!
//! A session's folder outlives a Kairo that stops mid-session (a crash, being killed, the PC
//! shutting down): the next start joins what it holds.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use kairo::config::Settings;
use kairo::{args, library};

use crate::ffmpeg;
use crate::log::{info, warning};
use crate::recorder::{Shared, Track, now_us};

/// A session being recorded.
pub struct Session {
    pub start_us: i64,
    /// Where its files grow until it's joined.
    work: PathBuf,
    title: Option<String>,
}

/// Sessions are split at this length (a 16-bit WAV can't hold much more of stereo sound).
const MAX_SESSION_US: i64 = 5 * 3600 * 1_000_000;

/// Kept free on the drive sessions grow on: a full system drive stalls Windows itself.
const KEEP_FREE: u64 = 10_000_000_000;

/// Whether writing `bytes` more would leave less than [`KEEP_FREE`]. Unknown free space is no
/// reason to stop.
fn out_of_room(bytes: u64, free: Option<u64>) -> bool {
    free.is_some_and(|f| f < bytes.saturating_add(KEEP_FREE))
}

/// Whether a session of `bytes` so far should end while joining it (a copy of as much) still
/// fits, next to the joins still running (`joining` bytes yet to be copied).
fn should_end(bytes: u64, joining: u64, free: Option<u64>) -> bool {
    out_of_room(bytes.saturating_add(joining), free)
}

/// Whether a new session may start: with room to grow ([`KEEP_FREE`] more) once the joins
/// still running have copied their `joining` bytes. A session stopped for space would
/// otherwise start again straight away, while its own join fills the drive.
fn may_start(joining: u64, free: Option<u64>) -> bool {
    !out_of_room(KEEP_FREE.saturating_add(joining), free)
}

/// Free space for sessions: the smaller of the drive their files grow on and the one they're
/// joined onto (usually the same drive).
fn free_for_sessions(settings: &Settings) -> Option<u64> {
    let work = crate::platform::free_space_for(&work_root());
    let out = crate::platform::free_space_for(&dir(settings));
    match (work, out) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// What a session folder needs to be joined by a later Kairo.
#[derive(Serialize, Deserialize)]
struct Note {
    start_us: i64,
    title: Option<String>,
}

const NOTE: &str = "session.json";
const VIDEO: &str = "video.ts";
/// Left in a folder that couldn't be joined after all, so it isn't tried at every start.
const FAILED: &str = "failed.txt";

/// The folder sessions are saved in.
pub fn dir(settings: &Settings) -> PathBuf {
    settings.save_dir().join("Sessions")
}

/// Where sessions' files grow: local app data, not the roaming settings folder, as they can
/// be many gigabytes.
fn work_root() -> PathBuf {
    kairo::config::local_dir().join("sessions")
}

/// Sessions being joined. Quitting waits for them, or their files would wait for the next start.
static JOINING: AtomicUsize = AtomicUsize::new(0);
/// The video those joins are copying, in bytes: room they're about to take.
static JOINING_BYTES: AtomicU64 = AtomicU64::new(0);

struct Joining(u64);

impl Joining {
    /// A join of a session with `bytes` of video.
    fn new(bytes: u64) -> Joining {
        JOINING.fetch_add(1, Ordering::SeqCst);
        JOINING_BYTES.fetch_add(bytes, Ordering::SeqCst);
        Joining(bytes)
    }
}

impl Drop for Joining {
    fn drop(&mut self) {
        JOINING_BYTES.fetch_sub(self.0, Ordering::SeqCst);
        JOINING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Waits until no session is being joined.
pub fn wait_for_joins() {
    while JOINING.load(Ordering::SeqCst) > 0 {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Starts recording a session, if one isn't running. Needs the recorder running.
pub fn start(shared: &Arc<Shared>, title: Option<String>) -> Result<(), String> {
    let mut slot = shared.session.lock().unwrap();
    if slot.is_some() {
        return Ok(());
    }
    let work = work_root().join(format!("{}-{}", std::process::id(), now_us()));
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let start_us = match shared.ring.lock().unwrap().start_tape(&work.join(VIDEO)) {
        Ok(t) => t,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&work);
            return Err(e);
        }
    };
    let note = Note {
        start_us,
        title: title.clone(),
    };
    if let Err(e) = std::fs::write(
        work.join(NOTE),
        serde_json::to_vec(&note).unwrap_or_default(),
    ) {
        warning!("session note: {e}");
    }
    for t in Track::ALL {
        if let Some(r) = shared.track(t).lock().unwrap().as_mut()
            && let Err(e) = r.start_tape(&wav_path(&work, t), start_us)
        {
            warning!("session sound ({}) won't be kept: {e}", t.name());
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

fn wav_path(work: &Path, t: Track) -> PathBuf {
    work.join(format!("{}.wav", t.name()))
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
            ring.codec() == Some(kairo::ts::Codec::Hevc),
        )
    };
    // Including sound files waiting for their track's next ring (sound was restarting).
    let sounds = shared.take_tapes();
    let end_us = now_us();
    let marks: Vec<i64> = shared.marks.lock().unwrap().iter().copied().collect();
    let inputs = shared.inputs_within(s.start_us, end_us);
    let settings = shared.settings();
    let joining = Joining::new(video.as_ref().map_or(0, |t| t.bytes));
    std::thread::spawn(move || {
        let _joining = joining;
        let result = (|| {
            let video = video
                .ok_or("the recording stopped before the session had any video")?
                .finish()
                .map_err(|e| e.to_string())?;
            let mut audio: Vec<(PathBuf, &str)> = Vec::new();
            for (t, tape) in sounds {
                match tape.finish() {
                    Ok(p) => audio.push((p, t.name())),
                    Err(e) => warning!("session sound ({}): {e}", t.name()),
                }
            }
            let seconds = (end_us - s.start_us) as f64 / 1e6;
            let marks = library::marks_within(&marks, s.start_us, end_us);
            join(
                &settings,
                &video,
                &audio,
                hevc,
                s.title.as_deref(),
                seconds,
                &inputs,
                marks,
            )
        })();
        // A session that failed to join is kept for the next start to try again.
        if result.is_ok() {
            let _ = std::fs::remove_dir_all(&s.work);
        }
        done(result);
    });
}

/// Joins a session's video and sound files into a clip in the Sessions folder.
#[allow(clippy::too_many_arguments)]
fn join(
    settings: &Settings,
    video: &Path,
    audio: &[(PathBuf, &str)],
    hevc: bool,
    title: Option<&str>,
    seconds: f64,
    inputs: &[kairo::keys::KeyEvent],
    marks: Vec<f64>,
) -> Result<(PathBuf, f64), String> {
    let out_dir = dir(settings);
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let label = match title {
        Some(t) => format!("{t} session"),
        None => "Session".to_string(),
    };
    let out = crate::save::clip_path(&out_dir, Some(&label));
    let part = out.with_extension("mp4.part");
    let refs: Vec<(&Path, &str)> = audio.iter().map(|(p, n)| (p.as_path(), *n)).collect();
    let ff = ffmpeg::path()?;
    // Joining copies the video: minutes, not hours, even for long sessions.
    let limit = Duration::from_secs(120 + seconds as u64 / 10);
    if let Err(e) = ffmpeg::run(&ff, &args::save_args(video, &refs, hevc, &part), limit) {
        let _ = std::fs::remove_file(&part);
        return Err(format!("could not join the session: {e}"));
    }
    std::fs::rename(&part, &out).map_err(|e| e.to_string())?;
    let thumb = crate::save::thumb_for(&out);
    let _ = std::fs::create_dir_all(crate::save::thumbs_dir());
    let _ = ffmpeg::run(
        &ff,
        &args::thumbnail_args(&out, &thumb),
        Duration::from_secs(30),
    );
    crate::save::write_keys(&out, inputs);
    if !marks.is_empty()
        && let Err(e) = library::Library::edit(|lib| lib.set_markers(&out, marks))
    {
        warning!("session bookmarks: {e}");
    }
    info!("session saved: {} ({seconds:.0} s)", out.display());
    Ok((out, seconds))
}

/// Session folders left by a Kairo that stopped mid-session, or whose join failed. Only the
/// background process calls this, holding its lock and before it records, so nothing is
/// writing to any of them.
pub fn leftovers() -> Vec<PathBuf> {
    let mut roots = vec![work_root(), kairo::config::config_dir().join("sessions")];
    roots.dedup();
    let mut found: Vec<PathBuf> = roots
        .iter()
        .filter_map(|r| std::fs::read_dir(r).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !p.join(FAILED).exists())
        .collect();
    found.sort();
    found
}

/// Joins a left-over session folder into a clip, ending where its files stopped growing.
/// `Ok(None)` if it held nothing worth keeping (it's removed). Its marks and keys are lost:
/// they lived in the stopped process.
pub fn recover(work: &Path, settings: &Settings) -> Result<Option<(PathBuf, f64)>, String> {
    let video = work.join(VIDEO);
    let Some(meta) = std::fs::metadata(&video)
        .ok()
        .filter(|m| m.len() >= 64 * kairo::ts::PACKET as u64)
    else {
        let _ = std::fs::remove_dir_all(work);
        return Ok(None);
    };
    let _joining = Joining::new(meta.len());
    let note: Option<Note> = std::fs::read(work.join(NOTE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let start_us = note
        .as_ref()
        .map(|n| n.start_us)
        .or_else(|| start_from_name(work))
        .unwrap_or_else(|| unix_us(std::fs::metadata(work).and_then(|m| m.created()).ok()));
    let end_us = unix_us(meta.modified().ok());
    let seconds = ((end_us - start_us) as f64 / 1e6).max(0.0);
    let mut head = Vec::new();
    let _ = std::fs::File::open(&video).and_then(|f| f.take(1 << 20).read_to_end(&mut head));
    let hevc = kairo::ts::codec_of(&head) == Some(kairo::ts::Codec::Hevc);
    let audio: Vec<(PathBuf, &str)> = Track::ALL
        .into_iter()
        .map(|t| (wav_path(work, t), t.name()))
        .filter(|(p, _)| std::fs::metadata(p).is_ok_and(|m| m.len() > 44))
        .collect();
    let title = note.and_then(|n| n.title);
    match join(
        settings,
        &video,
        &audio,
        hevc,
        title.as_deref(),
        seconds,
        &[],
        Vec::new(),
    ) {
        Ok(r) => {
            let _ = std::fs::remove_dir_all(work);
            Ok(Some(r))
        }
        Err(e) => {
            let _ = std::fs::write(work.join(FAILED), &e);
            Err(format!("{e} (its files are kept in {})", work.display()))
        }
    }
}

/// When a folder named `{pid}-{start µs}` (as sessions are) started.
fn start_from_name(work: &Path) -> Option<i64> {
    let name = work.file_name()?.to_str()?;
    name.split_once('-')?.1.parse().ok()
}

fn unix_us(t: Option<SystemTime>) -> i64 {
    t.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or_else(now_us, |d| d.as_micros() as i64)
}

/// Removes half-written clips (`.part` files) a stopped Kairo left in the clips folder, its
/// game folders and the Sessions folder. One still growing (an export running) is left alone.
pub fn sweep_parts(settings: &Settings) {
    let root = settings.save_dir();
    let mut dirs = vec![root.clone()];
    if let Ok(entries) = std::fs::read_dir(&root) {
        dirs.extend(entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    let stale = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(600))
    };
    for d in dirs {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for p in entries.flatten().map(|e| e.path()) {
            if p.extension().is_some_and(|x| x == "part") && p.is_file() && stale(&p) {
                info!("removing a half-written clip: {}", p.display());
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

/// Keeps sessions going while one is wanted: starts one when recording runs, joins one whose
/// stream ended (new settings, a pause) or that got too long, then starts the next. Called
/// from the background process's tick.
pub fn tend(shared: &Arc<Shared>, title: impl Fn() -> Option<String>) -> bool {
    let wanted = shared.session_wanted.load(Ordering::SeqCst);
    let (ended, tape) = {
        let ring = shared.ring.lock().unwrap();
        (ring.tape_ended(), ring.tape_progress())
    };
    let too_long = tape.is_some_and(|(s, _)| now_us() - s > MAX_SESSION_US);
    let free = (wanted || tape.is_some())
        .then(|| free_for_sessions(&shared.settings()))
        .flatten();
    let joining = JOINING_BYTES.load(Ordering::SeqCst);
    let full = tape.is_some_and(|(_, bytes)| should_end(bytes, joining, free));
    let active = shared.session.lock().unwrap().is_some();
    if active && (ended || too_long || !wanted || full) {
        if full {
            warning!(
                "the drive is nearly full: saving the session; the next starts once there's room"
            );
        }
        return true;
    }
    if wanted && !active && may_start(joining, free) && shared.status.lock().unwrap().recording {
        let _ = start(shared, title());
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_in(dir: &Path) -> Settings {
        Settings {
            save_dir: dir.display().to_string(),
            ..Settings::default()
        }
    }

    #[test]
    fn a_folder_with_no_video_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("123-1700000000000000");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("Game.wav"), [0u8; 100]).unwrap();
        let s = settings_in(tmp.path());
        assert_eq!(recover(&work, &s), Ok(None));
        assert!(!work.exists());
    }

    #[test]
    fn a_session_stops_while_joining_it_still_fits() {
        const GB: u64 = 1_000_000_000;
        // 30 GB recorded: joining copies it, and 10 GB must stay free.
        assert!(!should_end(30 * GB, 0, Some(41 * GB)));
        assert!(should_end(30 * GB, 0, Some(39 * GB)));
        assert!(!should_end(30 * GB, 0, None));
        // A new session needs 20 GB free to start.
        assert!(!may_start(0, Some(19 * GB)));
        assert!(may_start(0, Some(21 * GB)));
    }

    #[test]
    fn joins_still_running_count_against_the_room() {
        const GB: u64 = 1_000_000_000;
        // 55 GB free, a 45 GB session still being copied: no new session, though 55 > 20.
        assert!(!may_start(45 * GB, Some(55 * GB)));
        assert!(may_start(45 * GB, Some(66 * GB)));
        // And one already running ends sooner.
        assert!(should_end(5 * GB, 45 * GB, Some(59 * GB)));
        assert!(!should_end(5 * GB, 45 * GB, Some(61 * GB)));
    }

    #[test]
    fn a_session_folder_names_its_start() {
        assert_eq!(
            start_from_name(Path::new("x/4242-1700000000123456")),
            Some(1_700_000_000_123_456)
        );
        assert_eq!(start_from_name(Path::new("x/odd")), None);
    }

    /// A session cut short: its stream and an unfinished WAV (the header still says it's empty)
    /// come back as one clip with all its sound. Needs ffmpeg with libx264, and `KAIRO_HOME` set
    /// to a scratch folder (the thumbnail goes there).
    #[test]
    #[ignore]
    fn a_session_cut_short_is_saved_whole() {
        assert!(std::env::var_os("KAIRO_HOME").is_some(), "set KAIRO_HOME");
        let tmp = tempfile::tempdir().unwrap();
        let t0: i64 = 1_700_000_000_000_000;
        let work = tmp.path().join(format!("999-{t0}"));
        std::fs::create_dir_all(&work).unwrap();
        let ff = ffmpeg::path().expect("ffmpeg");
        let video = work.join(VIDEO);
        let made = ffmpeg::command(&ff)
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("testsrc=size=320x240:rate=30")
            .args(["-t", "6", "-c:v", "libx264", "-g", "30", "-f", "mpegts"])
            .arg(&video)
            .status()
            .unwrap();
        assert!(made.success());
        let mut ring = kairo::pcm::PcmRing::new(48_000, 2, 60_000_000);
        ring.start_tape(&wav_path(&work, Track::Game), t0).unwrap();
        for i in 1..=60 {
            ring.push(&vec![0.25f32; 4800 * 2], t0 + i * 100_000);
        }
        drop(ring.take_tape()); // flushed, but never finished: as a crash leaves it
        let wav = std::fs::read(wav_path(&work, Track::Game)).unwrap();
        assert_eq!(&wav[40..44], &[0, 0, 0, 0]);
        let note = Note {
            start_us: t0,
            title: Some("Test Game".into()),
        };
        std::fs::write(work.join(NOTE), serde_json::to_vec(&note).unwrap()).unwrap();
        let ended = UNIX_EPOCH + Duration::from_micros(t0 as u64 + 6_000_000);
        std::fs::File::options()
            .write(true)
            .open(&video)
            .unwrap()
            .set_modified(ended)
            .unwrap();

        let clips = tmp.path().join("clips");
        let (out, seconds) = recover(&work, &settings_in(&clips)).unwrap().unwrap();
        assert!(!work.exists());
        assert!((seconds - 6.0).abs() < 0.01, "{seconds}");
        assert!(out.starts_with(clips.join("Sessions")), "{}", out.display());
        assert!(
            out.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("Test Game session")
        );
        let info = ffmpeg::probe_clip(&ff, &out).unwrap();
        assert_eq!(info.video_codec, "h264");
        assert_eq!(info.audio_tracks.len(), 1);
        assert!((info.duration - 6.0).abs() < 0.3, "{}", info.duration);
        // The sound is all there, not cut to the empty length its header gave.
        let pcm = ffmpeg::command(&ff)
            .args(["-loglevel", "error", "-i"])
            .arg(&out)
            .args([
                "-map", "0:a:0", "-f", "s16le", "-ac", "1", "-ar", "8000", "-",
            ])
            .output()
            .unwrap();
        let secs = pcm.stdout.len() as f64 / 2.0 / 8000.0;
        assert!((secs - 6.0).abs() < 0.2, "{secs} s of sound");
    }

    #[test]
    fn only_stale_half_clips_are_swept() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("Some Game");
        std::fs::create_dir_all(&game).unwrap();
        let old = game.join("a.mp4.part");
        let fresh = tmp.path().join("b.mp4.part");
        let clip = tmp.path().join("c.mp4");
        for p in [&old, &fresh, &clip] {
            std::fs::write(p, b"x").unwrap();
        }
        let hour_ago = SystemTime::now() - Duration::from_secs(3600);
        for p in [&old, &clip] {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(hour_ago)
                .unwrap();
        }
        sweep_parts(&settings_in(tmp.path()));
        assert!(!old.exists());
        assert!(fresh.exists());
        assert!(clip.exists());
    }
}
