//! Kairo's window, built with iced: the clip library, the editor and the settings. The
//! "clip saved" card is its own small window (`toast`).

mod achievements;
mod editor;
mod friends;
pub mod icons;
mod library;
mod settings;
mod stats;
pub mod theme;
pub mod toast;
mod welcome;
mod widgets;

#[cfg(test)]
mod preview;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use iced::alignment::Horizontal;
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{
    Space, button, column, container, image, pick_list, qr_code, row, scrollable, space, stack,
    text, text_input,
};
use iced::{Alignment, Element, Fill, Padding, Size, Subscription, Task, Theme, window};

use kairo::config::{self, Settings};
use kairo::library::{self as lib, Library, Link};

use crate::daemon::{self, Report, Request};
use crate::share;
use crate::single_instance::{self, InstanceLock};
use icons::{Icon, icon};
pub use library::{ClipItem, ShotItem, load_clips, load_shots};
use theme::{BOLD, MEDIUM, SEMIBOLD};
use widgets::dialog;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    All,
    Favorites,
    Shared,
    Screenshots,
    Sessions,
    Game(String),
    Tag(String),
    /// Recently deleted.
    Trash,
    /// Your stats: the library counted.
    Stats,
    /// A folder the user made.
    Folder(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Newest,
    Oldest,
    Longest,
    Largest,
}

impl Sort {
    const ALL: [Sort; 4] = [Sort::Newest, Sort::Oldest, Sort::Longest, Sort::Largest];
}

impl std::fmt::Display for Sort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Sort::Newest => "Newest first",
            Sort::Oldest => "Oldest first",
            Sort::Longest => "Longest first",
            Sort::Largest => "Largest first",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Library,
    Editor,
    Settings,
    Friends,
}

enum Dialog {
    /// Naming a new folder (`from` is `None`) or renaming one.
    Folder {
        from: Option<String>,
        name: String,
        error: Option<String>,
    },
    /// After an update: the highlights of the version now running.
    WhatsNew {
        version: String,
        notes: String,
    },
    Rename {
        path: PathBuf,
        name: String,
        error: Option<String>,
    },
    Delete(Vec<PathBuf>),
    /// A share link, to copy or scan.
    Link {
        url: String,
        expires: Option<u64>,
        qr: Option<qr_code::Data>,
    },
    /// Picking friends to send a clip to, and a line to go with it.
    SendTo {
        clip: PathBuf,
        picked: Vec<String>,
        note: String,
    },
}

/// A clip on its way to friends: it goes once its upload has a link.
struct FriendSend {
    clip: PathBuf,
    to: Vec<String>,
    note: String,
}

struct Notice {
    text: String,
    ok: bool,
    at: Instant,
    /// Files in Recently deleted that Undo puts back.
    undo: Vec<PathBuf>,
}

pub struct App {
    pub page: Page,
    pub filter: Filter,
    pub search: String,
    pub sort: Sort,
    pub clips: Vec<ClipItem>,
    pub shots: Vec<ShotItem>,
    pub loaded: bool,
    /// The card the pointer is over, and where across it (0 to 1).
    scrub: Option<(PathBuf, f32)>,
    /// When the pointer last moved over the hovered card: resting on it plays the preview.
    scrub_moved: Option<Instant>,
    /// The first-start setup's step, while it's showing.
    welcome: Option<usize>,
    /// When the page (or setup step) on screen and the open dialog appeared, for their entrances.
    page_in: Option<Instant>,
    dialog_in: Option<Instant>,
    /// Achievements earned, with when.
    earned: achievements::Earned,
    /// Keyframe pictures for hover previews, most recent last.
    strips: HashMap<PathBuf, Vec<image::Handle>>,
    strip_order: Vec<PathBuf>,
    strip_loading: HashSet<PathBuf>,
    uploads: Vec<share::Upload>,
    friends: friends::Friends,
    friend_sends: Vec<FriendSend>,
    /// Clips on their way to the Discord channel.
    posts: Vec<share::Post>,
    /// What's in Recently deleted.
    pub trash: Vec<lib::Trashed>,
    /// The games and tags in the clip list, with how many clips each, for the sidebar: counted
    /// when the list changes rather than every frame.
    games: Vec<(String, usize)>,
    /// The folders, with how many clips are in each (empty ones too).
    folders: Vec<(String, usize)>,
    tags: Vec<(String, usize)>,
    pub report: Option<Report>,
    pub settings: Settings,
    saved: Settings,
    changed_at: Option<Instant>,
    editor: Option<editor::Editor>,
    setup: settings::State,
    dialog: Option<Dialog>,
    notice: Option<Notice>,
    /// For the preview test: a fixed "now", so relative times don't drift.
    pub now: Option<SystemTime>,
    window: Option<window::Id>,
    ticks: u64,
    /// `library::folder_signature` of the last look at the folders.
    scan_sig: Option<u64>,
    /// The newest clip's banner picture, and the clip it's being made for.
    hero: Option<(PathBuf, image::Handle)>,
    hero_loading: Option<PathBuf>,
    /// The menu of clip lengths under the save button is open.
    save_menu: bool,
    test: Option<SelfTest>,
    /// Exports `--ui-self-test` still has to make, after the trimmed copy.
    test_exports: Vec<editor::ExportKind>,
    test_current: Option<editor::ExportKind>,
    /// What the self-test's exports made, in order.
    test_made: Vec<PathBuf>,
    /// Picking clips (for a montage, or to delete several).
    selecting: bool,
    /// The picked clips, in the order picked.
    selected: Vec<PathBuf>,
    montage: Option<crate::export::Montage>,
    _lock: Option<InstanceLock>,
}

/// `--ui-self-test`: where the run through the real window is.
enum SelfTest {
    Loading(Instant),
    Playing(Instant),
    Exporting(Instant),
    Montage(Instant),
}

static TEST_RESULT: std::sync::Mutex<Option<Result<String, String>>> = std::sync::Mutex::new(None);

/// What a look at the library's folders found.
#[derive(Debug, Clone)]
pub struct Scan {
    sig: u64,
    clips: Vec<ClipItem>,
    shots: Vec<ShotItem>,
    trash: Vec<lib::Trashed>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    Report(Option<Box<Report>>),
    Clips(Vec<ClipItem>),
    Shots(Vec<ShotItem>),
    Trash(Vec<lib::Trashed>),
    /// Puts back what was just deleted.
    Undo,
    /// Puts a file in Recently deleted back.
    Restore(PathBuf),
    /// Asks before deleting files in Recently deleted for good.
    AskPurge(Vec<PathBuf>),
    CancelPost(PathBuf),
    /// Starts or stops picking clips.
    Select(bool),
    ToggleSelect(PathBuf),
    MakeMontage,
    CancelMontage,
    MontageTick,
    /// Moves the picked clips to Recently deleted.
    DeleteSelected,
    /// Starts or stops recording a whole session.
    ToggleSession,
    /// Opens or closes the menu of clip lengths under the save button.
    SaveMenu(bool),
    /// Saves the last this many seconds.
    SaveFor(u32),
    /// The banner picture for a clip is ready (or couldn't be made).
    Hero(PathBuf, Option<image::Handle>),
    /// The folders changed: their new signature and contents.
    Rescanned(Option<Box<Scan>>),
    Scrub(PathBuf, f32),
    /// Moves a resting hover preview on by a frame.
    HoverTick,
    /// A frame of an entrance (pages, dialogs, notices).
    AnimTick,
    /// A step of the first-start setup.
    WelcomeStep(usize),
    /// The first-start setup is finished (or skipped).
    WelcomeDone,
    ScrubEnd(PathBuf),
    Strip(PathBuf, Vec<image::Handle>),
    Share(PathBuf),
    ShowLink(PathBuf),
    CopyText(String),
    OpenUrl(String),
    UploadTick,
    CancelUpload(PathBuf),
    TakeScreenshot,
    OpenShotsFolder,
    OpenExternal(PathBuf),
    Filter(Filter),
    Search(String),
    Sort(Sort),
    Open(PathBuf),
    Back,
    Favorite(PathBuf),
    Copy(PathBuf),
    Reveal(PathBuf),
    AskRename(PathBuf),
    RenameInput(String),
    RenameConfirm,
    /// Opens the dialog to make a folder (`None`) or rename one.
    FolderDialog(Option<String>),
    FolderInput(String),
    FolderConfirm,
    RemoveFolder(String),
    /// Puts the picked clips in a folder, or in none.
    MoveSelected(FolderPick),
    AskDelete(PathBuf),
    DeleteConfirm,
    CloseDialog,
    Page(Page),
    SaveClip,
    TogglePause,
    StartRecorder,
    OpenFolder,
    Editor(editor::Msg),
    Settings(settings::Msg),
    Friends(friends::Msg),
    /// Open "Send to friends" for a clip.
    SendTo(PathBuf),
    /// Tick a friend in it ("*" for everyone).
    SendPick(String),
    SendNote(String),
    SendGo,
    SentToFriends(Result<String, String>),
    Key(keyboard::Event),
    Opened(window::Id),
}

/// Runs blocking work on a thread and resolves with its result.
fn background<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> impl Future<Output = T> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    async move { rx.await.expect("worker thread") }
}

impl App {
    pub fn new(settings: Settings) -> (Self, Task<Message>) {
        theme::set_accent(&settings.accent);
        theme::apply_look(&settings.look);
        let (setup, tasks) = settings::State::new(&settings);
        (
            App {
                page: Page::Library,
                filter: Filter::All,
                search: String::new(),
                sort: Sort::Newest,
                clips: Vec::new(),
                shots: Vec::new(),
                loaded: false,
                scrub: None,
                scrub_moved: None,
                welcome: None,
                page_in: None,
                dialog_in: None,
                earned: achievements::load(),
                strips: HashMap::new(),
                strip_order: Vec::new(),
                strip_loading: HashSet::new(),
                uploads: Vec::new(),
                friends: friends::Friends::new(),
                friend_sends: Vec::new(),
                posts: Vec::new(),
                trash: Vec::new(),
                games: Vec::new(),
                folders: Vec::new(),
                tags: Vec::new(),
                report: None,
                saved: settings.clone(),
                settings,
                changed_at: None,
                editor: None,
                setup,
                dialog: None,
                notice: None,
                now: None,
                window: None,
                ticks: 0,
                scan_sig: None,
                hero: None,
                hero_loading: None,
                save_menu: false,
                test: None,
                test_exports: Vec::new(),
                test_current: None,
                test_made: Vec::new(),
                selecting: false,
                selected: Vec::new(),
                montage: None,
                _lock: None,
            },
            tasks.map(Message::Settings),
        )
    }

    fn boot(
        lock: Option<InstanceLock>,
        clip: Option<PathBuf>,
        update: bool,
        friends: bool,
    ) -> (Self, Task<Message>) {
        let (settings, _) = config::load_or_create(&config::default_path());
        let (mut app, tasks) = App::new(settings);
        app._lock = lock;
        if std::env::var("KAIRO_TAB").is_ok_and(|t| t == "settings") {
            app.page = Page::Settings;
        }
        if update {
            app.page = Page::Settings;
            app.setup.update.auto_install = true;
        }
        if friends {
            app.page = Page::Friends;
        }
        app.whats_new(update);
        let mut all = vec![tasks, app.reload(), Task::done(Message::Tick)];
        if let Some(c) = clip {
            all.push(app.open_editor(c));
        }
        (app, Task::batch(all))
    }

