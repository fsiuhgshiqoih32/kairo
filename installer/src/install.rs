//! Installing and uninstalling: the files first, then the Windows parts (shortcuts, the entry in
//! Settings > Apps, start with Windows), reporting progress as it goes.

use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{payload, win};

pub const VERSION: &str = env!("KAIRO_VERSION");
pub const PROGRAM: &str = "kairo.exe";
pub const UNINSTALLER: &str = "uninstall.exe";
/// The files an install wrote, one per line, so uninstalling removes exactly those.
pub const FILE_LIST: &str = "installed-files.txt";
const SHORTCUT: &str = "Kairo.lnk";
/// What Rewindr (Kairo's name before 0.2.0) left in its folder: its zip's files and the ones its
/// updater set aside.
const OLD_FILES: [&str; 10] = [
    "rewindr.exe",
    "ffmpeg.exe",
    "README.md",
    "LICENSE",
    "THIRD-PARTY-NOTICES.txt",
    "FFMPEG-LICENSE.txt",
    "FFMPEG-SOURCE.txt",
    "rewindr.old",
    "ffmpeg.old",
    "ffmpeg.new",
];
/// What a copy unzipped by hand (before there was an installer) holds.
const ZIP_FILES: [&str; 7] = [
    "kairo.exe",
    "ffmpeg.exe",
    "README.md",
    "LICENSE",
    "THIRD-PARTY-NOTICES.txt",
    "FFMPEG-LICENSE.txt",
    "FFMPEG-SOURCE.txt",
];

/// What to install and where.
#[derive(Clone, Debug)]
pub struct Options {
    pub dir: PathBuf,
    pub start_menu: bool,
    pub desktop: bool,
    /// Turn start with Windows on or off; `None` leaves it as it is.
    pub autostart: Option<bool>,
    /// The entry in Settings > Apps.
    pub register: bool,
    /// Close a running Kairo first.
    pub stop_running: bool,
    /// A Rewindr folder to replace (close it, take it out of start-up, remove its files).
    pub replace: Option<PathBuf>,
}

/// What to remove when uninstalling.
#[derive(Clone, Debug)]
pub struct Removal {
    pub dir: PathBuf,
    /// Settings, thumbnails, logs and the saved Discord channel too (never clips).
    pub settings: bool,
    /// Shortcuts, the Apps entry and start with Windows.
    pub system: bool,
}

/// How far along the work is, shared with the window.
#[derive(Debug, Default)]
pub struct Progress {
    pub fraction: f32,
    pub step: String,
    pub result: Option<Result<(), String>>,
}

pub type Shared = Arc<Mutex<Progress>>;

fn report(p: &Shared, fraction: f32, step: &str) {
    let mut p = p.lock().unwrap();
    p.fraction = fraction.clamp(0.0, 1.0);
    if !step.is_empty() {
        p.step = step.to_string();
    }
}

/// Where Kairo goes when `chosen` is picked: inside a Kairo folder of its own, so
/// uninstalling never touches anything else in the folder.
pub fn install_dir(chosen: &Path) -> PathBuf {
    if chosen
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("kairo"))
    {
        chosen.to_path_buf()
    } else {
        chosen.join("Kairo")
    }
}

/// Where Kairo is installed already, or where it goes for this user.
pub fn default_dir() -> PathBuf {
    win::installed_dir().unwrap_or_else(|| {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Programs")
            .join("Kairo")
    })
}

/// Kairo's own folder of settings, thumbnails and logs.
pub fn settings_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("kairo"))
}

/// Rewindr's, which Kairo moves to its own on first start.
fn old_settings_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rewindr"))
}

/// A Rewindr this replaces: where its start-up entry points, or where people usually put it.
pub fn old_rewindr() -> Option<PathBuf> {
    win::old_rewindr_dir().or_else(|| {
        let d = dirs::data_local_dir()?.join("Programs").join("Rewindr");
        d.join("rewindr.exe").exists().then_some(d)
    })
}

