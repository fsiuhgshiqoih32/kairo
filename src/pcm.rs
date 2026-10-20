//! Recent audio in memory, placed on the wall clock so it can be cut to match the video. Kept
//! as 16-bit samples: half the memory of floats, and as good as the AAC a clip gets.
//!
//! Capture callbacks deliver blocks of samples at slightly irregular moments. Within an unbroken
//! run, samples are placed by count from the run's start (the device clock is steadier than the
//! callback times). A block that arrives clearly later than the run predicts starts a new run:
//! loopback capture delivers nothing while nothing plays, and that silence must stay silence.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// A gap or overlap bigger than this between where a block should start and where its arrival
/// time puts it starts a new run.
const RESYNC_US: i64 = 40_000;

struct Block {
    start_us: i64,
    samples: Vec<i16>,
}

fn to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

fn to_f32(s: i16) -> f32 {
    s as f32 / 32767.0
}

pub struct PcmRing {
    pub rate: u32,
    pub channels: u16,
    keep_us: i64,
    blocks: VecDeque<Block>,
    /// Where the next block of the current run starts.
    next_us: Option<i64>,
    /// Everything from a moment on, also going to a file (a recorded session).
    tape: Option<PcmTape>,
}

/// A 16-bit WAV file that grows with the sound from `start_us` on, silence filling the gaps.
pub struct PcmTape {
    pub path: PathBuf,
    start_us: i64,
    rate: u32,
    channels: u16,
    frames: u64,
    file: BufWriter<File>,
}

impl PcmTape {
    pub fn create(path: &Path, start_us: i64, rate: u32, channels: u16) -> std::io::Result<Self> {
        let mut file = BufWriter::with_capacity(1 << 20, File::create(path)?);
        file.write_all(&wav_header(rate, channels, 0))?;
        Ok(PcmTape {
            path: path.to_path_buf(),
            start_us,
            rate: rate.max(1),
            channels: channels.max(1),
            frames: 0,
            file,
        })
    }

    /// The tape's own sample rate and channels (sound in another format is converted to them).
    pub fn format(&self) -> (u32, u16) {
        (self.rate, self.channels)
    }

    /// Writes a block captured at `rate` with `channels` that starts at `start_us`, converted to
    /// the tape's own format if it differs (the device changed while a session ran).
    fn write_from(&mut self, start_us: i64, samples: &[i16], rate: u32, channels: u16) {
        if (rate, channels) == (self.rate, self.channels) {
            self.write(start_us, samples);
            return;
        }
        let f: Vec<f32> = samples.iter().map(|s| to_f32(*s)).collect();
        let converted: Vec<i16> = convert(&f, rate, channels, self.rate, self.channels)
            .into_iter()
            .map(to_i16)
            .collect();
        self.write(start_us, &converted);
    }

    /// Writes a block that starts at `start_us`: silence first if it starts after the tape's
    /// end, its overlapping start dropped if before.
    fn write(&mut self, start_us: i64, samples: &[i16]) {
        let ch = self.channels as usize;
        let rate = self.rate as i128;
        let at = ((start_us - self.start_us) as i128 * rate).div_euclid(1_000_000) as i64;
        let gap = at - self.frames as i64;
        let mut skip = 0usize;
        if gap > 0 {
            // In pieces: an hour of silence (nothing played) is hundreds of megabytes, too much
            // to allocate at once inside an audio callback.
            const CHUNK: usize = 64 * 1024;
            let zeros = [0u8; CHUNK];
            let mut left = gap as usize * ch * 2;
            while left > 0 {
                let n = left.min(CHUNK);
                if self.file.write_all(&zeros[..n]).is_err() {
                    return;
                }
                left -= n;
            }
            self.frames += gap as u64;
        } else if gap < 0 {
            skip = (-gap) as usize;
        }
        let frames = samples.len() / ch;
        if skip >= frames {
            return;
        }
        let mut bytes = Vec::with_capacity((frames - skip) * ch * 2);
        for s in &samples[skip * ch..frames * ch] {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        if self.file.write_all(&bytes).is_ok() {
            self.frames += (frames - skip) as u64;
        }
    }

    /// Closes the file with its real length in the header.
    pub fn finish(mut self) -> std::io::Result<PathBuf> {
        let data = self.frames * self.channels as u64 * 2;
        self.file.flush()?;
        let mut f = self.file.into_inner().map_err(|e| e.into_error())?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&wav_header(
            self.rate,
            self.channels,
            data.min(u32::MAX as u64 - 44) as u32,
        ))?;
        Ok(self.path)
    }
}