    /// The first start of a fresh install shows the setup; the first start after an update
    /// shows what the version brings. Each happens once.
    fn whats_new(&mut self, updating: bool) {
        let current = crate::updater::current_version();
        // Before 0.3.0 nothing remembered the version: an older settings file means this
        // copy was updated rather than freshly installed.
        let upgraded = !self.settings.seen_version.is_empty()
            || std::fs::metadata(config::default_path())
                .and_then(|m| m.created().or_else(|_| m.modified()))
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(600));
        if !self.settings.setup_done {
            if upgraded || std::env::var_os("KAIRO_NO_WELCOME").is_some() {
                self.settings.setup_done = true;
                self.changed_at = Some(Instant::now());
            } else if !updating {
                self.welcome = Some(0);
            }
        }
        if self.settings.seen_version == current {
            return;
        }
        if upgraded
            && !updating
            && let Some(notes) =
                crate::updater::changelog_section(crate::updater::CHANGELOG, current)
        {
            self.dialog = Some(Dialog::WhatsNew {
                version: current.to_string(),
                notes,
            });
        }
        self.settings.seen_version = current.to_string();
        if let Err(e) = config::save(&config::default_path(), &self.settings) {
            crate::log::warning!("{e}");
        }
    }

    fn reload(&self) -> Task<Message> {
        let dir = self.settings.save_dir();
        let shots = self.settings.screenshot_dir();
        let bin = dir.clone();
        Task::batch([
            Task::perform(background(move || load_clips(&dir)), Message::Clips),
            Task::perform(background(move || load_shots(&shots)), Message::Shots),
            Task::perform(background(move || lib::trashed(&bin)), Message::Trash),
        ])
    }

    /// Makes the banner picture for the newest clip, once.
    fn ensure_hero(&mut self) -> Task<Message> {
        let Some(c) = self.clips.iter().max_by_key(|c| c.modified) else {
            return Task::none();
        };
        if self.hero.as_ref().is_some_and(|(p, _)| *p == c.path)
            || self.hero_loading.as_ref() == Some(&c.path)
        {
            return Task::none();
        }
        let (path, at) = (c.path.clone(), c.duration.unwrap_or(0.0) * 0.5);
        self.hero_loading = Some(path.clone());
        Task::perform(
            background(move || {
                let h = library::hero_picture(&path, at);
                (path, h)
            }),
            |(p, h)| Message::Hero(p, h),
        )
    }

    /// Reloads the library if anything in its folders changed since the last look.
    fn refresh(&self) -> Task<Message> {
        let dir = self.settings.save_dir();
        let shots = self.settings.screenshot_dir();
        let last = self.scan_sig;
        Task::perform(
            background(move || {
                let thumbs = crate::save::thumbs_dir();
                let sessions = dir.join("Sessions");
                let bin = lib::trash_dir(&dir);
                let sig = library::folder_signature(
                    &[&dir, &shots, &thumbs, &sessions, &bin],
                    &kairo::library::Library::path(),
                );
                (Some(sig) != last).then(|| {
                    Box::new(Scan {
                        sig,
                        clips: load_clips(&dir),
                        shots: load_shots(&shots),
                        trash: lib::trashed(&dir),
                    })
                })
            }),
            Message::Rescanned,
        )
    }

    /// How far along sharing `path` is, while it runs: "Uploading 42%" and 0 to 1.
    fn upload_progress(&self, path: &Path) -> Option<(String, f32)> {
        self.uploads
            .iter()
            .find(|u| u.clip == path)
            .map(|u| (u.label(), u.progress()))
    }

    /// The share link of a clip or screenshot, if it has one.
    fn link_of(&self, path: &Path) -> Option<&Link> {
        self.clips
            .iter()
            .find(|c| c.path == path)
            .and_then(|c| c.link.as_ref())
            .or_else(|| {
                self.shots
                    .iter()
                    .find(|s| s.path == path)
                    .and_then(|s| s.link.as_ref())
            })
    }

    /// Uploads `file` and remembers the link for `clip`.
    fn start_upload(&mut self, file: PathBuf, clip: PathBuf, range: Option<(f64, f64)>) {
        if self.upload_progress(&clip).is_some() {
            return;
        }
        self.uploads.push(share::start(
            &file,
            &clip,
            &self.settings.share_expiry,
            range,
        ));
    }

    /// "fabio", "fabio and ana", "fabio, ana and jo": for notes.
    fn friend_names(&self, ids: &[String]) -> String {
        let names: Vec<String> = ids.iter().map(|i| self.friends.inbox.name_of(i)).collect();
        match names.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        }
    }

    /// Sends `clip`'s share link `url` to the friends in `to`, in the background.
    fn send_clip(
        &self,
        clip: PathBuf,
        url: String,
        to: Vec<String>,
        note: String,
    ) -> Task<Message> {
        let (title, seconds) = self
            .clips
            .iter()
            .find(|c| c.path == clip)
            .map(|c| (c.name.clone(), c.duration.unwrap_or(0.0)))
            .unwrap_or_else(|| {
                let stem = clip
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                (stem, 0.0)
            });
        let names = self.friend_names(&to);
        let link = crate::friends::ClipLink {
            url,
            title,
            seconds,
        };
        Task::perform(
            background(move || {
                crate::friends::send(&to, &note, Some(&link)).map(|()| format!("Sent to {names}."))
            }),
            Message::SentToFriends,
        )
    }

    fn show_link(&mut self, link: &Link) -> Task<Message> {
        self.dialog = Some(Dialog::Link {
            url: link.url.clone(),
            expires: link.expires,
            qr: qr_code::Data::new(&link.url).ok(),
        });
        iced::clipboard::write(link.url.clone())
    }

    /// Finished uploads: show and copy the link, or say what went wrong.
    fn poll_uploads(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        let mut j = 0;
        while j < self.posts.len() {
            let Some(result) = self.posts[j].take() else {
                j += 1;
                continue;
            };
            self.posts.remove(j);
            match result {
                Ok(()) => self.note(
                    format!("Sent to the channel as {}.", self.settings.discord_channel),
                    true,
                ),
                Err(e) if e == "Cancelled" => self.note("Sending cancelled.", true),
                Err(e) => self.note(e, false),
            }
        }
        let mut i = 0;
        while i < self.uploads.len() {
            let Some(result) = self.uploads[i].take() else {
                i += 1;
                continue;
            };
            let up = self.uploads.remove(i);
            let (waiting, rest): (Vec<FriendSend>, Vec<FriendSend>) =
                std::mem::take(&mut self.friend_sends)
                    .into_iter()
                    .partition(|s| s.clip == up.clip);
            self.friend_sends = rest;
            match result {
                Ok(link) => {
                    for c in self.clips.iter_mut().filter(|c| c.path == up.clip) {
                        c.link = Some(link.clone());
                    }
                    for s in self.shots.iter_mut().filter(|s| s.path == up.clip) {
                        s.link = Some(link.clone());
                    }
                    if waiting.is_empty() {
                        self.note(
                            "Link copied. Paste it anywhere: friends can watch it in their browser.",
                            true,
                        );
                        tasks.push(self.show_link(&link));
                    }
                    for s in waiting {
                        tasks.push(self.send_clip(s.clip, link.url.clone(), s.to, s.note));
                    }
                    tasks.push(self.reload());
                }
                Err(e) if e == "Cancelled" => self.note("Upload cancelled.", true),
                Err(e) => self.note(e, false),
            }
        }
        Task::batch(tasks)
    }

    /// Counts the games and tags in the clip list.
    fn index_clips(&mut self) {
        let count = |names: &mut dyn Iterator<Item = &String>| {
            let mut m: HashMap<&String, usize> = HashMap::new();
            for n in names {
                *m.entry(n).or_default() += 1;
            }
            m.into_iter()
                .map(|(n, c)| (n.clone(), c))
                .collect::<Vec<_>>()
        };
        self.games = count(&mut self.clips.iter().map(|c| &c.game));
        self.games
            .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        self.tags = count(&mut self.clips.iter().flat_map(|c| &c.tags));
        self.tags.sort_by_key(|(t, _)| t.to_lowercase());
        let mut folders: HashMap<String, usize> = Library::load()
            .folders
            .into_iter()
            .map(|f| (f, 0))
            .collect();
        for c in &self.clips {
            if let Some(f) = &c.folder {
                *folders.entry(f.clone()).or_default() += 1;
            }
        }
        self.folders = folders.into_iter().collect();
        self.folders.sort_by_key(|(f, _)| f.to_lowercase());
    }

    /// Saves folder changes and shows them.
    fn folders_changed(&mut self, lib: Library) {
        if let Err(e) = lib.save() {
            self.note(format!("Couldn't keep the folder: {e}"), false);
        }
        for c in &mut self.clips {
            c.folder = lib.folder(&c.path);
        }
        self.index_clips();
        if let Filter::Folder(f) = &self.filter
            && !self.folders.iter().any(|(name, _)| name == f)
        {
            self.filter = Filter::All;
        }
    }

    /// Moves files to Recently deleted, with Undo on the notice.
    fn delete(&mut self, paths: Vec<PathBuf>) -> Task<Message> {
        let dir = self.settings.save_dir();
        let now = share::now_secs();
        let (mut moved, mut failed) = (Vec::new(), None);
        for p in &paths {
            if let Some(e) = &mut self.editor
                && e.clip == *p
            {
                e.release();
            }
            match lib::trash(&dir, p, now) {
                Ok(to) => {
                    moved.push(to);
                    self.clips.retain(|c| c.path != *p);
                    self.shots.retain(|s| s.path != *p);
                    self.selected.retain(|s| s != p);
                    self.index_clips();
                    if self.editor.as_ref().is_some_and(|e| e.clip == *p) {
                        self.editor = None;
                        self.page = Page::Library;
                    }
                }
                Err(e) => failed = Some(e.to_string()),
            }
        }
        if self.selected.is_empty() {
            self.selecting = false;
        }
        match failed {
            Some(e) => self.note(format!("Couldn't delete: {e}"), false),
            None => {
                self.note(
                    match moved.len() {
                        1 => "Moved to Recently deleted.".to_string(),
                        n => format!("Moved {n} files to Recently deleted."),
                    },
                    true,
                );
                if let Some(n) = &mut self.notice {
                    n.undo = moved;
                }
            }
        }
        self.reload()
    }

    /// Puts files in Recently deleted back where they were.
    fn restore(&mut self, paths: Vec<PathBuf>) -> Task<Message> {
        if paths.is_empty() {
            return Task::none();
        }
        let dir = self.settings.save_dir();
        let mut failed = None;
        for p in &paths {
            match lib::restore(&dir, p) {
                Ok(_) => self.trash.retain(|t| t.path != *p),
                Err(e) => failed = Some(e.to_string()),
            }
        }
        match failed {
            Some(e) => self.note(format!("Couldn't put it back: {e}"), false),
            None if paths.len() > 1 => self.note(format!("Put back {} files.", paths.len()), true),
            None => self.note("Put back.", true),
        }
        self.reload()
    }

    /// Awards the badges the library has reached and says which are new.
    fn check_achievements(&mut self) {
        let now = SystemTime::now();
        let st = stats::summarize(&self.clips, now, 1);
        let new = achievements::award(&mut self.earned, &st, now);
        match new.as_slice() {
            [] => return,
            [one] => self.note(format!("Achievement earned: {one}"), true),
            many => self.note(
                format!("{} achievements earned. See them in Stats.", many.len()),
                true,
            ),
        }
        achievements::save(&self.earned);
    }

    fn note(&mut self, text: impl Into<String>, ok: bool) {
        self.notice = Some(Notice {
            text: text.into(),
            ok,
            at: Instant::now(),
            undo: Vec::new(),
        });
    }

    fn hotkey(&self) -> String {
        self.report
            .as_ref()
            .and_then(|r| r.hotkey.clone())
            .unwrap_or_else(|| self.settings.hotkey.clone())
    }

    fn open_editor(&mut self, path: PathBuf) -> Task<Message> {
        let closed = self.close_editor();
        match editor::Editor::open(path, &self.settings) {
            Ok((e, task)) => {
                self.editor = Some(e);
                self.page = Page::Editor;
                Task::batch([closed, task.map(Message::Editor)])
            }
            Err(e) => {
                self.note(e, false);
                closed
            }
        }
    }

    /// Leaves the editor, keeping a name typed but not yet confirmed.
    fn close_editor(&mut self) -> Task<Message> {
        match self.editor.take() {
            Some(mut e) => {
                let effect = e.rename();
                self.editor_effect(effect)
            }
            None => Task::none(),
        }
    }

    fn editor_effect(&mut self, effect: editor::Effect) -> Task<Message> {
        if let Some(SelfTest::Exporting(_)) = &self.test {
            match &effect {
                editor::Effect::Exported => {
                    let made = self
                        .editor
                        .as_ref()
                        .and_then(|e| e.last_export())
                        .map(Path::to_path_buf);
                    if let (Some(kind), Some(path)) = (self.test_current, made) {
                        if let Err(e) = check_export(kind, &path) {
                            return self.finish_test(Err(e));
                        }
                        self.test_made.push(path);
                    }
                    let next = self.test_exports.pop();
                    self.test_current = next;
                    match (next, &mut self.editor) {
                        (Some(k), Some(ed)) => {
                            println!("ui self-test: exported; now {k:?}");
                            if k == editor::ExportKind::Square {
                                // Styled, with a caption burned in.
                                ed.effects = kairo::media::Effects {
                                    look: kairo::media::Look::Cinematic,
                                    fade: true,
                                    even_loudness: true,
                                    ..Default::default()
                                };
                                ed.captions = vec![kairo::captions::Caption {
                                    start: 0.6,
                                    end: 2.0,
                                    text: "Self test caption".into(),
                                }];
                                let key = |t: f64, key: u8, down: bool| kairo::keys::KeyEvent {
                                    t,
                                    key,
                                    down,
                                };
                                ed.use_keys(vec![key(0.7, 1, true), key(1.4, 1, false)]);
                            }
                            let effect = ed.update(editor::Msg::Export(k));
                            return self.editor_effect(effect);
                        }
                        _ => {
                            // Last, a montage of the clip and its trimmed copy, picked the way a
                            // person picks them.
                            let clip = self.editor.as_ref().map(|e| e.clip.clone());
                            let trimmed = self.test_made.first().cloned();
                            let (Some(clip), Some(trimmed)) = (clip, trimmed) else {
                                return self
                                    .finish_test(Err("nothing to make a montage of".into()));
                            };
                            println!("ui self-test: exported; now a montage");
                            self.test = Some(SelfTest::Montage(Instant::now()));
                            self.selecting = true;
                            self.selected = vec![clip, trimmed];
                            return self.update(Message::MakeMontage);
                        }
                    }
                }
                editor::Effect::Note(t, false) => {
                    return self.finish_test(Err(format!("export failed: {t}")));
                }
                _ => {}
            }
        }
        match effect {
            editor::Effect::None => {}
            editor::Effect::Note(t, ok) => self.note(t, ok),
            editor::Effect::Renamed { from, to } => {
                let mut l = Library::load();
                l.renamed(&from, &to);
                let _ = l.save();
                self.note("Renamed.", true);
                return self.reload();
            }
            editor::Effect::Exported => return self.reload(),
            editor::Effect::Favorite(p) => return self.update(Message::Favorite(p)),
            editor::Effect::AskDelete(p) => return self.delete(vec![p]),
            editor::Effect::SendToChannel {
                file,
                clip,
                message,
            } => {
                if !self.posts.iter().any(|p| p.clip == clip) {
                    self.posts
                        .push(share::post_to_channel(&file, &clip, &message));
                }
                return self.reload();
            }
            editor::Effect::CancelSend(clip) => return self.update(Message::CancelPost(clip)),
            editor::Effect::Folder { clip, folder } => {
                let mut l = Library::load();
                l.set_folder(&clip, folder.as_deref());
                self.folders_changed(l);
            }
            editor::Effect::Tag { clip, tag, add } => {
                let mut l = Library::load();
                if add {
                    l.add_tag(&clip, &tag);
                } else {
                    l.remove_tag(&clip, &tag);
                }
                if let Err(e) = l.save() {
                    self.note(format!("Couldn't keep the tag: {e}"), false);
                }
                let tags = l.tags(&clip);
                for c in self.clips.iter_mut().filter(|c| c.path == clip) {
                    c.tags = tags.clone();
                }
                self.index_clips();
                if let Filter::Tag(t) = &self.filter
                    && !l.all_tags().iter().any(|(name, _)| name == t)
                {
                    self.filter = Filter::All;
                }
            }
            editor::Effect::Back => return self.update(Message::Back),
            editor::Effect::Share { file, clip, range } => {
                self.start_upload(file, clip, range);
                return self.reload();
            }
            editor::Effect::CancelUpload(p) => return self.update(Message::CancelUpload(p)),
            editor::Effect::CopyText(t) => return self.update(Message::CopyText(t)),
            editor::Effect::OpenUrl(u) => return self.update(Message::OpenUrl(u)),
            editor::Effect::ShowLink(p) => return self.update(Message::ShowLink(p)),
            editor::Effect::SendToFriends(p) => return self.update(Message::SendTo(p)),
        }
        Task::none()
    }

    /// Handles a message, starting an entrance when it changes the page or opens a dialog.
    pub fn update(&mut self, message: Message) -> Task<Message> {
        let page = (self.page, self.filter.clone(), self.welcome);
        let had_dialog = self.dialog.is_some();
        let task = self.handle(message);
        if (self.page, self.filter.clone(), self.welcome) != page {
            self.page_in = Some(Instant::now());
            // The mic test and shortcut capture only run while Settings shows them (a capture
            // left armed would make the next key pressed anywhere a global shortcut).
            if self.page != Page::Friends {
                self.friends.left();
            }
            if self.page != Page::Settings {
                self.setup.stop_mic_test();
                if self.welcome.is_none() {
                    self.setup.capturing_key = None;
                }
            }
        }
        if !had_dialog && self.dialog.is_some() {
            self.dialog_in = Some(Instant::now());
        }
        task
    }

    /// No entrances: turned off, or left to a system that asks for less motion, or a preview.
    fn still(&self) -> bool {
        self.now.is_some()
            || match self.settings.motion.as_str() {
                "on" => false,
                "off" => true,
                _ => crate::platform::reduce_motion(),
            }
    }

    /// How far an entrance that started at `since` and lasts `ms` has come: 0 to 1, eased.
    fn entrance(&self, since: Option<Instant>, ms: u64) -> f32 {
        match since {
            Some(s) if !self.still() => {
                let t = (s.elapsed().as_secs_f32() * 1000.0 / ms as f32).min(1.0);
                1.0 - (1.0 - t).powi(3)
            }
            _ => 1.0,
        }
    }

    /// Something is still easing in, so the window draws every frame until it settles.
    fn animating(&self) -> bool {
        !self.still()
            && [
                self.page_in.map(|s| (s, PAGE_IN_MS)),
                self.dialog_in.map(|s| (s, DIALOG_IN_MS)),
                self.notice.as_ref().map(|n| (n.at, NOTICE_IN_MS)),
            ]
            .into_iter()
            .flatten()
            .any(|(s, ms)| s.elapsed() < Duration::from_millis(ms + 20))
    }

    /// `content` rising (or, `sideways`, sliding) into place out of the background.
    fn entering<'a>(
        &self,
        content: Element<'a, Message>,
        shown: f32,
        sideways: bool,
    ) -> Element<'a, Message> {
        // The same layout whether moving or settled: changing it when the entrance ends would
        // reset the page's scroll position and focus.
        let shown = shown.clamp(0.0, 1.0);
        let shift = (1.0 - shown) * 18.0;
        let moved: Element<'a, Message> = if sideways {
            row![Space::new().width(shift), content].into()
        } else {
            column![Space::new().height(shift), content].into()
        };
        stack![
            moved,
            container(Space::new())
                .width(Fill)
                .height(Fill)
                .style(move |_| container::Style {
                    background: Some(theme::alpha(theme::bg(), (1.0 - shown) * 0.92).into()),
                    ..Default::default()
                })
        ]
        .into()
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::AnimTick => {}
            Message::Tick => return self.tick(),
            Message::Report(r) => {
                let r = r.map(|b| *b);
                let new_clip = r.as_ref().and_then(|r| r.status.last_clip.clone());
                let old_clip = self
                    .report
                    .as_ref()
                    .and_then(|r| r.status.last_clip.clone());
                self.report = r;
                if new_clip.is_some() && new_clip != old_clip && old_clip.is_some() {
                    return self.reload();
                }
            }
            Message::Clips(c) => {
                self.clips = c;
                self.index_clips();
                self.loaded = true;
                self.check_achievements();
                return self.ensure_hero();
            }
            Message::Hero(p, h) => {
                if self.hero_loading.as_ref() == Some(&p) {
                    self.hero_loading = None;
                }
                if let Some(h) = h {
                    self.hero = Some((p, h));
                }
            }
            Message::Shots(s) => self.shots = s,
            Message::Trash(t) => self.trash = t,
            Message::Undo => {
                let undo = self.notice.take().map(|n| n.undo).unwrap_or_default();
                return self.restore(undo);
            }
            Message::Restore(p) => return self.restore(vec![p]),
            Message::AskPurge(paths) => {
                if !paths.is_empty() {
                    self.dialog = Some(Dialog::Delete(paths));
                }
            }
            Message::CancelPost(p) => {
                for post in self.posts.iter().filter(|post| post.clip == p) {
                    post.cancel
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            Message::Rescanned(found) => {
                if let Some(b) = found {
                    let Scan {
                        sig,
                        clips,
                        shots,
                        trash,
                    } = *b;
                    self.scan_sig = Some(sig);
                    self.clips = clips;
                    self.index_clips();
                    self.shots = shots;
                    self.trash = trash;
                    self.loaded = true;
                    return self.ensure_hero();
                }
            }
            Message::Scrub(p, f) => {
                let start = !self.strips.contains_key(&p) && self.strip_loading.insert(p.clone());
                self.scrub = Some((p.clone(), f));
                self.scrub_moved = Some(Instant::now());
                if start {
                    return Task::perform(
                        background(move || (p.clone(), preview_strip(&p))),
                        |(p, s)| Message::Strip(p, s),
                    );
                }
            }
            Message::ScrubEnd(p) => {
                if self.scrub.as_ref().is_some_and(|(s, _)| *s == p) {
                    self.scrub = None;
                    self.scrub_moved = None;
                }
            }
            Message::WelcomeStep(i) => {
                self.welcome = Some(i.min(welcome::STEPS - 1));
                self.setup.capturing_key = None;
            }
            Message::WelcomeDone => {
                self.welcome = None;
                self.setup.capturing_key = None;
                self.settings.setup_done = true;
                self.changed_at = Some(Instant::now());
            }
            Message::HoverTick => {
                let resting = self
                    .scrub_moved
                    .is_some_and(|t| t.elapsed() >= HOVER_PLAY_AFTER);
                if let Some((p, f)) = &mut self.scrub
                    && resting
                    && let Some(n) = self.strips.get(p).map(Vec::len).filter(|n| *n > 1)
                {
                    let next = *f + 1.0 / n as f32;
                    *f = if next >= 1.0 { 0.0 } else { next };
                }
            }
            Message::Strip(p, frames) => {
                self.strip_loading.remove(&p);
                self.strip_order.retain(|q| *q != p);
                self.strip_order.push(p.clone());
                self.strips.insert(p, frames);
                // Keep the pictures of the last few cards only.
                while self.strip_order.len() > 8 {
                    let old = self.strip_order.remove(0);
                    self.strips.remove(&old);
                }
            }
            Message::Share(p) => self.start_upload(p.clone(), p, None),
            Message::ShowLink(p) => {
                if let Some(link) = self.link_of(&p).cloned() {
                    return self.show_link(&link);
                }
            }
            Message::CopyText(t) => {
                self.note("Copied.", true);
                return iced::clipboard::write(t);
            }
            Message::OpenUrl(u) => crate::app::open(Path::new(&u)),
            Message::UploadTick => return self.poll_uploads(),
            Message::CancelUpload(p) => {
                for u in self.uploads.iter().filter(|u| u.clip == p) {
                    u.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            Message::TakeScreenshot => {
                if let Err(e) = daemon::request(Request::Screenshot) {
                    self.note(e, false);
                }
            }
            Message::OpenShotsFolder => {
                let dir = self.settings.screenshot_dir();
                let _ = std::fs::create_dir_all(&dir);
                crate::app::open(&dir);
            }
            Message::OpenExternal(p) => crate::app::open(&p),
            Message::Filter(f) => {
                self.filter = f;
                self.page = Page::Library;
                return self.close_editor();
            }
            Message::Search(s) => self.search = s,
            Message::Sort(s) => self.sort = s,
            Message::Open(p) => return self.open_editor(p),
            Message::Back => {
                self.page = Page::Library;
                return Task::batch([self.close_editor(), self.reload()]);
            }
            Message::Favorite(p) => {
                let on = !self.clips.iter().any(|c| c.path == p && c.favorite);
                if let Some(c) = self.clips.iter_mut().find(|c| c.path == p) {
                    c.favorite = on;
                }
                let mut l = Library::load();
                l.set_favorite(&p, on);
                if let Err(e) = l.save() {
                    self.note(e, false);
                }
            }
            Message::Copy(p) => match crate::platform::copy_files(std::slice::from_ref(&p)) {
                Ok(()) => self.note("Copied. Paste it into Discord, a chat or a folder.", true),
                Err(e) => self.note(e, false),
            },
            Message::Reveal(p) => crate::app::reveal(&p),
            Message::AskRename(p) => {
                let name = stem(&p);
                self.dialog = Some(Dialog::Rename {
                    path: p,
                    name,
                    error: None,
                });
                return iced::widget::operation::focus("rename");
            }
            Message::RenameInput(s) => {
                if let Some(Dialog::Rename { name, error, .. }) = &mut self.dialog {
                    *name = s;
                    *error = None;
                }
            }
            Message::RenameConfirm => {
                let Some(Dialog::Rename { path, name, error }) = &mut self.dialog else {
                    return Task::none();
                };
                if name.trim() == stem(path) {
                    self.dialog = None;
                    return Task::none();
                }
                let to = match lib::rename_target(path, name) {
                    Ok(t) => t,
                    Err(e) => {
                        *error = Some(e);
                        return Task::none();
                    }
                };
                if let Err(e) = std::fs::rename(&*path, &to) {
                    *error = Some(format!("Couldn't rename: {e}"));
                    return Task::none();
                }
                crate::save::move_sidecars(path, &to);
                let mut l = Library::load();
                l.renamed(path, &to);
                let _ = l.save();
                self.dialog = None;
                self.note("Renamed.", true);
                return self.reload();
            }
            Message::FolderDialog(from) => {
                self.dialog = Some(Dialog::Folder {
                    name: from.clone().unwrap_or_default(),
                    from,
                    error: None,
                });
                return iced::widget::operation::focus("folder");
            }
            Message::FolderInput(t) => {
                if let Some(Dialog::Folder { name, error, .. }) = &mut self.dialog {
                    *name = t;
                    *error = None;
                }
            }
            Message::FolderConfirm => {
                let Some(Dialog::Folder { from, name, error }) = &mut self.dialog else {
                    return Task::none();
                };
                let mut l = Library::load();
                let made = match from.clone() {
                    Some(old) if lib::clean_folder(name) == old => Some(old),
                    Some(old) => l.rename_folder(&old, name).then(|| lib::clean_folder(name)),
                    None if l.folders.contains(&lib::clean_folder(name)) => None,
                    None => l.add_folder(name),
                };
                let Some(made) = made else {
                    *error = Some(if lib::clean_folder(name).is_empty() {
                        "Give it a name.".into()
                    } else {
                        "There's a folder with that name already.".into()
                    });
                    return Task::none();
                };
                self.dialog = None;
                self.folders_changed(l);
                self.filter = Filter::Folder(made);
            }
            Message::RemoveFolder(f) => {
                let mut l = Library::load();
                l.remove_folder(&f);
                self.folders_changed(l);
                self.note("Folder removed. Its clips are still in All clips.", true);
            }
            Message::MoveSelected(pick) => {
                let mut l = Library::load();
                for p in &self.selected {
                    l.set_folder(p, pick.0.as_deref());
                }
                let n = self.selected.len();
                self.folders_changed(l);
                self.note(
                    match &pick.0 {
                        Some(f) if n == 1 => format!("Moved to {f}."),
                        Some(f) => format!("{n} clips moved to {f}."),
                        None => "Taken out of their folder.".into(),
                    },
                    true,
                );
                self.selecting = false;
                self.selected.clear();
            }
            Message::AskDelete(p) => return self.delete(vec![p]),
            Message::Select(on) => {
                self.selecting = on;
                self.selected.clear();
            }
            Message::ToggleSelect(p) => match self.selected.iter().position(|s| *s == p) {
                Some(i) => {
                    self.selected.remove(i);
                }
                None => self.selected.push(p),
            },
            Message::DeleteSelected => {
                if !self.selected.is_empty() {
                    return self.delete(self.selected.clone());
                }
            }
            Message::MakeMontage => {
                if self.montage.is_none() && self.selected.len() >= 2 {
                    match crate::ffmpeg::path() {
                        Ok(ff) => {
                            self.montage = Some(crate::export::start_montage(
                                ff,
                                self.selected.clone(),
                                self.settings.save_dir(),
                            ))
                        }
                        Err(e) => self.note(e, false),
                    }
                }
            }
            Message::CancelMontage => {
                if let Some(m) = &self.montage {
                    m.cancel();
                }
            }
            Message::MontageTick => {
                if !self.montage.as_ref().is_some_and(|m| m.finished()) {
                    return Task::none();
                }
                let m = self.montage.take().expect("finished montage");
                let result = m.status.lock().unwrap().result.take();
                match result {
                    Some(Ok(path)) => {
                        if let Some(SelfTest::Montage(_)) = &self.test {
                            let want: f64 = self
                                .selected
                                .iter()
                                .filter_map(|p| {
                                    crate::ffmpeg::probe_clip(&crate::ffmpeg::path().ok()?, p).ok()
                                })
                                .map(|i| i.duration)
                                .sum();
                            return self.finish_test(check_montage(&path, want).map(|_| {
                                "played, then saved a trimmed copy, a GIF, a vertical copy, slow motion, a styled square copy with a caption and the keys overlay, and a montage".into()
                            }));
                        }
                        self.selecting = false;
                        self.selected.clear();
                        self.note("Montage saved.", true);
                        return Task::batch([self.reload(), self.open_editor(path)]);
                    }
                    Some(Err(e)) => {
                        if self.test.is_some() {
                            return self.finish_test(Err(format!("montage failed: {e}")));
                        }
                        if e != "Cancelled" {
                            self.note(format!("Couldn't make the montage: {e}"), false);
                        }
                    }
                    None => {}
                }
            }
            Message::DeleteConfirm => {
                let Some(Dialog::Delete(paths)) = self.dialog.take() else {
                    return Task::none();
                };
                let dir = self.settings.save_dir();
                let mut l = Library::load();
                let (mut gone, mut failed) = (0, None);
                for p in &paths {
                    let from = self
                        .trash
                        .iter()
                        .find(|t| t.path == *p)
                        .map_or_else(|| p.clone(), |t| t.from.clone());
                    match lib::purge(&dir, p) {
                        Ok(()) => {
                            gone += 1;
                            crate::save::remove_sidecars(&from);
                            l.forget(&from);
                            self.trash.retain(|t| t.path != *p);
                        }
                        Err(e) => failed = Some(e.to_string()),
                    }
                }
                let _ = l.save();
                match failed {
                    Some(e) => self.note(format!("Couldn't delete: {e}"), false),
                    None if gone > 1 => self.note(format!("Deleted {gone} files for good."), true),
                    None => self.note("Deleted for good.", true),
                }
                return self.reload();
            }
            Message::CloseDialog => self.dialog = None,
            Message::Page(p) => {
                self.page = p;
                if p != Page::Editor {
                    return self.close_editor();
                }
            }
            Message::SaveClip => {
                self.save_menu = false;
                if let Err(e) = daemon::request(Request::Save) {
                    self.note(e, false);
                }
            }
            Message::SaveMenu(open) => self.save_menu = open,
            Message::ToggleSession => {
                if let Err(e) = daemon::request(Request::Session) {
                    self.note(e, false);
                }
            }
            Message::SaveFor(secs) => {
                self.save_menu = false;
                if let Err(e) = daemon::request_save(secs) {
                    self.note(e, false);
                }
            }
            Message::TogglePause => {
                let _ = daemon::request(Request::TogglePause);
            }
            Message::StartRecorder => {
                if let Err(e) = daemon::ensure_running() {
                    self.note(e, false);
                }
            }
            Message::OpenFolder => {
                let dir = self.settings.save_dir();
                let _ = std::fs::create_dir_all(&dir);
                crate::app::open(&dir);
            }
            Message::Editor(m) => {
                if let Some(e) = &mut self.editor {
                    let effect = e.update(m);
                    return self.editor_effect(effect);
                }
            }
            Message::Friends(m) => return self.friends.update(m).map(Message::Friends),
            Message::SendTo(clip) => {
                if !self.friends.can_send() {
                    self.page = Page::Friends;
                    self.note(
                        "Sign in and add a friend here first, then send them clips.",
                        false,
                    );
                    return Task::none();
                }
                self.dialog = Some(Dialog::SendTo {
                    clip,
                    picked: Vec::new(),
                    note: String::new(),
                });
            }
            Message::SendPick(id) => {
                let everyone: Vec<String> = self
                    .friends
                    .inbox
                    .friends_only()
                    .map(|f| f.id.clone())
                    .collect();
                if let Some(Dialog::SendTo { picked, .. }) = &mut self.dialog {
                    if id == "*" {
                        *picked = if picked.len() == everyone.len() {
                            Vec::new()
                        } else {
                            everyone
                        };
                    } else if let Some(i) = picked.iter().position(|p| *p == id) {
                        picked.remove(i);
                    } else {
                        picked.push(id);
                    }
                }
            }
            Message::SendNote(s) => {
                if let Some(Dialog::SendTo { note, .. }) = &mut self.dialog {
                    *note = s;
                }
            }
            Message::SendGo => {
                let Some(Dialog::SendTo { clip, picked, note }) = self.dialog.take() else {
                    return Task::none();
                };
                if picked.is_empty() {
                    return Task::none();
                }
                let live = self
                    .link_of(&clip)
                    .filter(|l| {
                        !l.expired(share::now_secs()) && crate::friends::is_clip_link(&l.url)
                    })
                    .map(|l| l.url.clone());
                match live {
                    Some(url) => return self.send_clip(clip, url, picked, note),
                    None => {
                        let names = self.friend_names(&picked);
                        self.friend_sends.push(FriendSend {
                            clip: clip.clone(),
                            to: picked,
                            note,
                        });
                        self.start_upload(clip.clone(), clip, None);
                        self.note(format!("Uploading, then sending it to {names}."), true);
                    }
                }
            }
            Message::SentToFriends(r) => match r {
                Ok(said) => {
                    self.note(said, true);
                    return self
                        .friends
                        .update(friends::Msg::Done(Ok(String::new())))
                        .map(Message::Friends);
                }
                Err(e) => self.note(e, false),
            },
            Message::Settings(m) => {
                let (task, effect) = self.setup.update(&mut self.settings, m);
                let task = task.map(Message::Settings);
                return match effect {
                    settings::Effect::None => task,
                    settings::Effect::Changed => {
                        self.changed_at = Some(Instant::now());
                        task
                    }
                    settings::Effect::OpenUrl(u) => {
                        crate::app::open(Path::new(&u));
                        task
                    }
                    settings::Effect::Restart => {
                        crate::log::info!("updated; restarting");
                        if daemon::is_running() {
                            let _ = daemon::request(Request::Restart);
                        }
                        let _ = crate::app::spawn_self(&["window", "--wait"]);
                        iced::exit()
                    }
                };
            }
            Message::Key(e) => return self.key(e),
            Message::Opened(id) => self.window = Some(id),
        }
        Task::none()
    }

    fn key(&mut self, e: keyboard::Event) -> Task<Message> {
        let keyboard::Event::KeyPressed {
            key,
            modifiers,
            physical_key,
            ..
        } = e
        else {
            return Task::none();
        };
        if self.setup.capturing_key.is_some() {
            if let settings::Effect::Changed =
                self.setup
                    .key(&mut self.settings, &key, &physical_key, modifiers)
            {
                self.changed_at = Some(Instant::now());
            }
            return Task::none();
        }
        if matches!(key.as_ref(), Key::Named(Named::Escape)) {
            if self.dialog.is_some() {
                self.dialog = None;
            } else if self.save_menu {
                self.save_menu = false;
            } else if self.selecting && self.montage.is_none() {
                self.selecting = false;
                self.selected.clear();
            } else if self.page != Page::Library {
                return self.update(Message::Back);
            }
            return Task::none();
        }
        if let Some(d) = &self.dialog {
            if matches!(key.as_ref(), Key::Named(Named::Enter)) {
                return self.update(match d {
                    Dialog::Rename { .. } => Message::RenameConfirm,
                    Dialog::Delete(_) => Message::DeleteConfirm,
                    Dialog::Link { .. } | Dialog::WhatsNew { .. } => Message::CloseDialog,
                    Dialog::Folder { .. } => Message::FolderConfirm,
                    Dialog::SendTo { .. } => Message::SendGo,
                });
            }
            return Task::none();
        }
        if self.page == Page::Editor
            && let Some(ed) = &mut self.editor
        {
            ed.key(&key, modifiers);
        }
        Task::none()
    }

    /// Twice a second: the recorder's state, settings to save, requests from other processes.
    fn tick(&mut self) -> Task<Message> {
        if self.test.is_some() {
            return self.test_step();
        }
        self.ticks += 1;
        // Following the computer's light or dark setting: look again now and then.
        if self.settings.look == "system" && self.ticks.is_multiple_of(8) {
            theme::apply_look("system");
        }
        let mut tasks = vec![Task::perform(
            background(|| daemon::read_report().map(Box::new)),
            Message::Report,
        )];
        if self.ticks.is_multiple_of(3) {
            tasks.push(self.refresh());
        }
        if let Some(t) = self.changed_at
            && t.elapsed() > Duration::from_millis(600)
        {
            self.changed_at = None;
            self.settings.sanitize();
            if self.settings != self.saved {
                let folder_changed = self.settings.save_dir() != self.saved.save_dir();
                match config::save(&config::default_path(), &self.settings) {
                    Ok(()) => self.saved = self.settings.clone(),
                    Err(e) => self.note(format!("Couldn't save the settings: {e}"), false),
                }
                if folder_changed {
                    tasks.push(self.reload());
                }
            }
        }
        if let Some(n) = &self.notice
            && n.at.elapsed()
                > Duration::from_secs(if !n.undo.is_empty() {
                    8
                } else if n.ok {
                    4
                } else {
                    9
                })
        {
            self.notice = None;
        }
        tasks.push(
            self.friends
                .tick(self.page == Page::Friends)
                .map(Message::Friends),
        );
        if let Some(text) = take_open_request() {
            if let Some(id) = self.window {
                tasks.push(window::minimize(id, false));
                tasks.push(window::gain_focus(id));
            }
            if text == UPDATE_REQUEST {
                self.page = Page::Settings;
                self.setup.update.auto_install = true;
                tasks.push(Task::done(Message::Settings(settings::Msg::CheckNow)));
            } else if text == FRIENDS_REQUEST {
                self.page = Page::Friends;
            } else if !text.is_empty() {
                tasks.push(self.open_editor(PathBuf::from(text)));
            }
        }
        Task::batch(tasks)
    }

    fn finish_test(&mut self, r: Result<String, String>) -> Task<Message> {
        *TEST_RESULT.lock().unwrap() = Some(r);
        self.test = None;
        iced::exit()
    }

    /// One step of `--ui-self-test`: wait for the first frame and the filmstrip, play for two
    /// seconds, then save a trimmed copy (the export's result ends the test).
    fn test_step(&mut self) -> Task<Message> {
        let Some(state) = &self.test else {
            return Task::none();
        };
        let state = match state {
            SelfTest::Loading(t) => SelfTest::Loading(*t),
            SelfTest::Playing(t) => SelfTest::Playing(*t),
            SelfTest::Exporting(t) => SelfTest::Exporting(*t),
            SelfTest::Montage(t) => SelfTest::Montage(*t),
        };
        let Some(ed) = &mut self.editor else {
            return self.finish_test(Err("the editor didn't open".into()));
        };
        match state {
            SelfTest::Loading(t) if ed.ready() => {
                println!(
                    "ui self-test: first frame and filmstrip shown after {:.1} s",
                    t.elapsed().as_secs_f64()
                );
                // Silent: the sound still runs (it's the playback clock), just at zero volume.
                ed.update(editor::Msg::Volume(0.0));
                ed.update(editor::Msg::TogglePlay);
                ed.frames_shown = 0;
                self.test = Some(SelfTest::Playing(Instant::now()));
            }
            SelfTest::Loading(t) if t.elapsed() > Duration::from_secs(40) => {
                return self
                    .finish_test(Err("the first frame or the filmstrip never arrived".into()));
            }
            SelfTest::Playing(t) if t.elapsed() > Duration::from_secs(2) => {
                let pos = ed.position();
                println!(
                    "ui self-test: played to {pos:.2} s in {:.1} s",
                    t.elapsed().as_secs_f64()
                );
                let shown = ed.frames_shown;
                println!("ui self-test: {shown} frames shown while playing");
                if shown < 20 {
                    return self.finish_test(Err(format!(
                        "only {shown} frames reached the screen in 2 s"
                    )));
                }
                if pos < 0.8 {
                    return self
                        .finish_test(Err(format!("playback didn't advance (at {pos:.2} s)")));
                }
                if ed.is_playing() {
                    ed.update(editor::Msg::TogglePlay);
                }
                ed.trim = (0.5, 2.5);
                self.test = Some(SelfTest::Exporting(Instant::now()));
                // Made in this order after the trimmed copy (popped from the end).
                self.test_exports = vec![
                    editor::ExportKind::Square,
                    editor::ExportKind::Slow(2),
                    editor::ExportKind::Vertical,
                    editor::ExportKind::Gif,
                ];
                self.test_current = Some(editor::ExportKind::Trim);
                let effect = ed.update(editor::Msg::Export(editor::ExportKind::Trim));
                return self.editor_effect(effect);
            }
            SelfTest::Exporting(t) if t.elapsed() > Duration::from_secs(150) => {
                return self.finish_test(Err("the export didn't finish".into()));
            }
            SelfTest::Montage(t) if t.elapsed() > Duration::from_secs(150) => {
                return self.finish_test(Err("the montage didn't finish".into()));
            }
            _ => {}
        }
        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            // Once a second is enough for the recorder card; every wake-up rebuilds the window.
            iced::time::every(Duration::from_millis(1000)).map(|_| Message::Tick),
            keyboard::listen().map(Message::Key),
            window::open_events().map(Message::Opened),
        ];
        if !self.uploads.is_empty() || !self.posts.is_empty() {
            subs.push(iced::time::every(Duration::from_millis(150)).map(|_| Message::UploadTick));
        }
        if self.montage.is_some() {
            subs.push(iced::time::every(Duration::from_millis(200)).map(|_| Message::MontageTick));
        }
        if self.animating() {
            subs.push(window::frames().map(|_| Message::AnimTick));
        }
        if self.setup.mic_testing() {
            subs.push(
                iced::time::every(Duration::from_millis(40))
                    .map(|_| Message::Settings(settings::Msg::MicTick)),
            );
        }
        if self.scrub.is_some() {
            // The hovered card's preview plays at about ten times speed.
            subs.push(iced::time::every(HOVER_FRAME).map(|_| Message::HoverTick));
        }
        if self.editor.as_ref().is_some_and(|e| e.busy()) {
            // A timer rather than the window's frames, so playback runs while it's hidden too.
            subs.push(
                iced::time::every(Duration::from_millis(8))
                    .map(|_| Message::Editor(editor::Msg::Frame)),
            );
        }
        Subscription::batch(subs)
    }

    pub fn theme(&self) -> Theme {
        theme::theme()
    }

    pub fn title(&self) -> String {
        match (&self.page, &self.editor) {
            (Page::Editor, Some(e)) => format!("{} - Kairo", e.name),
            _ => "Kairo".into(),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        if let Some(step) = self.welcome {
            let shown = self.entrance(self.page_in, PAGE_IN_MS);
            return self.entering(self.welcome_view(step), shown, true);
        }
        let page: Element<'_, Message> = match (self.page, &self.editor) {
            (Page::Editor, Some(e)) => {
                let item = self.clips.iter().find(|c| c.path == e.clip);
                let target = e.share_target();
                let share = editor::ShareView {
                    progress: self.upload_progress(&target),
                    link: self.link_of(&target).cloned(),
                    channel: (!self.settings.discord_channel.is_empty())
                        .then(|| self.settings.discord_channel.clone()),
                    sending: self
                        .posts
                        .iter()
                        .find(|p| p.clip == e.clip)
                        .map(share::Post::progress),
                    tags: self.tags.iter().map(|(t, _)| t.clone()).collect(),
                    folders: self.folders.iter().map(|(f, _)| f.clone()).collect(),
                    friends: crate::friends::available(),
                };
                column![
                    Space::new().height(18),
                    e.view(item, share).map(Message::Editor)
                ]
                .into()
            }
            (Page::Friends, _) => {
                column![self.top_bar(), self.friends.view().map(Message::Friends)].into()
            }
            (Page::Settings, _) => {
                let used = self.clips.iter().map(|c| c.size).sum();
                column![
                    self.top_bar(),
                    self.setup
                        .view(&self.settings, self.report.as_ref(), used, &self.games)
                        .map(Message::Settings)
                ]
                .into()
            }
            _ => column![self.top_bar(), self.library_view()].into(),
        };
        let page = self.entering(page, self.entrance(self.page_in, PAGE_IN_MS), false);
        let mut layers =
            stack![row![self.sidebar(), container(page).width(Fill).height(Fill)].height(Fill)];
        if let Some(n) = &self.notice {
            layers = layers.push(
                container(
                    container(
                        row![
                            icon(
                                if n.ok { Icon::Check } else { Icon::Close },
                                16.0,
                                if n.ok { theme::GREEN } else { theme::accent() }
                            ),
                            text(n.text.clone()).size(13).font(MEDIUM),
                        ]
                        .push((!n.undo.is_empty()).then(|| {
                            button(
                                row![
                                    icon(Icon::Undo, 14.0, theme::ink()),
                                    text("Undo").size(13).font(SEMIBOLD)
                                ]
                                .spacing(6)
                                .align_y(Alignment::Center),
                            )
                            .padding(Padding::from([5, 10]))
                            .style(theme::secondary)
                            .on_press(Message::Undo)
                        }))
                        .spacing(10)
                        .align_y(Alignment::Center),
                    )
                    .padding(Padding::from([10, 16]))
                    .max_width(560)
                    .style(theme::toast),
                )
                .width(Fill)
                .height(Fill)
                .align_x(Horizontal::Center)
                .align_y(iced::alignment::Vertical::Bottom)
                .padding(Padding {
                    // Rises from the window's edge.
                    bottom: 22.0 - (1.0 - self.entrance(Some(n.at), NOTICE_IN_MS)) * 20.0,
                    ..Padding::new(22.0)
                }),
            );
        }
        if self.selecting && self.page == Page::Library {
            layers = layers.push(self.selection_bar());
        }
        if self.save_menu {
            layers = layers.push(self.save_menu_view());
        }
        if let Some(d) = &self.dialog {
            layers = layers.push(self.dialog_view(d));
        }
        layers.into()
    }

    /// The bar at the bottom while picking clips: what's picked, and what to do with it.
    fn selection_bar(&self) -> Element<'_, Message> {
        let n = self.selected.len();
        let total: f64 = self
            .clips
            .iter()
            .filter(|c| self.selected.contains(&c.path))
            .filter_map(|c| c.duration)
            .sum();
        let content: Element<'_, Message> = if let Some(m) = &self.montage {
            let (stage, p) = {
                let st = m.status.lock().unwrap();
                (st.stage.clone(), st.progress)
            };
            row![
                icon(Icon::Layers, 17.0, theme::accent()),
                column![
                    text(format!("{stage}: {:.0}%", p * 100.0))
                        .size(13)
                        .font(MEDIUM),
                    iced::widget::progress_bar(0.0..=1.0, p)
                        .girth(5)
                        .style(theme::progress),
                ]
                .spacing(6)
                .width(Fill),
                button(text("Cancel").size(13).font(MEDIUM))
                    .padding(Padding::from([8, 12]))
                    .style(theme::ghost)
                    .on_press(Message::CancelMontage),
            ]
            .spacing(14)
            .align_y(Alignment::Center)
            .into()
        } else {
            let what = match n {
                0 => "Pick clips in the order they should play.".to_string(),
                1 => format!("1 clip, {}", duration(Some(total))),
                n => format!("{n} clips, {} in all", duration(Some(total))),
            };
            row![
                icon(Icon::Selected, 17.0, theme::accent()),
                text(what).size(14).font(MEDIUM),
                space::horizontal(),
                library::tip(
                    widgets::icon_button(Icon::Layers, "Make a montage", theme::on_accent())
                        .style(theme::primary)
                        .on_press_maybe((n >= 2).then_some(Message::MakeMontage)),
                    "Joins them into one video, fading between clips",
                ),
                pick_list(
                    std::iter::once(FolderPick(None))
                        .chain(
                            self.folders
                                .iter()
                                .map(|(f, _)| FolderPick(Some(f.clone())))
                        )
                        .collect::<Vec<_>>(),
                    None::<FolderPick>,
                    Message::MoveSelected,
                )
                .placeholder("Move to folder")
                .text_size(13)
                .padding(Padding::from([7, 12]))
                .style(theme::pick)
                .menu_style(theme::menu),
                widgets::icon_button(Icon::Trash, "Delete", theme::ink_2())
                    .style(theme::danger_ghost)
                    .on_press_maybe((n >= 1).then_some(Message::DeleteSelected)),
                button(text("Done").size(13).font(MEDIUM))
                    .padding(Padding::from([8, 12]))
                    .style(theme::ghost)
                    .on_press(Message::Select(false)),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .into()
        };
        container(
            container(content)
                .padding(Padding::from([10, 14]))
                .max_width(720)
                .style(|_| container::Style {
                    background: Some(theme::surface_hi().into()),
                    border: iced::Border {
                        color: theme::alpha(theme::accent(), 0.45),
                        width: 1.0,
                        radius: 14.0.into(),
                    },
                    shadow: iced::Shadow {
                        color: theme::alpha(iced::Color::BLACK, 0.6),
                        offset: iced::Vector::new(0.0, 10.0),
                        blur_radius: 34.0,
                    },
                    ..Default::default()
                }),
        )
        .width(Fill)
        .height(Fill)
        .align_x(Horizontal::Center)
        .align_y(iced::alignment::Vertical::Bottom)
        .padding(Padding {
            top: 0.0,
            right: 28.0,
            bottom: 22.0,
            left: 264.0,
        })
        .into()
    }

    /// The menu under the save button: the usual length, or less of it.
    fn save_menu_view(&self) -> Element<'_, Message> {
        let usual = self.settings.clip_seconds;
        let mut lengths: Vec<u32> = [10u32, 15, 30, 60, 120, 180]
            .into_iter()
            .filter(|l| *l < usual)
            .collect();
        lengths.push(usual);
        let mut items = column![
            container(
                text("Save the last")
                    .size(12)
                    .font(SEMIBOLD)
                    .color(theme::muted())
            )
            .padding(Padding::from([6, 12]))
        ]
        .spacing(2);
        for l in lengths.into_iter().rev() {
            let mut r = row![
                text(seconds_label(l)).size(14).font(MEDIUM),
                space::horizontal()
            ]
            .align_y(Alignment::Center);
            if l == usual {
                r = r.push(widgets::keycap(&self.hotkey()));
            }
            items = items.push(
                button(r)
                    .width(Fill)
                    .padding(Padding::from([8, 12]))
                    .style(theme::ghost)
                    .on_press(Message::SaveFor(l)),
            );
        }
        let card = container(items)
            .padding(6)
            .width(230)
            .style(|_| container::Style {
                background: Some(theme::surface().into()),
                border: iced::Border {
                    color: theme::edge(),
                    width: 1.0,
                    radius: 12.0.into(),
                },
                shadow: iced::Shadow {
                    color: theme::alpha(iced::Color::BLACK, 0.55),
                    offset: iced::Vector::new(0.0, 12.0),
                    blur_radius: 32.0,
                },
                ..Default::default()
            });
        stack![
            // A click anywhere else closes it.
            iced::widget::mouse_area(Space::new().width(Fill).height(Fill))
                .on_press(Message::SaveMenu(false)),
            container(card)
                .width(Fill)
                .align_x(Horizontal::Right)
                .padding(Padding {
                    top: 70.0,
                    right: 28.0,
                    bottom: 0.0,
                    left: 0.0,
                }),
        ]
        .into()
    }

    fn dialog_view<'a>(&'a self, d: &'a Dialog) -> Element<'a, Message> {
        let shown = self.entrance(self.dialog_in, DIALOG_IN_MS);
        match d {
            Dialog::Folder { from, name, error } => {
                let mut body = column![
                    text_input("Folder name", name)
                        .id("folder")
                        .on_input(Message::FolderInput)
                        .on_submit(Message::FolderConfirm)
                        .padding(Padding::from([9, 12]))
                        .size(14)
                        .style(theme::input)
                ]
                .spacing(8);
                if let Some(e) = error {
                    body = body.push(text(e.clone()).size(12).color(theme::accent()));
                }
                dialog(
                    if from.is_some() {
                        "Rename folder"
                    } else {
                        "New folder"
                    },
                    body,
                    row![
                        button(text("Cancel").size(13).font(MEDIUM))
                            .padding(Padding::from([8, 14]))
                            .style(theme::ghost)
                            .on_press(Message::CloseDialog),
                        button(
                            text(if from.is_some() { "Rename" } else { "Make it" })
                                .size(13)
                                .font(SEMIBOLD)
                        )
                        .padding(Padding::from([8, 14]))
                        .style(theme::primary)
                        .on_press(Message::FolderConfirm),
                    ]
                    .spacing(8),
                    Message::CloseDialog,
                    shown,
                )
            }
            Dialog::WhatsNew { version, notes } => {
                let mut body = column![
                    text("You're up to date. Here's what this version brings.")
                        .size(14)
                        .color(theme::ink_2())
                ]
                .spacing(10);
                for (title, detail) in crate::updater::points(notes, 6) {
                    let mut words =
                        column![text(title).size(14).font(SEMIBOLD).color(theme::ink())];
                    if !detail.is_empty() {
                        words = words.push(text(detail).size(13).color(theme::ink_2()));
                    }
                    body = body.push(
                        row![
                            container(icon(Icon::Check, 15.0, theme::accent())).padding(Padding {
                                top: 2.0,
                                ..Padding::ZERO
                            }),
                            words.spacing(2).width(Fill)
                        ]
                        .spacing(10),
                    );
                }
                dialog(
                    &format!("What's new in Kairo {version}"),
                    body,
                    row![
                        button(text("All release notes").size(13).font(MEDIUM))
                            .padding(Padding::from([8, 14]))
                            .style(theme::ghost)
                            .on_press(Message::OpenUrl(crate::updater::release_page(version))),
                        button(text("Got it").size(13).font(SEMIBOLD))
                            .padding(Padding::from([8, 14]))
                            .style(theme::primary)
                            .on_press(Message::CloseDialog),
                    ]
                    .spacing(8),
                    Message::CloseDialog,
                    shown,
                )
            }
            Dialog::Rename { name, error, .. } => {
                let mut body = column![
                    text_input("Clip name", name)
                        .id("rename")
                        .on_input(Message::RenameInput)
                        .on_submit(Message::RenameConfirm)
                        .padding(Padding::from([9, 12]))
                        .size(14)
                        .style(theme::input)
                ]
                .spacing(8);
                if let Some(e) = error {
                    body = body.push(text(e.clone()).size(12).color(theme::accent()));
                }
                dialog(
                    "Rename clip",
                    body,
                    row![
                        button(text("Cancel").size(13).font(MEDIUM))
                            .padding(Padding::from([8, 14]))
                            .style(theme::ghost)
                            .on_press(Message::CloseDialog),
                        button(text("Rename").size(13).font(SEMIBOLD))
                            .padding(Padding::from([8, 14]))
                            .style(theme::primary)
                            .on_press(Message::RenameConfirm),
                    ]
                    .spacing(8),
                    Message::CloseDialog,
                    shown,
                )
            }
            Dialog::SendTo { picked, note, .. } => {
                let everyone: Vec<&crate::friends::Friend> =
                    self.friends.inbox.friends_only().collect();
                let tick = |on: bool, label: String, m: Message| {
                    button(
                        row![
                            icon(
                                if on { Icon::Selected } else { Icon::Square },
                                17.0,
                                if on { theme::accent() } else { theme::muted() }
                            ),
                            text(label).size(14).font(MEDIUM)
                        ]
                        .spacing(10)
                        .align_y(Alignment::Center),
                    )
                    .width(iced::Fill)
                    .padding(Padding::from([7, 8]))
                    .style(theme::ghost)
                    .on_press(m)
                };
                let mut list = column![].spacing(2);
                if everyone.len() > 1 {
                    list = list.push(tick(
                        picked.len() == everyone.len(),
                        format!("Everyone ({})", everyone.len()),
                        Message::SendPick("*".into()),
                    ));
                }
                for f in &everyone {
                    list = list.push(tick(
                        picked.contains(&f.id),
                        f.username.clone(),
                        Message::SendPick(f.id.clone()),
                    ));
                }
                let body = column![
                    container(scrollable(list).style(theme::scroll)).max_height(260),
                    text_input("Add a message (optional)", note)
                        .on_input(Message::SendNote)
                        .on_submit(Message::SendGo)
                        .padding(Padding::from([9, 12]))
                        .size(14)
                        .style(theme::input),
                    text("A clip without a link is uploaded to catbox.moe first. Anyone with the link can watch it.")
                        .size(12)
                        .color(theme::muted()),
                ]
                .spacing(12);
                dialog(
                    "Send to friends",
                    body,
                    row![
                        button(text("Cancel").size(13).font(MEDIUM))
                            .padding(Padding::from([8, 14]))
                            .style(theme::ghost)
                            .on_press(Message::CloseDialog),
                        button(
                            row![
                                icon(Icon::Send, 14.0, iced::Color::WHITE),
                                text("Send").size(13).font(SEMIBOLD)
                            ]
                            .spacing(7)
                            .align_y(Alignment::Center)
                        )
                        .padding(Padding::from([8, 14]))
                        .style(theme::primary)
                        .on_press_maybe((!picked.is_empty()).then_some(Message::SendGo)),
                    ]
                    .spacing(8),
                    Message::CloseDialog,
                    shown,
                )
            }
            Dialog::Link { url, expires, qr } => {
                let lasts = match expires {
                    None => "It stays up while people keep watching it (removed after 2 years without views).".to_string(),
                    Some(t) => {
                        let hours = t.saturating_sub(share::now_secs()).div_ceil(3600);
                        if hours > 36 {
                            format!("It works for {} more days, then it's gone.", hours.div_ceil(24))
                        } else {
                            format!("It works for {hours} more hour{}, then it's gone.", if hours == 1 { "" } else { "s" })
                        }
                    }
                };
                let mut body = column![
                    text("Copied. Paste it in Discord, a group chat or anywhere: anyone with the link can watch.")
                        .size(14)
                        .color(theme::ink_2()),
                    row![
                        container(text(url.clone()).size(14).font(MEDIUM).wrapping(text::Wrapping::None))
                            .padding(Padding::from([9, 12]))
                            .width(iced::Fill)
                            .clip(true)
                            .style(theme::well),
                        button(row![icon(Icon::Copy, 15.0, iced::Color::WHITE), text("Copy").size(13).font(SEMIBOLD)].spacing(7).align_y(Alignment::Center))
                            .padding(Padding::from([9, 14]))
                            .style(theme::primary)
                            .on_press(Message::CopyText(url.clone())),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                ]
                .spacing(14);
                if let Some(data) = qr {
                    body = body.push(
                        row![
                            container(qr_code(data).cell_size(4).style(|_| {
                                iced::widget::qr_code::Style {
                                    cell: iced::Color::BLACK,
                                    background: iced::Color::WHITE,
                                }
                            }))
                            .padding(8)
                            .style(|_| container::Style {
                                background: Some(iced::Color::WHITE.into()),
                                border: iced::Border {
                                    radius: 8.0.into(),
                                    ..Default::default()
                                },
                                ..Default::default()
                            }),
                            column![
                                text("On your phone?").font(SEMIBOLD).size(14),
                                text("Scan this to open the clip there.")
                                    .size(13)
                                    .color(theme::ink_2()),
                                Space::new().height(6),
                                text(lasts).size(12).color(theme::muted()),
                            ]
                            .spacing(4),
                        ]
                        .spacing(16)
                        .align_y(Alignment::Center),
                    );
                }
                dialog(
                    "Your share link",
                    body,
                    row![
                        button(
                            row![
                                icon(Icon::Globe, 15.0, theme::ink_2()),
                                text("Open in browser").size(13).font(MEDIUM)
                            ]
                            .spacing(7)
                            .align_y(Alignment::Center)
                        )
                        .padding(Padding::from([8, 14]))
                        .style(theme::ghost)
                        .on_press(Message::OpenUrl(url.clone())),
                        button(text("Done").size(13).font(SEMIBOLD))
                            .padding(Padding::from([8, 14]))
                            .style(theme::secondary)
                            .on_press(Message::CloseDialog),
                    ]
                    .spacing(8),
                    Message::CloseDialog,
                    shown,
                )
            }
            Dialog::Delete(paths) => dialog(
                &match paths.len() {
                    1 => "Delete for good?".to_string(),
                    n => format!("Delete {n} files for good?"),
                },
                text(match paths.as_slice() {
                    [p] => format!(
                        "\"{}\" will be gone from your computer. This can't be undone.",
                        stem(p)
                    ),
                    _ => "They'll be gone from your computer. This can't be undone.".to_string(),
                })
                .size(14)
                .color(theme::ink_2()),
                row![
                    button(text("Keep it").size(13).font(MEDIUM))
                        .padding(Padding::from([8, 14]))
                        .style(theme::ghost)
                        .on_press(Message::CloseDialog),
                    button(
                        row![
                            icon(Icon::Trash, 15.0, theme::on_accent()),
                            text("Delete for good").size(13).font(SEMIBOLD)
                        ]
                        .spacing(7)
                        .align_y(Alignment::Center)
                    )
                    .padding(Padding::from([8, 14]))
                    .style(theme::primary)
                    .on_press(Message::DeleteConfirm),
                ]
                .spacing(8),
                Message::CloseDialog,
                shown,
            ),
        }
    }

    // --- Sidebar --------------------------------------------------------------------------

    fn sidebar(&self) -> Element<'_, Message> {
        let logo = row![
            image(image::Handle::from_bytes(
                include_bytes!("../../assets/icon.png").as_slice()
            ))
            .width(28)
            .height(28),
            text("Kairo").font(BOLD).size(19),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let favorites = self.clips.iter().filter(|c| c.favorite).count();
        let in_library = !matches!(self.page, Page::Settings | Page::Friends);
        let mut nav = column![
            nav_item(
                Icon::Clips,
                "All clips",
                Some(self.clips.len()),
                in_library && self.filter == Filter::All
            )
            .on_press(Message::Filter(Filter::All)),
            nav_item(
                Icon::Star,
                "Favourites",
                Some(favorites),
                in_library && self.filter == Filter::Favorites
            )
            .on_press(Message::Filter(Filter::Favorites)),
            nav_item(
                Icon::Link,
                "Shared",
                Some(self.clips.iter().filter(|c| c.link.is_some()).count()),
                in_library && self.filter == Filter::Shared
            )
            .on_press(Message::Filter(Filter::Shared)),
            nav_item(
                Icon::Camera,
                "Screenshots",
                Some(self.shots.len()),
                in_library && self.filter == Filter::Screenshots
            )
            .on_press(Message::Filter(Filter::Screenshots)),
            nav_item(
                Icon::Record,
                "Sessions",
                Some(
                    self.clips
                        .iter()
                        .filter(|c| c.kind == library::ClipKind::Session)
                        .count()
                ),
                in_library && self.filter == Filter::Sessions
            )
            .on_press(Message::Filter(Filter::Sessions)),
            nav_item(
                Icon::Trash,
                "Recently deleted",
                Some(self.trash.len()),
                in_library && self.filter == Filter::Trash
            )
            .on_press(Message::Filter(Filter::Trash)),
            nav_item(
                Icon::Sparkles,
                "Your stats",
                None,
                in_library && self.filter == Filter::Stats
            )
            .on_press(Message::Filter(Filter::Stats)),
        ]
        .spacing(2);

        nav = nav.push(Space::new().height(14)).push(
            row![
                container(
                    text("Folders")
                        .size(12)
                        .font(SEMIBOLD)
                        .color(theme::muted())
                )
                .padding(Padding::from([4, 12]))
                .width(Fill),
                library::tip(
                    button(icon(Icon::Folder, 14.0, theme::muted()))
                        .padding(Padding::from([3, 8]))
                        .style(theme::ghost)
                        .on_press(Message::FolderDialog(None)),
                    "New folder",
                ),
            ]
            .align_y(Alignment::Center),
        );
        for (f, n) in &self.folders {
            let active = in_library && matches!(&self.filter, Filter::Folder(x) if x == f);
            nav = nav.push(
                nav_item(Icon::Folder, f, Some(*n), active)
                    .on_press(Message::Filter(Filter::Folder(f.clone()))),
            );
        }
        if !self.games.is_empty() {
            nav = nav.push(Space::new().height(14)).push(
                container(text("Games").size(12).font(SEMIBOLD).color(theme::muted()))
                    .padding(Padding::from([4, 12])),
            );
            for (g, n) in &self.games {
                let active = in_library && matches!(&self.filter, Filter::Game(f) if f == g);
                nav = nav.push(
                    nav_item(Icon::Gamepad, g, Some(*n), active)
                        .on_press(Message::Filter(Filter::Game(g.clone()))),
                );
            }
        }
        if !self.tags.is_empty() {
            nav = nav.push(Space::new().height(14)).push(
                container(text("Tags").size(12).font(SEMIBOLD).color(theme::muted()))
                    .padding(Padding::from([4, 12])),
            );
            for (t, n) in &self.tags {
                let active = in_library && matches!(&self.filter, Filter::Tag(f) if f == t);
                nav = nav.push(
                    nav_item(Icon::Tag, t, Some(*n), active)
                        .on_press(Message::Filter(Filter::Tag(t.clone()))),
                );
            }
        }

        let mut bottom = column![].spacing(10);
        if let Some(u) = &self.setup.update.available {
            bottom = bottom.push(
                button(
                    row![
                        icon(Icon::Download, 16.0, theme::accent()),
                        column![
                            text(format!("Kairo {} is out", u.version))
                                .size(13)
                                .font(SEMIBOLD),
                            text("Update in Settings").size(11).color(theme::ink_2()),
                        ]
                        .spacing(1),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                )
                .width(Fill)
                .padding(10)
                .style(|_, s| iced::widget::button::Style {
                    background: Some(
                        theme::alpha(
                            theme::accent(),
                            if matches!(s, button::Status::Hovered) {
                                0.2
                            } else {
                                0.12
                            },
                        )
                        .into(),
                    ),
                    text_color: theme::ink(),
                    border: iced::Border {
                        color: theme::alpha(theme::accent(), 0.4),
                        width: 1.0,
                        radius: 10.0.into(),
                    },
                    ..Default::default()
                })
                .on_press(Message::Page(Page::Settings)),
            );
        }
        if crate::friends::available() {
            let waiting = self.friends.unread() + self.friends.requests();
            bottom = bottom.push(
                nav_item(
                    Icon::Users,
                    "Friends",
                    Some(waiting),
                    self.page == Page::Friends,
                )
                .on_press(Message::Page(Page::Friends)),
            );
        }
        bottom = bottom.push(self.engine_card()).push(
            nav_item(
                Icon::Settings,
                "Settings",
                None,
                self.page == Page::Settings,
            )
            .on_press(Message::Page(Page::Settings)),
        );

        container(
            column![
                container(logo).padding(Padding::from([6, 8])),
                Space::new().height(18),
                scrollable(nav).height(Fill).style(theme::scroll),
                bottom
            ]
            .height(Fill),
        )
        .width(236)
        .height(Fill)
        .padding(Padding::from([18, 12]))
        .style(theme::sidebar)
        .into()
    }

    /// What's being recorded, and how much replay is ready.
    fn engine_card(&self) -> Element<'_, Message> {
        let s = self.report.as_ref().map(|r| &r.status);
        let live = s.is_some_and(|s| s.recording && !s.paused);
        let (dot, label) = match s {
            None => (theme::muted(), "Not running"),
            Some(s) if s.waiting => (theme::muted(), "Waiting for a game"),
            Some(s) if s.paused => (theme::GOLD, "Paused"),
            Some(s) if s.recording => (theme::GREEN, "Recording"),
            Some(s) if s.error.is_some() => (theme::accent(), "Can't record"),
            Some(_) => (theme::muted(), "Starting"),
        };
        let detail = match s {
            Some(s) if s.recording => {
                let p = s.size.map(|(_, h)| format!("{h}p ")).unwrap_or_default();
                format!(
                    "{p}{} fps, {}",
                    s.fps,
                    s.encoder.clone().unwrap_or_default()
                )
            }
            Some(s) => s.error.clone().unwrap_or_else(|| "Getting ready...".into()),
            None => "Kairo isn't recording.".into(),
        };
        let clip_len = self.settings.clip_seconds as f32;
        let held = s.map_or(0.0, |s| s.buffer_seconds as f32);
        let fill = if live {
            (held / clip_len).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let bar = iced::widget::progress_bar(0.0..=1.0, fill)
            .girth(4)
            .style(move |_| iced::widget::progress_bar::Style {
                background: theme::raised().into(),
                bar: if fill >= 1.0 {
                    theme::GREEN
                } else {
                    theme::alpha(theme::GREEN, 0.75)
                }
                .into(),
                border: iced::Border {
                    radius: 2.0.into(),
                    ..Default::default()
                },
            });
        let action: Element<'_, Message> = match s {
            Some(st) => button(icon(
                if st.paused { Icon::Play } else { Icon::Pause },
                15.0,
                theme::ink_2(),
            ))
            .padding(5)
            .style(theme::ghost)
            .on_press(Message::TogglePause)
            .into(),
            None => button(text("Start").size(12).font(SEMIBOLD))
                .padding(Padding::from([4, 10]))
                .style(theme::secondary)
                .on_press(Message::StartRecorder)
                .into(),
        };
        let mut c = column![
            row![
                container(Space::new().width(8).height(8)).style(move |_| container::Style {
                    background: Some(dot.into()),
                    border: iced::Border {
                        radius: 4.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                text(label).font(SEMIBOLD).size(13),
                space::horizontal(),
                action,
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            text(detail).size(12).color(theme::muted()),
        ]
        .spacing(7);
        if let Some(st) = s
            && (st.voice.is_some() || st.voice_error.is_some())
        {
            let (color, line) = match (&st.voice, &st.voice_error) {
                (Some(p), _) => (theme::ink_2(), format!("Listening for \"{p}\"")),
                (None, Some(e)) => (theme::accent(), e.clone()),
                _ => (theme::muted(), String::new()),
            };
            c = c.push(
                row![
                    icon(Icon::Mic, 13.0, color),
                    text(line).size(12).color(color)
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        }
        if let Some(ses) = s.and_then(|st| st.session.as_ref()) {
            c = c.push(
                row![
                    icon(Icon::Record, 13.0, theme::accent()),
                    text(format!("Session {}", clock(ses.seconds)))
                        .size(12)
                        .font(SEMIBOLD)
                        .color(theme::ink()),
                    space::horizontal(),
                    text(human_size((ses.mb * 1024.0 * 1024.0) as u64))
                        .size(11)
                        .color(theme::muted()),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        }
        if let Some(st) = s
            && st.marks > 0
        {
            let key = self
                .report
                .as_ref()
                .and_then(|r| r.marker_hotkey.clone())
                .unwrap_or_default();
            c = c.push(
                row![
                    icon(Icon::Flag, 13.0, theme::GOLD),
                    text(match st.marks {
                        1 => "1 moment marked".to_string(),
                        n => format!("{n} moments marked"),
                    })
                    .size(12)
                    .color(theme::ink_2()),
                    space::horizontal(),
                    text(key).size(11).color(theme::muted()),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        }
        if live {
            c = c.push(bar).push(
                text(if held >= clip_len {
                    format!(
                        "Ready: the last {} is saved on {}",
                        seconds_label(self.settings.clip_seconds),
                        self.hotkey()
                    )
                } else {
                    format!(
                        "{} of {} ready",
                        duration(Some(held as f64)),
                        duration(Some(clip_len as f64))
                    )
                })
                .size(12)
                .color(theme::ink_2()),
            );
        }
        container(c).padding(12).style(theme::panel).into()
    }

    // --- Top bar ----------------------------------------------------------------------------

    fn top_bar(&self) -> Element<'_, Message> {
        let title = match &self.page {
            Page::Settings => "Settings".to_string(),
            Page::Friends => "Friends".to_string(),
            _ => match &self.filter {
                Filter::All => "All clips".into(),
                Filter::Favorites => "Favourites".into(),
                Filter::Shared => "Shared".into(),
                Filter::Screenshots => "Screenshots".into(),
                Filter::Sessions => "Sessions".into(),
                Filter::Game(g) => g.clone(),
                Filter::Tag(t) => t.clone(),
                Filter::Trash => "Recently deleted".into(),
                Filter::Stats => "Your stats".into(),
                Filter::Folder(f) => f.clone(),
            },
        };
        let live = self
            .report
            .as_ref()
            .is_some_and(|r| r.status.recording && !r.status.paused);
        let save_main = button(
            row![
                text(format!(
                    "Save last {}",
                    seconds_label(self.settings.clip_seconds)
                ))
                .font(SEMIBOLD)
                .size(14),
                widgets::keycap(&self.hotkey()),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([9, 14]))
        .style(|t, s| {
            let mut st = theme::primary(t, s);
            st.border.radius = iced::border::left(9.0);
            st
        })
        .on_press_maybe(live.then_some(Message::SaveClip));
        let save_more = library::tip(
            button(
                container(icon(Icon::ChevronDown, 16.0, theme::on_accent()))
                    .center(iced::Length::Fixed(20.0)),
            )
            .padding(Padding::from([9, 8]))
            .style(|t, s| {
                let mut st = theme::primary(t, s);
                st.border.radius = iced::border::right(9.0);
                st
            })
            .on_press_maybe(live.then_some(Message::SaveMenu(!self.save_menu))),
            "Save a shorter clip",
        );
        let save = row![save_main, save_more].spacing(1);
        let mut r = row![text(title).font(BOLD).size(24), space::horizontal()]
            .spacing(12)
            .align_y(Alignment::Center);
        if self.page == Page::Library {
            r = r.push(
                container(
                    row![
                        icon(Icon::Search, 15.0, theme::muted()),
                        text_input("Search clips", &self.search)
                            .on_input(Message::Search)
                            .padding(0)
                            .size(14)
                            .style(|t, s| {
                                let mut st = theme::input(t, s);
                                st.background = iced::Color::TRANSPARENT.into();
                                st.border.width = 0.0;
                                st
                            }),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([8, 12]))
                .width(260)
                .style(|_| container::Style {
                    background: Some(theme::surface().into()),
                    border: iced::Border {
                        color: theme::edge(),
                        width: 1.0,
                        radius: 9.0.into(),
                    },
                    ..Default::default()
                }),
            );
        }
        let shot_key = self
            .report
            .as_ref()
            .and_then(|r| r.screenshot_hotkey.clone())
            .unwrap_or_else(|| self.settings.screenshot_hotkey.clone());
        let shot_tip: &'static str = if shot_key.is_empty() {
            "Take a screenshot"
        } else {
            "Take a screenshot (the screenshot shortcut works in games)"
        };
        let session = self.report.as_ref().and_then(|r| r.status.session.clone());
        let session_button: Element<'_, Message> = match &session {
            Some(s) => button(
                row![
                    icon(Icon::Record, 16.0, theme::on_accent()),
                    text(format!("Stop  {}", clock(s.seconds)))
                        .font(SEMIBOLD)
                        .size(13)
                        .color(theme::on_accent()),
                ]
                .spacing(7)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([9, 12]))
            .style(theme::primary)
            .on_press(Message::ToggleSession)
            .into(),
            None => library::tip(
                button(
                    container(icon(Icon::Record, 17.0, theme::ink()))
                        .center(iced::Length::Fixed(20.0)),
                )
                .padding(9)
                .style(theme::secondary)
                .on_press_maybe(live.then_some(Message::ToggleSession)),
                "Record the whole session (saves and marks become bookmarks)",
            ),
        };
        r = r.push(session_button);
        r = r.push(library::tip(
            button(
                container(icon(Icon::Camera, 17.0, theme::ink())).center(iced::Length::Fixed(20.0)),
            )
            .padding(9)
            .style(theme::secondary)
            .on_press_maybe(live.then_some(Message::TakeScreenshot)),
            shot_tip,
        ));
        r = r.push(save);
        container(r)
            .padding(Padding {
                top: 20.0,
                right: 28.0,
                bottom: 14.0,
                left: 28.0,
            })
            .into()
    }
}

fn nav_item<'a>(
    i: Icon,
    label: &str,
    count: Option<usize>,
    active: bool,
) -> iced::widget::Button<'a, Message> {
    let color = if active { theme::ink() } else { theme::ink_2() };
    let mut r = row![
        icon(
            i,
            17.0,
            if active {
                theme::accent()
            } else {
                theme::muted()
            }
        ),
        container(
            text(label.to_string())
                .size(14)
                .font(MEDIUM)
                .color(color)
                .wrapping(text::Wrapping::None)
        )
        .width(Fill)
        .clip(true),
    ]
    .spacing(11)
    .align_y(Alignment::Center);
    if let Some(n) = count.filter(|n| *n > 0) {
        r = r.push(text(n.to_string()).size(12).color(theme::muted()));
    }
    button(r)
        .padding(Padding::from([8, 12]))
        .width(Fill)
        .style(theme::nav(active))
}

/// How long pages, dialogs and notices take to ease in.
const PAGE_IN_MS: u64 = 260;
const DIALOG_IN_MS: u64 = 220;
const NOTICE_IN_MS: u64 = 280;

/// How long the pointer rests on a card before its preview plays, and how fast it plays.
const HOVER_PLAY_AFTER: Duration = Duration::from_millis(350);
const HOVER_FRAME: Duration = Duration::from_millis(110);

/// Small keyframe pictures of a clip, for the hover preview.
fn preview_strip(clip: &Path) -> Vec<image::Handle> {
    let Ok(ff) = crate::ffmpeg::path() else {
        return Vec::new();
    };
    let Ok(info) = crate::ffmpeg::probe_clip(&ff, clip) else {
        return Vec::new();
    };
    let h = 180u32;
    let w = ((h as f64 * info.width as f64 / info.height.max(1) as f64).round() as u32).max(2) & !1;
    editor::filmstrip(&ff, clip, (w, h), info.duration)
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// "1:02:03" or "12:34".
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub fn seconds_label(s: u32) -> String {
    if s >= 60 && s.is_multiple_of(60) {
        format!("{} min", s / 60)
    } else {
        format!("{s} s")
    }
}

pub fn duration(secs: Option<f64>) -> String {
    match secs {
        Some(s) if s.is_finite() => {
            let s = s.max(0.0).round() as u64;
            format!("{}:{:02}", s / 60, s % 60)
        }
        _ => "--:--".into(),
    }
}

pub fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).max(1.0))
    }
}

/// "Just now", "12 min ago", "15:41".
pub fn when(t: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(t).map_or(0, |d| d.as_secs());
    if secs < 60 {
        "Just now".into()
    } else if secs < 3600 {
        format!("{} min ago", secs / 60)
    } else {
        let local: chrono::DateTime<chrono::Local> = t.into();
        local.format("%H:%M").to_string()
    }
}

/// "Today", "Yesterday", or "Saturday, Oct 17".
pub fn day_label(t: SystemTime, now: SystemTime) -> String {
    let a: chrono::DateTime<chrono::Local> = t.into();
    let b: chrono::DateTime<chrono::Local> = now.into();
    match (b.date_naive() - a.date_naive()).num_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => a.format("%A, %b %-d").to_string(),
    }
}

// --- Another process asking the open window to show a clip -------------------------------

fn open_request_path() -> PathBuf {
    config::config_dir().join("open.request")
}

/// A folder to move clips to, or none, as a pick list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderPick(pub Option<String>);

impl std::fmt::Display for FolderPick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_deref().unwrap_or("No folder"))
    }
}

