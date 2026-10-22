//! Sound capture into the shared PCM rings: what the computer plays ("desktop audio") and the
//! microphone, each stamped with the wall clock as it arrives.
//!
//! - Windows: desktop audio through WASAPI loopback (cpal opens the output device for input).
//! - Linux: desktop audio from the default sink's monitor through `parec` (or `pw-record`).
//! - macOS: desktop audio through ScreenCaptureKit (macOS 13 and later).
//!
//! The microphone's sound also goes to voice clipping (`shared.voice_tap`) while it listens; it's
//! only kept for clips when recording the microphone is on.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use kairo::config::Settings;
use kairo::pcm::PcmRing;

use crate::log::{info, warning};
use crate::recorder::{Shared, now_us};

pub fn microphones() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|it| it.map(|d| d.to_string()).collect())
        .unwrap_or_default()
}

/// Which ring a stream fills.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Desktop,
    Mic,
}

impl Kind {
    fn track(self) -> crate::recorder::Track {
        match self {
            Kind::Desktop => crate::recorder::Track::Game,
            Kind::Mic => crate::recorder::Track::Mic,
        }
    }
}

fn ring_of(shared: &Shared, kind: Kind) -> &Mutex<Option<PcmRing>> {
    match kind {
        Kind::Desktop => &shared.desktop,
        Kind::Mic => &shared.mic,
    }
}

/// Converts any sample format cpal hands us to f32.
pub fn to_f32(data: &cpal::Data) -> Option<Vec<f32>> {
    use cpal::SampleFormat as F;
    Some(match data.sample_format() {
        F::F32 => data.as_slice::<f32>()?.to_vec(),
        F::F64 => data.as_slice::<f64>()?.iter().map(|s| *s as f32).collect(),
        F::I16 => data
            .as_slice::<i16>()?
            .iter()
            .map(|s| *s as f32 / 32768.0)
            .collect(),
        F::I32 => data
            .as_slice::<i32>()?
            .iter()
            .map(|s| *s as f32 / 2_147_483_648.0)
            .collect(),
        F::U16 => data
            .as_slice::<u16>()?
            .iter()
            .map(|s| (*s as f32 - 32768.0) / 32768.0)
            .collect(),
        F::U8 => data
            .as_slice::<u8>()?
            .iter()
            .map(|s| (*s as f32 - 128.0) / 128.0)
            .collect(),
        F::I8 => data
            .as_slice::<i8>()?
            .iter()
            .map(|s| *s as f32 / 128.0)
            .collect(),
        _ => return None,
    })
}