/// A 16-bit PCM WAV header for `data_len` bytes of samples.
fn wav_header(rate: u32, channels: u16, data_len: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(44);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
    v.extend_from_slice(&(channels * 2).to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    v
}

impl PcmRing {
    pub fn new(rate: u32, channels: u16, keep_us: i64) -> Self {
        PcmRing {
            rate: rate.max(1),
            channels: channels.max(1),
            keep_us,
            blocks: VecDeque::new(),
            next_us: None,
            tape: None,
        }
    }

    /// Starts writing everything from `start_us` on to a WAV file at `path`, beginning with
    /// what's already held after that moment.
    pub fn start_tape(&mut self, path: &Path, start_us: i64) -> std::io::Result<()> {
        let mut tape = PcmTape::create(path, start_us, self.rate, self.channels)?;
        for b in &self.blocks {
            tape.write(b.start_us, &b.samples);
        }
        self.tape = Some(tape);
        Ok(())
    }

    /// Stops the tape, if one runs.
    pub fn take_tape(&mut self) -> Option<PcmTape> {
        self.tape.take()
    }

    /// Carries on `tape` (a session's file) from this ring's sound, from now on.
    pub fn adopt_tape(&mut self, tape: PcmTape) {
        self.tape = Some(tape);
    }

    pub fn set_keep_us(&mut self, keep_us: i64) {
        self.keep_us = keep_us;
    }

    fn frames_to_us(&self, frames: usize) -> i64 {
        frames as i64 * 1_000_000 / self.rate as i64
    }

    /// Adds interleaved samples whose last one was captured at `end_us` (wall clock).
    pub fn push(&mut self, samples: &[f32], end_us: i64) {
        let frames = samples.len() / self.channels as usize;
        if frames == 0 {
            return;
        }
        let by_clock = end_us - self.frames_to_us(frames);
        let start = match self.next_us {
            Some(n) if (by_clock - n).abs() <= RESYNC_US => n,
            _ => by_clock,
        };
        self.next_us = Some(start + self.frames_to_us(frames));
        let block = Block {
            start_us: start,
            samples: samples[..frames * self.channels as usize]
                .iter()
                .map(|s| to_i16(*s))
                .collect(),
        };
        if let Some(t) = &mut self.tape {
            t.write_from(block.start_us, &block.samples, self.rate, self.channels);
        }
        self.blocks.push_back(block);
        let cutoff = end_us - self.keep_us;
        while let Some(b) = self.blocks.front() {
            let b_end = b.start_us + self.frames_to_us(b.samples.len() / self.channels as usize);
            if b_end < cutoff {
                self.blocks.pop_front();
            } else {
                break;
            }
        }
    }

    /// Interleaved samples covering `from_us..to_us`, with silence where nothing was captured.
    pub fn extract(&self, from_us: i64, to_us: i64) -> Vec<f32> {
        let ch = self.channels as usize;
        let frames = ((to_us - from_us).max(0) as i128 * self.rate as i128 / 1_000_000) as usize;
        let mut out = vec![0.0f32; frames * ch];
        for b in &self.blocks {
            let offset =
                ((b.start_us - from_us) as i128 * self.rate as i128).div_euclid(1_000_000) as i64;
            let n = (b.samples.len() / ch) as i64;
            let lo = offset.max(0);
            let hi = (offset + n).min(frames as i64);
            if lo >= hi {
                continue;
            }
            let src = ((lo - offset) as usize) * ch..((hi - offset) as usize) * ch;
            for (o, s) in out[lo as usize * ch..hi as usize * ch]
                .iter_mut()
                .zip(&b.samples[src])
            {
                *o = to_f32(*s);
            }
        }
        out
    }

    /// Seconds of audio held (including silent gaps).
    pub fn seconds(&self) -> f64 {
        match (self.blocks.front(), self.next_us) {
            (Some(b), Some(end)) => (end - b.start_us) as f64 / 1e6,
            _ => 0.0,
        }
    }
}

/// A 32-bit float WAV file.
pub fn wav_f32(rate: u32, channels: u16, samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 4) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * channels as u32 * 4).to_le_bytes());
    v.extend_from_slice(&(channels * 4).to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

/// Above this, a boosted sample is eased towards full scale instead of clipping.
const KNEE: f32 = 0.85;

/// Makes `samples` louder or quieter by `gain` (1 is as recorded). Peaks past the knee are
/// softened so a boosted microphone doesn't crackle.
pub fn apply_gain(samples: &mut [f32], gain: f32) {
    if (gain - 1.0).abs() < 1e-4 {
        return;
    }
    for s in samples {
        let v = *s * gain;
        let a = v.abs();
        *s = if a <= KNEE {
            v
        } else {
            let room = 1.0 - KNEE;
            v.signum() * (KNEE + room * ((a - KNEE) / room).tanh())
        };
    }
}

/// The loudest sample, 0 to 1.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs())).min(1.0)
}

