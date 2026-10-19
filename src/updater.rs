//! Updates from GitHub Releases: compare the latest release's version with this build's, download
//! the package for this platform, check it against the release's SHA256SUMS.txt, and swap the
//! program (and the ffmpeg beside it) in place. The caller restarts Kairo afterwards.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

const LATEST_RELEASE: &str = "https://api.github.com/repos/fsiuhgshiqoih32/kairo/releases/latest";
const CHECKSUMS: &str = "SHA256SUMS.txt";
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

/// A newer release with a package for this platform.
#[derive(Clone, Debug)]
pub struct Update {
    pub version: String,
    package: Asset,
    checksums: Option<Asset>,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// `"v1.2.3"` or `"1.2"` as a comparable triple. Pre-release suffixes are ignored.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    Some((major, minor, patch))
}

/// The release package name ends with this for the current platform. (Rewindr, Kairo's name
/// before 0.2.0, looked for `-windows-x64.zip`, `-macos.zip` and `-linux-x86_64.tar.gz`; the
/// release workflow publishes `rewindr-…` packages under those names for its updater.)
fn package_suffix() -> Option<&'static str> {
    if cfg!(windows) && cfg!(target_arch = "x86_64") {
        Some("-windows-x64-portable.zip")
    } else if cfg!(target_os = "macos") {
        Some("-macos-universal.zip")
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        Some("-linux-x86_64-portable.tar.gz")
    } else {
        None
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .user_agent(concat!("Kairo/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// `Ok(None)` when this build is already the latest.
pub fn check() -> Result<Option<Update>, String> {
    let suffix = package_suffix().ok_or("no builds are published for this platform")?;
    let mut response = agent()
        .get(LATEST_RELEASE)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("could not reach GitHub: {e}"))?;
    let release: Release = response
        .body_mut()
        .read_json()
        .map_err(|e| format!("unexpected reply from GitHub: {e}"))?;
    let latest = parse_version(&release.tag_name)
        .ok_or_else(|| format!("unrecognised release tag {}", release.tag_name))?;
    let current = parse_version(current_version()).unwrap_or((0, 0, 0));
    if latest <= current {
        return Ok(None);
    }
    let package = release
        .assets
        .iter()
        .find(|a| a.name.ends_with(suffix))
        .cloned()
        .ok_or_else(|| {
            format!(
                "release {} has no package for this platform",
                release.tag_name
            )
        })?;
    let checksums = release.assets.iter().find(|a| a.name == CHECKSUMS).cloned();
    Ok(Some(Update {
        version: release.tag_name.trim_start_matches('v').to_string(),
        package,
        checksums,
    }))
}

/// Downloads `update`, verifies it, and replaces the running program (and ffmpeg) with it.
pub fn install(update: &Update) -> Result<(), String> {
    let agent = agent();
    let package = download(&agent, &update.package.browser_download_url)?;

    // Refuse anything that doesn't match the checksum the release workflow published.
    let sums = update
        .checksums
        .as_ref()
        .ok_or("the release has no checksums, so it can't be verified")?;
    let sums = String::from_utf8_lossy(&download(&agent, &sums.browser_download_url)?).into_owned();
    let expected = sums
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let hash = fields.next()?;
            let name = fields.next()?.trim_start_matches('*');
            (name == update.package.name).then(|| hash.to_ascii_lowercase())
        })
        .ok_or("the release's checksums don't list this package")?;
    if hex(&Sha256::digest(&package)) != expected {
        return Err("the download doesn't match its checksum; not installing it".into());
    }

    let zip = update.package.name.ends_with(".zip");
    let take = |name: &str| {
        if zip {
            from_zip(&package, name)
        } else {
            from_tar_gz(&package, name)
        }
    };
    let program = take(PROGRAM)?.ok_or("the package doesn't contain the program")?;
    let ffmpeg = take(FFMPEG)?;

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if let (Some(new), Some(dir)) = (ffmpeg, exe.parent()) {
        replace_beside(&dir.join(FFMPEG), &new)?;
    }
    let staged = std::env::temp_dir().join(format!("kairo-update-{}", std::process::id()));
    std::fs::write(&staged, &program).map_err(|e| format!("could not stage the update: {e}"))?;
    make_executable(&staged);
    let result = self_replace::self_replace(&staged)
        .map_err(|e| format!("could not replace the program file: {e}"));
    let _ = std::fs::remove_file(&staged);
    result
}

/// Replaces `path` with `bytes`, even while the old file is running (Windows lets a running
/// program be renamed, not overwritten).
fn replace_beside(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    let new = path.with_extension("new");
    std::fs::write(&new, bytes).map_err(|e| format!("could not write {}: {e}", new.display()))?;
    make_executable(&new);
    let old = path.with_extension("old");
    let _ = std::fs::remove_file(&old);
    if path.exists() {
        std::fs::rename(path, &old)
            .map_err(|e| format!("could not move {}: {e}", path.display()))?;
    }
    std::fs::rename(&new, path).map_err(|e| format!("could not place {}: {e}", path.display()))?;
    let _ = std::fs::remove_file(&old);
    Ok(())
}