/// Asks the open window to install the update (instead of a clip to open).
const UPDATE_REQUEST: &str = "kairo:update";
/// Asks the open window to show Friends.
const FRIENDS_REQUEST: &str = "kairo:friends";

fn take_open_request() -> Option<String> {
    let p = open_request_path();
    let text = std::fs::read_to_string(&p).ok()?;
    let _ = std::fs::remove_file(&p);
    Some(text.trim().to_string())
}

/// `--ui-self-test`: checks what an export made (the test trims 0.5 to 2.5 s of a 1280x720 clip).
/// A montage must be one clip about as long as its parts, at their size.
fn check_montage(path: &Path, want: f64) -> Result<(), String> {
    let ff = crate::ffmpeg::path()?;
    let info = crate::ffmpeg::probe_clip(&ff, path)?;
    println!(
        "ui self-test: montage is {}x{}, {:.2} s (parts {want:.2} s), {} sound track(s)",
        info.width,
        info.height,
        info.duration,
        info.audio_tracks.len()
    );
    if (info.width, info.height) != (1280, 720) || info.audio_tracks.is_empty() {
        return Err(format!("the montage is {}x{}", info.width, info.height));
    }
    if (info.duration - want).abs() > 0.5 {
        return Err(format!(
            "the montage lasts {:.2} s, not about {want:.2} s",
            info.duration
        ));
    }
    Ok(())
}

