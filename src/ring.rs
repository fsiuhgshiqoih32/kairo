//! The replay buffer: the encoder's MPEG-TS stream, cut at keyframes into GOPs and kept for the
//! last N seconds. Nothing becomes a clip until one is saved. Finished GOPs live in memory, or in
//! a scratch file reused round and round (`Spill`) when the buffer would be big: the system's
//! file cache holds that file and hands the memory to games when they need it.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ts::{self, Codec, PACKET};

/// One group of pictures: the TS packets from a keyframe up to the next keyframe.
pub struct Gop {
    /// Wall-clock time (Unix µs) of its first frame.
    pub start_us: i64,
    /// Wall-clock time of the newest frame in it.
    pub last_us: i64,
    /// The packets, while they're in memory (always, for the GOP being filled).
    pub bytes: Vec<u8>,
    /// Where the packets are in the spill file instead.
    spilled: Option<Spilled>,
}

#[derive(Clone, Copy, Debug)]
struct Spilled {
    offset: u64,
    len: usize,
    /// GOPs are numbered as they're written; see `SpillFile::valid_from`.
    seq: u64,
}

impl Gop {
    pub fn len(&self) -> usize {
        self.spilled.map_or(self.bytes.len(), |s| s.len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The scratch file behind a disk buffer. It's deleted when closed (on Windows even after a
/// crash; elsewhere it has no name once open).
pub struct SpillFile {
    file: File,
    /// GOPs numbered below this may have been overwritten by newer ones.
    valid_from: AtomicU64,
}

impl SpillFile {
    fn create(path: &Path) -> std::io::Result<SpillFile> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut o = std::fs::OpenOptions::new();
        o.read(true).write(true).create(true).truncate(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Temporary: kept in the file cache rather than written out while memory is free.
            const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x100;
            const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
            o.attributes(FILE_ATTRIBUTE_TEMPORARY)
                .custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
        }
        let file = o.open(path)?;
        #[cfg(not(windows))]
        let _ = std::fs::remove_file(path);
        Ok(SpillFile {
            file,
            valid_from: AtomicU64::new(0),
        })
    }

    fn write_at(&self, data: &[u8], offset: u64) -> std::io::Result<()> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            let mut done = 0;
            while done < data.len() {
                let n = self.file.seek_write(&data[done..], offset + done as u64)?;
                if n == 0 {
                    return Err(std::io::ErrorKind::WriteZero.into());
                }
                done += n;
            }
            Ok(())
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file.write_all_at(data, offset)
        }
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            let mut done = 0;
            while done < buf.len() {
                let n = self
                    .file
                    .seek_read(&mut buf[done..], offset + done as u64)?;
                if n == 0 {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
                done += n;
            }
            Ok(())
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file.read_exact_at(buf, offset)
        }
    }
}

/// Finished GOPs written one after another into a file of fixed size, wrapping to its start.
struct Spill {
    file: Arc<SpillFile>,
    path: PathBuf,
    capacity: u64,
    /// Where the next GOP goes.
    pos: u64,
    next_seq: u64,
    /// A write failed (the disk is full): GOPs stay in memory from then on.
    broken: bool,
}

/// What saving a clip needs: the stream headers and the GOPs, oldest first.
pub struct Snapshot {
    /// PAT and PMT packets, so the file can be read from its first byte.
    pub header: Vec<u8>,
    pub gops: Vec<Arc<Gop>>,
    spill: Option<Arc<SpillFile>>,
}

impl Snapshot {
    pub fn start_us(&self) -> i64 {
        self.gops.first().map_or(0, |g| g.start_us)
    }

    pub fn end_us(&self) -> i64 {
        self.gops.last().map_or(0, |g| g.last_us)
    }

    pub fn len_bytes(&self) -> usize {
        self.header.len() + self.gops.iter().map(|g| g.len()).sum::<usize>()
    }

    pub fn write_to(&self, mut w: impl Write) -> std::io::Result<()> {
        w.write_all(&self.header)?;
        let mut buf = Vec::new();
        let mut wrote = false;
        for g in &self.gops {
            let Some(s) = g.spilled else {
                w.write_all(&g.bytes)?;
                wrote = true;
                continue;
            };
            let Some(file) = &self.spill else {
                return Err(std::io::Error::other("the replay buffer's file is gone"));
            };
            buf.resize(s.len, 0);
            file.read_at(&mut buf, s.offset)?;
            if s.seq < file.valid_from.load(Ordering::SeqCst) {
                // Overwritten while saving: newer video took its place. Losing the start is fine
                // (the clip starts at the next keyframe); a hole in the middle isn't.
                if wrote {
                    return Err(std::io::Error::other(
                        "the replay buffer moved on while saving",
                    ));
                }
                continue;
            }
            w.write_all(&buf)?;
            wrote = true;
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
    /// Bytes of finished GOPs, wherever they are.
    done_bytes: usize,
    /// Bytes of finished GOPs held in memory.
    done_in_memory: usize,
    /// The last finished GOP's size: about what to reserve for the next one.
    gop_hint: usize,
    spill: Option<Spill>,
}

impl Ring {
    /// Keeps at least `keep_us` of video, but never more than `max_bytes` of it in memory.
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
            done_in_memory: 0,
            gop_hint: 256 * 1024,
            spill: None,
        }
    }

