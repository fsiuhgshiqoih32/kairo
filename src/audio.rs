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
    inputs(&cpal::default_host())
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// The input devices and their names. One whose name can't be read (it's going away) is left
/// out rather than stopping the list.
fn inputs(host: &cpal::Host) -> Vec<(String, cpal::Device)> {
    host.input_devices()
        .map(|it| {
            it.filter_map(|d| Some((d.description().ok()?.name().to_string(), d)))
                .collect()
        })
        .unwrap_or_default()
}

fn id_of(d: &cpal::Device) -> Option<String> {
    d.id().ok().map(|i| format!("{i:?}"))
}

/// The default output as the device it is now, not as "whatever is the default", as with the
/// microphone (see `mic_choices`): after a device change, the second kind kept failing to open
/// with "Cannot change thread mode after it is set" for a minute or two, and clips had no game
/// sound. A change of default is followed by `defaults_changed`.
#[cfg(windows)]
fn default_output(host: &cpal::Host) -> Option<cpal::Device> {
    let default = host.default_output_device()?;
    let id = id_of(&default);
    host.output_devices()
        .ok()
        .and_then(|mut it| it.find(|d| id.is_some() && id_of(d) == id))
        .or(Some(default))
}

/// How the microphone to open was found, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Found {
    /// The one chosen in the settings.
    Chosen,
    /// The chosen one under a slightly different name: Windows numbers a device again when it
    /// moves to another USB port ("Headset Microphone (2- Logitech G535 Gaming Headset)").
    Renamed,
    /// The only input of the chosen one's device, under another name.
    SameDevice,
    /// The system's default input.
    Default,
    /// With no default set, the first input that opens.
    First,
}