/// Opens a cpal input stream on `device` feeding `kind`'s ring. For an output device this is
/// loopback capture (WASAPI).
fn open(
    shared: &Arc<Shared>,
    device: &cpal::Device,
    kind: Kind,
    record: bool,
    failed: &Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, String> {
    let config = if kind == Kind::Desktop {
        device.default_output_config()
    } else {
        device.default_input_config()
    }
    .map_err(|e| e.to_string())?;
    let rate = config.sample_rate();
    let channels = config.channels();
    let keep = shared.settings().buffer_seconds() as i64 * 1_000_000;
    shared.set_ring(
        kind.track(),
        record.then(|| PcmRing::new(rate, channels, keep)),
    );
    let s2 = shared.clone();
    let f2 = failed.clone();
    let stream = device
        .build_input_stream_raw(
            config.config(),
            config.sample_format(),
            move |data: &cpal::Data, _: &cpal::InputCallbackInfo| {
                let Some(mut samples) = to_f32(data) else {
                    return;
                };
                if kind == Kind::Mic {
                    let gain = f32::from_bits(s2.mic_gain.load(Ordering::Relaxed));
                    kairo::pcm::apply_gain(&mut samples, gain);
                }
                if kind == Kind::Mic
                    && let Some(tap) = s2.voice_tap.lock().unwrap().as_ref()
                {
                    let _ = tap.try_send((samples.clone(), rate, channels));
                }
                if let Some(r) = ring_of(&s2, kind).lock().unwrap().as_mut() {
                    r.push(&samples, now_us());
                }
            },
            move |e: cpal::Error| {
                // Glitches (loopback reports one whenever playback stops and starts) and
                // reroutes are harmless; only a stream that can't continue is reopened.
                use cpal::ErrorKind as K;
                if matches!(
                    e.kind(),
                    K::DeviceNotAvailable | K::StreamInvalidated | K::HostUnavailable
                ) {
                    *f2.lock().unwrap() = Some(e.to_string());
                }
            },
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

/// Linux desktop audio: the default sink's monitor, read from `parec` (PulseAudio and PipeWire)
/// or `pw-record` as 48 kHz stereo float.
#[cfg(target_os = "linux")]
fn open_monitor(shared: &Arc<Shared>) -> Result<std::process::Child, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let keep = shared.settings().buffer_seconds() as i64 * 1_000_000;
    let tries: [(&str, Vec<&str>); 2] = [
        (
            "parec",
            vec![
                "--device=@DEFAULT_MONITOR@",
                "--format=float32le",
                "--rate=48000",
                "--channels=2",
                "--latency-msec=20",
                "--raw",
            ],
        ),
        (
            "pw-record",
            vec![
                "-P",
                "{ stream.capture.sink=true }",
                "--format",
                "f32",
                "--rate",
                "48000",
                "--channels",
                "2",
                "-",
            ],
        ),
    ];
    let mut last = String::new();
    for (prog, args) in tries {
        match Command::new(prog)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                shared.set_ring(
                    crate::recorder::Track::Game,
                    Some(PcmRing::new(48_000, 2, keep)),
                );
                let mut out = child.stdout.take().expect("piped");
                let s2 = shared.clone();
                std::thread::spawn(move || {
                    let mut buf = vec![0u8; 48_000 * 2 * 4 / 50];
                    while let Ok(n) = out.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        let usable = n - n % 8;
                        let samples: Vec<f32> = buf[..usable]
                            .chunks_exact(4)
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                            .collect();
                        if let Some(r) = s2.desktop.lock().unwrap().as_mut() {
                            r.push(&samples, now_us());
                        }
                    }
                });
                return Ok(child);
            }
            Err(e) => last = format!("{prog}: {e}"),
        }
    }
    Err(format!("desktop audio needs parec or pw-record ({last})"))
}

/// The open captures; dropping it stops them.
#[derive(Default)]
struct Open {
    streams: Vec<cpal::Stream>,
    #[cfg(target_os = "linux")]
    children: Vec<std::process::Child>,
    #[cfg(target_os = "macos")]
    system_audio: Option<crate::platform::macos_audio::SystemAudio>,
    /// Desktop audio split from a voice-chat app's (Windows 11).
    #[cfg(windows)]
    split: Option<crate::platform::windows_app_audio::Split>,
    output_id: Option<String>,
    input_id: Option<String>,
    /// The chosen microphone wasn't there, so the default one stands in for it.
    mic_stand_in: bool,
    /// What failed to open, for the log and the status.
    errors: Vec<String>,
    /// Which devices are open, for the log.
    report: String,
}

