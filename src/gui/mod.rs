//! Rewindr's window, built with iced: the clip library, the editor and the settings. The
//! "clip saved" card is its own small window (`toast`).

mod editor;
pub mod icons;
mod library;
mod settings;
pub mod theme;
pub mod toast;
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
    Space, button, column, container, image, qr_code, row, scrollable, space, stack, text,
    text_input,
};
use iced::{Alignment, Element, Fill, Padding, Size, Subscription, Task, Theme, window};

use rewindr::config::{self, Settings};
use rewindr::library::{self as lib, Library, Link};

use crate::daemon::{self, Report, Request};
use crate::share;
use crate::single_instance::{self, InstanceLock};
use icons::{Icon, icon};
pub use library::{ClipItem, ShotItem, load_clips, load_shots};
use theme::{BOLD, MEDIUM, MUTED, SEMIBOLD, TEXT, TEXT_2};
use widgets::dialog;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    All,
    Favorites,
    Shared,
    Screenshots,
    Game(String),
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
}

enum Dialog {
    Rename {
        path: PathBuf,
        name: String,
        error: Option<String>,
    },
    Delete(PathBuf),
    /// A share link, to copy or scan.
    Link {
        url: String,
        expires: Option<u64>,
        qr: Option<qr_code::Data>,
    },
}

struct Notice {
    text: String,
    ok: bool,
    at: Instant,
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
    /// Keyframe pictures for hover previews, most recent last.
    strips: HashMap<PathBuf, Vec<image::Handle>>,
    strip_order: Vec<PathBuf>,
    strip_loading: HashSet<PathBuf>,
    uploads: Vec<share::Upload>,
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
    test: Option<SelfTest>,
    /// Exports `--ui-self-test` still has to make, after the trimmed copy.
    test_exports: Vec<editor::ExportKind>,
    test_current: Option<editor::ExportKind>,
    _lock: Option<InstanceLock>,
}

/// `--ui-self-test`: where the run through the real window is.
enum SelfTest {
    Loading(Instant),
    Playing(Instant),
    Exporting(Instant),
}

static TEST_RESULT: std::sync::Mutex<Option<Result<String, String>>> = std::sync::Mutex::new(None);

