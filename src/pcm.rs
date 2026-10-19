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

    /// Writes a block that starts at `start_us`: silence first if it starts after the tape's
    /// end, its overlapping start dropped if before.
    fn write(&mut self, start_us: i64, samples: &[i16]) {
        let ch = self.channels as usize;
        let rate = self.rate as i128;
        let at = ((start_us - self.start_us) as i128 * rate).div_euclid(1_000_000) as i64;
        let gap = at - self.frames as i64;
        let mut skip = 0usize;
        if gap > 0 {
            let silence = vec![0u8; gap as usize * ch * 2];
            if self.file.write_all(&silence).is_err() {
                return;
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
            t.write(block.start_us, &block.samples);
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
        let dir = std::env::temp_dir().join(format!("rewindr-tape-{}", std::process::id()));
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
}