/// Where clips are saved: the folder in Kairo's (or Rewindr's) settings, or the default folder.
pub fn clips_dir() -> PathBuf {
    [settings_dir(), old_settings_dir()]
        .into_iter()
        .flatten()
        .find_map(|d| std::fs::read_to_string(d.join("config.toml")).ok())
        .and_then(|t| {
            t.lines().find_map(|l| {
                let v = l.trim().strip_prefix("save_dir")?.trim_start();
                let v = v.strip_prefix('=')?.trim();
                let v = v.strip_prefix('"')?.strip_suffix('"')?;
                (!v.is_empty()).then(|| PathBuf::from(v.replace("\\\\", "\\")))
            })
        })
        .unwrap_or_else(|| {
            // Kairo keeps using Videos\Rewindr when that's where Rewindr saved clips.
            let videos = dirs::video_dir().unwrap_or_else(|| PathBuf::from("."));
            let (new, old) = (videos.join("Kairo"), videos.join("Rewindr"));
            if !new.exists() && old.is_dir() {
                old
            } else {
                new
            }
        })
}

/// Closes the Rewindr in `old`, takes it out of start-up and removes its files (and the folder,
/// once empty). Settings and clips stay: Kairo carries them over.
fn replace_rewindr(old: &Path) {
    let exe = old.join("rewindr.exe");
    if exe.exists() {
        let _ = win::run_hidden(&exe, &["quit"], Duration::from_secs(10));
        wait_until_closed(&exe, Duration::from_secs(6));
    }
    win::remove_start_entries_into(old);
    for f in OLD_FILES {
        let _ = std::fs::remove_file(old.join(f));
    }
    let _ = std::fs::remove_dir(old);
}

/// Writes `bytes` to `path`, even over a program that's running: Windows can't overwrite a
/// running .exe but can rename it, so the old one steps aside (and goes on the next install).
fn replace_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("new");
    std::fs::write(&tmp, bytes)?;
    if std::fs::rename(&tmp, path).is_ok() {
        return Ok(());
    }
    let mut n = 1;
    let aside = loop {
        let p = path.with_extension(format!("old{n}"));
        if !p.exists() || std::fs::remove_file(&p).is_ok() {
            break p;
        }
        n += 1;
    };
    std::fs::rename(path, &aside)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::rename(&aside, path);
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Removes copies an earlier install set aside ("kairo.old1") that nothing runs any more.
fn remove_set_aside(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let ext = e
            .path()
            .extension()
            .map(|x| x.to_string_lossy().to_string())
            .unwrap_or_default();
        if ext.starts_with("old") && ext[3..].chars().all(|c| c.is_ascii_digit()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The path inside the zip for an entry, without its top folder ("kairo-v0.2.0-windows-x64/"),
/// or `None` for anything that would land outside the install folder.
fn entry_path(name: &str) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    let parts: Vec<&str> = name
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if parts.is_empty() || parts.iter().any(|p| *p == ".." || p.contains(':')) {
        return None;
    }
    let parts = if parts.len() > 1 {
        &parts[1..]
    } else {
        &parts[..]
    };
    Some(parts.iter().collect())
}

/// Unpacks the release `zip` into `dir`. Returns the files written, relative to `dir`.
pub fn extract(zip: &[u8], dir: &Path, mut progress: impl FnMut(f32)) -> io::Result<Vec<PathBuf>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip)).map_err(io::Error::other)?;
    std::fs::create_dir_all(dir)?;
    let total: u64 = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.size()))
        .sum::<u64>()
        .max(1);
    let mut done = 0;
    let mut written = Vec::new();
    for i in 0..archive.len() {
        let mut f = archive.by_index(i).map_err(io::Error::other)?;
        if !f.is_file() {
            continue;
        }
        let Some(rel) = f.name().ok().and_then(|n| entry_path(&n)) else {
            continue;
        };
        let mut bytes = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut bytes)?;
        let to = dir.join(&rel);
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p)?;
        }
        replace_file(&to, &bytes)?;
        done += bytes.len() as u64;
        progress(done as f32 / total as f32);
        written.push(rel);
    }
    if !written.iter().any(|p| p == Path::new(PROGRAM)) {
        return Err(io::Error::other(
            "the installer holds no kairo.exe; download it again",
        ));
    }
    Ok(written)
}

