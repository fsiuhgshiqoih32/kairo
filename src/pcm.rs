//! Recent audio in memory, placed on the wall clock so it can be cut to match the video.
//!
//! Capture callbacks deliver blocks of samples at slightly irregular moments. Within an unbroken
//! run, samples are placed by count from the run's start (the device clock is steadier than the
//! callback times). A block that arrives clearly later than the run predicts starts a new run:
//! loopback capture delivers nothing while nothing plays, and that silence must stay silence.

use std::collections::VecDeque;

/// A gap or overlap bigger than this between where a block should start and where its arrival
/// time puts it starts a new run.
const RESYNC_US: i64 = 40_000;

struct Block {
    start_us: i64,
    samples: Vec<f32>,
}

pub struct PcmRing {
    pub rate: u32,
    pub channels: u16,
    keep_us: i64,
    blocks: VecDeque<Block>,
    /// Where the next block of the current run starts.
    next_us: Option<i64>,
}

impl PcmRing {
    pub fn new(rate: u32, channels: u16, keep_us: i64) -> Self {
        PcmRing {
            rate: rate.max(1),
            channels: channels.max(1),
            keep_us,
            blocks: VecDeque::new(),
            next_us: None,
        }
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
        self.blocks.push_back(Block {
            start_us: start,
            samples: samples[..frames * self.channels as usize].to_vec(),
        });
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
            out[lo as usize * ch..hi as usize * ch].copy_from_slice(&b.samples[src]);
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

    #[test]
    fn jittery_blocks_stay_continuous() {
        // 48 kHz mono, 10 ms blocks, arrival times off by up to ±3 ms.
        let mut r = PcmRing::new(48_000, 1, 10_000_000);
        let jitter = [0, 3000, -2000, 1000, -3000];
        for i in 0..100i64 {
            let block: Vec<f32> = (0..480).map(|k| (i * 480 + k) as f32).collect();
            r.push(&block, T0 + (i + 1) * 10_000 + jitter[i as usize % 5]);
        }
        let out = r.extract(T0, T0 + 1_000_000);
        assert_eq!(out.len(), 48_000);
        // A continuous ramp: no dropped or doubled samples anywhere.
        for w in out.windows(2) {
            assert_eq!(w[1] - w[0], 1.0);
        }
    }

    #[test]
    fn silence_between_runs() {
        let mut r = PcmRing::new(1000, 2, 10_000_000);
        r.push(&[1.0; 200], T0 + 100_000); // 100 frames: 0..100 ms
        r.push(&[2.0; 200], T0 + 600_000); // nothing played for 400 ms, then 500..600 ms
        let out = r.extract(T0, T0 + 700_000);
        assert_eq!(out.len(), 700 * 2);
        assert_eq!(out[0], 1.0);
        assert_eq!(out[199], 1.0);
        assert_eq!(out[200], 0.0);
        assert_eq!(out[999], 0.0);
        assert_eq!(out[1000], 2.0);
        assert_eq!(out[1199], 2.0);
        assert_eq!(out[1200], 0.0);
    }

    #[test]
    fn window_partly_before_the_buffer() {
        let mut r = PcmRing::new(1000, 1, 10_000_000);
        r.push(&[5.0; 100], T0 + 1_100_000);
        let out = r.extract(T0, T0 + 1_100_000);
        assert_eq!(out.len(), 1100);
        assert!(out[..1000].iter().all(|s| *s == 0.0));
        assert!(out[1000..].iter().all(|s| *s == 5.0));
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
}
