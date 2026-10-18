//! The replay buffer: the encoder's MPEG-TS stream, cut at keyframes into GOPs and kept in
//! memory for the last N seconds. Nothing is written to disk until a clip is saved.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::Arc;

use crate::ts::{self, Codec, PACKET};

/// One group of pictures: the TS packets from a keyframe up to the next keyframe.
pub struct Gop {
    /// Wall-clock time (Unix µs) of its first frame.
    pub start_us: i64,
    /// Wall-clock time of the newest frame in it.
    pub last_us: i64,
    pub bytes: Vec<u8>,
}

/// What saving a clip needs: the stream headers and the GOPs, oldest first.
pub struct Snapshot {
    /// PAT and PMT packets, so the file can be read from its first byte.
    pub header: Vec<u8>,
    pub gops: Vec<Arc<Gop>>,
}

impl Snapshot {
    pub fn start_us(&self) -> i64 {
        self.gops.first().map_or(0, |g| g.start_us)
    }

    pub fn end_us(&self) -> i64 {
        self.gops.last().map_or(0, |g| g.last_us)
    }

    pub fn len_bytes(&self) -> usize {
        self.header.len() + self.gops.iter().map(|g| g.bytes.len()).sum::<usize>()
    }

    pub fn write_to(&self, mut w: impl Write) -> std::io::Result<()> {
        w.write_all(&self.header)?;
        for g in &self.gops {
            w.write_all(&g.bytes)?;
        }
        w.flush()
    }
}

pub struct Ring {
    keep_us: i64,
    max_bytes: usize,
    leftover: Vec<u8>,
    pmt_pid: Option<u16>,
    video: Option<(u16, Codec)>,
    pat: Option<Vec<u8>>,
    pmt: Option<Vec<u8>>,
    done: VecDeque<Arc<Gop>>,
    current: Option<Gop>,
    done_bytes: usize,
}

impl Ring {
    /// Keeps at least `keep_us` of video, but never more than `max_bytes`.
    pub fn new(keep_us: i64, max_bytes: usize) -> Self {
        Ring {
            keep_us,
            max_bytes,
            leftover: Vec::new(),
            pmt_pid: None,
            video: None,
            pat: None,
            pmt: None,
            done: VecDeque::new(),
            current: None,
            done_bytes: 0,
        }
    }

    pub fn set_keep_us(&mut self, keep_us: i64) {
        self.keep_us = keep_us;
        self.evict();
    }

    /// Forgets everything: a new encoder run with different settings, whose stream can't follow
    /// the old one in a file.
    pub fn clear(&mut self) {
        *self = Ring::new(self.keep_us, self.max_bytes);
    }

    /// A new encoder run with the same settings: what's held stays usable, but the old stream's
    /// unfinished packet and GOP end here.
    pub fn new_run(&mut self) {
        self.leftover.clear();
        if let Some(g) = self.current.take() {
            self.done_bytes += g.bytes.len();
            self.done.push_back(Arc::new(g));
        }
    }

    /// Seconds of video held, from the oldest GOP's start to the newest frame.
    pub fn seconds(&self) -> f64 {
        let first = self
            .done
            .front()
            .map(|g| g.start_us)
            .or(self.current.as_ref().map(|g| g.start_us));
        let last = self
            .current
            .as_ref()
            .map(|g| g.last_us)
            .or(self.done.back().map(|g| g.last_us));
        match (first, last) {
            (Some(a), Some(b)) => (b - a).max(0) as f64 / 1e6,
            _ => 0.0,
        }
    }

    pub fn bytes(&self) -> usize {
        self.done_bytes + self.current.as_ref().map_or(0, |g| g.bytes.len())
    }

