//! Importing clips from Medal (medal.tv's clipper): finding its clips folder, listing the clips
//! with their game, title and time, and copying them into Kairo's clips folder. Medal's files are
//! only ever read, never moved, changed or deleted.
//!
//! How Medal keeps them (Windows): one folder, `C:\Medal` unless changed in Medal's settings,
//! holding `Clips\<Game>\MedalTV<Game><YYYYMMDDHHMMSSmmm>.mp4`, trims and edits in `Edits\`,
//! `Screenshots\<Game>\*.png`, and its own `Thumbnails\`, `Reels\` (filmstrips), `Temp\` and
//! `Databases\`. The database, `medal-<user>.db` (SQLite, also copied to
//! `%APPDATA%\Medal\medal-<user>.db`), has the folder setting (table `key_values`, key
//! `ClipFolder`) and a row per clip (table `contents`: `video_path` or `image_path`, `created_at`
//! in Unix seconds, `thumbnail_path`, and `metadata` with `title`, `isFavorited` and more), its
//! values in SQLite's binary JSON.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::library::{self, Library};
use crate::sqlite::Db;

/// The Kairo folder imported clips go in.
pub const FOLDER: &str = "From Medal";

/// Medal's working folders inside its clips folder: nothing to import there.
const SKIP: [&str; 5] = [
    "Temp",
    "Thumbnails",
    "Reels",
    "Databases",
    "Recently deleted",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Clip,
    Screenshot,
}

/// A clip or screenshot in Medal's folder.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub path: PathBuf,
    pub kind: Kind,
    pub game: String,
    /// The title the user gave it in Medal (not one Medal made up from what happened in a match).
    pub title: Option<String>,
    /// When it was recorded.
    pub created: SystemTime,
    pub modified: SystemTime,
    pub size: u64,
    pub favorite: bool,
    /// Medal's own picture of it.
    pub thumb: Option<PathBuf>,
}

/// What a look for Medal's clips found.
#[derive(Clone, Debug, Default)]
pub struct Found {
    /// Medal folders with something in them, the one Medal's settings name first.
    pub folders: Vec<PathBuf>,
    /// Newest first.
    pub items: Vec<Item>,
}

impl Found {
    pub fn count(&self, kind: Kind) -> usize {
        self.items.iter().filter(|i| i.kind == kind).count()
    }

    pub fn bytes(&self) -> u64 {
        self.items.iter().map(|i| i.size).sum()
    }
}

/// Medal's settings folder, `%APPDATA%\Medal`, if it's there.
pub fn app_data() -> Option<PathBuf> {
    dirs::config_dir()
        .map(|d| d.join("Medal"))
        .filter(|d| d.is_dir())
}

/// Where Medal keeps clips unless told otherwise.
pub fn default_folders() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if cfg!(windows) {
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        v.push(PathBuf::from(format!("{drive}\\Medal")));
    }
    if let Some(d) = dirs::video_dir() {
        v.push(d.join("Medal"));
    }
    v
}

/// Medal looks to be on this computer (or to have been): its settings or its usual folder are there.
pub fn installed() -> bool {
    app_data().is_some() || default_folders().iter().any(|d| d.join("Clips").is_dir())
}

/// Medal's databases in `dir` (`medal-<user>.db`, and `medal-guest.db` from before signing in):
/// people's before the guest's, the most recently changed first.
fn databases_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<(bool, SystemTime, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            (name.starts_with("medal-") && name.ends_with(".db")).then(|| {
                let changed = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                (name == "medal-guest.db", changed, e.path())
            })
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    v.into_iter().map(|(_, _, p)| p).collect()
}

/// The clips folders Medal's settings name (`ClipFolder`), signed-in users' first.
pub fn setting_folders(app_data: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    for db in databases_in(app_data) {
        let Ok(t) = Db::open(&db).and_then(|d| d.table("key_values")) else {
            continue;
        };
        let folder = t
            .rows
            .iter()
            .find(|r| t.get(r, "key").as_str() == Some("ClipFolder"))
            .and_then(|r| t.get(r, "value").json())
            .and_then(|j| j.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty());
        // Without the "\" Medal ends it with.
        if let Some(f) = folder.map(|f| PathBuf::from(f).components().collect::<PathBuf>())
            && !v.contains(&f)
        {
            v.push(f);
        }
    }
    v
}

/// What Medal's database says about one file.
#[derive(Clone, Debug, Default)]
struct Row {
    created: Option<i64>,
    title: Option<String>,
    favorite: bool,
    /// From the folder it was saved in (`Clips\<Game>`), or the game Medal filed it under.
    game: Option<String>,
    category: Option<String>,
    thumb: Option<String>,
}

#[derive(Default)]
struct Meta {
    /// By file name, in lower case: the folder may have moved since.
    rows: HashMap<String, Row>,
    /// Medal's game ids, with the game's name, from clips in a game's folder.
    games: HashMap<String, String>,
    /// The Medal folders the database's files are in.
    roots: Vec<PathBuf>,
}

/// A path as Medal wrote it (Windows `\`), in pieces.
fn pieces(p: &str) -> Vec<&str> {
    p.split(['\\', '/']).filter(|s| !s.is_empty()).collect()
}

fn is_named(s: &str, names: &[&str]) -> bool {
    names.iter().any(|n| s.eq_ignore_ascii_case(n))
}

/// The game a file in `...\Clips\<Game>\` or `...\Screenshots\<Game>\` is from.
fn game_from_folders(parts: &[&str]) -> Option<String> {
    match parts {
        [.., group, game, _file] if is_named(group, &["Clips", "Screenshots"]) => {
            Some(game.to_string())
        }
        _ => None,
    }
}

