//! Keeps ffmpeg recording into the replay buffer: picks a capture method and encoder that work,
//! restarts after errors (a game changing the screen resolution ends the capture), and reports
//! what it's doing.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use kairo::args::{self, Encoder, Source, VideoPlan};
use kairo::config::{Settings, VideoCodec};
use kairo::pcm::PcmRing;
use kairo::ring::Ring;

use crate::log::{info, warning};
use crate::{ffmpeg, platform};

/// Memory the video buffer may use at most, whatever the settings.
const MAX_BUFFER_BYTES: usize = 1536 * 1024 * 1024;
/// The webcam's buffer: about 1 Mbit/s, so this holds over half an hour.
const MAX_CAM_BYTES: usize = 256 * 1024 * 1024;

/// Disk space a buffer on disk may use at most.
const MAX_SPILL_BYTES: u64 = 32 << 30;

/// The replay buffer's file, when it's on disk (deleted when Kairo closes).
fn spill_path() -> std::path::PathBuf {
    kairo::config::config_dir()
        .join("buffer")
        .join(format!("replay-{}.bin", std::process::id()))
}

pub fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64)
}

/// What the background process shows in the tray and the settings window.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub recording: bool,
    pub paused: bool,
    /// Paused because no game is in front (games-only), or a private app is.
    pub waiting: bool,
    pub encoder: Option<String>,
    /// "H.264" or "H.265".
    pub codec: Option<String>,
    pub source: Option<String>,
    pub size: Option<(u32, u32)>,
    pub fps: u32,
    pub buffer_seconds: f64,
    pub buffer_mb: f64,
    /// Whether the buffer is in a file rather than memory, and the memory it takes.
    pub buffer_on_disk: bool,
    pub memory_mb: f64,
    pub desktop_audio: Option<String>,
    pub microphone: Option<String>,
    /// The voice-chat track's state, when it's on.
    pub chat: Option<String>,
    pub error: Option<String>,
    pub audio_error: Option<String>,
    pub last_clip: Option<String>,
    /// The phrase voice clipping listens for, while it does.
    pub voice: Option<String>,
    pub voice_error: Option<String>,
    pub last_screenshot: Option<String>,
    /// Moments marked inside the replay buffer.
    pub marks: usize,
    /// The session being recorded: how long so far, and how big.
    pub session: Option<SessionStatus>,
    /// The camera being recorded for clips, while it is.
    pub webcam: Option<String>,
    pub webcam_error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionStatus {
    pub seconds: f64,
    pub mb: f64,
}

/// The sound tracks, each with its ring in `Shared`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Track {
    Game = 0,
    Mic = 1,
    Chat = 2,
}

impl Track {
    pub const ALL: [Track; 3] = [Track::Game, Track::Mic, Track::Chat];

    /// The name a session's file and a clip's track get.
    pub fn name(self) -> &'static str {
        match self {
            Track::Game => "Game",
            Track::Mic => "Microphone",
            Track::Chat => "Voice chat",
        }
    }
}

/// Everything the recorder, audio capture, saving and the tray share.
pub struct Shared {
    pub settings: Mutex<Settings>,
    pub ring: Mutex<Ring>,
    /// The webcam's own replay buffer.
    pub cam: Mutex<Ring>,
    pub desktop: Mutex<Option<PcmRing>>,
    pub mic: Mutex<Option<PcmRing>>,
    /// A voice-chat app's sound, when it has its own track.
    pub chat: Mutex<Option<PcmRing>>,
    /// A session's sound files while their track has no ring (audio restarting, paused), so
    /// they carry on in the next one instead of being lost.
    pub parked_tapes: Mutex<[Option<kairo::pcm::PcmTape>; 3]>,
    pub status: Mutex<Status>,
    pub stop: AtomicBool,
    pub paused: AtomicBool,
    /// Bumped when capture settings change: the recorder starts over with them.
    pub video_generation: AtomicU64,
    /// Bumped when sound settings change.
    pub audio_generation: AtomicU64,
    /// Bumped when webcam settings change.
    pub cam_generation: AtomicU64,
    /// The microphone's volume (an f32's bits; 1 is as recorded), applied as it's captured.
    pub mic_gain: std::sync::atomic::AtomicU32,
    pub saving: AtomicBool,
    /// The microphone's sound also goes here while voice clipping listens.
    pub voice_tap: crate::voice::Tap,
    /// Marked moments (wall-clock µs), oldest first, for as long as the buffer (or a session)
    /// could hold them.
    pub marks: Mutex<std::collections::VecDeque<i64>>,
    /// Overlay keys and clicks (wall-clock µs, key, down), oldest first, kept like marks.
    pub inputs: Mutex<std::collections::VecDeque<(i64, u8, bool)>>,
    /// The session being recorded, and whether one should be.
    pub session: Mutex<Option<crate::session::Session>>,
    pub session_wanted: AtomicBool,
}