    /// Adds encoder output that arrived at `now_us` (wall clock). Data may end mid-packet.
    pub fn push(&mut self, data: &[u8], now_us: i64) {
        let mut buf = std::mem::take(&mut self.leftover);
        buf.extend_from_slice(data);
        let mut i = 0;
        while i + PACKET <= buf.len() {
            if buf[i] != 0x47 {
                // Lost sync: skip to the next byte that starts two packets in a row.
                i += 1;
                while i + PACKET < buf.len() && !(buf[i] == 0x47 && buf[i + PACKET] == 0x47) {
                    i += 1;
                }
                continue;
            }
            self.packet(&buf[i..i + PACKET], now_us);
            i += PACKET;
        }
        self.leftover = buf[i..].to_vec();
        self.evict();
    }

    fn packet(&mut self, p: &[u8], now_us: i64) {
        let Some(h) = ts::header(p) else { return };
        let payload = h.payload.map(|o| &p[o..]).unwrap_or(&[]);
        if h.pid == 0 {
            if h.unit_start
                && let Some(pid) = ts::pat_pmt_pid(payload)
            {
                self.pmt_pid = Some(pid);
                self.pat = Some(p.to_vec());
            }
            return;
        }
        if Some(h.pid) == self.pmt_pid {
            if h.unit_start
                && let Some(v) = ts::pmt_video_pid(payload)
            {
                self.video = Some(v);
                self.pmt = Some(p.to_vec());
            }
            return;
        }
        let Some((video_pid, codec)) = self.video else {
            return;
        };
        if h.pid != video_pid {
            return;
        }
        if h.unit_start
            && let Some(pts) = ts::pes_pts(payload)
        {
            let at = ts::unwrap_pts_us(pts, now_us);
            if h.random_access || ts::pes_has_keyframe(payload, codec) {
                if let Some(g) = self.current.take() {
                    self.done_bytes += g.bytes.len();
                    self.done.push_back(Arc::new(g));
                }
                self.current = Some(Gop {
                    start_us: at,
                    last_us: at,
                    bytes: Vec::with_capacity(256 * 1024),
                });
            } else if let Some(g) = &mut self.current {
                g.last_us = g.last_us.max(at);
            }
        }
        // Packets before the first keyframe can't be decoded; drop them.
        if let Some(g) = &mut self.current {
            g.bytes.extend_from_slice(p);
        }
    }

    fn evict(&mut self) {
        let newest = self
            .current
            .as_ref()
            .map(|g| g.last_us)
            .or(self.done.back().map(|g| g.last_us));
        let Some(newest) = newest else { return };
        // Keep the GOP that straddles the cut-off: a clip may need to start inside it.
        while self.done.len() > 1 && self.done[1].start_us <= newest - self.keep_us {
            self.pop_oldest();
        }
        while !self.done.is_empty() && self.bytes() > self.max_bytes {
            self.pop_oldest();
        }
    }

    fn pop_oldest(&mut self) {
        if let Some(g) = self.done.pop_front() {
            self.done_bytes -= g.bytes.len();
        }
    }