    pub fn set_keep_us(&mut self, keep_us: i64) {
        self.keep_us = keep_us;
        self.evict();
    }

    /// Keeps finished GOPs in a `capacity`-byte file at `path`, or in memory (`None`). Returns
    /// whether that changed, which empties the buffer. On an error it stays in memory.
    pub fn set_spill(&mut self, to: Option<(PathBuf, u64)>) -> Result<bool, String> {
        let same = match (&self.spill, &to) {
            (None, None) => true,
            (Some(s), Some((p, c))) => s.path == *p && s.capacity == *c && !s.broken,
            _ => false,
        };
        if same {
            return Ok(false);
        }
        *self = Ring::new(self.keep_us, self.max_bytes);
        if let Some((path, capacity)) = to {
            let file = SpillFile::create(&path)
                .map_err(|e| format!("could not make {}: {e}", path.display()))?;
            self.spill = Some(Spill {
                file: Arc::new(file),
                path,
                capacity,
                pos: 0,
                next_seq: 0,
                broken: false,
            });
        }
        Ok(true)
    }

    /// The video codec of the stream, once its headers have arrived.
    pub fn codec(&self) -> Option<Codec> {
        self.video.map(|(_, c)| c)
    }

    /// Whether finished GOPs go to a file.
    pub fn on_disk(&self) -> bool {
        self.spill.as_ref().is_some_and(|s| !s.broken)
    }

    /// Forgets everything: a new encoder run with different settings, whose stream can't follow
    /// the old one in a file.
    pub fn clear(&mut self) {
        let spill = self.spill.take().map(|mut s| {
            // Snapshots still holding old GOPs must not read the space new ones will take.
            s.file.valid_from.store(s.next_seq, Ordering::SeqCst);
            s.pos = 0;
            s
        });
        *self = Ring::new(self.keep_us, self.max_bytes);
        self.spill = spill;
    }