/// Reads the clips' rows from Medal's databases; the first database to mention a file wins.
fn read_meta(dbs: &[PathBuf]) -> Meta {
    let mut meta = Meta::default();
    for db in dbs {
        let Ok(t) = Db::open(db).and_then(|d| d.table("contents")) else {
            continue;
        };
        for r in &t.rows {
            let Some(path) = t
                .get(r, "video_path")
                .as_str()
                .or_else(|| t.get(r, "image_path").as_str())
            else {
                continue;
            };
            let parts = pieces(path);
            let Some(name) = parts.last().map(|n| n.to_lowercase()) else {
                continue;
            };
            if let Some(i) = parts
                .iter()
                .position(|p| is_named(p, &["Clips", "Edits", "Screenshots"]))
                && i > 0
            {
                let sep = if path.contains('\\') { "\\" } else { "/" };
                let mut root = parts[..i].join(sep);
                if path.starts_with(sep) {
                    root.insert_str(0, sep);
                }
                if root.ends_with(':') {
                    root.push_str(sep);
                }
                let root = PathBuf::from(root);
                if !meta.roots.contains(&root) {
                    meta.roots.push(root);
                }
            }
            if meta.rows.contains_key(&name) {
                continue;
            }
            let json = t.get(r, "metadata").json().unwrap_or_default();
            // Medal titles clips from what happened ("👊3K", "♠️Ace"); those match the event's
            // name. A title of the user's own doesn't.
            let event = json["recorder"]["eventobj"]["name"].as_str().unwrap_or("");
            let title = json["title"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty() && *t != event.trim())
                .map(str::to_string);
            let category = t.get(r, "category_id").as_str().map(str::to_string);
            let game = game_from_folders(&parts).or_else(|| {
                json["remoteContent"]["category"]["categoryName"]
                    .as_str()
                    .map(str::to_string)
            });
            if let (Some(c), Some(g)) = (&category, game_from_folders(&parts)) {
                meta.games.entry(c.clone()).or_insert(g);
            }
            let created = t.get(r, "created_at").as_i64().map(|t| {
                // Seconds, or milliseconds in case it ever changes.
                if t > 100_000_000_000 { t / 1000 } else { t }
            });
            meta.rows.insert(
                name,
                Row {
                    created,
                    title,
                    favorite: json["isFavorited"].as_bool() == Some(true),
                    game,
                    category,
                    thumb: t
                        .get(r, "thumbnail_path")
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                },
            );
        }
    }
    meta
}

/// The databases to read: each folder's own (`Databases\`, the one Medal writes), then the copy
/// in Medal's settings folder.
fn databases(app_data: Option<&Path>, roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = roots
        .iter()
        .flat_map(|r| databases_in(&r.join("Databases")))
        .collect();
    if let Some(a) = app_data {
        v.extend(databases_in(a));
    }
    v
}

/// Medal's clips: in the folders its settings name, the usual places, and wherever its database
/// says clips are.
pub fn find(app_data: Option<&Path>, defaults: &[PathBuf]) -> Found {
    let mut roots = app_data.map(setting_folders).unwrap_or_default();
    roots.extend(defaults.iter().cloned());
    let meta = read_meta(&databases(app_data, &roots));
    roots.extend(meta.roots.iter().cloned());
    collect(&roots, &meta)
}

/// The clips in a folder the user picked (Medal's database still gives titles and games).
pub fn scan_folder(folder: &Path, app_data: Option<&Path>) -> Found {
    let roots = [folder.to_path_buf()];
    collect(&roots, &read_meta(&databases(app_data, &roots)))
}

/// A folder's key for telling it apart from the same folder written another way.
fn same_key(p: &Path) -> String {
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = p
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

fn collect(roots: &[PathBuf], meta: &Meta) -> Found {
    let mut found = Found::default();
    let mut seen_roots = HashSet::new();
    let mut seen = HashSet::new();
    for root in roots {
        if !root.is_dir() || !seen_roots.insert(same_key(root)) {
            continue;
        }
        let mut files = Vec::new();
        walk(root, root, 0, &mut files);
        let before = found.items.len();
        for (path, kind) in files {
            if !seen.insert(same_key(&path)) {
                continue;
            }
            if let Some(item) = item(root, path, kind, meta) {
                found.items.push(item);
            }
        }
        if found.items.len() > before {
            found.folders.push(root.clone());
        }
    }
    found.items.sort_by_key(|i| std::cmp::Reverse(i.created));
    found
}

/// The clips (any `.mp4`) and screenshots (pictures in a `Screenshots` folder) under `dir`.
fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(PathBuf, Kind)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let in_shots = dir
        .strip_prefix(root)
        .ok()
        .into_iter()
        .flat_map(|rel| rel.components())
        .chain(root.file_name().map(std::path::Component::Normal))
        .any(|c| c.as_os_str().eq_ignore_ascii_case("Screenshots"));
    for e in rd.flatten() {
        let Ok(t) = e.file_type() else { continue };
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if t.is_dir() {
            if depth < 3 && !is_named(&name, &SKIP) && !name.starts_with('.') {
                walk(root, &path, depth + 1, out);
            }
            continue;
        }
        if !t.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .map(|x| x.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "mp4" => out.push((path, Kind::Clip)),
            "png" | "jpg" | "jpeg" if in_shots => out.push((path, Kind::Screenshot)),
            _ => {}
        }
    }
}