impl Shared {
    pub fn new(settings: Settings) -> Arc<Shared> {
        let keep = settings.buffer_seconds() as i64 * 1_000_000;
        let mic_gain = (settings.mic_volume as f32 / 100.0).to_bits();
        Arc::new(Shared {
            settings: Mutex::new(settings),
            ring: Mutex::new(Ring::new(keep, MAX_BUFFER_BYTES)),
            cam: Mutex::new(Ring::new(keep, MAX_CAM_BYTES)),
            desktop: Mutex::new(None),
            mic: Mutex::new(None),
            chat: Mutex::new(None),
            parked_tapes: Mutex::default(),
            status: Mutex::new(Status::default()),
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            video_generation: AtomicU64::new(0),
            audio_generation: AtomicU64::new(0),
            cam_generation: AtomicU64::new(0),
            mic_gain: std::sync::atomic::AtomicU32::new(mic_gain),
            saving: AtomicBool::new(false),
            voice_tap: Default::default(),
            marks: Mutex::default(),
            inputs: Mutex::default(),
            session: Mutex::new(None),
            session_wanted: AtomicBool::new(false),
        })
    }

    pub fn track(&self, t: Track) -> &Mutex<Option<PcmRing>> {
        match t {
            Track::Game => &self.desktop,
            Track::Mic => &self.mic,
            Track::Chat => &self.chat,
        }
    }

    /// Puts `ring` (or nothing) in `t`'s place. A session's file being written from the old ring
    /// carries on in the new one, or waits for the next ring, rather than being dropped with it.
    pub fn set_ring(&self, t: Track, ring: Option<PcmRing>) {
        let mut slot = self.track(t).lock().unwrap();
        let mut parked = self.parked_tapes.lock().unwrap();
        let tape = slot
            .as_mut()
            .and_then(|r| r.take_tape())
            .or_else(|| parked[t as usize].take());
        *slot = ring;
        if let Some(tape) = tape {
            match slot.as_mut() {
                Some(r) => r.adopt_tape(tape),
                None => parked[t as usize] = Some(tape),
            }
        }
    }

    /// A session's sound files, from the rings and any waiting for one.
    pub fn take_tapes(&self) -> Vec<(Track, kairo::pcm::PcmTape)> {
        Track::ALL
            .into_iter()
            .filter_map(|t| {
                let from_ring = self
                    .track(t)
                    .lock()
                    .unwrap()
                    .as_mut()
                    .and_then(|r| r.take_tape());
                from_ring
                    .or_else(|| self.parked_tapes.lock().unwrap()[t as usize].take())
                    .map(|tape| (t, tape))
            })
            .collect()
    }