fn check_export(kind: editor::ExportKind, path: &Path) -> Result<(), String> {
    let what = format!("{kind:?} export {}", path.display());
    if kind == editor::ExportKind::Gif {
        let head = std::fs::read(path).map_err(|e| format!("{what}: {e}"))?;
        return if head.starts_with(b"GIF89a") && head.len() > 10_000 {
            println!("ui self-test: GIF, {} bytes", head.len());
            Ok(())
        } else {
            Err(format!("{what} isn't a GIF"))
        };
    }
    let ff = crate::ffmpeg::path()?;
    let info = crate::ffmpeg::probe_clip(&ff, path)?;
    let (want_size, want_secs) = match kind {
        editor::ExportKind::Vertical => (Some((1080, 1920)), 2.0),
        editor::ExportKind::Square => (Some((1080, 1080)), 2.0),
        editor::ExportKind::Portrait => (Some((1080, 1350)), 2.0),
        editor::ExportKind::Slow(f) => (Some((1280, 720)), 2.0 * f as f64),
        _ => (None, 2.0),
    };
    println!(
        "ui self-test: {kind:?} is {}x{}, {:.2} s",
        info.width, info.height, info.duration
    );
    if want_size.is_some_and(|s| s != (info.width, info.height)) {
        return Err(format!(
            "{what} is {}x{}, not {want_size:?}",
            info.width, info.height
        ));
    }
    if (info.duration - want_secs).abs() > 0.35 {
        return Err(format!(
            "{what} lasts {:.2} s, not about {want_secs} s",
            info.duration
        ));
    }
    Ok(())
}