fn item(root: &Path, path: PathBuf, kind: Kind, meta: &Meta) -> Option<Item> {
    let m = std::fs::metadata(&path).ok()?;
    let name = path.file_name()?.to_string_lossy().to_string();
    let stem = path.file_stem()?.to_string_lossy().to_string();
    let modified = m.modified().ok()?;
    let row = meta
        .rows
        .get(&name.to_lowercase())
        .cloned()
        .unwrap_or_default();
    let here = path.to_string_lossy().to_string();
    let game = game_from_folders(&pieces(&here))
        .or(row.game)
        .or_else(|| row.category.and_then(|c| meta.games.get(&c).cloned()))
        .or_else(|| game_from_name(&stem))
        .unwrap_or_else(|| "Medal".into());
    let created = row
        .created
        .and_then(|s| u64::try_from(s).ok())
        .map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s))
        .or_else(|| time_from_name(&stem))
        .unwrap_or(modified);
    let thumb = row
        .thumb
        .map(PathBuf::from)
        .filter(|t| t.is_file())
        .or_else(|| Some(root.join("Thumbnails").join(format!("{stem}.jpg"))))
        .filter(|t| t.is_file());
    Some(Item {
        path,
        kind,
        game,
        title: row.title,
        created,
        modified,
        size: m.len(),
        favorite: row.favorite,
        thumb,
    })
}

/// "MedalTVGeometryDash20260713143819888" → "Geometry Dash".
fn game_from_name(stem: &str) -> Option<String> {
    let rest = stem.strip_prefix("MedalTV")?;
    let letters: Vec<char> = rest.chars().take_while(|c| !c.is_ascii_digit()).collect();
    let mut out = String::new();
    for (i, &c) in letters.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| letters[j]);
        let next = letters.get(i + 1);
        if c.is_uppercase()
            && prev.is_some_and(|p| {
                p.is_lowercase() || (p.is_uppercase() && next.is_some_and(|n| n.is_lowercase()))
            })
        {
            out.push(' ');
        }
        out.push(c);
    }
    let out = out.trim().trim_end_matches(['-', '_']).trim().to_string();
    (!out.is_empty()).then_some(out)
}

/// The local time in Medal's file names ("...20260713143819888": 2026-07-13 14:38:19).
fn time_from_name(stem: &str) -> Option<SystemTime> {
    let digits: String = stem
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    // A game name ending in a number runs into it ("...Warfare4" + 17 digits).
    let digits = digits.get(digits.len().saturating_sub(17)..)?;
    let t = chrono::NaiveDateTime::parse_from_str(digits.get(..14)?, "%Y%m%d%H%M%S").ok()?;
    let local = t.and_local_timezone(chrono::Local).earliest()?;
    Some(local.into())
}

/// Characters a name can't have on Windows (or anywhere it's copied to).
fn clean_name(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s: String = s.chars().take(80).collect();
    let s = s.trim().trim_end_matches('.').trim().to_string();
    let reserved = ["CON", "PRN", "AUX", "NUL"];
    let upper = s.to_ascii_uppercase();
    let device = reserved.contains(&upper.as_str())
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit());
    if device { format!("{s} clip") } else { s }
}

/// The name it gets in Kairo (without the extension): its title, or "<Game> <date time>" like
/// Kairo's own clips, with " (trim)" for one Medal trimmed, as Kairo names its trimmed copies.
pub fn kairo_name(item: &Item) -> String {
    if let Some(t) = item.title.as_deref().map(clean_name)
        && !t.is_empty()
    {
        return t;
    }
    let game = clean_name(&item.game);
    let game = if game.is_empty() {
        "Medal".into()
    } else {
        game
    };
    let stamp = chrono::DateTime::<chrono::Local>::from(item.created).format("%Y-%m-%d %H-%M-%S");
    let trimmed = item
        .path
        .file_stem()
        .is_some_and(|s| s.to_string_lossy().contains("-trim-"));
    if trimmed {
        format!("{game} {stamp} (trim)")
    } else {
        format!("{game} {stamp}")
    }
}

/// Kairo's folders and files an import writes to.
#[derive(Clone, Debug)]
pub struct Target {
    pub clips: PathBuf,
    pub screenshots: PathBuf,
    /// clips.json, for the "From Medal" folder and favourites.
    pub library: PathBuf,
    /// What was imported before, so it isn't again.
    pub record: PathBuf,
}

impl Target {
    pub fn kairo(settings: &crate::config::Settings) -> Target {
        Target {
            clips: settings.save_dir(),
            screenshots: settings.screenshot_dir(),
            library: Library::path(),
            record: crate::config::config_dir().join("medal.json"),
        }
    }
}

/// Medal files imported before, by size and time changed, with the name each got in Kairo.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Record {
    imported: BTreeMap<String, String>,
}

impl Record {
    fn load(path: &Path) -> Record {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        library::write_atomic(path, text.as_bytes()).map_err(|e| e.to_string())
    }
}

fn unix(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn key(item: &Item) -> String {
    format!("{}:{}", item.size, unix(item.modified))
}

/// What importing would do.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// Each file to copy, with where it goes.
    pub copies: Vec<(Item, PathBuf)>,
    /// Imported before, or already in Kairo: skipped.
    pub already: usize,
}

impl Plan {
    pub fn count(&self, kind: Kind) -> usize {
        self.copies.iter().filter(|(i, _)| i.kind == kind).count()
    }

    pub fn bytes(&self) -> u64 {
        self.copies.iter().map(|(i, _)| i.size).sum()
    }
}

/// Sizes and times (Unix seconds) of the files in `dir`.
fn files_in(dir: &Path) -> HashMap<u64, Vec<u64>> {
    let mut m: HashMap<u64, Vec<u64>> = HashMap::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if let Ok(meta) = e.metadata()
            && meta.is_file()
        {
            let t = meta.modified().map_or(0, unix);
            m.entry(meta.len()).or_default().push(t);
        }
    }
    m
}