/// Waits for `exe` to stop running (until `limit`): a running program can't be opened for
/// writing.
fn wait_until_closed(exe: &Path, limit: Duration) {
    let start = Instant::now();
    while start.elapsed() < limit {
        if std::fs::OpenOptions::new().write(true).open(exe).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Asks a running Kairo to close and waits a little for it.
fn close_kairo(dir: &Path) {
    let exe = dir.join(PROGRAM);
    if exe.exists() {
        let _ = win::run_hidden(&exe, &["quit"], Duration::from_secs(10));
        wait_until_closed(&exe, Duration::from_secs(6));
    }
}

fn folder_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => folder_size(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Installs (or updates) Kairo from the release `zip`; `program` is this installer, whose
/// program part becomes the uninstaller.
pub fn install(zip: &[u8], program: &Path, o: &Options, p: &Shared) -> Result<(), String> {
    let dir = &o.dir;
    if o.stop_running {
        report(p, 0.02, "Closing Kairo");
        close_kairo(dir);
    }
    if let Some(old) = &o.replace {
        report(p, 0.03, "Replacing Rewindr");
        replace_rewindr(old);
    }
    report(p, 0.05, "Copying files");
    std::fs::create_dir_all(dir).map_err(|e| {
        format!(
            "Couldn't make {} ({e}). Choose a folder you can write to.",
            dir.display()
        )
    })?;
    remove_set_aside(dir);
    let mut files = extract(zip, dir, |f| report(p, 0.05 + f * 0.8, ""))
        .map_err(|e| format!("Couldn't copy the files: {e}"))?;

    report(p, 0.87, "Adding the uninstaller");
    let tmp = dir.join("uninstall.new");
    payload::write_program(program, &tmp)
        .and_then(|_| std::fs::read(&tmp))
        .and_then(|bytes| {
            let _ = std::fs::remove_file(&tmp);
            replace_file(&dir.join(UNINSTALLER), &bytes)
        })
        .map_err(|e| format!("Couldn't add the uninstaller: {e}"))?;
    files.push(PathBuf::from(UNINSTALLER));
    let list: String = files.iter().map(|f| format!("{}\n", f.display())).collect();
    std::fs::write(dir.join(FILE_LIST), list).map_err(|e| e.to_string())?;

    let exe = dir.join(PROGRAM);
    report(p, 0.92, "Adding shortcuts");
    if o.start_menu
        && let Some(menu) = win::start_menu_dir()
    {
        win::shortcut(&menu.join(SHORTCUT), &exe, dir)?;
    }
    if let Some(desk) = dirs::desktop_dir() {
        let lnk = desk.join(SHORTCUT);
        if o.desktop {
            win::shortcut(&lnk, &exe, dir)?;
        } else if lnk.exists() {
            let _ = std::fs::remove_file(lnk);
        }
    }
    if o.register {
        report(p, 0.96, "Adding Kairo to Settings > Apps");
        win::register(dir, VERSION, (folder_size(dir) / 1024) as u32)?;
    }
    if let Some(on) = o.autostart {
        let _ = win::run_hidden(
            &exe,
            &["autostart", if on { "on" } else { "off" }],
            Duration::from_secs(10),
        );
    }
    report(p, 1.0, "Done");
    Ok(())
}

/// The files an install wrote in `dir`.
fn installed_files(dir: &Path) -> Vec<PathBuf> {
    match std::fs::read_to_string(dir.join(FILE_LIST)) {
        Ok(t) => t
            .lines()
            .filter_map(entry_path_in_dir)
            .chain(std::iter::once(PathBuf::from(UNINSTALLER)))
            .collect(),
        Err(_) => ZIP_FILES.iter().map(PathBuf::from).collect(),
    }
}

/// A line of the file list, if it stays inside the install folder.
fn entry_path_in_dir(line: &str) -> Option<PathBuf> {
    let line = line.trim();
    let p = Path::new(line);
    (!line.is_empty()
        && p.is_relative()
        && !p
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir)))
    .then(|| p.to_path_buf())
}

/// Removes empty folders under `dir` (deepest first), then `dir` if it's empty.
fn remove_empty(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                remove_empty(&e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// Uninstalls Kairo from `r.dir`. The uninstaller itself is left for the caller, which may be
/// running it.
pub fn uninstall(r: &Removal, p: &Shared) -> Result<(), String> {
    let dir = &r.dir;
    let exe = dir.join(PROGRAM);
    if r.system {
        report(p, 0.05, "Closing Kairo");
        close_kairo(dir);
        if exe.exists() {
            let _ = win::run_hidden(&exe, &["autostart", "off"], Duration::from_secs(10));
        }
        report(p, 0.2, "Removing shortcuts");
        for folder in [win::start_menu_dir(), dirs::desktop_dir()]
            .into_iter()
            .flatten()
        {
            let lnk = folder.join(SHORTCUT);
            if lnk.exists() {
                let _ = std::fs::remove_file(lnk);
            }
        }
        win::unregister();
    }
    report(p, 0.4, "Removing files");
    let files = installed_files(dir);
    let mut stuck = None;
    for f in &files {
        if f == Path::new(UNINSTALLER) {
            continue;
        }
        let path = dir.join(f);
        if path.exists()
            && let Err(e) = std::fs::remove_file(&path)
        {
            stuck = Some(format!("{} ({e})", path.display()));
        }
    }
    remove_set_aside(dir);
    let _ = std::fs::remove_file(dir.join(FILE_LIST));
    for f in &files {
        if let Some(parent) = f.parent().filter(|p| !p.as_os_str().is_empty()) {
            remove_empty(&dir.join(parent));
        }
    }
    if r.settings {
        report(p, 0.8, "Removing settings");
        for s in [settings_dir(), old_settings_dir()].into_iter().flatten() {
            let _ = std::fs::remove_dir_all(s);
        }
        win::forget_secrets();
    }
    report(p, 1.0, "Done");
    match stuck {
        Some(f) => Err(format!("Couldn't remove {f}. Close Kairo and try again.")),
        None => Ok(()),
    }
}

/// Deletes the uninstaller and then the folder, once nothing runs from it (the copy of the
/// uninstaller that started this one may take a moment to close).
pub fn remove_uninstaller(dir: &Path) {
    let path = dir.join(UNINSTALLER);
    let start = Instant::now();
    while path.exists() && start.elapsed() < Duration::from_secs(10) {
        if std::fs::remove_file(&path).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    remove_empty(dir);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn release_zip() -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            for (name, data) in [
                (
                    "kairo-v9.9.9-windows-x64-portable/kairo.exe",
                    &b"new program"[..],
                ),
                ("kairo-v9.9.9-windows-x64-portable/ffmpeg.exe", b"ffmpeg"),
                ("kairo-v9.9.9-windows-x64-portable\\README.md", b"readme"),
                ("kairo-v9.9.9-windows-x64-portable/../../evil.txt", b"no"),
            ] {
                z.start_file(name, o).unwrap();
                z.write_all(data).unwrap();
            }
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    fn quiet(dir: &Path) -> Options {
        Options {
            dir: dir.to_path_buf(),
            start_menu: false,
            desktop: false,
            autostart: None,
            register: false,
            stop_running: false,
            replace: None,
        }
    }

    #[test]
    fn install_folders_are_kairos_own() {
        let base = Path::new(r"C:\Games");
        assert_eq!(install_dir(base), base.join("Kairo"));
        assert_eq!(
            install_dir(&base.join("kairo")),
            base.join("kairo"),
            "already its own folder"
        );
        assert_eq!(
            entry_path("top/a/b.txt"),
            Some(PathBuf::from("a").join("b.txt"))
        );
        assert_eq!(entry_path("only.txt"), Some(PathBuf::from("only.txt")));
        assert_eq!(entry_path("top/../x"), None);
        assert_eq!(entry_path("top/C:/x"), None);
    }

    #[test]
    fn installs_updates_and_uninstalls_only_its_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Kairo");
        let installer = tmp.path().join("setup.exe");
        std::fs::write(&installer, b"MZ installer").unwrap();
        // Something of the user's in the same folder stays put.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), b"mine").unwrap();
        let p = Shared::default();

        install(&release_zip(), &installer, &quiet(&dir), &p).unwrap();
        assert_eq!(
            std::fs::read(dir.join("kairo.exe")).unwrap(),
            b"new program"
        );
        assert_eq!(std::fs::read(dir.join("README.md")).unwrap(), b"readme");
        assert_eq!(
            std::fs::read(dir.join(UNINSTALLER)).unwrap(),
            b"MZ installer"
        );
        assert!(!tmp.path().join("evil.txt").exists());
        assert_eq!(p.lock().unwrap().fraction, 1.0);

        // Updating over a running copy, opened the way Windows opens a program it runs (others
        // may read it or rename it, not write it): the old program steps aside.
        #[cfg(windows)]
        let running = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(1 | 4)
                .open(dir.join("kairo.exe"))
                .unwrap()
        };
        install(&release_zip(), &installer, &quiet(&dir), &p).unwrap();
        assert_eq!(
            std::fs::read(dir.join("kairo.exe")).unwrap(),
            b"new program"
        );
        #[cfg(windows)]
        drop(running);

        let r = Removal {
            dir: dir.clone(),
            settings: false,
            system: false,
        };
        uninstall(&r, &p).unwrap();
        remove_uninstaller(&dir);
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(left, vec!["notes.txt"]);
    }

    #[test]
    fn replaces_an_old_rewindr() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("Rewindr");
        std::fs::create_dir_all(&old).unwrap();
        for f in ["rewindr.exe", "ffmpeg.exe", "LICENSE", "rewindr.old"] {
            std::fs::write(old.join(f), b"x").unwrap();
        }
        let dir = tmp.path().join("Kairo");
        let installer = tmp.path().join("setup.exe");
        std::fs::write(&installer, b"MZ installer").unwrap();
        let o = Options {
            replace: Some(old.clone()),
            ..quiet(&dir)
        };
        install(&release_zip(), &installer, &o, &Shared::default()).unwrap();
        assert!(!old.exists(), "Rewindr's folder is gone");
        assert!(dir.join("kairo.exe").exists());

        // Something of the user's keeps the folder (and itself) in place.
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("rewindr.exe"), b"x").unwrap();
        std::fs::write(old.join("mine.txt"), b"x").unwrap();
        install(&release_zip(), &installer, &o, &Shared::default()).unwrap();
        assert!(old.join("mine.txt").exists());
        assert!(!old.join("rewindr.exe").exists());
    }

    #[test]
    fn a_zip_without_kairo_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let mut buf = Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("top/other.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            z.finish().unwrap();
        }
        assert!(extract(&buf.into_inner(), tmp.path(), |_| {}).is_err());
    }

    #[test]
    fn a_hand_unzipped_copy_uninstalls_too() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        for f in ["kairo.exe", "ffmpeg.exe", "LICENSE"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let r = Removal {
            dir: dir.clone(),
            settings: false,
            system: false,
        };
        uninstall(&r, &Shared::default()).unwrap();
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
    }
}
