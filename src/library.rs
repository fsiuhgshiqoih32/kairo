//! The clips folder: listing clips, which game each is from, favourites, tags, the Recently
//! deleted folder, and keeping the folder under a size limit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub path: PathBuf,
    /// The file name without `.mp4`.
    pub name: String,
    pub size: u64,
    pub modified: SystemTime,
}

impl Clip {
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    pub fn game(&self) -> String {
        game_of(&self.name)
    }
}

/// The clips in `dir`, newest first.
pub fn scan(dir: &Path) -> Vec<Clip> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<Clip> = rd
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("mp4"))
        })
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            meta.is_file().then_some(())?;
            Some(Clip {
                name: e.path().file_stem()?.to_string_lossy().to_string(),
                path: e.path(),
                size: meta.len(),
                modified: meta.modified().ok()?,
            })
        })
        .collect();
    v.sort_by_key(|c| std::cmp::Reverse(c.modified));
    v
}

/// The game (or app) a clip came from: its name without the date Rewindr adds and the
/// suffixes of copies ("(trim)", "(Discord)", "(2)").
pub fn game_of(name: &str) -> String {
    let mut s = name.trim();
    loop {
        let before = s;
        if let Some(i) = s.rfind(" (")
            && s.ends_with(')')
        {
            let inner = &s[i + 2..s.len() - 1];
            let ours = inner.chars().all(|c| c.is_ascii_digit())
                || inner.starts_with("trim")
                || inner.starts_with("Discord");
            if ours {
                s = s[..i].trim_end();
            }
        }
        // " 2026-10-18 16-54-03"
        if s.len() >= 20 {
            let tail = &s[s.len() - 20..];
            let b = tail.as_bytes();
            let digits = [1, 2, 3, 4, 6, 7, 9, 10, 12, 13, 15, 16, 18, 19];
            if b[0] == b' '
                && b[5] == b'-'
                && b[8] == b'-'
                && b[11] == b' '
                && b[14] == b'-'
                && b[17] == b'-'
                && digits.iter().all(|&i| b[i].is_ascii_digit())
            {
                s = s[..s.len() - 20].trim_end();
            }
        }
        if s == before {
            break;
        }
    }
    if s.is_empty() || s == "Rewindr" {
        "Desktop".to_string()
    } else {
        s.to_string()
    }
}

/// Clips the user starred, links they shared and moments they marked, by file name. Stored in
/// `clips.json` in the config folder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    pub favorites: BTreeSet<String>,
    pub links: std::collections::BTreeMap<String, Link>,
    /// Marked moments, in seconds from the clip's start.
    pub markers: std::collections::BTreeMap<String, Vec<f64>>,
    /// Tags the user put on clips.
    pub tags: BTreeMap<String, BTreeSet<String>>,
}

/// Tags are short: this many characters at most.
pub const MAX_TAG: usize = 24;

/// A tag as typed, tidied: trimmed, single spaces, at most [`MAX_TAG`] characters.
pub fn clean_tag(tag: &str) -> String {
    let words: Vec<&str> = tag.split_whitespace().collect();
    words
        .join(" ")
        .trim_start_matches('#')
        .chars()
        .take(MAX_TAG)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Marks closer together than this are one.
const SAME_MARK: f64 = 0.25;

/// The marks (wall-clock µs) inside `start_us..=end_us`, as seconds into a clip that starts at
/// `start_us`.
pub fn marks_within(marks_us: &[i64], start_us: i64, end_us: i64) -> Vec<f64> {
    let mut v: Vec<f64> = marks_us
        .iter()
        .filter(|m| (start_us..=end_us).contains(*m))
        .map(|m| (m - start_us) as f64 / 1e6)
        .collect();
    tidy(&mut v);
    v
}

/// Sorted, without near-duplicates.
fn tidy(v: &mut Vec<f64>) {
    v.retain(|t| t.is_finite() && *t >= 0.0);
    v.sort_by(f64::total_cmp);
    v.dedup_by(|b, a| *b - *a < SAME_MARK);
}

/// A share link for a clip.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Link {
    pub url: String,
    /// When it was made, in Unix seconds.
    pub created: u64,
    /// When it stops working, in Unix seconds; `None` for links that last.
    pub expires: Option<u64>,
}

impl Link {
    pub fn expired(&self, now: u64) -> bool {
        self.expires.is_some_and(|e| e <= now)
    }
}

impl Library {
    pub fn path() -> PathBuf {
        crate::config::config_dir().join("clips.json")
    }