    /// From when events are still worth keeping: the buffer's reach, or a session's start.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn keep_since(&self, now: i64) -> i64 {
        let keep = self.settings().buffer_seconds() as i64 * 1_000_000;
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .map_or(now - keep, |s| s.start_us.min(now - keep))
    }

    /// A key or click for the overlay, kept only while recording.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn input(&self, at: i64, key: u8, down: bool) {
        let live = {
            let st = self.status.lock().unwrap();
            st.recording && !self.paused.load(Ordering::Relaxed)
        };
        if !live {
            return;
        }
        let since = self.keep_since(at);
        let mut q = self.inputs.lock().unwrap();
        q.push_back((at, key, down));
        while q.front().is_some_and(|e| e.0 < since) {
            q.pop_front();
        }
    }

    /// The overlay events between `start` and `end` (wall clock), timed from `start`.
    pub fn inputs_within(&self, start: i64, end: i64) -> Vec<kairo::keys::KeyEvent> {
        self.inputs
            .lock()
            .unwrap()
            .iter()
            .filter(|e| (start..=end).contains(&e.0))
            .map(|e| kairo::keys::KeyEvent {
                t: (e.0 - start) as f64 / 1e6,
                key: e.1,
                down: e.2,
            })
            .collect()
    }

    /// Marks this moment; saved clips that hold it show it on their timeline. Returns how many
    /// marks the buffer holds now.
    pub fn mark(&self) -> usize {
        let now = now_us();
        let keep = self.settings().buffer_seconds() as i64 * 1_000_000;
        // A session keeps its marks from its start, however long ago.
        let since = self
            .session
            .lock()
            .unwrap()
            .as_ref()
            .map_or(now - keep, |s| s.start_us.min(now - keep));
        let mut m = self.marks.lock().unwrap();
        m.push_back(now);
        while m.front().is_some_and(|t| *t < since) {
            m.pop_front();
        }
        m.len()
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    /// Takes new settings, restarting whatever they affect.
    pub fn apply(&self, new: Settings) {
        let old = std::mem::replace(&mut *self.settings.lock().unwrap(), new.clone());
        let keep = new.buffer_seconds() as i64 * 1_000_000;
        self.ring.lock().unwrap().set_keep_us(keep);
        self.cam.lock().unwrap().set_keep_us(keep);
        self.mic_gain
            .store((new.mic_volume as f32 / 100.0).to_bits(), Ordering::Relaxed);
        for ring in [&self.desktop, &self.mic, &self.chat] {
            if let Some(r) = ring.lock().unwrap().as_mut() {
                r.set_keep_us(keep);
            }
        }
        let video = (
            old.fps,
            old.resolution,
            old.quality,
            &old.encoder,
            &old.monitor,
            old.replay_storage,
            old.codec,
        ) != (
            new.fps,
            new.resolution,
            new.quality,
            &new.encoder,
            &new.monitor,
            new.replay_storage,
            new.codec,
        );
        if video {
            self.video_generation.fetch_add(1, Ordering::SeqCst);
        }
        let audio = (
            old.desktop_audio,
            old.microphone,
            old.voice_clip,
            &old.microphone_device,
            old.chat_track,
            &old.chat_app,
        ) != (
            new.desktop_audio,
            new.microphone,
            new.voice_clip,
            &new.microphone_device,
            new.chat_track,
            &new.chat_app,
        );
        if audio {
            self.audio_generation.fetch_add(1, Ordering::SeqCst);
        }
        if (old.webcam, &old.webcam_device) != (new.webcam, &new.webcam_device) {
            self.cam_generation.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub fn status(&self) -> Status {
        let mut s = self.status.lock().unwrap().clone();
        let ring = self.ring.lock().unwrap();
        s.buffer_seconds = ring.seconds();
        s.buffer_mb = ring.bytes() as f64 / (1024.0 * 1024.0);
        s.buffer_on_disk = ring.on_disk();
        s.memory_mb = ring.memory_bytes() as f64 / (1024.0 * 1024.0);
        s.session = ring.tape_progress().map(|(start, bytes)| SessionStatus {
            seconds: (now_us() - start).max(0) as f64 / 1e6,
            mb: bytes as f64 / (1024.0 * 1024.0),
        });
        s.paused = self.paused.load(Ordering::Relaxed);
        let since = now_us() - (s.buffer_seconds * 1e6) as i64;
        s.marks = self
            .marks
            .lock()
            .unwrap()
            .iter()
            .filter(|t| **t >= since)
            .count();
        s
    }
}

/// How one ffmpeg run ended.
pub enum Outcome {
    /// Asked to stop, pause, or pick up new settings.
    Interrupted,
    /// Never produced video: this capture method or encoder doesn't work here.
    FailedToStart(String),
    /// Recorded for a while, then stopped (the screen changed, a driver hiccup).
    Ended(String),
}

/// Runs until `shared.stop` is set. Call on its own thread.
pub fn run(shared: Arc<Shared>) {
    let ffmpeg = loop {
        match ffmpeg::path() {
            Ok(p) => break p,
            Err(e) => {
                shared.status.lock().unwrap().error = Some(e);
                if wait_or_stop(&shared, Duration::from_secs(30)) {
                    return;
                }
            }
        }
    };
    info!(
        "ffmpeg {} at {}",
        ffmpeg::version(&ffmpeg).unwrap_or_default(),
        ffmpeg.display()
    );
    // The combination that worked last, tried first after a restart.
    let mut good: Option<(Source, Encoder, VideoCodec)> = None;
    let mut encoders: Option<Vec<Encoder>> = None;
    let mut last_plan: Option<VideoPlan> = None;
    while !shared.stop.load(Ordering::Relaxed) {
        if shared.paused.load(Ordering::Relaxed) {
            // Nothing from before the pause may end up in a clip saved after it.
            shared.ring.lock().unwrap().clear();
            set_idle(&shared, None);
            if wait_or_stop(&shared, Duration::from_millis(250)) {
                return;
            }
            continue;
        }
        let generation = shared.video_generation.load(Ordering::SeqCst);
        let settings = shared.settings();
        let Some(monitor) = platform::choose_monitor(&settings.monitor) else {
            set_idle(&shared, Some("No screen found to record.".into()));
            if wait_or_stop(&shared, Duration::from_secs(5)) {
                return;
            }
            continue;
        };
        let forced = Encoder::from_id(&settings.encoder);
        let encoder_list: Vec<Encoder> = match forced {
            Some(e) => vec![e],
            None => encoders
                .get_or_insert_with(|| ffmpeg::working_encoders(&ffmpeg))
                .clone(),
        };
        if encoder_list.is_empty() {
            set_idle(
                &shared,
                Some("No video encoder works on this computer.".into()),
            );
            encoders = None;
            if wait_or_stop(&shared, Duration::from_secs(30)) {
                return;
            }
            continue;
        }
        // H.265 first where it's asked for and the encoder has it; H.264 if that won't start.
        let mut combos: Vec<(Source, Encoder, VideoCodec)> = Vec::new();
        for e in &encoder_list {
            for s in platform::capture_sources(&monitor) {
                if settings.codec == VideoCodec::Hevc
                    && e.ffmpeg_name_for(VideoCodec::Hevc).is_some()
                {
                    combos.push((s.clone(), *e, VideoCodec::Hevc));
                }
                combos.push((s, *e, VideoCodec::H264));
            }
        }
        if let Some(g) = &good
            && let Some(i) = combos.iter().position(|c| c == g)
        {
            let g = combos.remove(i);
            combos.insert(0, g);
        }
        let mut last_error = None;
        let mut restart_now = false;
        for (source, encoder, codec) in combos {
            let size = (monitor.w, monitor.h);
            let (ow, oh) = args::output_size(size, settings.resolution.max_height());
            let bitrate = settings
                .quality
                .bitrate(ow as u64 * oh as u64, settings.fps) as f64
                * codec.bitrate_factor();
            let plan = VideoPlan {
                source: source.clone(),
                encoder,
                codec,
                fps: settings.fps,
                src_size: size,
                max_height: settings.resolution.max_height(),
                bitrate: bitrate as u64,
                vaapi_device: ffmpeg::vaapi_device(),
            };
            {
                let mut ring = shared.ring.lock().unwrap();
                let spill = settings.buffer_on_disk(plan.bitrate).then(|| {
                    let room = settings.buffer_worst_bytes(plan.bitrate) + 64 * 1024 * 1024;
                    (spill_path(), room.min(MAX_SPILL_BYTES))
                });
                let moved = ring.set_spill(spill).unwrap_or_else(|e| {
                    warning!("keeping the replay in memory: {e}");
                    true
                });
                if last_plan.as_ref() == Some(&plan) && !moved {
                    ring.new_run()
                } else {
                    ring.clear()
                }
            }
            last_plan = Some(plan.clone());
            match run_plan(&ffmpeg, &plan, &shared, generation) {
                Outcome::Interrupted => {
                    restart_now = true;
                    break;
                }
                Outcome::FailedToStart(e) => {
                    warning!(
                        "{} with {} ({}) didn't start: {e}",
                        source.label(),
                        encoder.label(),
                        codec.label()
                    );
                    if good.as_ref() == Some(&(source.clone(), encoder, codec)) {
                        good = None;
                    }
                    last_error = Some(e);
                }
                Outcome::Ended(e) => {
                    warning!("recording stopped: {e}");
                    good = Some((source, encoder, codec));
                    // Pick everything again: the screen may have changed.
                    if wait_or_stop(&shared, Duration::from_millis(500)) {
                        return;
                    }
                    restart_now = true;
                    break;
                }
            }
        }
        if restart_now {
            continue;
        }
        // Nothing worked. Maybe the hardware changed: probe encoders again next time.
        encoders = None;
        set_idle(
            &shared,
            Some(format!(
                "Can't record the screen: {}",
                last_error.unwrap_or_default()
            )),
        );
        if wait_or_stop(&shared, Duration::from_secs(15)) {
            return;
        }
    }
}

fn set_idle(shared: &Shared, error: Option<String>) {
    let mut st = shared.status.lock().unwrap();
    st.recording = false;
    st.error = error;
}

/// Sleeps up to `d`; true if asked to stop meanwhile.
fn wait_or_stop(shared: &Shared, d: Duration) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if shared.stop.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    shared.stop.load(Ordering::Relaxed)
}

/// Records `plan` until it ends or is interrupted (stop, pause, or `video_generation` moving on
/// from `generation`).
pub fn run_plan(ffmpeg: &Path, plan: &VideoPlan, shared: &Arc<Shared>, generation: u64) -> Outcome {
    let args = args::record_args(plan);
    let mut child: Child = match ffmpeg::spawn(
        ffmpeg::command(ffmpeg)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    ) {
        Ok(c) => c,
        Err(e) => return Outcome::FailedToStart(format!("could not start ffmpeg: {e}")),
    };
    let got_data = Arc::new(AtomicBool::new(false));
    let reader = {
        let mut out = child.stdout.take().expect("piped stdout");
        let shared = shared.clone();
        let got = got_data.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                match out.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        shared.ring.lock().unwrap().push(&buf[..n], now_us());
                        got.store(true, Ordering::Relaxed);
                    }
                }
            }
        })
    };
    let errors = Arc::new(Mutex::new(String::new()));
    let err_reader = {
        let mut err = child.stderr.take().expect("piped stderr");
        let errors = errors.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut e = errors.lock().unwrap();
                e.push_str(&String::from_utf8_lossy(&buf[..n]));
                if e.len() > 16_384 {
                    let cut = e.len() - 8_192;
                    let cut = (cut..e.len())
                        .find(|i| e.is_char_boundary(*i))
                        .unwrap_or(cut);
                    e.drain(..cut);
                }
            }
        })
    };
    let started = Instant::now();
    let mut announced = false;
    let mut last_look = Instant::now();
    let outcome = loop {
        if let Ok(Some(status)) = child.try_wait() {
            let _ = reader.join();
            let _ = err_reader.join();
            let text = ffmpeg::last_lines(&errors.lock().unwrap(), 3)
                .unwrap_or_else(|| format!("ffmpeg exited ({status})"));
            break if got_data.load(Ordering::Relaxed) && started.elapsed() > Duration::from_secs(3)
            {
                Outcome::Ended(text)
            } else {
                Outcome::FailedToStart(text)
            };
        }
        // A game changing the display mode changes the screen's size: start over at the new
        // size rather than keep scaling to the old one. (A test picture has no screen.)
        let resized = !matches!(plan.source, Source::Test { .. })
            && last_look.elapsed() >= Duration::from_secs(2)
            && {
                last_look = Instant::now();
                let now = platform::choose_monitor(&shared.settings().monitor).map(|m| (m.w, m.h));
                let changed = now.is_some_and(|s| s != plan.src_size);
                if changed {
                    info!(
                        "the screen changed from {}x{} to {}x{}: starting over",
                        plan.src_size.0,
                        plan.src_size.1,
                        now.map_or(0, |s| s.0),
                        now.map_or(0, |s| s.1)
                    );
                }
                changed
            };
        let interrupted = shared.stop.load(Ordering::Relaxed)
            || shared.paused.load(Ordering::Relaxed)
            || shared.video_generation.load(Ordering::SeqCst) != generation
            || resized;
        let stalled =
            !got_data.load(Ordering::Relaxed) && started.elapsed() > Duration::from_secs(8);
        if interrupted || stalled {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            let _ = err_reader.join();
            break if interrupted {
                Outcome::Interrupted
            } else {
                Outcome::FailedToStart("no video arrived".into())
            };
        }
        if !announced && shared.ring.lock().unwrap().seconds() > 0.0 {
            announced = true;
            let size = args::output_size(plan.src_size, plan.max_height);
            info!(
                "recording {} {} via {} at {}x{} {} fps",
                plan.encoder.label(),
                plan.codec.label(),
                plan.source.label(),
                size.0,
                size.1,
                plan.fps
            );
            let mut st = shared.status.lock().unwrap();
            st.recording = true;
            st.error = None;
            st.encoder = Some(plan.encoder.label().into());
            st.codec = Some(plan.codec.label().into());
            st.source = Some(plan.source.label().into());
            st.size = Some(size);
            st.fps = plan.fps;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    shared.status.lock().unwrap().recording = false;
    outcome
}