/// `--ui-self-test`: the real window, hidden, on a short generated clip in a temporary folder.
/// It must show the first frame and the filmstrip, play, and save a trimmed copy.
pub fn self_test() -> Result<(), String> {
    let home = std::env::temp_dir().join(format!("kairo-uitest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let clips = home.join("Clips");
    std::fs::create_dir_all(&clips).map_err(|e| e.to_string())?;
    // Safe: nothing else runs yet.
    unsafe {
        std::env::set_var("KAIRO_HOME", &home);
        std::env::set_var("KAIRO_NO_UPDATE_CHECK", "1");
        std::env::set_var("KAIRO_NO_WELCOME", "1");
    }
    let ff = crate::ffmpeg::path()?;
    let clip = clips.join("Self test 2026-01-01 00-00-00.mp4");
    let mut make: Vec<String> = [
        "-y",
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=1280x720:rate=30",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440",
        "-t",
        "4",
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "30",
        "-c:a",
        "aac",
        "-movflags",
        "+faststart",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    make.push(clip.display().to_string());
    crate::ffmpeg::run(&ff, &make, Duration::from_secs(60))?;
    let app = iced::application(
        move || {
            let (settings, _) = config::load_or_create(&config::default_path());
            let (mut app, tasks) = App::new(settings);
            app.test = Some(SelfTest::Loading(Instant::now()));
            let open = app.open_editor(clip.clone());
            (app, Task::batch([tasks, open]))
        },
        App::update,
        App::view,
    )
    .theme(App::theme)
    .subscription(App::subscription)
    .default_font(theme::FONT)
    .window(window::Settings {
        size: Size::new(1100.0, 720.0),
        visible: false,
        ..Default::default()
    });
    let ran = app.run().map_err(|e| e.to_string());
    let result = TEST_RESULT.lock().unwrap().take();
    let _ = std::fs::remove_dir_all(&home);
    ran?;
    match result {
        Some(Ok(what)) => {
            println!("ui self-test passed: {what}");
            Ok(())
        }
        Some(Err(e)) => Err(e),
        None => Err("the window closed before the test finished".into()),
    }
}

/// Opens the window (on `clip` in the editor), or asks the open one to come forward.
/// `wait`: after an update, the old window may still be closing.
pub fn run(clip: Option<PathBuf>, wait: bool, update: bool, friends: bool) -> Result<(), String> {
    let lock = if wait {
        single_instance::acquire_patiently(single_instance::WINDOW)?
    } else {
        single_instance::acquire(single_instance::WINDOW)?
    };
    let Some(lock) = lock else {
        let text = if update {
            UPDATE_REQUEST.to_string()
        } else if friends {
            FRIENDS_REQUEST.to_string()
        } else {
            clip.map(|p| p.display().to_string()).unwrap_or_default()
        };
        std::fs::write(open_request_path(), text).map_err(|e| e.to_string())?;
        println!("The Kairo window is already open.");
        return Ok(());
    };
    let _ = std::fs::remove_file(open_request_path());
    if let Err(e) = daemon::ensure_running() {
        crate::log::warning!("{e}");
    }
    let start = Cell::new(Some((lock, clip)));
    let mut app = iced::application(
        move || {
            let (lock, clip) = start.take().expect("window booted once");
            App::boot(Some(lock), clip, update, friends)
        },
        App::update,
        App::view,
    )
    .title(App::title)
    .theme(App::theme)
    .subscription(App::subscription)
    .default_font(theme::FONT)
    .window(window::Settings {
        size: Size::new(1240.0, 800.0),
        min_size: Some(Size::new(840.0, 560.0)),
        position: window::Position::Centered,
        icon: window::icon::from_rgba(kairo::art::app_icon(64), 64, 64).ok(),
        ..Default::default()
    });
    for f in theme::FONTS {
        app = app.font(f);
    }
    app.run().map_err(|e| e.to_string())
}
