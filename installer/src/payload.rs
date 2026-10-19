//! What the installer installs: Kairo's release zip, appended to the end of the installer's own
//! file. Windows runs the program part and ignores what follows, so one file carries both; the
//! same program without the zip is the uninstaller.
//!
//! Layout: `[program][zip][zip length: u64 little-endian][MAGIC]`.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"KAIROPK1";
const TRAILER: u64 = 16;

/// Where the program part of `exe` ends, and the appended zip's length (0 when there's none).
pub fn layout(exe: &Path) -> io::Result<(u64, u64)> {
    let mut f = std::fs::File::open(exe)?;
    let size = f.metadata()?.len();
    if size < TRAILER {
        return Ok((size, 0));
    }
    f.seek(SeekFrom::Start(size - TRAILER))?;
    let mut t = [0u8; TRAILER as usize];
    f.read_exact(&mut t)?;
    if &t[8..] != MAGIC {
        return Ok((size, 0));
    }
    let len = u64::from_le_bytes(t[..8].try_into().expect("8 bytes"));
    if len + TRAILER > size {
        return Err(io::Error::other(
            "the installer file is damaged; download it again",
        ));
    }
    Ok((size - TRAILER - len, len))
}

/// The zip appended to `exe`, if it has one.
pub fn read(exe: &Path) -> io::Result<Option<Vec<u8>>> {
    let (start, len) = layout(exe)?;
    if len == 0 {
        return Ok(None);
    }
    let mut f = std::fs::File::open(exe)?;
    f.seek(SeekFrom::Start(start))?;
    let mut zip = vec![0u8; len as usize];
    f.read_exact(&mut zip)?;
    Ok(Some(zip))
}

/// Writes the program part of `exe` alone to `out`: the uninstaller.
pub fn write_program(exe: &Path, out: &Path) -> io::Result<()> {
    let (end, _) = layout(exe)?;
    let mut f = std::fs::File::open(exe)?;
    let mut o = std::fs::File::create(out)?;
    io::copy(&mut (&mut f).take(end), &mut o)?;
    o.flush()
}

/// Makes an installer: the program part of `exe`, then `zip`.
pub fn pack(exe: &Path, zip: &Path, out: &Path) -> io::Result<()> {
    write_program(exe, out)?;
    let data = std::fs::read(zip)?;
    let mut o = std::fs::OpenOptions::new().append(true).open(out)?;
    o.write_all(&data)?;
    o.write_all(&(data.len() as u64).to_le_bytes())?;
    o.write_all(MAGIC)?;
    o.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("setup.exe");
        let zip = dir.path().join("app.zip");
        let out = dir.path().join("packed.exe");
        std::fs::write(&exe, b"MZ program bytes").unwrap();
        std::fs::write(&zip, b"PK zip bytes").unwrap();
        assert_eq!(layout(&exe).unwrap(), (16, 0));
        assert_eq!(read(&exe).unwrap(), None);

        pack(&exe, &zip, &out).unwrap();
        assert_eq!(layout(&out).unwrap(), (16, 12));
        assert_eq!(read(&out).unwrap().unwrap(), b"PK zip bytes");

        // Packing a packed file replaces the zip rather than stacking a second one.
        std::fs::write(&zip, b"PK newer").unwrap();
        let again = dir.path().join("again.exe");
        pack(&out, &zip, &again).unwrap();
        assert_eq!(read(&again).unwrap().unwrap(), b"PK newer");

        let uninstaller = dir.path().join("uninstall.exe");
        write_program(&again, &uninstaller).unwrap();
        assert_eq!(std::fs::read(&uninstaller).unwrap(), b"MZ program bytes");
    }

    #[test]
    fn a_cut_short_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.exe");
        let mut data = b"MZ".to_vec();
        data.extend_from_slice(&1000u64.to_le_bytes());
        data.extend_from_slice(MAGIC);
        std::fs::write(&bad, data).unwrap();
        assert!(layout(&bad).is_err());
    }
}
