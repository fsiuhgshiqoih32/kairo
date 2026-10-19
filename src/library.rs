//! The clips folder: listing clips, which game each is from, favourites, and keeping the
//! folder under a size limit.

use std::collections::BTreeSet;
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

/// Clips the user starred and links they shared, by file name. Stored in `clips.json` in the
/// config folder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    pub favorites: BTreeSet<String>,
    pub links: std::collections::BTreeMap<String, Link>,
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

    /// Keeps a favourite's star and a shared link when the file is renamed.
    pub fn renamed(&mut self, from: &Path, to: &Path) {
        if self.is_favorite(from) {
            self.set_favorite(from, false);
            self.set_favorite(to, true);
        }
        let key = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_string());
        if let (Some(f), Some(t)) = (key(from), key(to))
            && let Some(l) = self.links.remove(&f)
        {
            self.links.insert(t, l);
        }
    }
}

/// Deletes the oldest clips (never favourites) until the folder's clips total at most
/// `limit_bytes`. Returns what was deleted.
pub fn clean_up(dir: &Path, limit_bytes: u64, lib: &Library) -> Vec<PathBuf> {
    let clips = scan(dir);
    let mut total: u64 = clips.iter().map(|c| c.size).sum();
    let mut deleted = Vec::new();
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
}