/// What a look at the library's folders found.
#[derive(Debug, Clone)]
pub struct Scan {
    sig: u64,
    clips: Vec<ClipItem>,
    shots: Vec<ShotItem>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    Report(Option<Box<Report>>),
    Clips(Vec<ClipItem>),
    Shots(Vec<ShotItem>),
    /// The folders changed: their new signature and contents.
    Rescanned(Option<Box<Scan>>),
    Scrub(PathBuf, f32),
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
                strips: HashMap::new(),
                strip_order: Vec::new(),
                strip_loading: HashSet::new(),
                uploads: Vec::new(),
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
                test: None,
                test_exports: Vec::new(),
                test_current: None,
                _lock: None,
            },
            tasks.map(Message::Settings),
        )
    }

    fn boot(lock: Option<InstanceLock>, clip: Option<PathBuf>) -> (Self, Task<Message>) {
        let (settings, _) = config::load_or_create(&config::default_path());
        let (mut app, tasks) = App::new(settings);
        app._lock = lock;
        if std::env::var("REWINDR_TAB").is_ok_and(|t| t == "settings") {
            app.page = Page::Settings;
        }
        let mut all = vec![tasks, app.reload(), Task::done(Message::Tick)];
        if let Some(c) = clip {
            all.push(app.open_editor(c));
        }
        (app, Task::batch(all))
    }

    fn reload(&self) -> Task<Message> {
        let dir = self.settings.save_dir();
        let shots = self.settings.screenshot_dir();
        Task::batch([
            Task::perform(background(move || load_clips(&dir)), Message::Clips),
            Task::perform(background(move || load_shots(&shots)), Message::Shots),
        ])
    }

    /// Reloads the library if anything in its folders changed since the last look.
    fn refresh(&self) -> Task<Message> {
        let dir = self.settings.save_dir();
        let shots = self.settings.screenshot_dir();
        let last = self.scan_sig;
        Task::perform(
            background(move || {
                let thumbs = crate::save::thumbs_dir();
                let sig = library::folder_signature(
                    &[&dir, &shots, &thumbs],
                    &rewindr::library::Library::path(),
                );
                (Some(sig) != last).then(|| {
                    Box::new(Scan {
                        sig,
                        clips: load_clips(&dir),
                        shots: load_shots(&shots),
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
        let mut i = 0;
        while i < self.uploads.len() {
            let Some(result) = self.uploads[i].take() else {
                i += 1;
                continue;
            };
            let up = self.uploads.remove(i);
            match result {
                Ok(link) => {
                    for c in self.clips.iter_mut().filter(|c| c.path == up.clip) {
                        c.link = Some(link.clone());
                    }
                    for s in self.shots.iter_mut().filter(|s| s.path == up.clip) {
                        s.link = Some(link.clone());
                    }
                    self.note(
                        "Link copied. Paste it anywhere: friends can watch it in their browser.",
                        true,
                    );
                    tasks.push(self.show_link(&link));
                    tasks.push(self.reload());
                }
                Err(e) if e == "Cancelled" => self.note("Upload cancelled.", true),
                Err(e) => self.note(e, false),
            }
        }
        Task::batch(tasks)
    }

    fn note(&mut self, text: impl Into<String>, ok: bool) {
        self.notice = Some(Notice {
            text: text.into(),
            ok,
            at: Instant::now(),
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
                    if let (Some(kind), Some(path)) = (self.test_current, made)
                        && let Err(e) = check_export(kind, &path)
                    {
                        return self.finish_test(Err(e));
                    }
                    let next = self.test_exports.pop();
                    self.test_current = next;
                    match (next, &mut self.editor) {
                        (Some(k), Some(ed)) => {
                            println!("ui self-test: exported; now {k:?}");
                            let effect = ed.update(editor::Msg::Export(k));
                            return self.editor_effect(effect);
                        }
                        _ => {
                            return self.finish_test(Ok(
                                "played, then saved a trimmed copy, a GIF, a vertical copy and slow motion".into(),
                            ));
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
            editor::Effect::AskDelete(p) => self.dialog = Some(Dialog::Delete(p)),
            editor::Effect::Back => return self.update(Message::Back),
            editor::Effect::Share { file, clip, range } => {
                self.start_upload(file, clip, range);
                return self.reload();
            }
            editor::Effect::CancelUpload(p) => return self.update(Message::CancelUpload(p)),
            editor::Effect::CopyText(t) => return self.update(Message::CopyText(t)),
            editor::Effect::OpenUrl(u) => return self.update(Message::OpenUrl(u)),
            editor::Effect::ShowLink(p) => return self.update(Message::ShowLink(p)),
        }
        Task::none()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
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
                self.loaded = true;
            }
            Message::Shots(s) => self.shots = s,
            Message::Rescanned(found) => {
                if let Some(b) = found {
                    let Scan { sig, clips, shots } = *b;
                    self.scan_sig = Some(sig);
                    self.clips = clips;
                    self.shots = shots;
                    self.loaded = true;
                }
            }
            Message::Scrub(p, f) => {
                let start = !self.strips.contains_key(&p) && self.strip_loading.insert(p.clone());
                self.scrub = Some((p.clone(), f));
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
                let _ = std::fs::rename(crate::save::thumb_for(path), crate::save::thumb_for(&to));
                let mut l = Library::load();
                l.renamed(path, &to);
                let _ = l.save();
                self.dialog = None;
                self.note("Renamed.", true);
                return self.reload();
            }
            Message::AskDelete(p) => self.dialog = Some(Dialog::Delete(p)),
            Message::DeleteConfirm => {
                let Some(Dialog::Delete(p)) = self.dialog.take() else {
                    return Task::none();
                };
                if let Some(e) = &mut self.editor
                    && e.clip == p
                {
                    e.release();
                }
                match std::fs::remove_file(&p) {
                    Ok(()) => {
                        let _ = std::fs::remove_file(crate::save::thumb_for(&p));
                        let mut l = Library::load();
                        if l.is_favorite(&p)
                            || l.links
                                .contains_key(&*p.file_name().unwrap_or_default().to_string_lossy())
                        {
                            l.set_favorite(&p, false);
                            l.set_link(&p, None);
                            let _ = l.save();
                        }
                        self.clips.retain(|c| c.path != p);
                        self.shots.retain(|s| s.path != p);
                        if self.editor.as_ref().is_some_and(|e| e.clip == p) {
                            self.editor = None;
                            self.page = Page::Library;
                        }
                        self.note("Deleted.", true);
                    }
                    Err(e) => self.note(format!("Couldn't delete: {e}"), false),
                }
            }
            Message::CloseDialog => self.dialog = None,
            Message::Page(p) => {
                self.page = p;
                if p != Page::Editor {
                    return self.close_editor();
                }
            }
            Message::SaveClip => {
                if let Err(e) = daemon::request(Request::Save) {
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
            Message::Settings(m) => {
                let (task, effect) = self.setup.update(&mut self.settings, m);
                let task = task.map(Message::Settings);
                return match effect {
                    settings::Effect::None => task,
                    settings::Effect::Changed => {
                        self.changed_at = Some(Instant::now());
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
                    Dialog::Link { .. } => Message::CloseDialog,
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
            && n.at.elapsed() > Duration::from_secs(if n.ok { 4 } else { 9 })
        {
            self.notice = None;
        }
        if let Some(text) = take_open_request() {
            if let Some(id) = self.window {
                tasks.push(window::minimize(id, false));
                tasks.push(window::gain_focus(id));
            }
            if !text.is_empty() {
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
        if !self.uploads.is_empty() {
            subs.push(iced::time::every(Duration::from_millis(150)).map(|_| Message::UploadTick));
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
            (Page::Editor, Some(e)) => format!("{} - Rewindr", e.name),
            _ => "Rewindr".into(),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let page: Element<'_, Message> = match (self.page, &self.editor) {
            (Page::Editor, Some(e)) => {
                let item = self.clips.iter().find(|c| c.path == e.clip);
                let target = e.share_target();
                let share = editor::ShareView {
                    progress: self.upload_progress(&target),
                    link: self.link_of(&target).cloned(),
                };
                column![
                    Space::new().height(18),
                    e.view(item, share).map(Message::Editor)
                ]
                .into()
            }
            (Page::Settings, _) => {
                let used = self.clips.iter().map(|c| c.size).sum();
                column![
                    self.top_bar(),
                    self.setup
                        .view(&self.settings, self.report.as_ref(), used)
                        .map(Message::Settings)
                ]
                .into()
            }
            _ => column![self.top_bar(), self.library_view()].into(),
        };
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
                                if n.ok { theme::GREEN } else { theme::ACCENT }
                            ),
                            text(n.text.clone()).size(13).font(MEDIUM),
                        ]
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
                .padding(22),
            );
        }
        if let Some(d) = &self.dialog {
            layers = layers.push(self.dialog_view(d));
        }
        layers.into()
    }

    fn dialog_view<'a>(&'a self, d: &'a Dialog) -> Element<'a, Message> {
        match d {
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
                    body = body.push(text(e.clone()).size(12).color(theme::ACCENT));
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
                        .color(TEXT_2),
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
                                    .color(TEXT_2),
                                Space::new().height(6),
                                text(lasts).size(12).color(MUTED),
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
                                icon(Icon::Globe, 15.0, TEXT_2),
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
                )
            }
            Dialog::Delete(p) => dialog(
                if p.extension().is_some_and(|e| e != "mp4") {
                    "Delete this screenshot?"
                } else {
                    "Delete this clip?"
                },
                text(format!(
                    "\"{}\" will be deleted from your clips folder. This can't be undone.",
                    stem(p)
                ))
                .size(14)
                .color(TEXT_2),
                row![
                    button(text("Keep it").size(13).font(MEDIUM))
                        .padding(Padding::from([8, 14]))
                        .style(theme::ghost)
                        .on_press(Message::CloseDialog),
                    button(
                        row![
                            icon(Icon::Trash, 15.0, iced::Color::WHITE),
                            text("Delete").size(13).font(SEMIBOLD)
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
            text("Rewindr").font(BOLD).size(19),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let favorites = self.clips.iter().filter(|c| c.favorite).count();
        let in_library = self.page != Page::Settings;
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
        ]
        .spacing(2);

        let mut games: Vec<(String, usize)> = Vec::new();
        for c in &self.clips {
            match games.iter_mut().find(|(g, _)| *g == c.game) {
                Some((_, n)) => *n += 1,
                None => games.push((c.game.clone(), 1)),
            }
        }
        games.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        if !games.is_empty() {
            nav = nav.push(Space::new().height(14)).push(
                container(text("Games").size(12).font(SEMIBOLD).color(MUTED))
                    .padding(Padding::from([4, 12])),
            );
            for (g, n) in games {
                let active = in_library && self.filter == Filter::Game(g.clone());
                nav = nav.push(
                    nav_item(Icon::Gamepad, &g, Some(n), active)
                        .on_press(Message::Filter(Filter::Game(g))),
                );
            }
        }

        let mut bottom = column![].spacing(10);
        if let Some(u) = &self.setup.update.available {
            bottom = bottom.push(
                button(
                    row![
                        icon(Icon::Download, 16.0, theme::ACCENT),
                        column![
                            text(format!("Rewindr {} is out", u.version))
                                .size(13)
                                .font(SEMIBOLD),
                            text("Update in Settings").size(11).color(TEXT_2),
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
                            theme::ACCENT,
                            if matches!(s, button::Status::Hovered) {
                                0.2
                            } else {
                                0.12
                            },
                        )
                        .into(),
                    ),
                    text_color: TEXT,
                    border: iced::Border {
                        color: theme::alpha(theme::ACCENT, 0.4),
                        width: 1.0,
                        radius: 10.0.into(),
                    },
                    ..Default::default()
                })
                .on_press(Message::Page(Page::Settings)),
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
            None => (MUTED, "Not running"),
            Some(s) if s.paused => (theme::GOLD, "Paused"),
            Some(s) if s.recording => (theme::GREEN, "Recording"),
            Some(s) if s.error.is_some() => (theme::ACCENT, "Can't record"),
            Some(_) => (MUTED, "Starting"),
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
            None => "Rewindr isn't recording.".into(),
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
                background: theme::RAISED.into(),
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
                TEXT_2,
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
            text(detail).size(12).color(MUTED),
        ]
        .spacing(7);
        if let Some(st) = s
            && (st.voice.is_some() || st.voice_error.is_some())
        {
            let (color, line) = match (&st.voice, &st.voice_error) {
                (Some(p), _) => (TEXT_2, format!("Listening for \"{p}\"")),
                (None, Some(e)) => (theme::ACCENT, e.clone()),
                _ => (MUTED, String::new()),
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
                .color(TEXT_2),
            );
        }
        container(c).padding(12).style(theme::panel).into()
    }

    // --- Top bar ----------------------------------------------------------------------------

    fn top_bar(&self) -> Element<'_, Message> {
        let title = match &self.page {
            Page::Settings => "Settings".to_string(),
            _ => match &self.filter {
                Filter::All => "All clips".into(),
                Filter::Favorites => "Favourites".into(),
                Filter::Shared => "Shared".into(),
                Filter::Screenshots => "Screenshots".into(),
                Filter::Game(g) => g.clone(),
            },
        };
        let live = self
            .report
            .as_ref()
            .is_some_and(|r| r.status.recording && !r.status.paused);
        let save = button(
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
        .style(theme::primary)
        .on_press_maybe(live.then_some(Message::SaveClip));
        let mut r = row![text(title).font(BOLD).size(24), space::horizontal()]
            .spacing(12)
            .align_y(Alignment::Center);
        if self.page == Page::Library {
            r = r.push(
                container(
                    row![
                        icon(Icon::Search, 15.0, MUTED),
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
                    background: Some(theme::SURFACE.into()),
                    border: iced::Border {
                        color: theme::BORDER,
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
        r = r.push(library::tip(
            button(container(icon(Icon::Camera, 17.0, TEXT)).center(iced::Length::Fixed(20.0)))
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
    let color = if active { TEXT } else { TEXT_2 };
    let mut r = row![
        icon(i, 17.0, if active { theme::ACCENT } else { MUTED }),
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
    if let Some(n) = count {
        r = r.push(text(n.to_string()).size(12).color(MUTED));
    }
    button(r)
        .padding(Padding::from([8, 12]))
        .width(Fill)
        .style(theme::nav(active))
}

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
    editor::filmstrip(&ff, clip, (w, h))
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
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

fn take_open_request() -> Option<String> {
    let p = open_request_path();
    let text = std::fs::read_to_string(&p).ok()?;
    let _ = std::fs::remove_file(&p);
    Some(text.trim().to_string())
}

/// `--ui-self-test`: checks what an export made (the test trims 0.5 to 2.5 s of a 1280x720 clip).
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
    let home = std::env::temp_dir().join(format!("rewindr-uitest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let clips = home.join("Clips");
    std::fs::create_dir_all(&clips).map_err(|e| e.to_string())?;
    // Safe: nothing else runs yet.
    unsafe {
        std::env::set_var("REWINDR_HOME", &home);
        std::env::set_var("REWINDR_NO_UPDATE_CHECK", "1");
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
pub fn run(clip: Option<PathBuf>, wait: bool) -> Result<(), String> {
    let lock = if wait {
        single_instance::acquire_patiently(single_instance::WINDOW)?
    } else {
        single_instance::acquire(single_instance::WINDOW)?
    };
    let Some(lock) = lock else {
        let text = clip.map(|p| p.display().to_string()).unwrap_or_default();
        std::fs::write(open_request_path(), text).map_err(|e| e.to_string())?;
        println!("The Rewindr window is already open.");
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
            App::boot(Some(lock), clip)
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
        icon: window::icon::from_rgba(rewindr::art::app_icon(64), 64, 64).ok(),
        ..Default::default()
    });
    for f in theme::FONTS {
        app = app.font(f);
    }
    app.run().map_err(|e| e.to_string())
}