impl Drop for Open {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        for c in &mut self.children {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn start(shared: &Arc<Shared>, settings: &Settings, failed: &Arc<Mutex<Option<String>>>) -> Open {
    let host = cpal::default_host();
    let mut open_now = Open::default();
    let mut errors: Vec<String> = Vec::new();
    let mut desktop_name = None;
    let mut mic_name = None;
    for t in crate::recorder::Track::ALL {
        shared.set_ring(t, None);
    }
    #[allow(unused_mut)]
    let mut split = false;
    #[cfg(windows)]
    if settings.desktop_audio && settings.chat_track {
        match crate::platform::windows_app_audio::start(shared.clone(), settings.chat_app.clone()) {
            Ok(s) => {
                desktop_name = Some(format!("Everything but {}", settings.chat_app));
                open_now.split = Some(s);
                split = true;
            }
            Err(e) => errors.push(format!(
                "{} on its own track needs Windows 11 ({e})",
                settings.chat_app
            )),
        }
    }
    if settings.desktop_audio && !split {
        #[cfg(windows)]
        match host.default_output_device() {
            Some(d) => {
                open_now.output_id = d.id().ok().map(|i| format!("{i:?}"));
                match open(shared, &d, Kind::Desktop, true, failed) {
                    Ok(s) => {
                        desktop_name = Some(d.to_string());
                        open_now.streams.push(s);
                    }
                    Err(e) => errors.push(format!("desktop audio: {e}")),
                }
            }
            None => errors.push("desktop audio: no output device".into()),
        }
        #[cfg(target_os = "linux")]
        match open_monitor(shared) {
            Ok(c) => {
                desktop_name = Some("Default output".into());
                open_now.children.push(c);
            }
            Err(e) => errors.push(e),
        }
        #[cfg(target_os = "macos")]
        match crate::platform::macos_audio::start(shared.clone()) {
            Ok(s) => {
                desktop_name = Some("System audio".into());
                open_now.system_audio = Some(s);
            }
            Err(e) => errors.push(e),
        }
    }
    if settings.microphone || settings.voice_clip {
        let wanted = settings.microphone_device.trim();
        let chosen = (!wanted.is_empty())
            .then(|| {
                host.input_devices()
                    .ok()
                    .and_then(|mut it| it.find(|d| d.to_string() == wanted))
            })
            .flatten();
        // The chosen microphone can be missing for a moment while devices switch over: use the
        // default meanwhile, and the chosen one again once it's back (see `chosen_mic_back`).
        open_now.mic_stand_in = !wanted.is_empty() && chosen.is_none();
        let device = chosen.or_else(|| host.default_input_device());
        match device {
            Some(d) => {
                open_now.input_id = d.id().ok().map(|i| format!("{i:?}"));
                match open(shared, &d, Kind::Mic, settings.microphone, failed) {
                    Ok(s) => {
                        if settings.microphone {
                            mic_name = Some(d.to_string());
                        }
                        open_now.streams.push(s);
                    }
                    Err(e) => errors.push(format!("microphone: {e}")),
                }
            }
            None => errors.push("microphone: none found".into()),
        }
    }
    open_now.report = format!("audio: desktop {desktop_name:?}, microphone {mic_name:?}");
    let mut st = shared.status.lock().unwrap();
    st.desktop_audio = desktop_name;
    st.microphone = mic_name;
    st.chat = None;
    st.audio_error = (!errors.is_empty()).then(|| errors.join("; "));
    open_now.errors = errors;
    open_now
}

/// The chosen microphone is there again after `start` had to use the default one instead.
fn chosen_mic_back(open: &Open, settings: &Settings) -> bool {
    let wanted = settings.microphone_device.trim();
    open.mic_stand_in
        && cpal::default_host()
            .input_devices()
            .is_ok_and(|mut it| it.any(|d| d.to_string() == wanted))
}

/// The wait before opening again what failed to open: 2 s, doubling up to 5 minutes. A device
/// switching over usually opens on the next try; one that's gone isn't reopened all the time.
#[derive(Default)]
struct Backoff {
    tries: u32,
}

impl Backoff {
    fn next(&mut self) -> Duration {
        let wait = Duration::from_secs(2 << self.tries.min(8)).min(Duration::from_secs(300));
        self.tries += 1;
        wait
    }