/// A device's name as people would say it: without the number Windows puts in front of a
/// device it has seen before ("Microphone (2- Shure MV7+)"), in lower case, spaces tidied.
fn plain_name(name: &str) -> String {
    let mut out = String::new();
    let mut rest = name.trim();
    loop {
        // "2- " at the start, or just inside a bracket.
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits > 0 && rest[digits..].starts_with("- ") {
            rest = &rest[digits + 2..];
        }
        match rest.find('(') {
            Some(at) => {
                out.push_str(&rest[..=at]);
                rest = &rest[at + 1..];
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The device in an input's name: "shure mv7+" in "microphone (shure mv7+)".
fn device_part(plain: &str) -> Option<&str> {
    let (_, rest) = plain.split_once('(')?;
    Some(rest.strip_suffix(')').unwrap_or(rest).trim()).filter(|d| !d.is_empty())
}

/// The inputs to try for the microphone, best first, by their place in `names`: the one chosen
/// (`wanted`) under its name or one close to it; else the default input (`default`); else, with
/// no default set, each input in turn.
fn mic_order(wanted: &str, names: &[String], default: Option<usize>) -> Vec<(usize, Found)> {
    let wanted = wanted.trim();
    if !wanted.is_empty() {
        if let Some(i) = names.iter().position(|n| n == wanted) {
            return vec![(i, Found::Chosen)];
        }
        let plain = plain_name(wanted);
        let plains: Vec<String> = names.iter().map(|n| plain_name(n)).collect();
        if let Some(i) = plains.iter().position(|n| *n == plain) {
            return vec![(i, Found::Renamed)];
        }
        // Another input of the same device only counts when it's the device's one input: a
        // sound card's line-in is no stand-in for its microphone.
        if let Some(device) = device_part(&plain) {
            let same: Vec<usize> = (0..names.len())
                .filter(|&i| device_part(&plains[i]) == Some(device))
                .collect();
            if let [i] = same[..] {
                return vec![(i, Found::SameDevice)];
            }
        }
    }
    match default {
        Some(i) => vec![(i, Found::Default)],
        None => (0..names.len()).map(|i| (i, Found::First)).collect(),
    }
}

/// The microphones to try for `wanted`, best first (see `mic_order`), with their names.
fn mic_choices(host: &cpal::Host, wanted: &str) -> Vec<(cpal::Device, String, Found)> {
    let mut list = inputs(host);
    // The default is opened as the device it is now, not as "whatever is the default": on
    // Windows that kind failed to open after device changes ("Cannot change thread mode after
    // it is set"), and a change of default is followed here anyway (`defaults_changed`).
    let default = host.default_input_device();
    let default_id = default.as_ref().and_then(id_of);
    let mut at = default_id
        .as_ref()
        .and_then(|id| list.iter().position(|(_, d)| id_of(d).as_ref() == Some(id)));
    // Where the default isn't in the list (some Linux set-ups list it apart), it's added.
    if at.is_none()
        && let Some(d) = default
        && let Ok(desc) = d.description()
    {
        list.push((desc.name().to_string(), d));
        at = Some(list.len() - 1);
    }
    let names: Vec<String> = list.iter().map(|(n, _)| n.clone()).collect();
    let mut list: Vec<Option<(String, cpal::Device)>> = list.into_iter().map(Some).collect();
    mic_order(wanted, &names, at)
        .into_iter()
        .filter_map(|(i, found)| list[i].take().map(|(name, d)| (d, name, found)))
        .collect()
}

/// The microphone clips would get for `wanted` (see `mic_order`) and its name, for the mic test.
pub fn microphone_for(wanted: &str) -> Option<(cpal::Device, String)> {
    mic_choices(&cpal::default_host(), wanted)
        .into_iter()
        .next()
        .map(|(d, name, _)| (d, name))
}

/// What the log says about the microphone opened, and why it's that one.
fn mic_report(name: &str, found: Found, wanted: &str) -> String {
    match found {
        Found::Chosen => format!("{name:?}"),
        Found::Renamed | Found::SameDevice => format!("{name:?} (chosen as {wanted:?})"),
        Found::Default if wanted.is_empty() => format!("{name:?} (the default)"),
        Found::Default => format!("{name:?} (the default, until {wanted:?} is connected)"),
        Found::First => format!("{name:?} (the first that opens; no default is set)"),
    }
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
    if record {
        shared.keep_ring(kind.track(), rate, channels, keep);
    } else {
        shared.set_ring(kind.track(), None);
    }
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
                shared.keep_ring(crate::recorder::Track::Game, 48_000, 2, keep);
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
    /// The default input when the sound was opened, to notice it changing.
    input_default_id: Option<String>,
    /// How the open microphone was found.
    mic: Option<Found>,
    /// A microphone is wanted but none is connected; `better_mic` looks for one.
    no_mic: bool,
    /// What failed to open, for the log and the status; it's tried again later.
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
    let mut mic_said = "off".to_string();
    let mut mic_note = None;
    let mut mic_error = None;
    // Sound that's still wanted stays in its ring while the devices reopen, and stays there
    // if one doesn't open: a clip saved meanwhile keeps what came before.
    for t in crate::recorder::Track::ALL {
        let wanted = match t {
            crate::recorder::Track::Game => settings.desktop_audio,
            crate::recorder::Track::Mic => settings.microphone,
            crate::recorder::Track::Chat => {
                cfg!(windows) && settings.desktop_audio && settings.chat_track
            }
        };
        if !wanted {
            shared.set_ring(t, None);
        }
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
        match default_output(&host) {
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
        // The chosen microphone can be missing for a while (unplugged, or a wireless headset
        // turned off): something close to it, or the default, is used meanwhile, and the
        // chosen one again once it's back (see `better_mic`).
        let wanted = settings.microphone_device.trim();
        open_now.input_default_id = host.default_input_device().as_ref().and_then(id_of);
        let choices = mic_choices(&host, wanted);
        open_now.no_mic = choices.is_empty();
        let mut first_error = None;
        for (d, name, found) in choices {
            match open(shared, &d, Kind::Mic, settings.microphone, failed) {
                Ok(s) => {
                    open_now.streams.push(s);
                    open_now.mic = Some(found);
                    mic_said = mic_report(&name, found, wanted);
                    if !settings.microphone {
                        mic_said = format!("off (voice clipping hears {mic_said})");
                    }
                    if found == Found::Default && !wanted.is_empty() {
                        mic_note = Some(format!(
                            "{wanted} isn't connected, so Kairo records {name} until it is."
                        ));
                    }
                    if settings.microphone {
                        mic_name = Some(name);
                    }
                    first_error = None;
                    break;
                }
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        if open_now.no_mic {
            mic_said = "none found; looking for one every few seconds".into();
            mic_error = Some(
                "No microphone found. Kairo starts using one as soon as you connect it.".into(),
            );
        } else if let Some(e) = first_error {
            mic_said = "none".into();
            mic_error = Some(format!("Couldn't open the microphone: {e}"));
            errors.push(format!("microphone: {e}"));
        }
    }
    let desktop_said = match &desktop_name {
        Some(name) => format!("{name:?}"),
        None if settings.desktop_audio => "none".into(),
        None => "off".into(),
    };
    open_now.report = format!("audio: desktop {desktop_said}, microphone {mic_said}");
    let mut st = shared.status.lock().unwrap();
    st.desktop_audio = desktop_name;
    st.microphone = mic_name;
    st.mic_note = mic_note;
    st.chat = None;
    st.audio_error = errors
        .iter()
        .cloned()
        .chain(
            open_now
                .no_mic
                .then(|| "microphone: none found".to_string()),
        )
        .reduce(|a, b| format!("{a}; {b}"));
    st.mic_error = mic_error;
    open_now.errors = errors;
    open_now
}

/// A better microphone than the open one is there now: the chosen one is back, or one was
/// connected where there was none. Returns its name, for the log.
fn better_mic(open: &Open, settings: &Settings) -> Option<String> {
    let wanted = settings.microphone_device.trim();
    let best = if wanted.is_empty() {
        Found::Default
    } else {
        Found::Chosen
    };
    let now = match open.mic {
        _ if open.no_mic => None,
        Some(f) if f > best => Some(f),
        // The best one is open, or the one found wouldn't open (that's tried again later).
        _ => return None,
    };
    let (_, name, found) = mic_choices(&cpal::default_host(), wanted)
        .into_iter()
        .next()?;
    now.is_none_or(|f| found < f).then_some(name)
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
/// on a device counts: the voice-chat split captures by process, whatever the device. The
/// microphone counts while it's the default (or the first input, with no default set).
fn defaults_changed(open: &Open, settings: &Settings) -> bool {
    let host = cpal::default_host();
    let id = |d: Option<cpal::Device>| d.as_ref().and_then(id_of);
    (cfg!(windows)
        && settings.desktop_audio
        && open.output_id.is_some()
        && id(host.default_output_device()) != open.output_id)
        || (matches!(open.mic, Some(Found::Default | Found::First))
            && id(host.default_input_device()) != open.input_default_id)
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
                if let Some(name) = better_mic(&open_now, &settings) {
                    info!("microphone {name:?} is there now; reopening");
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

    /// Inputs as Windows lists them on a PC with a USB mic, a wireless headset and virtual ones.
    fn listed() -> Vec<String> {
        [
            "CABLE Output (VB-Audio Virtual Cable)",
            "Microphone (Voicemod)",
            "Microphone (HD Pro Webcam C920)",
            "Microphone (Shure MV7+)",
            "Headset Microphone (4- Logitech G535 Gaming Headset)",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn device_names_drop_windows_numbering() {
        assert_eq!(
            plain_name("Headset Microphone (4- Logitech G535 Gaming Headset)"),
            "headset microphone (logitech g535 gaming headset)"
        );
        assert_eq!(
            plain_name("12- Microphone  (Shure MV7+) "),
            "microphone (shure mv7+)"
        );
        // A number that's part of the name stays.
        assert_eq!(
            plain_name("Mic (2-channel Realtek(R) Audio)"),
            "mic (2-channel realtek(r) audio)"
        );
        assert_eq!(
            device_part("microphone (realtek(r) audio)"),
            Some("realtek(r) audio")
        );
        assert_eq!(device_part("line in"), None);
    }

    #[test]
    fn the_chosen_microphone_is_found_by_its_name() {
        let names = listed();
        assert_eq!(
            mic_order("Microphone (Shure MV7+)", &names, Some(1)),
            [(3, Found::Chosen)]
        );
    }

    #[test]
    fn a_microphone_numbered_again_is_still_found() {
        // The headset's dongle moved to another USB port: "4-" became "2-", or went.
        let mut names = listed();
        names[4] = "Headset Microphone (2- Logitech G535 Gaming Headset)".into();
        let wanted = "Headset Microphone (4- Logitech G535 Gaming Headset)";
        assert_eq!(mic_order(wanted, &names, Some(1)), [(4, Found::Renamed)]);
        names[4] = "Headset Microphone (Logitech G535 Gaming Headset)".into();
        assert_eq!(mic_order(wanted, &names, Some(1)), [(4, Found::Renamed)]);
        // Renamed in Windows' sound settings: still the device's one input.
        names[4] = "Headset Mic (2- Logitech G535 Gaming Headset)".into();
        assert_eq!(mic_order(wanted, &names, Some(1)), [(4, Found::SameDevice)]);
    }

    #[test]
    fn another_input_of_the_same_card_isnt_the_microphone() {
        let names = [
            "Line In (Realtek(R) Audio)".to_string(),
            "Stereo Mix (Realtek(R) Audio)".to_string(),
        ];
        assert_eq!(
            mic_order("Microphone (Realtek(R) Audio)", &names, Some(1)),
            [(1, Found::Default)]
        );
    }

    #[test]
    fn a_missing_microphone_falls_back_to_the_default_then_the_first_that_opens() {
        let mut names = listed();
        names.remove(3);
        assert_eq!(
            mic_order("Microphone (Shure MV7+)", &names, Some(1)),
            [(1, Found::Default)]
        );
        assert_eq!(mic_order("", &names, Some(0)), [(0, Found::Default)]);
        // No default set: every input in turn, until one opens.
        assert_eq!(
            mic_order("", &names[..2], None),
            [(0, Found::First), (1, Found::First)]
        );
        assert_eq!(mic_order("Microphone (Shure MV7+)", &[], None), []);
    }

    #[test]
    fn the_log_says_why_a_microphone_was_used() {
        let wanted = "Microphone (Shure MV7+)";
        assert_eq!(
            mic_report("Microphone (Shure MV7+)", Found::Chosen, wanted),
            "\"Microphone (Shure MV7+)\""
        );
        assert_eq!(
            mic_report("Microphone (Voicemod)", Found::Default, wanted),
            "\"Microphone (Voicemod)\" (the default, until \"Microphone (Shure MV7+)\" is connected)"
        );
        assert_eq!(
            mic_report("Microphone (Voicemod)", Found::Default, ""),
            "\"Microphone (Voicemod)\" (the default)"
        );
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
            // Desktop sound opens the default as the device it is now.
            let now = default_output(&host).expect("the default output");
            assert!(id_of(&now).is_some() && id_of(&now) == id_of(&out));
            now.default_output_config().expect("its format");
            for d in host.input_devices().expect("inputs") {
                d.default_input_config()
                    .unwrap_or_else(|e| panic!("{d}: {e}"));
            }
        })
        .join()
        .unwrap();
    }
}
