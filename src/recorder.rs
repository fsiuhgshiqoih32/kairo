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

use rewindr::args::{self, Encoder, Source, VideoPlan};
use rewindr::config::Settings;
use rewindr::pcm::PcmRing;
use rewindr::ring::Ring;

use crate::log::{info, warning};
use crate::{ffmpeg, platform};

/// Memory the video buffer may use at most, whatever the settings.
const MAX_BUFFER_BYTES: usize = 1536 * 1024 * 1024;

pub fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64)
}

/// What the background process shows in the tray and the settings window.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub recording: bool,
    pub paused: bool,
    pub encoder: Option<String>,
    pub source: Option<String>,
    pub size: Option<(u32, u32)>,
    pub fps: u32,
    pub buffer_seconds: f64,
    pub buffer_mb: f64,
    pub desktop_audio: Option<String>,
    pub microphone: Option<String>,
    pub error: Option<String>,
    pub audio_error: Option<String>,
    pub last_clip: Option<String>,
}

/// Everything the recorder, audio capture, saving and the tray share.
pub struct Shared {
    pub settings: Mutex<Settings>,
    pub ring: Mutex<Ring>,
    pub desktop: Mutex<Option<PcmRing>>,
    pub mic: Mutex<Option<PcmRing>>,
    pub status: Mutex<Status>,
    pub stop: AtomicBool,
    pub paused: AtomicBool,
    /// Bumped when capture settings change: the recorder starts over with them.
    pub video_generation: AtomicU64,
    /// Bumped when sound settings change.
    pub audio_generation: AtomicU64,
    pub saving: AtomicBool,
}

impl Shared {
    pub fn new(settings: Settings) -> Arc<Shared> {
        let keep = settings.buffer_seconds() as i64 * 1_000_000;
        Arc::new(Shared {
            settings: Mutex::new(settings),
            ring: Mutex::new(Ring::new(keep, MAX_BUFFER_BYTES)),
            desktop: Mutex::new(None),
            mic: Mutex::new(None),
            status: Mutex::new(Status::default()),
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            video_generation: AtomicU64::new(0),
            audio_generation: AtomicU64::new(0),
            saving: AtomicBool::new(false),
        })
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    /// Takes new settings, restarting whatever they affect.
    pub fn apply(&self, new: Settings) {
        let old = std::mem::replace(&mut *self.settings.lock().unwrap(), new.clone());
        let keep = new.buffer_seconds() as i64 * 1_000_000;
        self.ring.lock().unwrap().set_keep_us(keep);
        for ring in [&self.desktop, &self.mic] {
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
        ) != (
            new.fps,
            new.resolution,
            new.quality,
            &new.encoder,
            &new.monitor,
        );
        if video {
            self.video_generation.fetch_add(1, Ordering::SeqCst);
        }
        let audio = (old.desktop_audio, old.microphone, &old.microphone_device)
            != (new.desktop_audio, new.microphone, &new.microphone_device);
        if audio {
            self.audio_generation.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub fn status(&self) -> Status {
        let mut s = self.status.lock().unwrap().clone();
        let ring = self.ring.lock().unwrap();
        s.buffer_seconds = ring.seconds();
        s.buffer_mb = ring.bytes() as f64 / (1024.0 * 1024.0);
        s.paused = self.paused.load(Ordering::Relaxed);
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
    let mut good: Option<(Source, Encoder)> = None;
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
        let mut combos: Vec<(Source, Encoder)> = Vec::new();
        for e in &encoder_list {
            for s in platform::capture_sources(&monitor) {
                combos.push((s, *e));
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
        for (source, encoder) in combos {
            let size = (monitor.w, monitor.h);
            let (ow, oh) = args::output_size(size, settings.resolution.max_height());
            let plan = VideoPlan {
                source: source.clone(),
                encoder,
                fps: settings.fps,
                src_size: size,
                max_height: settings.resolution.max_height(),
                bitrate: settings
                    .quality
                    .bitrate(ow as u64 * oh as u64, settings.fps),
                vaapi_device: ffmpeg::vaapi_device(),
            };
            {
                let mut ring = shared.ring.lock().unwrap();
                if last_plan.as_ref() == Some(&plan) {
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
                        "{} with {} didn't start: {e}",
                        source.label(),
                        encoder.label()
                    );
                    if good.as_ref() == Some(&(source.clone(), encoder)) {
                        good = None;
                    }
                    last_error = Some(e);
                }
                Outcome::Ended(e) => {
                    warning!("recording stopped: {e}");
                    good = Some((source, encoder));
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
    let mut child: Child = match ffmpeg::command(ffmpeg)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
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
        let interrupted = shared.stop.load(Ordering::Relaxed)
            || shared.paused.load(Ordering::Relaxed)
            || shared.video_generation.load(Ordering::SeqCst) != generation;
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
                "recording {} via {} at {}x{} {} fps",
                plan.encoder.label(),
                plan.source.label(),
                size.0,
                size.1,
                plan.fps
            );
            let mut st = shared.status.lock().unwrap();
            st.recording = true;
            st.error = None;
            st.encoder = Some(plan.encoder.label().into());
            st.source = Some(plan.source.label().into());
            st.size = Some(size);
            st.fps = plan.fps;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    shared.status.lock().unwrap().recording = false;
    outcome
}