fn make_executable(_path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(_path, std::fs::Permissions::from_mode(0o755));
    }
}

fn download(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    response
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()
        .map_err(|e| format!("download failed: {e}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const PROGRAM: &str = if cfg!(windows) { "kairo.exe" } else { "kairo" };
const FFMPEG: &str = if cfg!(windows) {
    "ffmpeg.exe"
} else {
    "ffmpeg"
};

/// True for `<dir>/<name>` in a package, including inside the macOS app bundle
/// (`Kairo.app/Contents/MacOS/<name>`).
fn is_named(path: &str, name: &str) -> bool {
    let path = path.replace('\\', "/");
    path == name || path.ends_with(&format!("/{name}"))
}

fn from_zip(bytes: &[u8], name: &str) -> Result<Option<Vec<u8>>, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("bad package: {e}"))?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| format!("bad package: {e}"))?;
        let path = file.name().map(|n| n.into_owned()).unwrap_or_default();
        if file.is_file() && is_named(&path, name) {
            let mut out = Vec::new();
            file.read_to_end(&mut out)
                .map_err(|e| format!("bad package: {e}"))?;
            return Ok(Some(out));
        }
    }
    Ok(None)
}

fn from_tar_gz(bytes: &[u8], name: &str) -> Result<Option<Vec<u8>>, String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    for entry in archive.entries().map_err(|e| format!("bad package: {e}"))? {
        let mut entry = entry.map_err(|e| format!("bad package: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("bad package: {e}"))?
            .to_string_lossy()
            .into_owned();
        if entry.header().entry_type().is_file() && is_named(&path, name) {
            let mut out = Vec::new();
            entry
                .read_to_end(&mut out)
                .map_err(|e| format!("bad package: {e}"))?;
            return Ok(Some(out));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.10"), Some((0, 10, 0)));
        assert_eq!(parse_version("2.0.0-beta.1"), Some((2, 0, 0)));
        assert!(parse_version("v0.10.0") > parse_version("v0.9.9"));
        assert_eq!(parse_version("latest"), None);
    }

    #[test]
    fn finds_files_in_each_package_layout() {
        assert!(is_named(
            &format!("kairo-v0.2.0-windows-x64\\{PROGRAM}"),
            PROGRAM
        ));
        assert!(is_named(
            &format!("Kairo.app/Contents/MacOS/{PROGRAM}"),
            PROGRAM
        ));
        assert!(is_named(
            &format!("kairo-v0.2.0-linux-x86_64/{FFMPEG}"),
            FFMPEG
        ));
        assert!(!is_named("kairo-v0.2.0/kairo.png", PROGRAM));
        assert!(!is_named("kairo-v0.2.0/FFMPEG-LICENSE.txt", FFMPEG));
    }

    #[test]
    fn old_and_new_updaters_pick_different_packages() {
        // Rewindr 0.1's updater takes the first asset ending in its suffix, so none of Kairo's
        // packages may end that way, and Kairo's must skip the `rewindr-…` ones made for it.
        for (new, old) in [
            ("kairo-v0.2.0-windows-x64-portable.zip", "-windows-x64.zip"),
            ("kairo-v0.2.0-windows-x64-setup.exe", "-windows-x64.zip"),
            ("kairo-v0.2.0-macos-universal.zip", "-macos.zip"),
            (
                "kairo-v0.2.0-linux-x86_64-portable.tar.gz",
                "-linux-x86_64.tar.gz",
            ),
        ] {
            assert!(!new.ends_with(old), "{new}");
        }
        let suffix = package_suffix().unwrap_or("-none");
        for old in [
            "rewindr-v0.2.0-windows-x64.zip",
            "rewindr-v0.2.0-macos.zip",
            "rewindr-v0.2.0-linux-x86_64.tar.gz",
            "kairo-v0.2.0-windows-x64-setup.exe",
        ] {
            assert!(!old.ends_with(suffix), "{old}");
        }
    }

    #[test]
    fn extracts_from_zip() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file("pkg/README.md", opts).unwrap();
            w.write_all(b"readme").unwrap();
            w.start_file(format!("pkg/{PROGRAM}"), opts).unwrap();
            w.write_all(b"binary").unwrap();
            w.finish().unwrap();
        }
        assert_eq!(
            from_zip(buf.get_ref(), PROGRAM).unwrap().unwrap(),
            b"binary"
        );
        assert_eq!(from_zip(buf.get_ref(), FFMPEG).unwrap(), None);
    }

    #[test]
    fn replaces_a_file_beside_us() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FFMPEG);
        std::fs::write(&p, b"old").unwrap();
        replace_beside(&p, b"new").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
        assert!(!p.with_extension("old").exists());
        replace_beside(&dir.path().join("fresh"), b"x").unwrap();
    }
}