/// `dir/<base>.<ext>`, or "<base> (2).<ext>" and so on when that's taken (on disk or by a name
/// given out already).
fn free_name(dir: &Path, base: &str, ext: &str, taken: &HashSet<String>) -> PathBuf {
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{base}.{ext}")
        } else {
            format!("{base} ({n}).{ext}")
        };
        let p = dir.join(&name);
        if !p.exists() && !taken.contains(&name.to_lowercase()) {
            return p;
        }
        n += 1;
    }
}

fn extension(item: &Item) -> String {
    item.path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| "mp4".into())
}

/// Which of `found` to copy and where: everything not imported before (by size and time; a copy
/// keeps both) and not already in Kairo's folders.
pub fn plan(found: &Found, to: &Target) -> Plan {
    let record = Record::load(&to.record);
    let have = [files_in(&to.clips), files_in(&to.screenshots)];
    let mut taken = HashSet::new();
    let mut plan = Plan::default();
    for item in &found.items {
        let t = unix(item.modified);
        let in_kairo = have
            .iter()
            .filter_map(|m| m.get(&item.size))
            .flatten()
            .any(|&x| x.abs_diff(t) <= 2);
        if in_kairo || record.imported.contains_key(&key(item)) {
            plan.already += 1;
            continue;
        }
        let dir = match item.kind {
            Kind::Clip => &to.clips,
            Kind::Screenshot => &to.screenshots,
        };
        let dest = free_name(dir, &kairo_name(item), &extension(item), &taken);
        if let Some(n) = dest.file_name() {
            taken.insert(n.to_string_lossy().to_lowercase());
        }
        plan.copies.push((item.clone(), dest));
    }
    plan
}

/// Room left free on the drive after an import, for the clips still to come.
pub const SPARE_BYTES: u64 = 1_000_000_000;

/// Whether `need` bytes fit in `free` with some to spare (when the free space isn't known, they're
/// taken to fit; a full disk still stops the copying).
pub fn fits(need: u64, free: Option<u64>) -> bool {
    free.is_none_or(|f| need.checked_add(SPARE_BYTES).is_some_and(|n| f >= n))
}

/// How many of the clips to import Kairo's "keep clips" setting (`keep_days`, 0 for forever) would
/// move to Recently deleted, going by their age. Starred ones stay.
pub fn expiring(plan: &Plan, keep_days: u32, now: SystemTime) -> usize {
    if keep_days == 0 {
        return 0;
    }
    let cutoff = unix(now).saturating_sub(keep_days as u64 * 24 * 3600);
    plan.copies
        .iter()
        .filter(|(i, _)| i.kind == Kind::Clip && !i.favorite && unix(i.modified) < cutoff)
        .count()
}

/// How an import is going, for the progress bar; whole once `finished`.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    /// Files to copy, and how many are done (copied or failed).
    pub total: usize,
    pub done: usize,
    pub bytes: u64,
    pub total_bytes: u64,
    pub clips: usize,
    pub screenshots: usize,
    /// Files that couldn't be copied, and why.
    pub failed: Vec<(PathBuf, String)>,
    pub cancelled: bool,
    /// Why it stopped before the end (the disk filled up).
    pub stopped: Option<String>,
    pub finished: bool,
}

impl Progress {
    /// What came of it, in a line, and whether it all went well.
    pub fn summary(&self) -> (String, bool) {
        let what = describe(self.clips, self.screenshots, "");
        let some = self.clips + self.screenshots > 0;
        let failed = self.failed.len();
        match (&self.stopped, self.cancelled) {
            (Some(why), _) if some => (
                format!("{why} Imported {what} from Medal before it stopped."),
                false,
            ),
            (Some(why), _) => (format!("Couldn't import from Medal. {why}"), false),
            (None, true) if some => (
                format!("Stopped. Imported {what} from Medal; import again for the rest."),
                true,
            ),
            (None, true) => ("Stopped. Nothing was imported.".into(), true),
            (None, false) if failed > 0 && !some => (
                format!(
                    "Couldn't import from Medal: {}",
                    self.failed[0].1.trim_end_matches('.')
                ),
                false,
            ),
            (None, false) if failed > 0 => (
                format!("Imported {what} from Medal. {failed} couldn't be copied."),
                false,
            ),
            (None, false) => (format!("Imported {what} from Medal."), true),
        }
    }
}

/// "87 clips", "1 clip and 3 screenshots", with a word before the first ("87 Medal clips").
pub fn describe(clips: usize, screenshots: usize, word: &str) -> String {
    let word = if word.is_empty() {
        String::new()
    } else {
        format!("{word} ")
    };
    let n = |count: usize, one: &str, many: &str| {
        format!("{count} {}", if count == 1 { one } else { many })
    };
    match (clips, screenshots) {
        (c, 0) => n(c, &format!("{word}clip"), &format!("{word}clips")),
        (0, s) => n(
            s,
            &format!("{word}screenshot"),
            &format!("{word}screenshots"),
        ),
        (c, s) => format!(
            "{} and {}",
            n(c, &format!("{word}clip"), &format!("{word}clips")),
            n(s, "screenshot", "screenshots")
        ),
    }
}

enum CopyError {
    Cancelled,
    Io(std::io::Error),
}

impl From<std::io::Error> for CopyError {
    fn from(e: std::io::Error) -> Self {
        CopyError::Io(e)
    }
}

/// Copies `from` to `to` through `<to>.part`, keeping its time changed. Leaves nothing behind if
/// it fails or is cancelled.
fn copy(
    from: &Path,
    to: &Path,
    cancel: &AtomicBool,
    mut copied: impl FnMut(u64),
) -> Result<(), CopyError> {
    let mut part = to.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let result = (|| {
        let mut src = std::fs::File::open(from)?;
        let modified = src.metadata()?.modified()?;
        let mut dst = std::fs::File::create(&part)?;
        let mut buf = vec![0u8; 4 << 20];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(CopyError::Cancelled);
            }
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n])?;
            copied(n as u64);
        }
        dst.set_modified(modified)?;
        drop(dst);
        std::fs::rename(&part, to)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