    pub fn load() -> Library {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    pub fn is_favorite(&self, clip: &Path) -> bool {
        clip.file_name()
            .is_some_and(|n| self.favorites.contains(&*n.to_string_lossy()))
    }

    pub fn set_favorite(&mut self, clip: &Path, on: bool) {
        let Some(n) = clip.file_name().map(|n| n.to_string_lossy().to_string()) else {
            return;
        };
        if on {
            self.favorites.insert(n);
        } else {
            self.favorites.remove(&n);
        }
    }

    /// The clip's share link, if it has one that still works.
    pub fn link(&self, clip: &Path, now: u64) -> Option<&Link> {
        let n = clip.file_name()?.to_string_lossy().to_string();
        self.links.get(&n).filter(|l| !l.expired(now))
    }

    pub fn set_link(&mut self, clip: &Path, link: Option<Link>) {
        let Some(n) = clip.file_name().map(|n| n.to_string_lossy().to_string()) else {
            return;
        };
        match link {
            Some(l) => {
                self.links.insert(n, l);
            }
            None => {
                self.links.remove(&n);
            }
        }
    }

    /// The clip's marked moments, in seconds, in order.
    pub fn markers(&self, clip: &Path) -> &[f64] {
        clip.file_name()
            .and_then(|n| self.markers.get(&*n.to_string_lossy()))
            .map_or(&[], Vec::as_slice)
    }

    pub fn set_markers(&mut self, clip: &Path, mut marks: Vec<f64>) {
        let Some(n) = clip.file_name().map(|n| n.to_string_lossy().to_string()) else {
            return;
        };
        tidy(&mut marks);
        if marks.is_empty() {
            self.markers.remove(&n);
        } else {
            self.markers.insert(n, marks);
        }
    }

    /// Adds a mark at `t`, or takes away the one there already. Returns the clip's marks.
    pub fn toggle_marker(&mut self, clip: &Path, t: f64) -> Vec<f64> {
        let mut marks = self.markers(clip).to_vec();
        match marks.iter().position(|m| (m - t).abs() < SAME_MARK) {
            Some(i) => {
                marks.remove(i);
            }
            None => marks.push(t),
        }
        self.set_markers(clip, marks);
        self.markers(clip).to_vec()
    }

    /// The clip's tags, by name.
    pub fn tags(&self, clip: &Path) -> Vec<String> {
        clip.file_name()
            .and_then(|n| self.tags.get(&*n.to_string_lossy()))
            .map(|t| t.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Tags a clip. A tag that's already in use elsewhere keeps its spelling ("Clutch", not
    /// "clutch"). Returns false when there was nothing to add.
    pub fn add_tag(&mut self, clip: &Path, tag: &str) -> bool {
        let tag = clean_tag(tag);
        let Some(n) = clip.file_name().map(|n| n.to_string_lossy().to_string()) else {
            return false;
        };
        if tag.is_empty() {
            return false;
        }
        let tag = self
            .tags
            .values()
            .flatten()
            .find(|t| t.eq_ignore_ascii_case(&tag))
            .cloned()
            .unwrap_or(tag);
        self.tags.entry(n).or_default().insert(tag)
    }

    pub fn remove_tag(&mut self, clip: &Path, tag: &str) {
        let Some(n) = clip.file_name().map(|n| n.to_string_lossy().to_string()) else {
            return;
        };
        if let Some(t) = self.tags.get_mut(&n) {
            t.remove(tag);
            if t.is_empty() {
                self.tags.remove(&n);
            }
        }
    }

    /// Every tag in use, and on how many clips, by name.
    pub fn all_tags(&self) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for t in self.tags.values().flatten() {
            *counts.entry(t.clone()).or_default() += 1;
        }
        let mut v: Vec<(String, usize)> = counts.into_iter().collect();
        v.sort_by_key(|(t, _)| t.to_lowercase());
        v
    }

    /// Forgets everything kept about a clip that's gone for good.
    pub fn forget(&mut self, clip: &Path) {
        self.set_favorite(clip, false);
        self.set_link(clip, None);
        self.set_markers(clip, Vec::new());
        if let Some(n) = clip.file_name() {
            self.tags.remove(&*n.to_string_lossy());
        }
    }

    /// Keeps a favourite's star, a shared link and marks when the file is renamed.
    pub fn renamed(&mut self, from: &Path, to: &Path) {
        if self.is_favorite(from) {
            self.set_favorite(from, false);
            self.set_favorite(to, true);
        }
        let key = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_string());
        if let (Some(f), Some(t)) = (key(from), key(to)) {
            if let Some(l) = self.links.remove(&f) {
                self.links.insert(t.clone(), l);
            }
            if let Some(m) = self.markers.remove(&f) {
                self.markers.insert(t.clone(), m);
            }
            if let Some(tags) = self.tags.remove(&f) {
                self.tags.insert(t, tags);
            }
        }
    }
}

/// Deletes for good until the folder's clips and Recently deleted total at most `limit_bytes`:
/// what's in Recently deleted first, then the oldest clips (never favourites). Returns where
/// what went was saved.
pub fn clean_up(dir: &Path, limit_bytes: u64, lib: &Library) -> Vec<PathBuf> {
    let clips = scan(dir);
    let bin = trashed(dir);
    let mut total: u64 =
        clips.iter().map(|c| c.size).sum::<u64>() + bin.iter().map(|t| t.size).sum::<u64>();
    let mut deleted = Vec::new();
    for t in bin.iter().rev() {
        if total <= limit_bytes {
            break;
        }
        if purge(dir, &t.path).is_ok() {
            total = total.saturating_sub(t.size);
            deleted.push(t.from.clone());
        }
    }
    for c in clips.iter().rev() {
        if total <= limit_bytes {
            break;
        }
        if lib.is_favorite(&c.path) {
            continue;
        }
        if std::fs::remove_file(&c.path).is_ok() {
            total = total.saturating_sub(c.size);
            deleted.push(c.path.clone());
        }
    }
    deleted
}

// --- Recently deleted ------------------------------------------------------------------------

/// Deleted clips and screenshots wait in this folder inside the clips folder, so a delete can be
/// undone.
pub const TRASH_DIR: &str = "Recently deleted";
/// How long they wait before they're gone for good.
pub const TRASH_DAYS: u64 = 30;
const TRASH_SECONDS: u64 = TRASH_DAYS * 24 * 3600;
/// Where each file in the trash came from, and when it was deleted.
const TRASH_LIST: &str = ".deleted.json";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct TrashEntry {
    from: PathBuf,
    deleted: u64,
}

/// A file in Recently deleted.
#[derive(Clone, Debug, PartialEq)]
pub struct Trashed {
    /// Where it is now.
    pub path: PathBuf,
    /// Where it was.
    pub from: PathBuf,
    /// When it was deleted, in Unix seconds.
    pub deleted: u64,
    pub size: u64,
}

impl Trashed {
    /// When it goes for good, in Unix seconds.
    pub fn purge_at(&self) -> u64 {
        self.deleted + TRASH_SECONDS
    }
}

pub fn trash_dir(dir: &Path) -> PathBuf {
    dir.join(TRASH_DIR)
}

fn read_trash_list(dir: &Path) -> BTreeMap<String, TrashEntry> {
    std::fs::read_to_string(trash_dir(dir).join(TRASH_LIST))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_trash_list(dir: &Path, list: &BTreeMap<String, TrashEntry>) {
    let path = trash_dir(dir).join(TRASH_LIST);
    if list.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(list) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn unix(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `path`, or "<stem> (2).<ext>" and so on if that's taken.
fn free_name(path: &Path) -> PathBuf {
    let dir = path.parent().unwrap_or(Path::new("."));
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut p = path.to_path_buf();
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    p
}

/// Moves a file, copying when it has to cross drives. Fails without leaving a copy behind.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    std::fs::remove_file(from).inspect_err(|_| {
        let _ = std::fs::remove_file(to);
    })
}

/// Moves `file` (and its captions) into Recently deleted in `dir`. Returns where it went.
pub fn trash(dir: &Path, file: &Path, now: u64) -> std::io::Result<PathBuf> {
    let bin = trash_dir(dir);
    std::fs::create_dir_all(&bin)?;
    let name = file
        .file_name()
        .ok_or_else(|| std::io::Error::other("not a file"))?;
    let to = free_name(&bin.join(name));
    move_file(file, &to)?;
    let srt = file.with_extension("srt");
    if srt.exists() {
        let _ = move_file(&srt, &to.with_extension("srt"));
    }
    let mut list = read_trash_list(dir);
    list.insert(
        to.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
        TrashEntry {
            from: file.to_path_buf(),
            deleted: now,
        },
    );
    write_trash_list(dir, &list);
    Ok(to)
}

/// What's in Recently deleted, most recently deleted first. Files someone put there by hand
/// count too, as if deleted when they were last changed.
pub fn trashed(dir: &Path) -> Vec<Trashed> {
    let Ok(rd) = std::fs::read_dir(trash_dir(dir)) else {
        return Vec::new();
    };
    let list = read_trash_list(dir);
    let mut v: Vec<Trashed> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let name = e.file_name().to_string_lossy().to_string();
            if !meta.is_file() || name.starts_with('.') || name.ends_with(".srt") {
                return None;
            }
            let entry = list.get(&name);
            Some(Trashed {
                path: e.path(),
                from: entry.map_or_else(|| dir.join(&name), |t| t.from.clone()),
                deleted: entry.map_or_else(|| meta.modified().map_or(0, unix), |t| t.deleted),
                size: meta.len(),
            })
        })
        .collect();
    v.sort_by_key(|t| std::cmp::Reverse(t.deleted));
    v
}