    /// The video from `from_us` (wall clock) to now, starting at the last keyframe at or before
    /// it (or the oldest one held). `None` until a keyframe and the stream headers have arrived.
    pub fn snapshot(&self, from_us: i64) -> Option<Snapshot> {
        let header = [self.pat.as_ref()?.as_slice(), self.pmt.as_ref()?.as_slice()].concat();
        let mut all: Vec<Arc<Gop>> = self.done.iter().cloned().collect();
        if let Some(g) = &self.current {
            all.push(Arc::new(Gop {
                start_us: g.start_us,
                last_us: g.last_us,
                bytes: g.bytes.clone(),
            }));
        }
        if all.is_empty() {
            return None;
        }
        let first = all.iter().rposition(|g| g.start_us <= from_us).unwrap_or(0);
        Some(Snapshot {
            header,
            gops: all.split_off(first),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts::tests::{packet, pat, pes, pmt};

    const SEC: i64 = 1_000_000;
    const BASE_US: i64 = 1_792_347_000_000_000;

    /// A stream with a keyframe every `gop` frames at `fps`, starting at BASE_US.
    fn stream(frames: usize, fps: i64, gop: usize) -> Vec<(Vec<u8>, i64)> {
        let mut out = Vec::new();
        for f in 0..frames {
            let at = BASE_US + f as i64 * SEC / fps;
            let pts = (at * 9 / 100).rem_euclid(ts::PTS_WRAP);
            let key = f % gop == 0;
            let mut data = Vec::new();
            if key {
                data.extend_from_slice(&packet(0, true, false, &pat(0x1000)));
                data.extend_from_slice(&packet(0x1000, true, false, &pmt(0x100)));
            }
            let es: &[u8] = if key {
                &[0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x65]
            } else {
                &[0, 0, 0, 1, 0x41, 2]
            };
            data.extend_from_slice(&packet(0x100, true, key, &pes(pts, es)));
            // A continuation packet with more of the frame.
            data.extend_from_slice(&packet(0x100, false, false, &[7u8; 184]));
            out.push((data, at + 30_000));
        }
        out
    }

    #[test]
    fn cuts_at_keyframes_and_keeps_the_window() {
        let mut ring = Ring::new(5 * SEC, usize::MAX);
        for (data, now) in stream(60 * 10, 60, 60) {
            // Arrive in odd-sized pieces, as reads from a pipe do.
            for piece in data.chunks(100) {
                ring.push(piece, now);
            }
        }
        // 10 s streamed, 5 s kept (plus the GOP straddling the cut-off).
        assert!((5.0..=6.1).contains(&ring.seconds()), "{}", ring.seconds());
        let now = BASE_US + 10 * SEC;
        let snap = ring.snapshot(now - 3 * SEC).unwrap();
        // Starts at the keyframe at or before 7 s, which is 7 s exactly.
        assert_eq!(snap.start_us(), BASE_US + 7 * SEC);
        assert_eq!(snap.gops.len(), 3);
        assert!(snap.end_us() >= BASE_US + 9 * SEC + 900_000);
        // Every GOP is whole packets, beginning with a random-access video packet.
        for g in &snap.gops {
            assert_eq!(g.bytes.len() % PACKET, 0);
            let h = ts::header(&g.bytes).unwrap();
            assert!(h.random_access && h.pid == 0x100);
        }
        let mut file = Vec::new();
        snap.write_to(&mut file).unwrap();
        assert_eq!(file.len(), snap.len_bytes());
        assert_eq!(ts::header(&file).unwrap().pid, 0);
    }

    #[test]
    fn asking_for_more_than_is_held_starts_at_the_oldest() {
        let mut ring = Ring::new(2 * SEC, usize::MAX);
        for (data, now) in stream(60 * 4, 60, 30) {
            ring.push(&data, now);
        }
        let snap = ring.snapshot(BASE_US - 100 * SEC).unwrap();
        assert!(snap.start_us() >= BASE_US + SEC, "older GOPs were evicted");
    }

    #[test]
    fn byte_cap_wins_over_time() {
        let mut ring = Ring::new(60 * SEC, 20 * 1024);
        for (data, now) in stream(60 * 4, 60, 30) {
            ring.push(&data, now);
        }
        assert!(ring.bytes() <= 20 * 1024 + 64 * 1024, "{}", ring.bytes());
        assert!(ring.seconds() < 2.0);
    }

    #[test]
    fn nothing_before_the_first_keyframe() {
        let mut ring = Ring::new(5 * SEC, usize::MAX);
        assert!(ring.snapshot(0).is_none());
        let mut s = stream(90, 60, 60);
        // Drop the first keyframe's packets: frames 1..59 are undecodable.
        s.remove(0);
        for (data, now) in s {
            ring.push(&data, now);
        }
        let snap = ring.snapshot(0).unwrap();
        assert_eq!(snap.start_us(), BASE_US + SEC);
    }

    #[test]
    fn resyncs_after_garbage() {
        let mut ring = Ring::new(5 * SEC, usize::MAX);
        let s = stream(120, 60, 60);
        ring.push(&[1, 2, 3, 0x47, 9], s[0].1);
        for (data, now) in s {
            ring.push(&data, now);
        }
        assert!(ring.snapshot(0).is_some());
    }
}