    fn reset(&mut self) {
        self.tries = 0;
    }
}

/// The default output or input device changed (headphones plugged in, say). Only a stream opened
/// on a device counts: the voice-chat split captures by process, whatever the device.
fn defaults_changed(open: &Open, settings: &Settings) -> bool {
    let host = cpal::default_host();
    let id = |d: Option<cpal::Device>| d.and_then(|d| d.id().ok()).map(|i| format!("{i:?}"));
    (cfg!(windows)
        && settings.desktop_audio
        && open.output_id.is_some()
        && id(host.default_output_device()) != open.output_id)
        || ((settings.microphone || settings.voice_clip)
            && settings.microphone_device.trim().is_empty()
            && id(host.default_input_device()) != open.input_id)
}

/// Runs until `shared.stop` is set. Call on its own thread: cpal streams stay on the thread that
/// opened them.
pub fn run(shared: Arc<Shared>) {
    // Devices are opened from COM's multithreaded apartment, which WASAPI expects of a thread
    // without a message loop. cpal would make this thread single-threaded; after a device
    // change, opening devices there failed with RPC_E_CHANGED_MODE ("Cannot change thread mode
    // after it is set") and the sound stayed off. cpal works either way.
    #[cfg(windows)]
    crate::platform::windows_app_audio::join_mta();
    let failed: Arc<Mutex<Option<String>>> = Arc::default();
    let mut backoff = Backoff::default();
    let mut retrying = false;
    let mut logged = String::new();
    while !shared.stop.load(Ordering::Relaxed) {
        let generation = shared.audio_generation.load(Ordering::SeqCst);
        let settings = shared.settings();
        let paused = shared.paused.load(Ordering::Relaxed);
        let open_now = if paused {
            for t in crate::recorder::Track::ALL {
                shared.set_ring(t, None);
            }
            Open::default()
        } else {
            start(&shared, &settings, &failed)
        };
        // A retry that fails the same way says nothing new.
        let said = format!("{}\n{}", open_now.errors.join("; "), open_now.report);
        if !paused && !(retrying && said == logged) {
            if !open_now.errors.is_empty() {
                warning!("{}", open_now.errors.join("; "));
            }
            info!("{}", open_now.report);
        }
        logged = said;
        // Something asked for didn't open: try again soon, then less and less often.
        let retry_at = if open_now.errors.is_empty() {
            backoff.reset();
            None
        } else {
            Some(Instant::now() + backoff.next())
        };
        retrying = false;
        let mut last_check = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(100));
            if shared.stop.load(Ordering::Relaxed)
                || shared.audio_generation.load(Ordering::SeqCst) != generation
                || shared.paused.load(Ordering::Relaxed) != paused
            {
                break;
            }
            if let Some(e) = failed.lock().unwrap().take() {
                warning!("audio stream error: {e}; reopening");
                // Usually a device switching over: give it a moment to finish.
                std::thread::sleep(Duration::from_secs(1));
                break;
            }
            if retry_at.is_some_and(|t| Instant::now() >= t) {
                retrying = true;
                break;
            }
            #[cfg(windows)]
            if let Some(s) = &open_now.split {
                let app = &settings.chat_app;
                let line = if *s.app_running.lock().unwrap() {
                    format!("{app} is on its own track")
                } else {
                    format!("Waiting for {app}")
                };
                shared.status.lock().unwrap().chat = Some(line);
            }
            if !paused && last_check.elapsed() > Duration::from_secs(2) {
                last_check = Instant::now();
                if defaults_changed(&open_now, &settings) {
                    info!("default audio device changed; reopening");
                    break;
                }
                if chosen_mic_back(&open_now, &settings) {
                    info!("{} is back; reopening", settings.microphone_device.trim());
                    break;
                }
            }
        }
        drop(open_now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_audio_is_retried_less_and_less_often() {
        let mut b = Backoff::default();
        let waits: Vec<u64> = (0..10).map(|_| b.next().as_secs()).collect();
        assert_eq!(waits, [2, 4, 8, 16, 32, 64, 128, 256, 300, 300]);
        b.reset();
        assert_eq!(b.next(), Duration::from_secs(2));
    }

    /// cpal reads devices from a multithreaded COM thread, as the audio thread now is. Asks for
    /// formats only: nothing is captured.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn devices_open_from_a_multithreaded_com_thread() {
        std::thread::spawn(|| {
            crate::platform::windows_app_audio::join_mta();
            let host = cpal::default_host();
            let out = host.default_output_device().expect("an output device");
            out.default_output_config().expect("output format");
            for d in host.input_devices().expect("inputs") {
                d.default_input_config()
                    .unwrap_or_else(|e| panic!("{d}: {e}"));
            }
        })
        .join()
        .unwrap();
    }
}