/// A level for a meter, 0 to 1, from a peak: -60 dB and below is empty, 0 dB is full.
pub fn meter(peak: f32) -> f32 {
    if peak <= 0.001 {
        return 0.0;
    }
    ((20.0 * peak.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Interleaved sound at `from_rate` with `from_ch` channels, converted for an output at `to_rate`
/// with `to_ch` channels (linear interpolation; extra output channels repeat the last input one).
pub fn convert(
    samples: &[f32],
    from_rate: u32,
    from_ch: u16,
    to_rate: u32,
    to_ch: u16,
) -> Vec<f32> {
    let (fc, tc) = (from_ch.max(1) as usize, to_ch.max(1) as usize);
    let frames = samples.len() / fc;
    if frames == 0 {
        return Vec::new();
    }
    let out_frames = (frames as u64 * to_rate as u64 / from_rate.max(1) as u64) as usize;
    let step = from_rate as f64 / to_rate.max(1) as f64;
    let mut out = Vec::with_capacity(out_frames * tc);
    for j in 0..out_frames {
        let t = j as f64 * step;
        let i = (t.floor() as usize).min(frames - 1);
        let next = (i + 1).min(frames - 1);
        let k = (t - i as f64) as f32;
        for c in 0..tc {
            let ch = c.min(fc - 1);
            let (a, b) = (samples[i * fc + ch], samples[next * fc + ch]);
            out.push(a + (b - a) * k);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_792_347_000_000_000;

    /// Sample `n` of a test ramp: a distinct 16-bit value for each.
    fn ramp(n: i64) -> f32 {
        to_f32((n % 30_000) as i16)
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1.0 / 32767.0
    }

    #[test]
    fn jittery_blocks_stay_continuous() {
        // 48 kHz mono, 10 ms blocks, arrival times off by up to ±3 ms.
        let mut r = PcmRing::new(48_000, 1, 10_000_000);
        let jitter = [0, 3000, -2000, 1000, -3000];
        for i in 0..100i64 {
            let block: Vec<f32> = (0..480).map(|k| ramp(i * 480 + k)).collect();
            r.push(&block, T0 + (i + 1) * 10_000 + jitter[i as usize % 5]);
        }
        let out = r.extract(T0, T0 + 1_000_000);
        assert_eq!(out.len(), 48_000);
        // The whole ramp, exactly: no dropped or doubled samples anywhere.
        for (n, s) in out.iter().enumerate() {
            assert_eq!(*s, ramp(n as i64), "sample {n}");
        }
    }

    #[test]
    fn silence_between_runs() {
        let mut r = PcmRing::new(1000, 2, 10_000_000);
        r.push(&[0.25; 200], T0 + 100_000); // 100 frames: 0..100 ms
        r.push(&[-0.5; 200], T0 + 600_000); // nothing played for 400 ms, then 500..600 ms
        let out = r.extract(T0, T0 + 700_000);
        assert_eq!(out.len(), 700 * 2);
        assert!(near(out[0], 0.25));
        assert!(near(out[199], 0.25));
        assert_eq!(out[200], 0.0);
        assert_eq!(out[999], 0.0);
        assert!(near(out[1000], -0.5));
        assert!(near(out[1199], -0.5));
        assert_eq!(out[1200], 0.0);
    }

    #[test]
    fn window_partly_before_the_buffer() {
        let mut r = PcmRing::new(1000, 1, 10_000_000);
        r.push(&[0.5; 100], T0 + 1_100_000);
        let out = r.extract(T0, T0 + 1_100_000);
        assert_eq!(out.len(), 1100);
        assert!(out[..1000].iter().all(|s| *s == 0.0));
        assert!(out[1000..].iter().all(|s| near(*s, 0.5)));
    }

    #[test]
    fn loud_samples_clip_instead_of_wrapping() {
        let mut r = PcmRing::new(1000, 1, 10_000_000);
        r.push(&[3.0, -3.0, 1.0, -1.0], T0 + 4_000);
        assert_eq!(r.extract(T0, T0 + 4_000), [1.0, -1.0, 1.0, -1.0]);
    }

    #[test]
    fn old_blocks_drop_off() {
        let mut r = PcmRing::new(1000, 1, 2_000_000);
        for i in 0..50 {
            r.push(&[0.5; 100], T0 + (i + 1) * 100_000);
        }
        assert!(r.seconds() <= 2.2, "{}", r.seconds());
    }

    #[test]
    fn wav_header() {
        let w = wav_f32(48_000, 2, &[0.0, 1.0, -1.0, 0.5]);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes([w[20], w[21]]), 3);
        assert_eq!(u32::from_le_bytes([w[40], w[41], w[42], w[43]]), 16);
        assert_eq!(w.len(), 44 + 16);
    }

    #[test]
    fn a_tape_keeps_everything_from_its_start_with_gaps_as_silence() {
        let dir = std::env::temp_dir().join(format!("kairo-tape-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        // 1 kHz mono: 100 frames = 100 ms.
        let mut r = PcmRing::new(1000, 1, 10_000_000);
        r.push(&[0.25; 100], T0 + 100_000); // 0..100 ms, held before the tape starts
        r.start_tape(&path, T0 + 50_000).unwrap(); // the tape starts at 50 ms
        r.push(&[0.5; 100], T0 + 200_000); // 100..200 ms
        r.push(&[-0.5; 100], T0 + 700_000); // nothing for 400 ms, then 600..700 ms
        let wav = r.take_tape().unwrap().finish().unwrap();
        let bytes = std::fs::read(&wav).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        let data = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as usize;
        assert_eq!(data, bytes.len() - 44);
        let s: Vec<i16> = bytes[44..]
            .chunks(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        // 50 ms of the first block, 100 of the second, 400 of silence, 100 of the third.
        assert_eq!(s.len(), 650);
        assert!(s[..50].iter().all(|v| *v == to_i16(0.25)));
        assert!(s[50..150].iter().all(|v| *v == to_i16(0.5)));
        assert!(s[150..550].iter().all(|v| *v == 0));
        assert!(s[550..].iter().all(|v| *v == to_i16(-0.5)));
        // Later sound doesn't go to a stopped tape.
        r.push(&[0.5; 100], T0 + 800_000);
        assert_eq!(std::fs::read(&wav).unwrap().len(), 44 + 650 * 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_tape_carries_on_in_a_new_ring_of_another_format() {
        let dir = std::env::temp_dir().join(format!("kairo-tape2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        // The session starts on a 1 kHz mono device...
        let mut a = PcmRing::new(1000, 1, 10_000_000);
        a.start_tape(&path, T0).unwrap();
        a.push(&[0.5; 100], T0 + 100_000);
        // ...which is swapped for a 2 kHz stereo one (headphones plugged in) 200 ms later.
        let tape = a.take_tape().unwrap();
        assert_eq!(tape.format(), (1000, 1));
        let mut b = PcmRing::new(2000, 2, 10_000_000);
        b.adopt_tape(tape);
        b.push(&[-0.5; 400], T0 + 400_000); // 200 stereo frames: 300..400 ms
        let wav = b.take_tape().unwrap().finish().unwrap();
        let bytes = std::fs::read(&wav).unwrap();
        let s: Vec<i16> = bytes[44..]
            .chunks(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        // Still 1 kHz mono: 100 ms of sound, 200 ms of silence while it changed, 100 ms more.
        assert_eq!(s.len(), 400);
        assert!(s[..100].iter().all(|v| *v == to_i16(0.5)));
        assert!(s[100..300].iter().all(|v| *v == 0));
        assert!(s[300..].iter().all(|v| *v == to_i16(-0.5)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn long_silence_is_written_in_pieces() {
        let dir = std::env::temp_dir().join(format!("kairo-tape3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut r = PcmRing::new(48_000, 2, 10_000_000);
        r.start_tape(&dir.join("t.wav"), T0).unwrap();
        // Ten minutes of nothing, then a block: 10 * 60 * 48000 * 4 bytes of silence.
        r.push(&[0.1; 96], T0 + 600_000_000 + 1_000);
        let wav = r.take_tape().unwrap().finish().unwrap();
        let len = std::fs::metadata(&wav).unwrap().len();
        assert_eq!(len, 44 + (600 * 48_000 + 48) * 4);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mic_volume_boosts_without_clipping() {
        let mut s = vec![0.1, -0.2, 0.5, -0.9];
        apply_gain(&mut s, 1.0);
        assert_eq!(s, vec![0.1, -0.2, 0.5, -0.9], "100% leaves it alone");
        apply_gain(&mut s, 2.0);
        assert!((s[0] - 0.2).abs() < 1e-6 && (s[1] + 0.4).abs() < 1e-6);
        // Pushed past full scale, peaks bend towards it but never pass it, keeping their sign.
        assert!(s[2] > KNEE && s[2] < 1.0);
        assert!(s[3] < -KNEE && s[3] >= -1.0);
        let mut quiet = vec![0.8];
        apply_gain(&mut quiet, 0.5);
        assert!((quiet[0] - 0.4).abs() < 1e-6);
        assert_eq!(peak(&[0.2, -0.7, 0.1]), 0.7);
        assert_eq!(meter(1.0), 1.0);
        assert_eq!(meter(0.0), 0.0);
        assert!(
            (meter(0.1) - 2.0 / 3.0).abs() < 1e-3,
            "-20 dB is two thirds up"
        );
    }

    #[test]
    fn converts_for_play_back() {
        // Mono 1 kHz to stereo 2 kHz: twice the frames, each sample on both channels.
        let out = convert(&[0.0, 1.0], 1000, 1, 2000, 2);
        assert_eq!(out, vec![0.0, 0.0, 0.5, 0.5, 1.0, 1.0, 1.0, 1.0]);
        // Stereo 48 kHz to stereo 24 kHz: half the frames, channels kept apart.
        let input: Vec<f32> = (0..8).map(|i| i as f32).collect();
        assert_eq!(
            convert(&input, 48_000, 2, 24_000, 2),
            vec![0.0, 1.0, 4.0, 5.0]
        );
        assert!(convert(&[], 48_000, 2, 44_100, 2).is_empty());
    }
}