/// Puts a deleted file back where it was (under a free name if that's taken now). Returns
/// where it went.
pub fn restore(dir: &Path, trashed: &Path) -> std::io::Result<PathBuf> {
    let name = trashed
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut list = read_trash_list(dir);
    let from = list
        .get(&name)
        .map_or_else(|| dir.join(&name), |t| t.from.clone());
    if let Some(parent) = from.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let to = free_name(&from);
    move_file(trashed, &to)?;
    let srt = trashed.with_extension("srt");
    if srt.exists() {
        let _ = move_file(&srt, &to.with_extension("srt"));
    }
    list.remove(&name);
    write_trash_list(dir, &list);
    Ok(to)
}

/// Deletes a file in Recently deleted for good.
pub fn purge(dir: &Path, trashed: &Path) -> std::io::Result<()> {
    std::fs::remove_file(trashed)?;
    let _ = std::fs::remove_file(trashed.with_extension("srt"));
    let mut list = read_trash_list(dir);
    if let Some(n) = trashed.file_name() {
        list.remove(&*n.to_string_lossy());
    }
    write_trash_list(dir, &list);
    Ok(())
}

/// Deletes for good what's waited in Recently deleted for [`TRASH_DAYS`]. Returns what went.
pub fn purge_expired(dir: &Path, now: u64) -> Vec<Trashed> {
    trashed(dir)
        .into_iter()
        .filter(|t| t.purge_at() <= now)
        .filter(|t| purge(dir, &t.path).is_ok())
        .collect()
}

