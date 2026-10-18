//! Clip playback for the editor: ffmpeg decodes frames and sound from a position on, the sound
//! plays through the default output device, and frames are shown when the sound reaches them.
//! Seeking or pausing stops the decoders; playing starts new ones at the position.

use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use rewindr::media::{self, Info};

use crate::ffmpeg;

pub struct Frame {
    pub time: f64,
    pub rgba: Vec<u8>,
}

/// An ffmpeg decoding frames from `start` on; frames arrive on `rx`, `None` at the end.
struct VideoDecoder {
    child: Child,
    rx: Receiver<Option<Frame>>,
}

impl VideoDecoder {
    fn start(
        ff: &Path,
        clip: &Path,
        start: f64,
        size: (u32, u32),
        fps: f64,
        frames: Option<u32>,
    ) -> Result<Self, String> {
        let mut child = ffmpeg::command(ff)
            .args(media::video_args(clip, start, size, frames))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start ffmpeg: {e}"))?;
        let mut out = child.stdout.take().expect("piped");
        let (tx, rx): (SyncSender<Option<Frame>>, _) = sync_channel(4);
        let frame_bytes = size.0 as usize * size.1 as usize * 4;
        let fps = if fps > 0.0 { fps } else { 30.0 };
        std::thread::spawn(move || {
            let mut n = 0u64;
            loop {
                let mut buf = vec![0u8; frame_bytes];
                if out.read_exact(&mut buf).is_err() {
                    let _ = tx.send(None);
                    return;
                }
                let time = start + n as f64 / fps;
                n += 1;
                if tx.send(Some(Frame { time, rgba: buf })).is_err() {
                    return;
                }
            }
        });
        Ok(VideoDecoder { child, rx })
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Decoded sound waiting to be played, and how much has been played.
struct AudioShared {
    buf: Mutex<VecDeque<f32>>,
    played_frames: AtomicU64,
    volume: AtomicU32,
    eof: AtomicBool,
    /// Sound stays silent (and the clock still) until the first frame is ready to show.
    open: AtomicBool,
}

/// An ffmpeg decoding sound from `start` on into a buffer the output device drains.
struct AudioOut {
    child: Child,
    _stream: cpal::Stream,
    shared: Arc<AudioShared>,
    rate: u32,
    stop: Arc<AtomicBool>,
}

impl AudioOut {
    fn start(
        ff: &Path,
        clip: &Path,
        start: f64,
        track: usize,
        volume: f32,
    ) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no sound output device")?;
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        let rate = config.sample_rate();
        let channels = config.channels();
        let shared = Arc::new(AudioShared {
            buf: Mutex::new(VecDeque::with_capacity(rate as usize * channels as usize)),
            played_frames: AtomicU64::new(0),
            volume: AtomicU32::new(volume.to_bits()),
            eof: AtomicBool::new(false),
            open: AtomicBool::new(false),
        });
        let mut child = ffmpeg::command(ff)
            .args(media::audio_args(clip, start, track, rate, channels))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start ffmpeg: {e}"))?;
        let mut out = child.stdout.take().expect("piped");
        let stop = Arc::new(AtomicBool::new(false));
        {
            let shared = shared.clone();
            let stop = stop.clone();
            // Half a second of sound ahead is plenty; more would only delay seeking.
            let ahead = rate as usize * channels as usize / 2;
            std::thread::spawn(move || {
                let mut bytes = vec![0u8; 16 * 1024];
                let mut carry: Vec<u8> = Vec::new();
                loop {
                    while shared.buf.lock().unwrap().len() > ahead {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    match out.read(&mut bytes) {
                        Ok(0) | Err(_) => {
                            shared.eof.store(true, Ordering::SeqCst);
                            return;
                        }
                        Ok(n) => {
                            carry.extend_from_slice(&bytes[..n]);
                            let usable = carry.len() - carry.len() % 4;
                            let mut b = shared.buf.lock().unwrap();
                            for c in carry[..usable].chunks_exact(4) {
                                b.push_back(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                            }
                            carry.drain(..usable);
                        }
                    }
                }
            });
        }
        let ch = channels as usize;
        let fill = {
            let shared = shared.clone();
            move |out: &mut dyn FnMut(usize, f32), len: usize| {
                let vol = f32::from_bits(shared.volume.load(Ordering::Relaxed));
                let mut b = shared.buf.lock().unwrap();
                let frames = if shared.open.load(Ordering::Relaxed) {
                    (len / ch).min(b.len() / ch)
                } else {
                    0
                };
                for i in 0..frames * ch {
                    out(i, b.pop_front().unwrap_or(0.0) * vol);
                }
                for i in frames * ch..len {
                    out(i, 0.0);
                }
                shared
                    .played_frames
                    .fetch_add(frames as u64, Ordering::Relaxed);
            }
        };
        let err = |e: cpal::Error| crate::log::warning!("sound output: {e}");
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream(
                config.config(),
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let len = data.len();
                    fill(&mut |i, v| data[i] = v, len);
                },
                err,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream(
                config.config(),
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    let len = data.len();
                    fill(
                        &mut |i, v| data[i] = (v.clamp(-1.0, 1.0) * 32767.0) as i16,
                        len,
                    );
                },
                err,
                None,
            ),
            f => return Err(format!("unsupported output format {f:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(AudioOut {
            child,
            _stream: stream,
            shared,
            rate,
            stop,
        })
    }

    fn played_secs(&self) -> f64 {
        self.shared.played_frames.load(Ordering::Relaxed) as f64 / self.rate as f64
    }

    /// The decoder finished and everything decoded has been played.
    fn drained(&self) -> bool {
        self.shared.eof.load(Ordering::SeqCst) && self.shared.buf.lock().unwrap().is_empty()
    }
}

impl Drop for AudioOut {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Playback {
    start: f64,
    video: VideoDecoder,
    audio: Option<AudioOut>,
    pending: Option<Frame>,
    video_done: bool,
    /// When the first frame arrived: the clock for clips without sound.
    first_frame_at: Option<Instant>,
    /// The audio clock's last value and when it changed, to keep time once the sound ends.
    last_audio: (f64, Instant),
}

pub struct Player {
    ff: PathBuf,
    pub clip: PathBuf,
    pub info: Info,
    /// The size frames are decoded at.
    pub size: (u32, u32),
    playing: Option<Playback>,
    /// The time of the frame on screen (or being fetched).
    pub position: f64,
    /// A new frame to show, taken by the window.
    pub frame: Option<Frame>,
    still: Option<VideoDecoder>,
    still_wanted: Option<f64>,
    pub volume: f32,
    pub track: usize,
    pub error: Option<String>,
}

impl Player {
    pub fn new(ff: PathBuf, clip: PathBuf, info: Info, max: (u32, u32)) -> Player {
        let size = media::fit((info.width, info.height), max);
        let mut p = Player {
            ff,
            clip,
            info,
            size,
            playing: None,
            position: 0.0,
            frame: None,
            still: None,
            still_wanted: None,
            volume: 1.0,
            track: 0,
            error: None,
        };
        p.request_still(0.0);
        p
    }

    #[cfg(test)]
    pub fn has_pending_still(&self) -> bool {
        self.still.is_some() || self.still_wanted.is_some()
    }

    pub fn is_playing(&self) -> bool {
        self.playing.is_some()
    }

    pub fn duration(&self) -> f64 {
        self.info.duration
    }

    pub fn frame_step(&self) -> f64 {
        1.0 / if self.info.fps > 0.0 {
            self.info.fps
        } else {
            30.0
        }
    }

    pub fn play(&mut self) {
        if self.playing.is_some() {
            return;
        }
        self.still = None;
        self.still_wanted = None;
        let start = self.position.clamp(0.0, self.duration());
        let video = match VideoDecoder::start(
            &self.ff,
            &self.clip,
            start,
            self.size,
            self.info.fps,
            None,
        ) {
            Ok(v) => v,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        let audio = if self.info.audio_tracks.is_empty() {
            None
        } else {
            let track = self.track.min(self.info.audio_tracks.len() - 1);
            match AudioOut::start(&self.ff, &self.clip, start, track, self.volume) {
                Ok(a) => Some(a),
                Err(e) => {
                    crate::log::warning!("playing without sound: {e}");
                    None
                }
            }
        };
        self.playing = Some(Playback {
            start,
            video,
            audio,
            pending: None,
            video_done: false,
            first_frame_at: None,
            last_audio: (start, Instant::now()),
        });
    }

    pub fn pause(&mut self) {
        if self.playing.take().is_some() {
            // The frame on screen stays; the position is where it is.
        }
    }

    /// Jumps to `t`: playback restarts there, or the frame there is fetched.
    pub fn seek(&mut self, t: f64) {
        let t = t.clamp(0.0, self.duration());
        let was_playing = self.playing.take().is_some();
        self.position = t;
        if was_playing {
            self.play();
        } else {
            self.request_still(t);
        }
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v;
        if let Some(a) = self.playing.as_ref().and_then(|p| p.audio.as_ref()) {
            a.shared.volume.store(v.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn set_track(&mut self, track: usize) {
        if track != self.track {
            self.track = track;
            if self.is_playing() {
                let t = self.position;
                self.seek(t);
            }
        }
    }

    /// Stops everything reading the file (before renaming or deleting it).
    pub fn release_file(&mut self) {
        self.playing = None;
        self.still = None;
    }

    fn request_still(&mut self, t: f64) {
        if self.still.is_some() {
            // One at a time; fetch the newest position when this one is done.
            self.still_wanted = Some(t);
            return;
        }
        match VideoDecoder::start(&self.ff, &self.clip, t, self.size, self.info.fps, Some(1)) {
            Ok(d) => self.still = Some(d),
            Err(e) => self.error = Some(e),
        }
    }

    /// Advances playback; call every frame. Playback stops at `end`. True if something changed.
    pub fn tick(&mut self, end: f64) -> bool {
        let mut changed = false;
        if let Some(d) = &self.still {
            match d.rx.try_recv() {
                Ok(Some(f)) => {
                    self.frame = Some(f);
                    self.still = None;
                    changed = true;
                }
                Ok(None) | Err(TryRecvError::Disconnected) => self.still = None,
                Err(TryRecvError::Empty) => {}
            }
            if self.still.is_none()
                && let Some(t) = self.still_wanted.take()
            {
                self.request_still(t);
            }
        }
        let duration = self.info.duration;
        let Some(p) = &mut self.playing else {
            return changed;
        };
        let now = Instant::now();
        let clock = match &p.audio {
            Some(a) => {
                let t = p.start + a.played_secs();
                if t > p.last_audio.0 {
                    p.last_audio = (t, now);
                }
                if a.drained() {
                    p.last_audio.0 + now.duration_since(p.last_audio.1).as_secs_f64()
                } else {
                    t
                }
            }
            None => match p.first_frame_at {
                Some(at) => p.start + now.duration_since(at).as_secs_f64(),
                None => p.start,
            },
        };
        loop {
            if p.pending.is_none() && !p.video_done {
                match p.video.rx.try_recv() {
                    Ok(Some(f)) => {
                        if p.first_frame_at.is_none() {
                            p.first_frame_at = Some(now);
                            p.last_audio = (p.start, now);
                            if let Some(a) = &p.audio {
                                a.shared.open.store(true, Ordering::SeqCst);
                            }
                        }
                        p.pending = Some(f);
                    }
                    Ok(None) | Err(TryRecvError::Disconnected) => p.video_done = true,
                    Err(TryRecvError::Empty) => {}
                }
            }
            match &p.pending {
                Some(f) if f.time <= clock => {
                    self.frame = p.pending.take();
                    changed = true;
                }
                _ => break,
            }
        }
        if p.first_frame_at.is_some() {
            self.position = clock.min(duration);
        }
        let finished = p.video_done && p.pending.is_none();
        if self.position >= end || finished {
            self.position = self.position.min(end);
            self.playing = None;
            changed = true;
        }
        changed
    }
}
