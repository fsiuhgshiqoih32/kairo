//! Settings' mic test: a live level for the chosen microphone, and a few seconds recorded and
//! played back, both at the mic volume set, so you hear what clips will get. It runs on a thread
//! of its own (cpal streams stay on the thread that opened them) and stops when dropped.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// How long "Record and play back" records.
pub const RECORD_SECONDS: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Listening,
    /// Recording, until then.
    Recording(Instant),
    /// Playing the recording back, until then.
    Playing(Instant),
}

pub struct MicTest {
    stop: Arc<AtomicBool>,
    /// The loudest sample since the level was last read (f32 bits).
    peak: Arc<AtomicU32>,
    gain: Arc<AtomicU32>,
    record: Arc<AtomicBool>,
    phase: Arc<Mutex<Phase>>,
    error: Arc<Mutex<Option<String>>>,
    /// The microphone being tested.
    pub name: Arc<Mutex<String>>,
}

impl MicTest {
    /// Starts listening to `device` (by name; empty for the system's default) at `gain`.
    pub fn start(device: &str, gain: f32) -> MicTest {
        let t = MicTest {
            stop: Arc::default(),
            peak: Arc::default(),
            gain: Arc::new(AtomicU32::new(gain.to_bits())),
            record: Arc::default(),
            phase: Arc::new(Mutex::new(Phase::Listening)),
            error: Arc::default(),
            name: Arc::default(),
        };
        let (stop, peak, gain, record, phase, error, name) = (
            t.stop.clone(),
            t.peak.clone(),
            t.gain.clone(),
            t.record.clone(),
            t.phase.clone(),
            t.error.clone(),
            t.name.clone(),
        );
        let wanted = device.trim().to_string();
        let _ = std::thread::Builder::new()
            .name("mic test".into())
            .spawn(move || {
                if let Err(e) = run(&wanted, &stop, &peak, &gain, &record, &phase, &name) {
                    *error.lock().unwrap() = Some(e);
                }
            });
        t
    }

    /// A test that opens nothing, for previews.
    #[cfg(test)]
    pub fn idle(name: &str) -> MicTest {
        let t = MicTest {
            stop: Arc::default(),
            peak: Arc::default(),
            gain: Arc::default(),
            record: Arc::default(),
            phase: Arc::new(Mutex::new(Phase::Listening)),
            error: Arc::default(),
            name: Arc::default(),
        };
        *t.name.lock().unwrap() = name.to_string();
        t
    }

    /// The loudest sample since the last call, 0 to 1.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }

    pub fn set_gain(&self, gain: f32) {
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    /// Records a few seconds, then plays them back.
    pub fn record(&self) {
        if *self.phase.lock().unwrap() == Phase::Listening {
            self.record.store(true, Ordering::SeqCst);
        }
    }

    pub fn phase(&self) -> Phase {
        *self.phase.lock().unwrap()
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }
}

impl Drop for MicTest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn run(
    wanted: &str,
    stop: &AtomicBool,
    peak: &Arc<AtomicU32>,
    gain: &Arc<AtomicU32>,
    record: &AtomicBool,
    phase: &Mutex<Phase>,
    name: &Mutex<String>,
) -> Result<(), String> {
    // The one clips get, so the test hears what they will.
    let (device, found) = crate::audio::microphone_for(wanted).ok_or("No microphone found.")?;
    *name.lock().unwrap() = found;
    let config = device.default_input_config().map_err(|e| e.to_string())?;
    let (rate, channels) = (config.sample_rate(), config.channels());
    let take: Arc<Mutex<Option<Vec<f32>>>> = Arc::default();
    let stream = {
        let (peak, gain, take) = (peak.clone(), gain.clone(), take.clone());
        device
            .build_input_stream_raw(
                config.config(),
                config.sample_format(),
                move |data: &cpal::Data, _: &cpal::InputCallbackInfo| {
                    let Some(mut s) = crate::audio::to_f32(data) else {
                        return;
                    };
                    kairo::pcm::apply_gain(&mut s, f32::from_bits(gain.load(Ordering::Relaxed)));
                    let p = kairo::pcm::peak(&s);
                    let _ = peak.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                        (p > f32::from_bits(old)).then_some(p.to_bits())
                    });
                    if let Some(t) = take.lock().unwrap().as_mut() {
                        t.extend_from_slice(&s);
                    }
                },
                |e| crate::log::warning!("mic test: {e}"),
                None,
            )
            .map_err(|e| e.to_string())?
    };
    stream.play().map_err(|e| e.to_string())?;
    let wanted_samples = rate as usize * channels as usize * RECORD_SECONDS as usize;
    let mut playing: Option<cpal::Stream> = None;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(30));
        let now = Instant::now();
        let current = *phase.lock().unwrap();
        match current {
            Phase::Listening if record.swap(false, Ordering::SeqCst) => {
                *take.lock().unwrap() = Some(Vec::with_capacity(wanted_samples));
                *phase.lock().unwrap() =
                    Phase::Recording(now + Duration::from_secs(RECORD_SECONDS as u64));
            }
            Phase::Recording(until) => {
                let done = take
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|t| t.len() >= wanted_samples);
                // A mic that sends nothing (or too little) mustn't leave the test recording forever.
                let late = now > until + Duration::from_secs(2);
                if done || late {
                    let got = take.lock().unwrap().take().unwrap_or_default();
                    if got.is_empty() {
                        *phase.lock().unwrap() = Phase::Listening;
                        return Err(
                            "The microphone sent no sound. Is it muted or unplugged?".into()
                        );
                    }
                    let (out, secs) = play(&got, rate, channels)?;
                    playing = Some(out);
                    *phase.lock().unwrap() = Phase::Playing(now + secs);
                }
            }
            Phase::Playing(until) if now >= until => {
                playing = None;
                *phase.lock().unwrap() = Phase::Listening;
            }
            _ => {}
        }
    }
    drop(playing);
    drop(stream);
    Ok(())
}

/// Plays `samples` (at `rate`, `channels`) on the default output; returns the stream and how
/// long it runs.
fn play(samples: &[f32], rate: u32, channels: u16) -> Result<(cpal::Stream, Duration), String> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("No sound output to play the test on.")?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let (out_rate, out_ch) = (config.sample_rate(), config.channels());
    let sound = kairo::pcm::convert(samples, rate, channels, out_rate, out_ch);
    let secs = Duration::from_secs_f64(
        sound.len() as f64 / (out_rate as f64 * out_ch.max(1) as f64) + 0.3,
    );
    let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(sound)));
    let err = |e: cpal::Error| crate::log::warning!("mic test play-back: {e}");
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            let q = queue.clone();
            device.build_output_stream(
                config.config(),
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut q = q.lock().unwrap();
                    for v in data.iter_mut() {
                        *v = q.pop_front().unwrap_or(0.0);
                    }
                },
                err,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let q = queue.clone();
            device.build_output_stream(
                config.config(),
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    let mut q = q.lock().unwrap();
                    for v in data.iter_mut() {
                        *v = (q.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0) * 32767.0) as i16;
                    }
                },
                err,
                None,
            )
        }
        f => return Err(format!("unsupported output format {f:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, secs))
}
