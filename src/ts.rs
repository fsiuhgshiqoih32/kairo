//! Just enough MPEG-TS to cut a live stream into GOPs: packet headers, the random-access flag,
//! PES timestamps, and the PAT/PMT that say which PID carries the video.

pub const PACKET: usize = 188;
const SYNC: u8 = 0x47;

/// The 33-bit PES clock wraps after this many 90 kHz ticks (about 26.5 hours).
pub const PTS_WRAP: i64 = 1 << 33;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub pid: u16,
    /// A PES packet or PSI section starts in this packet.
    pub unit_start: bool,
    /// The adaptation field marks a random access point (a keyframe starts here).
    pub random_access: bool,
    /// Where the payload starts, if the packet has one.
    pub payload: Option<usize>,
}

pub fn header(p: &[u8]) -> Option<Header> {
    if p.len() < PACKET || p[0] != SYNC {
        return None;
    }
    let unit_start = p[1] & 0x40 != 0;
    let pid = (((p[1] & 0x1f) as u16) << 8) | p[2] as u16;
    let control = (p[3] >> 4) & 0x3;
    let mut offset = 4;
    let mut random_access = false;
    if control & 0x2 != 0 {
        let len = p[4] as usize;
        if len > 0 {
            random_access = p[5] & 0x40 != 0;
        }
        offset = 5 + len;
    }
    let payload = (control & 0x1 != 0 && offset < PACKET).then_some(offset);
    Some(Header {
        pid,
        unit_start,
        random_access,
        payload,
    })
}