    /// A new encoder run with the same settings: what's held stays usable, but the old stream's
    /// unfinished packet and GOP end here.
    pub fn new_run(&mut self) {
        self.leftover.clear();
        if let Some(g) = self.current.take() {
            self.finish(g);
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

    /// Bytes of video held, in memory or on disk.
    pub fn bytes(&self) -> usize {
        self.done_bytes + self.current.as_ref().map_or(0, |g| g.bytes.len())
    }

    /// Memory the held video takes.
    pub fn memory_bytes(&self) -> usize {
        self.done_in_memory + self.current.as_ref().map_or(0, |g| g.bytes.capacity())
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
                    self.finish(g);
                }
                self.current = Some(Gop {
                    start_us: at,
                    last_us: at,
                    // About the last one's size: rarely regrown, little left over.
                    bytes: Vec::with_capacity(self.gop_hint + self.gop_hint / 8),
                    spilled: None,
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

    /// Files a complete GOP: into the spill file if there is one, else into memory with its
    /// spare room given back.
    fn finish(&mut self, mut g: Gop) {
        let len = g.bytes.len();
        self.gop_hint = len.max(16 * 1024);
        if let Some(at) = self.spill_slot(len) {
            let s = self
                .spill
                .as_mut()
                .expect("a slot comes from the spill file");
            if s.file.write_at(&g.bytes, at).is_ok() {
                g.spilled = Some(Spilled {
                    offset: at,
                    len,
                    seq: s.next_seq,
                });
                s.next_seq += 1;
                s.pos = at + len as u64;
                g.bytes = Vec::new();
                self.done_bytes += len;
                self.done.push_back(Arc::new(g));
                return;
            }
            // Full or failing disk. What's in the file stays readable; no more goes there.
            s.broken = true;
        }
        if g.bytes.capacity() > len + len / 8 + 4096 {
            g.bytes.shrink_to_fit();
        }
        self.done_bytes += len;
        self.done_in_memory += len;
        self.done.push_back(Arc::new(g));
    }

    /// Where in the spill file a GOP of `len` bytes goes, once the old GOPs in its way are
    /// dropped. `None` to keep it in memory.
    fn spill_slot(&mut self, len: usize) -> Option<u64> {
        let s = self.spill.as_ref().filter(|s| !s.broken)?;
        let (old, len, file) = (s.pos, len as u64, s.file.clone());
        if len > s.capacity {
            return None;
        }
        let wrapped = old + len > s.capacity;
        let lo = if wrapped { 0 } else { old };
        let hi = lo + len;
        // The GOPs in the way are always the oldest. After a wrap, those in the file's unused
        // end go too: they're older than the ones about to be overwritten at its start.
        while let Some(f) = self.done.front().and_then(|g| g.spilled) {
            let end = f.offset + f.len as u64;
            if !((wrapped && f.offset >= old) || (f.offset < hi && end > lo)) {
                break;
            }
            file.valid_from.store(f.seq + 1, Ordering::SeqCst);
            self.pop_oldest();
        }
        Some(lo)
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
        while !self.done.is_empty() && self.memory_bytes() > self.max_bytes {
            self.pop_oldest();
        }
    }

    fn pop_oldest(&mut self) {
        if let Some(g) = self.done.pop_front() {
            self.done_bytes -= g.len();
            if g.spilled.is_none() {
                self.done_in_memory -= g.bytes.len();
            }
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
                spilled: None,
            }));
        }
        if all.is_empty() {
            return None;
        }
        let first = all.iter().rposition(|g| g.start_us <= from_us).unwrap_or(0);
        Some(Snapshot {
            header,
            gops: all.split_off(first),
            spill: self.spill.as_ref().map(|s| s.file.clone()),
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

    fn spill_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rewindr-ring-{}-{name}.bin", std::process::id()))
    }

    fn file_of(snap: &Snapshot) -> Vec<u8> {
        let mut v = Vec::new();
        snap.write_to(&mut v).unwrap();
        v
    }

    /// Bytes in one GOP of `stream(.., 60, 60)`: two packets a frame (the PAT and PMT aren't kept).
    const GOP: usize = 60 * 2 * PACKET;

    #[test]
    fn memory_isnt_wasted_on_spare_room() {
        let mut ring = Ring::new(60 * SEC, usize::MAX);
        for (data, now) in stream(60 * 10, 60, 60) {
            ring.push(&data, now);
        }
        let (held, used) = (ring.bytes(), ring.memory_bytes());
        assert!(used <= held + held / 4, "{used} bytes of memory for {held}");
    }

    #[test]
    fn disk_buffer_saves_the_same_clip_with_little_memory() {
        let mut mem = Ring::new(8 * SEC, usize::MAX);
        let mut disk = Ring::new(8 * SEC, usize::MAX);
        assert!(
            disk.set_spill(Some((spill_path("same"), 64 << 20)))
                .unwrap()
        );
        assert!(
            !disk
                .set_spill(Some((spill_path("same"), 64 << 20)))
                .unwrap()
        );
        assert!(disk.on_disk());
        for (data, now) in stream(60 * 10, 60, 60) {
            for piece in data.chunks(150) {
                mem.push(piece, now);
                disk.push(piece, now);
            }
        }
        assert_eq!(mem.seconds(), disk.seconds());
        assert_eq!(mem.bytes(), disk.bytes());
        // Only the GOP being filled is in memory.
        assert!(disk.memory_bytes() < GOP * 2, "{}", disk.memory_bytes());
        assert!(mem.memory_bytes() > GOP * 8, "{}", mem.memory_bytes());
        let from = BASE_US + 4 * SEC;
        let (a, b) = (mem.snapshot(from).unwrap(), disk.snapshot(from).unwrap());
        assert_eq!(a.len_bytes(), b.len_bytes());
        assert_eq!(file_of(&a), file_of(&b));
    }

    #[test]
    fn disk_buffer_wraps_and_keeps_the_newest_in_order() {
        let mut ring = Ring::new(60 * SEC, usize::MAX);
        // Room for three and a half GOPs.
        ring.set_spill(Some((spill_path("wrap"), (GOP * 7 / 2) as u64)))
            .unwrap();
        for (data, now) in stream(60 * 20, 60, 60) {
            ring.push(&data, now);
        }
        // Three finished GOPs fit in the file, plus the one being filled.
        assert!((2.9..=4.1).contains(&ring.seconds()), "{}", ring.seconds());
        let snap = ring.snapshot(0).unwrap();
        for w in snap.gops.windows(2) {
            assert_eq!(w[1].start_us, w[0].start_us + SEC);
        }
        let file = file_of(&snap);
        assert_eq!(file.len(), snap.len_bytes());
        let body = &file[snap.header.len()..];
        for (i, g) in snap.gops.iter().enumerate() {
            // Each GOP read back starts with its keyframe packet.
            let at = snap.gops[..i].iter().map(|g| g.len()).sum::<usize>();
            let h = ts::header(&body[at..]).unwrap();
            assert!(h.random_access && h.pid == 0x100, "GOP {i}");
            assert_eq!(g.len(), GOP);
        }
        assert!(body.chunks(PACKET).all(|p| p[0] == 0x47));
    }

    #[test]
    fn a_cleared_disk_buffer_doesnt_hand_out_old_video() {
        let mut ring = Ring::new(60 * SEC, usize::MAX);
        ring.set_spill(Some((spill_path("clear"), 1 << 20)))
            .unwrap();
        for (data, now) in stream(60 * 3, 60, 60) {
            ring.push(&data, now);
        }
        let old = ring.snapshot(0).unwrap();
        ring.clear();
        for (data, now) in stream(60 * 3, 60, 60) {
            ring.push(&data, now);
        }
        // The old snapshot's GOPs in the file are void now; its last one (in memory) is fine.
        let file = file_of(&old);
        assert_eq!(
            file.len(),
            old.header.len() + old.gops.last().unwrap().len()
        );
        // Back to memory: the buffer starts over.
        assert!(ring.set_spill(None).unwrap());
        assert!(!ring.on_disk() && ring.bytes() == 0);
    }
}
