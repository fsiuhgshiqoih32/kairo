//! Sound capture into the shared PCM rings: what the computer plays ("desktop audio") and the
//! microphone, each stamped with the wall clock as it arrives.
//!
//! - Windows: desktop audio through WASAPI loopback (cpal opens the output device for input).
//! - Linux: desktop audio from the default sink's monitor through `parec` (or `pw-record`).
//! - macOS: no desktop audio (it needs ScreenCaptureKit); the microphone works.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use rewindr::config::Settings;
use rewindr::pcm::PcmRing;

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

fn ring_of(shared: &Shared, kind: Kind) -> &Mutex<Option<PcmRing>> {
    match kind {
        Kind::Desktop => &shared.desktop,
        Kind::Mic => &shared.mic,
    }
}

/// Converts any sample format cpal hands us to f32.
fn to_f32(data: &cpal::Data) -> Option<Vec<f32>> {
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
    *ring_of(shared, kind).lock().unwrap() = Some(PcmRing::new(rate, channels, keep));
    let s2 = shared.clone();
    let f2 = failed.clone();
    let stream = device
        .build_input_stream_raw(
            config.config(),
            config.sample_format(),
            move |data: &cpal::Data, _: &cpal::InputCallbackInfo| {
                if let Some(samples) = to_f32(data)
                    && let Some(r) = ring_of(&s2, kind).lock().unwrap().as_mut()
                {
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
                *shared.desktop.lock().unwrap() = Some(PcmRing::new(48_000, 2, keep));
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
    output_id: Option<String>,
    input_id: Option<String>,
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
    // Never set on macOS, which has no desktop audio yet.
    #[cfg_attr(target_os = "macos", allow(unused_mut))]
    let mut desktop_name = None;
    let mut mic_name = None;
    *shared.desktop.lock().unwrap() = None;
    *shared.mic.lock().unwrap() = None;
    if settings.desktop_audio {
        #[cfg(windows)]
        match host.default_output_device() {
            Some(d) => {
                open_now.output_id = d.id().ok().map(|i| format!("{i:?}"));
                match open(shared, &d, Kind::Desktop, failed) {
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
        errors.push("Desktop audio isn't available on macOS yet; the microphone works.".into());
    }
    if settings.microphone {
        let wanted = settings.microphone_device.trim();
        let device = if wanted.is_empty() {
            host.default_input_device()
        } else {
            host.input_devices()
                .ok()
                .and_then(|mut it| it.find(|d| d.to_string() == wanted))
                .or_else(|| host.default_input_device())
        };
        match device {
            Some(d) => {
                open_now.input_id = d.id().ok().map(|i| format!("{i:?}"));
                match open(shared, &d, Kind::Mic, failed) {
                    Ok(s) => {
                        mic_name = Some(d.to_string());
                        open_now.streams.push(s);
                    }
                    Err(e) => errors.push(format!("microphone: {e}")),
                }
            }
            None => errors.push("microphone: none found".into()),
        }
    }
    if !errors.is_empty() {
        warning!("{}", errors.join("; "));
    }
    info!("audio: desktop {desktop_name:?}, microphone {mic_name:?}");
    let mut st = shared.status.lock().unwrap();
    st.desktop_audio = desktop_name;
    st.microphone = mic_name;
    st.audio_error = (!errors.is_empty()).then(|| errors.join("; "));
    open_now
}

/// The default output or input device changed (headphones plugged in, say).
fn defaults_changed(open: &Open, settings: &Settings) -> bool {
    let host = cpal::default_host();
    let id = |d: Option<cpal::Device>| d.and_then(|d| d.id().ok()).map(|i| format!("{i:?}"));
    (cfg!(windows) && settings.desktop_audio && id(host.default_output_device()) != open.output_id)
        || (settings.microphone
            && settings.microphone_device.trim().is_empty()
            && id(host.default_input_device()) != open.input_id)
}

/// Runs until `shared.stop` is set. Call on its own thread: cpal streams stay on the thread that
/// opened them.
pub fn run(shared: Arc<Shared>) {
    let failed: Arc<Mutex<Option<String>>> = Arc::default();
    while !shared.stop.load(Ordering::Relaxed) {
        let generation = shared.audio_generation.load(Ordering::SeqCst);
        let settings = shared.settings();
        let paused = shared.paused.load(Ordering::Relaxed);
        let open_now = if paused {
            Open::default()
        } else {
            start(&shared, &settings, &failed)
        };
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
                std::thread::sleep(Duration::from_millis(500));
                break;
            }
            if !paused && last_check.elapsed() > Duration::from_secs(2) {
                last_check = Instant::now();
                if defaults_changed(&open_now, &settings) {
                    info!("default audio device changed; reopening");
                    break;
                }
            }
        }
        drop(open_now);
    }
}