/// Clips in `dir` and its Sessions folder older than `days` (by when they were saved), except
/// favourites: the ones the "keep clips" setting moves to Recently deleted.
pub fn expired_clips(dir: &Path, days: u32, lib: &Library, now: u64) -> Vec<PathBuf> {
    if days == 0 {
        return Vec::new();
    }
    let cutoff = now.saturating_sub(days as u64 * 24 * 3600);
    scan(dir)
        .into_iter()
        .chain(scan(&dir.join("Sessions")))
        .filter(|c| unix(c.modified) < cutoff && !lib.is_favorite(&c.path))
        .map(|c| c.path)
        .collect()
}

/// A name for a new file next to `clip`: "<name> (<what>).mp4", numbered if taken.
pub fn sibling(clip: &Path, what: &str) -> PathBuf {
    sibling_with(clip, what, "mp4")
}

/// Like [`sibling`], with another file type ("gif").
pub fn sibling_with(clip: &Path, what: &str, ext: &str) -> PathBuf {
    let dir = clip.parent().unwrap_or(Path::new("."));
    let stem = clip
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut p = dir.join(format!("{stem} ({what}).{ext}"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{stem} ({what} {n}).{ext}"));
        n += 1;
    }
    p
}

/// Checks a new clip name typed by the user; returns the new path.
pub fn rename_target(clip: &Path, new_name: &str) -> Result<PathBuf, String> {
    let name = new_name.trim().trim_end_matches(".mp4").trim();
    if name.is_empty() {
        return Err("The name can't be empty.".into());
    }
    if name.chars().any(|c| {
        matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control()
    }) {
        return Err("Names can't contain < > : \" / \\ | ? *".into());
    }
    let to = clip
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{name}.mp4"));
    if to != clip && to.exists() {
        return Err("A clip with that name already exists.".into());
    }
    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn games_from_names() {
        assert_eq!(
            game_of("Minecraft 26.3 - Multiplayer (3rd-party Server) 2026-10-18 16-54-03"),
            "Minecraft 26.3 - Multiplayer (3rd-party Server)"
        );
        assert_eq!(game_of("VALORANT 2026-10-18 16-54-03 (2)"), "VALORANT");
        assert_eq!(game_of("VALORANT 2026-10-18 16-54-03 (trim)"), "VALORANT");
        assert_eq!(
            game_of("VALORANT 2026-10-18 16-54-03 (trim) (Discord)"),
            "VALORANT"
        );
        assert_eq!(game_of("Rewindr 2026-10-18 16-54-03"), "Desktop");
        assert_eq!(game_of("my best shot"), "my best shot");
    }

    #[test]
    fn favourites_and_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |name: &str, kb: usize, age_s: u64| {
            let p = dir.path().join(name);
            std::fs::write(&p, vec![0u8; kb * 1024]).unwrap();
            let t = SystemTime::now() - std::time::Duration::from_secs(age_s);
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(t)
                .unwrap();
            p
        };
        let oldest = mk("a.mp4", 100, 300);
        let old_fav = mk("b.mp4", 100, 200);
        let newer = mk("c.mp4", 100, 100);
        let newest = mk("d.mp4", 100, 10);
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        let clips = scan(dir.path());
        assert_eq!(clips.len(), 4);
        assert_eq!(clips[0].path, newest);
        let mut lib = Library::default();
        lib.set_favorite(&old_fav, true);
        assert!(lib.is_favorite(&old_fav));
        // Keep 250 KB: delete the oldest non-favourite clips first.
        let deleted = clean_up(dir.path(), 250 * 1024, &lib);
        assert_eq!(deleted, vec![oldest.clone(), newer.clone()]);
        assert!(old_fav.exists() && newest.exists());
        let to = dir.path().join("renamed.mp4");
        lib.renamed(&old_fav, &to);
        assert!(lib.is_favorite(&to) && !lib.is_favorite(&old_fav));
    }

    #[test]
    fn names_for_copies_and_renames() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("Game 2026-10-18 16-54-03.mp4");
        std::fs::write(&clip, b"x").unwrap();
        let s = sibling(&clip, "trim");
        assert!(s.ends_with("Game 2026-10-18 16-54-03 (trim).mp4"));
        std::fs::write(&s, b"x").unwrap();
        assert!(sibling(&clip, "trim").ends_with("Game 2026-10-18 16-54-03 (trim 2).mp4"));
        assert!(rename_target(&clip, "  ").is_err());
        assert!(rename_target(&clip, "a/b").is_err());
        assert!(rename_target(&clip, "Game 2026-10-18 16-54-03 (trim)").is_err());
        assert_eq!(
            rename_target(&clip, "Ace clutch.mp4").unwrap(),
            dir.path().join("Ace clutch.mp4")
        );
        assert_eq!(
            rename_target(&clip, "Game 2026-10-18 16-54-03").unwrap(),
            clip
        );
    }

    #[test]
    fn links_follow_renames_and_expire() {
        let mut lib = Library::default();
        let a = Path::new("/clips/a.mp4");
        let b = Path::new("/clips/b.mp4");
        lib.set_link(
            a,
            Some(Link {
                url: "https://x/1.mp4".into(),
                created: 10,
                expires: Some(100),
            }),
        );
        assert_eq!(
            lib.link(a, 50).map(|l| l.url.as_str()),
            Some("https://x/1.mp4")
        );
        assert!(lib.link(a, 100).is_none(), "expired");
        lib.renamed(a, b);
        assert!(lib.link(a, 50).is_none());
        assert!(lib.link(b, 50).is_some());
        // Files written before links existed still load.
        let old: Library = serde_json::from_str(r#"{"favorites":["x.mp4"]}"#).unwrap();
        assert!(old.links.is_empty());
    }

    #[test]
    fn marks_land_in_the_clip_that_holds_them() {
        let s = 1_000_000i64;
        let marks = [2 * s, 5 * s, 5 * s + 100_000, 9 * s, 30 * s];
        // A clip from 4 s to 10 s: the marks at 5 s (twice, close together) and 9 s.
        assert_eq!(marks_within(&marks, 4 * s, 10 * s), [1.0, 5.0]);
        assert!(marks_within(&marks, 40 * s, 50 * s).is_empty());
    }

    #[test]
    fn marks_are_kept_sorted_and_follow_renames() {
        let mut lib = Library::default();
        let a = Path::new("/clips/a.mp4");
        lib.set_markers(a, vec![9.0, 1.5, 1.6, f64::NAN, -2.0]);
        assert_eq!(lib.markers(a), [1.5, 9.0]);
        assert_eq!(lib.toggle_marker(a, 4.0), [1.5, 4.0, 9.0]);
        assert_eq!(lib.toggle_marker(a, 4.1), [1.5, 9.0]);
        let b = Path::new("/clips/b.mp4");
        lib.renamed(a, b);
        assert!(lib.markers(a).is_empty());
        assert_eq!(lib.markers(b), [1.5, 9.0]);
        lib.set_markers(b, Vec::new());
        assert!(lib.markers.is_empty());
        // Old files without marks still load.
        let old: Library = serde_json::from_str(r#"{"favorites":["x.mp4"]}"#).unwrap();
        assert!(old.markers.is_empty());
    }

    #[test]
    fn tags_keep_one_spelling_and_follow_renames() {
        let mut lib = Library::default();
        let a = Path::new("/c/A.mp4");
        let b = Path::new("/c/B.mp4");
        assert!(lib.add_tag(a, "  #Clutch   round  "));
        assert!(!lib.add_tag(a, "clutch round"), "same tag, other case");
        assert!(lib.add_tag(b, "CLUTCH ROUND"));
        assert!(lib.add_tag(b, "funny"));
        assert!(!lib.add_tag(b, "   "));
        assert_eq!(lib.tags(b), vec!["Clutch round", "funny"]);
        assert_eq!(
            lib.all_tags(),
            vec![("Clutch round".to_string(), 2), ("funny".to_string(), 1)]
        );
        assert_eq!(clean_tag(&"x".repeat(40)).len(), MAX_TAG);
        lib.renamed(b, Path::new("/c/C.mp4"));
        assert!(lib.tags(b).is_empty());
        assert_eq!(lib.tags(Path::new("/c/C.mp4")).len(), 2);
        lib.remove_tag(a, "Clutch round");
        assert!(!lib.tags.contains_key("A.mp4"));
        lib.set_favorite(Path::new("/c/C.mp4"), true);
        lib.forget(Path::new("/c/C.mp4"));
        assert_eq!(lib, Library::default());
    }

    #[test]
    fn recently_deleted_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join("Sessions")).unwrap();
        let clip = d.join("Game 2026-10-19 10-00-00.mp4");
        let session = d.join("Sessions").join("Game session.mp4");
        std::fs::write(&clip, b"clip").unwrap();
        std::fs::write(clip.with_extension("srt"), b"1").unwrap();
        std::fs::write(&session, b"session").unwrap();

        let t1 = trash(d, &clip, 1_000).unwrap();
        let t2 = trash(d, &session, 2_000).unwrap();
        assert!(!clip.exists() && !clip.with_extension("srt").exists());
        assert!(t1.with_extension("srt").exists());
        // Deleted clips aren't clips any more.
        assert!(scan(d).is_empty());
        let list = trashed(d);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].from, session, "most recent first");
        assert_eq!(list[1].size, 4);

        // A new file with the same name was saved since: restoring doesn't overwrite it.
        std::fs::write(&clip, b"new").unwrap();
        let back = restore(d, &t1).unwrap();
        assert_ne!(back, clip);
        assert_eq!(std::fs::read(&back).unwrap(), b"clip");
        assert!(back.with_extension("srt").exists());
        assert_eq!(restore(d, &t2).unwrap(), session);
        assert!(trashed(d).is_empty());

        // Gone for good after 30 days, not before.
        let t3 = trash(d, &session, 5_000).unwrap();
        assert!(purge_expired(d, 5_000 + TRASH_SECONDS - 1).is_empty());
        assert_eq!(purge_expired(d, 5_000 + TRASH_SECONDS).len(), 1);
        assert!(!t3.exists());
        assert!(!trash_dir(d).join(TRASH_LIST).exists());
    }

    #[test]
    fn old_clips_expire_but_favourites_stay() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let old = d.join("Old.mp4");
        let fav = d.join("Fav.mp4");
        let new = d.join("New.mp4");
        for p in [&old, &fav, &new] {
            std::fs::write(p, b"x").unwrap();
        }
        let now = unix(SystemTime::now());
        let ten_days_ago = SystemTime::now() - std::time::Duration::from_secs(10 * 24 * 3600);
        for p in [&old, &fav] {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(ten_days_ago)
                .unwrap();
        }
        let mut lib = Library::default();
        lib.set_favorite(&fav, true);
        assert_eq!(expired_clips(d, 7, &lib, now), vec![old.clone()]);
        assert!(expired_clips(d, 30, &lib, now).is_empty());
        assert!(expired_clips(d, 0, &lib, now).is_empty());
    }
}