/// Copies what `plan` lists into Kairo, one file at a time, until done or `cancel` is set; what's
/// copied stays. Clips go in the "From Medal" folder, starred if they were in Medal. `after` runs
/// for each file copied (to make its thumbnail).
pub fn run(
    plan: &Plan,
    to: &Target,
    progress: &Mutex<Progress>,
    cancel: &AtomicBool,
    after: &dyn Fn(&Item, &Path),
) {
    {
        let mut p = progress.lock().unwrap();
        p.total = plan.copies.len();
        p.total_bytes = plan.bytes();
    }
    let mut record = Record::load(&to.record);
    for (item, planned) in &plan.copies {
        if cancel.load(Ordering::Relaxed) {
            progress.lock().unwrap().cancelled = true;
            break;
        }
        // Something else may have taken the name since it was planned.
        let dest = if planned.exists() {
            let dir = planned.parent().unwrap_or(Path::new("."));
            free_name(dir, &kairo_name(item), &extension(item), &HashSet::new())
        } else {
            planned.clone()
        };
        if let Some(d) = dest.parent()
            && let Err(e) = std::fs::create_dir_all(d)
        {
            let mut p = progress.lock().unwrap();
            p.failed.push((item.path.clone(), e.to_string()));
            p.stopped = Some(format!("Couldn't make {}: {e}.", d.display()));
            break;
        }
        let base = progress.lock().unwrap().bytes;
        let mut sent = 0;
        let result = copy(&item.path, &dest, cancel, |n| {
            sent += n;
            progress.lock().unwrap().bytes = base + sent;
        });
        match result {
            Ok(()) => {
                after(item, &dest);
                if item.kind == Kind::Clip {
                    let _ = Library::edit_at(&to.library, |lib| {
                        lib.set_folder(&dest, Some(FOLDER));
                        if item.favorite {
                            lib.set_favorite(&dest, true);
                        }
                    });
                }
                let name = dest
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                record.imported.insert(key(item), name);
                let _ = record.save(&to.record);
                let mut p = progress.lock().unwrap();
                match item.kind {
                    Kind::Clip => p.clips += 1,
                    Kind::Screenshot => p.screenshots += 1,
                }
            }
            Err(CopyError::Cancelled) => {
                progress.lock().unwrap().cancelled = true;
                break;
            }
            Err(CopyError::Io(e)) => {
                let mut p = progress.lock().unwrap();
                p.failed.push((item.path.clone(), e.to_string()));
                if e.kind() == std::io::ErrorKind::StorageFull {
                    p.stopped = Some("The disk is full.".into());
                    break;
                }
            }
        }
        let mut p = progress.lock().unwrap();
        p.done += 1;
        p.bytes = base + item.size;
    }
    progress.lock().unwrap().finished = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::{Value, testdb};

    const CONTENTS: &str = "CREATE TABLE contents (\n  created_at INTEGER NOT NULL,\n  category_id TEXT,\n  video_path TEXT,\n  image_path TEXT,\n  thumbnail_path TEXT,\n  metadata BLOB NOT NULL,\n  remote_content_id TEXT UNIQUE,\n  local_content_id TEXT UNIQUE NOT NULL,\n  parent_id TEXT\n, film_reel_path TEXT)";
    const KEY_VALUES: &str =
        "CREATE TABLE key_values (\n  key TEXT UNIQUE NOT NULL,\n  value BLOB NOT NULL\n)";

    /// A Medal database: its clips folder setting, and a row per (path, created, title, event,
    /// favourite). Values as JSON text; Medal's own are binary JSON (read in sqlite's tests).
    fn medal_db(path: &Path, folder: &Path, clips: &[(PathBuf, i64, &str, &str, bool)]) {
        let folder = serde_json::to_string(&folder.display().to_string()).unwrap();
        let rows = clips
            .iter()
            .enumerate()
            .map(|(i, (p, created, title, event, fav))| {
                let meta = serde_json::json!({
                    "title": title,
                    "isFavorited": fav,
                    "recorder": {"eventobj": {"name": event}},
                });
                vec![
                    Value::Int(*created),
                    Value::Text("game-1".into()),
                    Value::Text(p.display().to_string()),
                    Value::Null,
                    Value::Null,
                    Value::Blob(meta.to_string().into_bytes()),
                    Value::Null,
                    Value::Text(format!("id-{i}")),
                    Value::Null,
                    Value::Null,
                ]
            })
            .collect();
        testdb::write(
            path,
            &[
                (
                    KEY_VALUES,
                    "key_values",
                    vec![vec![
                        Value::Text("ClipFolder".into()),
                        Value::Blob(folder.into_bytes()),
                    ]],
                ),
                (CONTENTS, "contents", rows),
            ],
        );
    }

    fn file(p: &Path, bytes: usize, age_days: u64) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![7u8; bytes]).unwrap();
        let t = SystemTime::now() - Duration::from_secs(age_days * 86_400);
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    /// A Medal setup as on Windows: settings in AppData, clips in a folder of their own.
    struct Setup {
        _dir: tempfile::TempDir,
        app_data: PathBuf,
        medal: PathBuf,
        kairo: Target,
    }

    fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let app_data = d.join("AppData/Medal");
        let medal = d.join("D/Medal");
        std::fs::create_dir_all(&app_data).unwrap();
        let clips = medal.join("Clips");
        let a = clips.join("Valorant/MedalTVValorant20260713143819888.mp4");
        let b = clips.join(
            "Tom Clancys Rainbow Six Siege/MedalTVTomClancysRainbowSixSiege20260714200000123.mp4",
        );
        let c = clips.join("Valorant/MedalTVValorant20260715120000000.mp4");
        let edit = medal.join("Edits/MedalTVGeometryDash20260716090000000-trim-1784000000000.mp4");
        let shot = medal.join("Screenshots/Valorant/MedalTVValorant20260713150000000.png");
        file(&a, 3000, 100);
        file(&b, 2000, 99);
        file(&c, 1000, 98);
        file(&edit, 500, 97);
        file(&shot, 100, 96);
        // Medal's own working files: not clips.
        file(
            &medal.join("Thumbnails/MedalTVValorant20260713143819888.jpg"),
            10,
            1,
        );
        file(&medal.join("Temp/RecordingBuffer/buffer.mp4"), 10, 1);
        file(&medal.join("Reels/x.webp"), 10, 1);
        // The paths as Medal wrote them, on a drive that isn't there now (the folder moved).
        let as_medal = |p: &Path| {
            PathBuf::from(format!(
                "Q:\\Medal test\\{}",
                p.strip_prefix(&medal)
                    .unwrap()
                    .display()
                    .to_string()
                    .replace('/', "\\")
            ))
        };
        medal_db(
            &app_data.join("medal-321.db"),
            &medal,
            &[
                (as_medal(&a), 1_783_967_901, "", "", false),
                (as_medal(&b), 1_784_059_200, "Ace clutch: 1v4?", "", true),
                (
                    as_medal(&c),
                    1_784_116_800,
                    "\u{1F44A}3K",
                    "\u{1F44A}3K",
                    false,
                ),
                (as_medal(&shot), 1_783_969_200, "", "", false),
            ],
        );
        let kairo = Target {
            clips: d.join("Kairo"),
            screenshots: d.join("Kairo/Screenshots"),
            library: d.join("home/clips.json"),
            record: d.join("home/medal.json"),
        };
        Setup {
            app_data,
            medal,
            kairo,
            _dir: dir,
        }
    }

    /// Every file under `dir` with its bytes and time changed.
    fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
        let mut m = BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let t = e.metadata().unwrap().modified().unwrap();
                    m.insert(p.clone(), (std::fs::read(&p).unwrap(), t));
                }
            }
        }
        m
    }

    #[test]
    fn finds_the_folder_from_medals_settings_then_the_default() {
        let s = setup();
        assert_eq!(setting_folders(&s.app_data), vec![s.medal.clone()]);
        let other = s.medal.parent().unwrap().join("C-Medal");
        file(
            &other.join("Clips/Peak/MedalTVPeak20260101120000000.mp4"),
            10,
            5,
        );
        let found = find(Some(&s.app_data), std::slice::from_ref(&other));
        assert_eq!(found.folders, vec![s.medal.clone(), other.clone()]);
        assert_eq!(found.count(Kind::Clip), 5);
        assert_eq!(found.count(Kind::Screenshot), 1);
        // Medal's working folders are left out.
        assert!(
            found
                .items
                .iter()
                .all(|i| !i.path.starts_with(s.medal.join("Temp")))
        );

        // No settings: the usual place.
        let found = find(None, std::slice::from_ref(&other));
        assert_eq!(found.folders, vec![other.clone()]);
        assert_eq!(found.items.len(), 1);
        assert_eq!(found.items[0].game, "Peak");
        // Neither: nothing, not an error.
        assert!(find(None, &[s.medal.join("nope")]).items.is_empty());
        // A folder picked by hand still gets its titles from the database.
        let picked = scan_folder(&s.medal.join("Clips"), Some(&s.app_data));
        assert_eq!(picked.count(Kind::Clip), 3);
        assert!(
            picked
                .items
                .iter()
                .any(|i| i.title.as_deref() == Some("Ace clutch: 1v4?"))
        );
    }

    #[test]
    fn games_titles_and_times_from_medal() {
        let s = setup();
        let found = find(Some(&s.app_data), &[]);
        let by = |name: &str| {
            found
                .items
                .iter()
                .find(|i| i.path.to_string_lossy().contains(name))
                .unwrap()
        };
        let a = by("20260713143819888");
        assert_eq!(a.game, "Valorant");
        assert_eq!(a.title, None);
        assert_eq!(
            unix(a.created),
            1_783_967_901,
            "Medal's time, not the file's"
        );
        assert!(a.thumb.is_some());
        let b = by("20260714200000123");
        assert_eq!(b.game, "Tom Clancys Rainbow Six Siege");
        assert_eq!(b.title.as_deref(), Some("Ace clutch: 1v4?"));
        assert!(b.favorite);
        // Medal's own title for what happened isn't the user's.
        assert_eq!(by("20260715120000000").title, None);
        // Not in the database: the game and time come from the name.
        let edit = by("-trim-");
        assert_eq!(edit.game, "Geometry Dash");
        assert_eq!(
            chrono::DateTime::<chrono::Local>::from(edit.created)
                .format("%Y-%m-%d %H-%M-%S")
                .to_string(),
            "2026-07-16 09-00-00"
        );
        assert_eq!(by(".png").kind, Kind::Screenshot);
        assert!(found.items.windows(2).all(|w| w[0].created >= w[1].created));
    }

    #[test]
    fn names_like_kairos_own() {
        let mut item = Item {
            path: PathBuf::from("x.mp4"),
            kind: Kind::Clip,
            game: "Valorant".into(),
            title: None,
            created: chrono::NaiveDate::from_ymd_opt(2026, 7, 13)
                .unwrap()
                .and_hms_opt(14, 38, 21)
                .unwrap()
                .and_local_timezone(chrono::Local)
                .unwrap()
                .into(),
            modified: SystemTime::now(),
            size: 1,
            favorite: false,
            thumb: None,
        };
        assert_eq!(kairo_name(&item), "Valorant 2026-07-13 14-38-21");
        assert_eq!(library::game_of(&kairo_name(&item)), "Valorant");
        // Trimmed in Medal: named like Kairo's trimmed copies.
        item.path = PathBuf::from("MedalTVValorant20260713143819888-trim-1784000000000.mp4");
        assert_eq!(kairo_name(&item), "Valorant 2026-07-13 14-38-21 (trim)");
        assert_eq!(library::game_of(&kairo_name(&item)), "Valorant");
        item.path = PathBuf::from("x.mp4");
        item.title = Some("  Ace clutch: 1v4? ".into());
        assert_eq!(kairo_name(&item), "Ace clutch 1v4");
        item.title = Some("<>|".into());
        assert_eq!(kairo_name(&item), "Valorant 2026-07-13 14-38-21");
        item.title = Some("con".into());
        assert_eq!(kairo_name(&item), "con clip");
        item.title = Some("x".repeat(200));
        assert_eq!(kairo_name(&item).len(), 80);
        item.title = None;
        item.game = "a/b:c".into();
        assert!(kairo_name(&item).starts_with("a b c 2026"));
        assert_eq!(
            game_from_name("MedalTVREANIMAL20260101120000000"),
            Some("REANIMAL".into())
        );
        assert_eq!(
            game_from_name("MedalTVScreenRecording20260101"),
            Some("Screen Recording".into())
        );
        assert_eq!(game_from_name("clip"), None);
    }

    #[test]
    fn imports_copies_and_never_touches_medals_files() {
        let s = setup();
        let medal_before = snapshot(&s.medal);
        let settings_before = snapshot(&s.app_data);
        let found = find(Some(&s.app_data), &[]);
        let plan = plan(&found, &s.kairo);
        assert_eq!((plan.copies.len(), plan.already), (5, 0));
        assert_eq!(plan.bytes(), 6600);
        let progress = Mutex::new(Progress::default());
        let thumbs = Mutex::new(Vec::new());
        run(
            &plan,
            &s.kairo,
            &progress,
            &AtomicBool::new(false),
            &|_, p| {
                thumbs.lock().unwrap().push(p.to_path_buf());
            },
        );
        let p = progress.into_inner().unwrap();
        assert!(p.finished && !p.cancelled && p.failed.is_empty());
        assert_eq!((p.clips, p.screenshots, p.done, p.bytes), (4, 1, 5, 6600));
        assert_eq!(thumbs.into_inner().unwrap().len(), 5);
        assert_eq!(
            p.summary(),
            ("Imported 4 clips and 1 screenshot from Medal.".into(), true)
        );

        let clips = library::scan(&s.kairo.clips);
        let names: Vec<&str> = clips.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(clips.len(), 4);
        assert!(names.contains(&"Ace clutch 1v4"), "{names:?}");
        assert_eq!(
            names
                .iter()
                .filter(|n| n.starts_with("Valorant 2026-07-1"))
                .count(),
            2
        );
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("Geometry Dash 2026-07-16"))
        );
        // Times kept, so they sort where they were recorded.
        for c in &clips {
            let src = found.items.iter().find(|i| i.size == c.size).unwrap();
            assert_eq!(c.modified, src.modified);
        }
        let shots: Vec<_> = std::fs::read_dir(&s.kairo.screenshots)
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(shots.len(), 1);
        assert!(shots[0].file_name().to_string_lossy().ends_with(".png"));
        // In the "From Medal" folder, starred as in Medal.
        let lib = Library::load_from(&s.kairo.library);
        assert_eq!(lib.all_folders(), vec![(FOLDER.to_string(), 4)]);
        assert_eq!(lib.favorites.len(), 1);
        assert!(lib.favorites.contains("Ace clutch 1v4.mp4"));
        // Nothing half-copied left over.
        assert!(
            std::fs::read_dir(&s.kairo.clips)
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().ends_with(".part"))
        );

        // Medal's clips, settings and database are as they were: nothing moved, changed or added.
        assert_eq!(snapshot(&s.medal), medal_before);
        assert_eq!(snapshot(&s.app_data), settings_before);
    }

    #[test]
    fn importing_again_skips_what_came_over_before() {
        let s = setup();
        let found = find(Some(&s.app_data), &[]);
        let first = plan(&found, &s.kairo);
        run(
            &first,
            &s.kairo,
            &Mutex::new(Progress::default()),
            &AtomicBool::new(false),
            &|_, _| {},
        );
        let again = plan(&find(Some(&s.app_data), &[]), &s.kairo);
        assert_eq!((again.copies.len(), again.already), (0, 5));
        // Even renamed or deleted in Kairo since: what was imported isn't brought back.
        for c in library::scan(&s.kairo.clips) {
            std::fs::remove_file(c.path).unwrap();
        }
        assert_eq!(plan(&found, &s.kairo).already, 5);
        // Without the record, clips already in Kairo (same size and time) are still skipped.
        std::fs::remove_file(&s.kairo.record).unwrap();
        let some = &found.items[0];
        file(&s.kairo.clips.join("renamed.mp4"), some.size as usize, 0);
        std::fs::File::options()
            .write(true)
            .open(s.kairo.clips.join("renamed.mp4"))
            .unwrap()
            .set_modified(some.modified)
            .unwrap();
        // (The screenshot is still in Kairo's screenshots folder too.)
        let p = plan(&found, &s.kairo);
        assert_eq!((p.copies.len(), p.already), (3, 2));
        // A new name never overwrites: a second clip with the same name is numbered.
        let mut twins = Found::default();
        for i in found
            .items
            .iter()
            .filter(|i| i.kind == Kind::Clip)
            .skip(1)
            .take(2)
        {
            let mut i = i.clone();
            i.title = Some("Same".into());
            twins.items.push(i);
        }
        let p = plan(&twins, &s.kairo);
        let names: Vec<String> = p
            .copies
            .iter()
            .map(|(_, d)| d.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, ["Same.mp4", "Same (2).mp4"]);
    }

    #[test]
    fn a_clip_that_fails_is_counted_and_the_rest_still_come() {
        let s = setup();
        let found = find(Some(&s.app_data), &[]);
        let plan = plan(&found, &s.kairo);
        // One goes missing between looking and copying.
        let gone = plan.copies[1].0.path.clone();
        std::fs::remove_file(&gone).unwrap();
        let progress = Mutex::new(Progress::default());
        run(
            &plan,
            &s.kairo,
            &progress,
            &AtomicBool::new(false),
            &|_, _| {},
        );
        let p = progress.into_inner().unwrap();
        assert_eq!(p.failed.len(), 1);
        assert_eq!(p.failed[0].0, gone);
        assert_eq!(
            p.summary(),
            (
                "Imported 3 clips and 1 screenshot from Medal. 1 couldn't be copied.".into(),
                false
            )
        );
        assert_eq!(p.clips + p.screenshots, 4);
        assert_eq!(p.done, 5);
        // It isn't remembered as imported: it's tried again next time.
        file(&gone, 10, 1);
        let again = super::plan(&find(Some(&s.app_data), &[]), &s.kairo);
        assert_eq!(again.copies.len(), 1);
    }

    #[test]
    fn cancelling_keeps_what_is_done() {
        let s = setup();
        let plan = plan(&find(Some(&s.app_data), &[]), &s.kairo);
        let progress = Mutex::new(Progress::default());
        let cancel = AtomicBool::new(false);
        run(&plan, &s.kairo, &progress, &cancel, &|_, _| {
            cancel.store(true, Ordering::Relaxed);
        });
        let p = progress.into_inner().unwrap();
        assert!(p.cancelled && p.finished);
        assert_eq!(p.clips + p.screenshots, 1);
        assert_eq!(
            p.summary().0,
            "Stopped. Imported 1 clip from Medal; import again for the rest."
        );
        let left: Vec<_> = std::fs::read_dir(&s.kairo.clips)
            .unwrap()
            .flatten()
            .collect();
        assert!(
            left.iter()
                .all(|e| !e.file_name().to_string_lossy().ends_with(".part"))
        );
        // The rest come next time.
        let rest = super::plan(&find(Some(&s.app_data), &[]), &s.kairo);
        assert_eq!((rest.copies.len(), rest.already), (4, 1));
    }

    #[test]
    fn says_what_came_of_it() {
        assert_eq!(describe(1, 0, "Medal"), "1 Medal clip");
        assert_eq!(describe(87, 0, "Medal"), "87 Medal clips");
        assert_eq!(describe(0, 2, "Medal"), "2 Medal screenshots");
        assert_eq!(describe(3, 1, ""), "3 clips and 1 screenshot");
        let mut p = Progress {
            clips: 2,
            stopped: Some("The disk is full.".into()),
            ..Default::default()
        };
        assert_eq!(
            p.summary(),
            (
                "The disk is full. Imported 2 clips from Medal before it stopped.".into(),
                false
            )
        );
        p.clips = 0;
        assert_eq!(
            p.summary().0,
            "Couldn't import from Medal. The disk is full."
        );
        p.stopped = None;
        p.failed = vec![(PathBuf::from("a.mp4"), "Access is denied.".into())];
        assert_eq!(
            p.summary(),
            ("Couldn't import from Medal: Access is denied".into(), false)
        );
        p.failed.clear();
        p.cancelled = true;
        assert_eq!(p.summary(), ("Stopped. Nothing was imported.".into(), true));
    }

    #[test]
    fn room_and_clips_kairo_would_tidy_away() {
        assert!(fits(5_000, None));
        assert!(fits(5_000, Some(SPARE_BYTES + 5_000)));
        assert!(!fits(5_000, Some(SPARE_BYTES + 4_999)));
        assert!(!fits(u64::MAX, Some(u64::MAX)));

        let s = setup();
        let plan = plan(&find(Some(&s.app_data), &[]), &s.kairo);
        // The clips are 97 to 100 days old; the starred one stays, and so does the screenshot.
        let now = SystemTime::now();
        assert_eq!(expiring(&plan, 0, now), 0);
        assert_eq!(expiring(&plan, 30, now), 3);
        assert_eq!(expiring(&plan, 365, now), 0);
    }

    /// Medal's real database format (binary JSON, interior and overflow pages, the log).
    #[test]
    fn reads_medals_own_database() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/medal-1234.db");
        for ext in ["", "-wal"] {
            let mut name = fixture.as_os_str().to_owned();
            name.push(ext);
            std::fs::copy(&name, dir.path().join(format!("medal-1234.db{ext}"))).unwrap();
        }
        assert_eq!(
            setting_folders(dir.path()),
            vec![PathBuf::from("Q:\\Medal\\")]
        );
        let meta = read_meta(&databases_in(dir.path()));
        assert_eq!(meta.rows.len(), 45);
        let row = &meta.rows["medaltvothergame20260102090000000.mp4"];
        assert_eq!(row.title.as_deref(), Some("My best shot"));
        assert!(row.favorite);
        assert_eq!(row.game.as_deref(), Some("Other Game"));
        assert_eq!(
            meta.rows["medaltvothergame20260102091500000.mp4"].title,
            None
        );
        assert_eq!(
            meta.rows["medaltvlategame20260103100000000.mp4"]
                .title
                .as_deref(),
            Some("In the log only")
        );
        assert_eq!(meta.roots, vec![PathBuf::from("Q:\\Medal")]);
    }
}