/// The presentation timestamp (90 kHz, 33 bits) of a PES packet that starts at `pes`.
pub fn pes_pts(pes: &[u8]) -> Option<i64> {
    if pes.len() < 14 || pes[0..3] != [0, 0, 1] || pes[7] & 0x80 == 0 {
        return None;
    }
    let b = &pes[9..14];
    Some(
        (((b[0] >> 1) & 0x07) as i64) << 30
            | (b[1] as i64) << 22
            | ((b[2] >> 1) as i64) << 15
            | (b[3] as i64) << 7
            | (b[4] >> 1) as i64,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

/// True if the PES payload (after its header) holds an IDR slice or SPS (H.264), or an IRAP
/// picture or parameter set (HEVC): the frame can be decoded on its own.
pub fn pes_has_keyframe(pes: &[u8], codec: Codec) -> bool {
    if pes.len() < 9 || pes[0..3] != [0, 0, 1] {
        return false;
    }
    let start = 9 + pes[8] as usize;
    let es = pes.get(start..).unwrap_or(&[]);
    let mut i = 0;
    while i + 3 < es.len() {
        if es[i] == 0 && es[i + 1] == 0 && es[i + 2] == 1 {
            let nal = es[i + 3];
            let key = match codec {
                Codec::H264 => matches!(nal & 0x1f, 5 | 7),
                Codec::Hevc => matches!((nal >> 1) & 0x3f, 16..=23 | 32 | 33),
            };
            if key {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// The section inside a PSI packet's payload, after the pointer field.
fn section(payload: &[u8]) -> Option<&[u8]> {
    let pointer = *payload.first()? as usize;
    let s = payload.get(1 + pointer..)?;
    let len = (((*s.get(1)? & 0x0f) as usize) << 8) | *s.get(2)? as usize;
    s.get(..3 + len)
}

/// The PMT PID of the first program in a PAT.
pub fn pat_pmt_pid(payload: &[u8]) -> Option<u16> {
    let s = section(payload)?;
    if s[0] != 0x00 || s.len() < 12 + 4 {
        return None;
    }
    // Program loops start after the 8-byte header; each entry is 4 bytes; the last 4 are CRC.
    s[8..s.len() - 4].chunks_exact(4).find_map(|e| {
        let program = u16::from_be_bytes([e[0], e[1]]);
        (program != 0).then(|| u16::from_be_bytes([e[2] & 0x1f, e[3]]))
    })
}

/// The PID and codec of the first video stream (H.264 or HEVC) in a PMT.
pub fn pmt_video_pid(payload: &[u8]) -> Option<(u16, Codec)> {
    let s = section(payload)?;
    if s[0] != 0x02 || s.len() < 16 {
        return None;
    }
    let info_len = (((s[10] & 0x0f) as usize) << 8) | s[11] as usize;
    let mut i = 12 + info_len;
    let end = s.len() - 4;
    while i + 5 <= end {
        let stream_type = s[i];
        let pid = u16::from_be_bytes([s[i + 1] & 0x1f, s[i + 2]]);
        let es_len = (((s[i + 3] & 0x0f) as usize) << 8) | s[i + 4] as usize;
        match stream_type {
            0x1b => return Some((pid, Codec::H264)),
            0x24 => return Some((pid, Codec::Hevc)),
            _ => {}
        }
        i += 5 + es_len;
    }
    None
}

/// Turns a 33-bit PTS into Unix microseconds, given roughly when the frame was made. Streams
/// carry wall-clock timestamps (ffmpeg's `-use_wallclock_as_timestamps 1 -copyts`), which the
/// 33-bit field wraps; the true value is the one closest to `around_us`.
pub fn unwrap_pts_us(pts: i64, around_us: i64) -> i64 {
    let around = around_us * 9 / 100;
    let base = around - around.rem_euclid(PTS_WRAP);
    let candidates = [base - PTS_WRAP + pts, base + pts, base + PTS_WRAP + pts];
    let best = candidates
        .into_iter()
        .min_by_key(|c| (c - around).abs())
        .unwrap_or(base + pts);
    best * 100 / 9
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A TS packet for tests: header, optional adaptation field with the random-access flag,
    /// payload, stuffing.
    pub fn packet(pid: u16, unit_start: bool, random_access: bool, payload: &[u8]) -> [u8; PACKET] {
        let mut p = [0xffu8; PACKET];
        p[0] = SYNC;
        p[1] = ((unit_start as u8) << 6) | ((pid >> 8) as u8 & 0x1f);
        p[2] = pid as u8;
        let room = PACKET - 4;
        let needs_af = random_access || payload.len() < room;
        if needs_af {
            let af_len = room - 1 - payload.len();
            p[3] = 0x30;
            p[4] = af_len as u8;
            if af_len > 0 {
                p[5] = if random_access { 0x40 } else { 0x00 };
            }
            p[5 + af_len..5 + af_len + payload.len()].copy_from_slice(payload);
        } else {
            p[3] = 0x10;
            p[4..4 + payload.len()].copy_from_slice(payload);
        }
        p
    }

    /// A video PES header with a PTS, followed by `es`.
    pub fn pes(pts: i64, es: &[u8]) -> Vec<u8> {
        let mut v = vec![0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5];
        v.push(0x21 | (((pts >> 30) & 0x07) << 1) as u8);
        v.push((pts >> 22) as u8);
        v.push(0x01 | (((pts >> 15) & 0x7f) << 1) as u8);
        v.push((pts >> 7) as u8);
        v.push(0x01 | ((pts & 0x7f) << 1) as u8);
        v.extend_from_slice(es);
        v
    }

    pub fn pat(pmt_pid: u16) -> Vec<u8> {
        let mut s = vec![0x00, 0xb0, 13, 0x00, 0x01, 0xc1, 0, 0, 0x00, 0x01];
        s.extend_from_slice(&[0xe0 | (pmt_pid >> 8) as u8, pmt_pid as u8]);
        s.extend_from_slice(&[0, 0, 0, 0]);
        let mut p = vec![0];
        p.extend(s);
        p
    }

    pub fn pmt(video_pid: u16) -> Vec<u8> {
        let mut s = vec![
            0x02, 0xb0, 18, 0x00, 0x01, 0xc1, 0, 0, 0xe1, 0x00, 0xf0, 0x00,
        ];
        s.extend_from_slice(&[
            0x1b,
            0xe0 | (video_pid >> 8) as u8,
            video_pid as u8,
            0xf0,
            0x00,
        ]);
        s.extend_from_slice(&[0, 0, 0, 0]);
        let mut p = vec![0];
        p.extend(s);
        p
    }

    #[test]
    fn reads_headers_and_pts() {
        let es = [0, 0, 0, 1, 0x65, 1, 2, 3];
        let p = packet(0x100, true, true, &pes(123_456_789, &es));
        let h = header(&p).unwrap();
        assert_eq!(h.pid, 0x100);
        assert!(h.unit_start && h.random_access);
        let payload = &p[h.payload.unwrap()..];
        assert_eq!(pes_pts(payload), Some(123_456_789));
        assert!(pes_has_keyframe(payload, Codec::H264));
        let p2 = packet(0x100, true, false, &pes(5, &[0, 0, 1, 0x41, 9]));
        let h2 = header(&p2).unwrap();
        assert!(!h2.random_access);
        // 0x41 is an H.264 slice, though as HEVC it would read as a parameter set.
        assert!(!pes_has_keyframe(&p2[h2.payload.unwrap()..], Codec::H264));
        assert!(pes_has_keyframe(&pes(5, &[0, 0, 1, 0x40, 1]), Codec::Hevc));
        assert_eq!(header(&[0u8; PACKET]), None);
    }

    #[test]
    fn reads_pat_and_pmt() {
        let p = packet(0, true, false, &pat(0x1000));
        let h = header(&p).unwrap();
        assert_eq!(pat_pmt_pid(&p[h.payload.unwrap()..]), Some(0x1000));
        let p = packet(0x1000, true, false, &pmt(0x100));
        let h = header(&p).unwrap();
        assert_eq!(
            pmt_video_pid(&p[h.payload.unwrap()..]),
            Some((0x100, Codec::H264))
        );
    }

    #[test]
    fn unwraps_wall_clock_pts() {
        let now_us: i64 = 1_792_347_052_185_537;
        let exact = now_us * 9 / 100;
        let pts = exact.rem_euclid(PTS_WRAP);
        // The frame was made a moment before it arrived.
        let got = unwrap_pts_us(pts, now_us + 300_000);
        assert!((got - now_us).abs() < 20, "{got} vs {now_us}");
        // Just across a wrap in either direction.
        let wrap_us = (PTS_WRAP * 100 / 9) * (now_us / (PTS_WRAP * 100 / 9) + 1);
        let pts = (wrap_us - 1000) * 9 / 100;
        let got = unwrap_pts_us(pts.rem_euclid(PTS_WRAP), wrap_us + 50_000);
        assert!((got - (wrap_us - 1000)).abs() < 20);
    }
}
