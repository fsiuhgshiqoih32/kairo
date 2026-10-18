//! Reads an MP4's length from its `moov/mvhd` box without loading the file.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The duration in seconds, if the file is an MP4 with a movie header.
pub fn duration_secs(path: &Path) -> Option<f64> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    read_duration(&mut f, len)
}

/// Box header at the reader's position: (type, start of contents, end of box).
fn box_header(r: &mut (impl Read + Seek), end: u64) -> Option<([u8; 4], u64, u64)> {
    let start = r.stream_position().ok()?;
    if start + 8 > end {
        return None;
    }
    let mut h = [0u8; 8];
    r.read_exact(&mut h).ok()?;
    let size = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as u64;
    let kind = [h[4], h[5], h[6], h[7]];
    let (body, box_end) = match size {
        0 => (start + 8, end),
        1 => {
            let mut b = [0u8; 8];
            r.read_exact(&mut b).ok()?;
            (start + 16, start + u64::from_be_bytes(b))
        }
        s => (start + 8, start + s),
    };
    (box_end > start && box_end <= end).then_some((kind, body, box_end))
}

/// Finds `kind` among the boxes from the reader's position to `end`; leaves the reader at its
/// contents and returns the box's end.
fn find(r: &mut (impl Read + Seek), kind: &[u8; 4], end: u64) -> Option<u64> {
    loop {
        let (k, body, box_end) = box_header(r, end)?;
        if &k == kind {
            r.seek(SeekFrom::Start(body)).ok()?;
            return Some(box_end);
        }
        r.seek(SeekFrom::Start(box_end)).ok()?;
    }
}

pub fn read_duration(r: &mut (impl Read + Seek), len: u64) -> Option<f64> {
    r.seek(SeekFrom::Start(0)).ok()?;
    let moov_end = find(r, b"moov", len)?;
    find(r, b"mvhd", moov_end)?;
    let mut version = [0u8; 4];
    r.read_exact(&mut version).ok()?;
    let (timescale, duration) = if version[0] == 1 {
        let mut b = [0u8; 28];
        r.read_exact(&mut b).ok()?;
        (
            u32::from_be_bytes(b[16..20].try_into().ok()?) as u64,
            u64::from_be_bytes(b[20..28].try_into().ok()?),
        )
    } else {
        let mut b = [0u8; 16];
        r.read_exact(&mut b).ok()?;
        (
            u32::from_be_bytes(b[8..12].try_into().ok()?) as u64,
            u32::from_be_bytes(b[12..16].try_into().ok()?) as u64,
        )
    };
    (timescale > 0).then(|| duration as f64 / timescale as f64)
}

/// "0:30", "2:05", "1:02:03".
pub fn format_duration(secs: f64) -> String {
    let s = secs.round().max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn reads_mvhd() {
        let mut mvhd = vec![0u8; 4];
        mvhd.extend_from_slice(&[0; 8]); // creation, modification
        mvhd.extend_from_slice(&1000u32.to_be_bytes());
        mvhd.extend_from_slice(&30_500u32.to_be_bytes());
        mvhd.extend_from_slice(&[0; 80]);
        let moov = bx(
            b"moov",
            &[bx(b"trak", &[1, 2, 3]), bx(b"mvhd", &mvhd)].concat(),
        );
        let file = [
            bx(b"ftyp", b"isom"),
            bx(b"free", &[]),
            moov,
            bx(b"mdat", &[9; 50]),
        ]
        .concat();
        let len = file.len() as u64;
        let got = read_duration(&mut std::io::Cursor::new(file), len).unwrap();
        assert!((got - 30.5).abs() < 1e-9);
        assert_eq!(
            read_duration(&mut std::io::Cursor::new(vec![0u8; 3]), 3),
            None
        );
    }

    #[test]
    fn formats() {
        assert_eq!(format_duration(29.6), "0:30");
        assert_eq!(format_duration(125.0), "2:05");
        assert_eq!(format_duration(3723.0), "1:02:03");
    }
}
