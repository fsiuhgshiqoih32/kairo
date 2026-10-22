//! The editor page: play the clip with sound, scrub a filmstrip timeline, trim it, and save a
//! trimmed copy or a copy small enough for Discord.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use iced::keyboard::{self, Key, key::Named};
use iced::mouse;
use iced::widget::canvas::{self, Action, Canvas, Frame, Geometry, Path as Shape, Stroke};
use iced::widget::{
    Space, button, column, container, image, mouse_area, pick_list, progress_bar, row, scrollable,
    slider, space, stack, text, text_input, toggler,
};
use iced::{
    Alignment, Color, ContentFit, Element, Fill, Font, Length, Padding, Point, Rectangle, Renderer,
    Size, Task, Theme,
};

use kairo::args::Encoder;
use kairo::library;
use kairo::media::{self, Info, format_time};
use kairo::overlays::{self, ANIMS, COLORS, Item};

use super::icons::{Icon, icon, icon_filled};
use super::library::{ClipItem, tip};
use super::theme::{self, BOLD, MEDIUM, SEMIBOLD};
use super::widgets::{divider, hint_key, icon_button, icon_only, panel, segmented};
use super::{background, human_size};
use crate::export::{self, Job, Kind};
use crate::player::Player;
use crate::{ffmpeg, platform};

/// Frames are decoded for playback at most this big: over twice the detail of 720p, and a 120 fps
/// clip still decodes in time (measured: about 170 fps at 1080p, but 86 at 1440p).
const PLAYBACK_MAX: (u32, u32) = (1920, 1080);
/// A frame shown still (a clip opened, seeked to or paused on) is decoded at the clip's own size,
/// up to this: one frame takes about as long at any size, and a still is when detail is looked at.
const STILL_MAX: (u32, u32) = (3840, 2160);
/// Height of the filmstrip pictures, in pixels.
const STRIP_H: u32 = 60;
/// "Mark" with its key, for the Moments panel.
fn mark_button<'a>() -> Element<'a, Msg> {
    button(
        row![
            icon(Icon::Flag, 13.0, theme::GOLD),
            text("Mark").size(12).font(MEDIUM),
            hint_key("M")
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([6, 9]))
    .style(theme::secondary)
    .on_press(Msg::Mark)
    .into()
}

/// Seconds kept either side of a mark by "Keep 10 s around it".
const MARK_SPAN: f64 = 5.0;
const TIMELINE_H: f32 = 78.0;
const HANDLE_W: f32 = 12.0;
/// Room above and below the filmstrip for the playhead.
const EDGE: f32 = 7.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    Seek,
    In,
    Out,
}

/// A sound track to play, for the pick list.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    index: usize,
    name: String,
}

impl std::fmt::Display for Track {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Frame,
    Strip(Vec<image::Handle>),
    TogglePlay,
    Seek(f64),
    Grab(Grab, f64),
    Drag(f64),
    Release,
    SetIn,
    SetOut,
    ResetTrim,
    Loop,
    Volume(f32),
    Track(Track),
    Name(String),
    Rename,
    Export(ExportKind),
    CancelExport,
    ShowExport,
    Copy,
    Reveal,
    External,
    Favorite,
    AskDelete,
    Back,
    GetLink,
    SendToFriends,
    ShareExport,
    CancelLink,
    CopyLink(String),
    OpenLink(String),
    ShowQr,
    /// Marks the moment at the playhead, or takes the mark there away.
    Mark,
    /// Trims to a few seconds either side of the mark nearest the playhead.
    TrimAroundMark,
    ClearMarks,
    Speed(f64),
    Look(media::Look),
    Fade(bool),
    PickMusic,
    MusicVolume(f32),
    ClearMusic,
    GameVolume(f32),
    MicVolume(f32),
    ChatVolume(f32),
    MicCleanup(bool),
    EvenLoudness(bool),
    ResetStyle,
    MakeCaptions,
    CancelCaptions,
    CaptionText(usize, String),
    DeleteCaption(usize),
    BurnCaptions(bool),
    RevealSrt,
    ShowKeys(bool),
    /// Makes the Discord-size copy and posts it to the connected channel.
    SendToChannel,
    CancelSend,
    TagText(String),
    AddTag,
    /// Adds a tag used on other clips.
    AddTagNamed(String),
    RemoveTag(String),
    SetFolder(super::FolderPick),
    /// Text being typed for a new text item.
    StampText(String),
    AddText,
    PickStamp(Option<usize>),
    /// Dragged on the picture: its new middle.
    MoveStamp(usize, f32, f32),
    DropStamp,
    StampWords(String),
    StampSize(f32),
    StampColor(usize),
    StampAnim(&'static str),
    /// The picked item starts (or ends) at the playhead.
    StampStart,
    StampEnd,
    DeleteStamp(usize),
    Undo,
    Redo,
    /// Splits the clip at the playhead.
    Split,
    /// Cuts out the part (between splits) under the playhead; the rest closes up.
    CutPart,
}

/// Everything an edit can change, kept whole for undo: the cut, the marks, the style, captions,
/// text and what copies include.
#[derive(Clone, PartialEq)]
struct Doc {
    trim: (f64, f64),
    splits: Vec<f64>,
    removed: Vec<(f64, f64)>,
    markers: Vec<f64>,
    effects: media::Effects,
    captions: Vec<kairo::captions::Caption>,
    burn_captions: bool,
    show_keys: bool,
    stamps: Vec<Item>,
}

/// Steps of undo kept.
const UNDO_STEPS: usize = 200;
/// Edits of the same thing closer together than this undo as one (a drag, a slider).
const MERGE_WITHIN: std::time::Duration = std::time::Duration::from_millis(700);

/// What a continuous edit is changing, so its many messages undo as one step.
fn merge_key(msg: &Msg) -> Option<String> {
    Some(match msg {
        Msg::Grab(..) | Msg::Drag(_) => "trim".into(),
        Msg::GameVolume(_) => "game".into(),
        Msg::MicVolume(_) => "mic".into(),
        Msg::ChatVolume(_) => "chat".into(),
        Msg::MusicVolume(_) => "music".into(),
        Msg::CaptionText(i, _) => format!("caption {i}"),
        Msg::StampWords(_) => "words".into(),
        Msg::StampSize(_) => "size".into(),
        Msg::MoveStamp(i, ..) => format!("move {i}"),
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    Trim,
    Copy,
    Share,
    Gif,
    Vertical,
    Square,
    Portrait,
    Slow(u32),
}

/// The window's view of the clip's share link, for the Share panel.
pub struct ShareView {
    pub progress: Option<(String, f32)>,
    pub link: Option<kairo::library::Link>,
    /// The connected Discord channel's webhook name, if one is connected.
    pub channel: Option<String>,
    /// How much of the clip has gone to the channel, while it's sending.
    pub sending: Option<f32>,
    /// Every tag in the library, for adding with a click.
    pub tags: Vec<String>,
    /// Every folder, for moving the clip.
    pub folders: Vec<String>,
    /// Whether sending to friends is offered.
    pub friends: bool,
}

/// What the editor asks of the window after a message.
pub enum Effect {
    None,
    Note(String, bool),
    Renamed {
        from: PathBuf,
        to: PathBuf,
    },
    Exported,
    Favorite(PathBuf),
    AskDelete(PathBuf),
    Back,
    /// Upload `file` (or its `range`) and remember its link for `clip`.
    Share {
        file: PathBuf,
        clip: PathBuf,
        range: Option<(f64, f64)>,
    },
    CancelUpload(PathBuf),
    CopyText(String),
    OpenUrl(String),
    ShowLink(PathBuf),
    /// Pick friends to send the clip to.
    SendToFriends(PathBuf),
    /// Post `file` (made from `clip`) to the Discord channel.
    SendToChannel {
        file: PathBuf,
        clip: PathBuf,
        message: String,
    },
    CancelSend(PathBuf),
    Tag {
        clip: PathBuf,
        tag: String,
        add: bool,
    },
    /// Put the clip in a folder, or in none.
    Folder {
        clip: PathBuf,
        folder: Option<String>,
    },
}

pub struct Editor {
    pub clip: PathBuf,
    pub name: String,
    ff: PathBuf,
    player: Player,
    /// The frame on screen.
    frame: Option<Picture>,
    /// Frames put on screen so far (for the self-test).
    pub frames_shown: u64,
    strip: Vec<image::Handle>,
    strip_loading: bool,
    pub trim: (f64, f64),
    grab: Option<Grab>,
    resume_after_grab: bool,
    last_scrub: f64,
    looping: bool,
    job: Option<Job>,
    encoders: Arc<Mutex<Option<Vec<Encoder>>>>,
    last_export: Option<(PathBuf, bool)>,
    discord_mb: u32,
    /// Upload the trimmed copy once it's made.
    share_after_export: bool,
    /// Post the Discord copy to the channel once it's made.
    send_after_export: bool,
    /// The tag being typed.
    tag_text: String,
    /// The file the Share panel is about: the clip, or a trimmed copy made for sharing.
    shared_file: Option<PathBuf>,
    /// Marked moments, in seconds (kept in the library with the clip).
    markers: Vec<f64>,
    /// The marker shortcut, for the hint.
    mark_key: String,
    /// Speed, look, fades, music and the mix, for every copy made from here.
    pub effects: media::Effects,
    /// What's said in the clip (saved as an .srt beside it).
    pub captions: Vec<kairo::captions::Caption>,
    /// Burn the captions into copies.
    burn_captions: bool,
    /// Keys and clicks recorded with the clip, and whether copies show them.
    keys: Vec<kairo::keys::KeyEvent>,
    show_keys: bool,
    /// Overlay pictures already drawn, by which keys are down.
    key_pictures: std::cell::RefCell<std::collections::HashMap<u32, image::Handle>>,
    caption_job: Option<export::CaptionJob>,
    /// Text over the clip (kept beside it), and the one being changed.
    stamps: Vec<Item>,
    stamp_picked: Option<usize>,
    stamp_text: String,
    /// Their pictures on the monitor, by what they look like.
    stamp_pictures: StampPictures,
    /// Earlier and undone states, newest last, and the last edit's kind and time.
    undo: Vec<Doc>,
    redo: Vec<Doc>,
    last_edit: Option<(String, std::time::Instant)>,
    /// Where the clip is split, and the parts cut out (seconds of the clip).
    splits: Vec<f64>,
    removed: Vec<(f64, f64)>,
}

/// Text pictures for the monitor, by what they look like, with their sizes.
type StampPictures =
    std::cell::RefCell<std::collections::HashMap<String, (image::Handle, (u32, u32))>>;

/// Text is drawn for the monitor as if the picture were this tall.
const STAMP_REF_H: u32 = 720;
const FONT_BOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-Bold.ttf");

impl Editor {
    pub fn open(
        clip: PathBuf,
        settings: &kairo::config::Settings,
    ) -> Result<(Editor, Task<Msg>), String> {
        let discord_mb = settings.discord_mb;
        let ff = ffmpeg::path()?;
        if !clip.is_file() {
            return Err(format!("{} doesn't exist any more.", clip.display()));
        }
        let info = ffmpeg::probe_clip(&ff, &clip)?;
        let encoders: Arc<Mutex<Option<Vec<Encoder>>>> = Arc::default();
        {
            let (enc, ff) = (encoders.clone(), ff.clone());
            std::thread::spawn(move || {
                export::best_encoder(&ff, &enc);
            });
        }
        let w = ((STRIP_H as f64 * info.width as f64 / info.height.max(1) as f64).round() as u32)
            .max(2)
            & !1;
        let strip = {
            let (ff, clip, info_duration) = (ff.clone(), clip.clone(), info.duration);
            Task::perform(
                background(move || filmstrip(&ff, &clip, (w, STRIP_H), info_duration)),
                Msg::Strip,
            )
        };
        let duration = info.duration;
        let name = stem(&clip);
        let clip_path = clip.clone();
        Ok((
            Editor {
                player: Player::new(ff.clone(), clip.clone(), info, PLAYBACK_MAX, STILL_MAX),
                ff,
                clip,
                name,
                frame: None,
                frames_shown: 0,
                strip: Vec::new(),
                strip_loading: true,
                trim: (0.0, duration),
                grab: None,
                resume_after_grab: false,
                last_scrub: -1.0,
                looping: false,
                job: None,
                encoders,
                last_export: None,
                discord_mb,
                share_after_export: false,
                send_after_export: false,
                tag_text: String::new(),
                shared_file: None,
                markers: library::Library::load().markers(&clip_path).to_vec(),
                mark_key: settings.marker_hotkey.clone(),
                effects: media::Effects::default(),
                captions: std::fs::read_to_string(clip_path.with_extension("srt"))
                    .map(|t| kairo::captions::parse_srt(&t))
                    .unwrap_or_default(),
                burn_captions: true,
                caption_job: None,
                keys: crate::save::read_keys(&clip_path),
                show_keys: true,
                key_pictures: Default::default(),
                stamps: overlays::load(&crate::save::stamps_for(&clip_path)),
                stamp_picked: None,
                stamp_text: String::new(),
                stamp_pictures: Default::default(),
                undo: Vec::new(),
                redo: Vec::new(),
                last_edit: None,
                splits: Vec::new(),
                removed: Vec::new(),
            },
            strip,
        ))
    }

    /// Loads the filmstrip and the frame at `at` right away, and sets a trim (for previews).
    #[cfg(test)]
    pub fn settle(&mut self, trim: (f64, f64), at: f64) {
        let w = ((STRIP_H as f64 * self.player.info.width as f64
            / self.player.info.height.max(1) as f64)
            .round() as u32)
            .max(2)
            & !1;
        self.strip = filmstrip(&self.ff, &self.clip, (w, STRIP_H), self.player.duration());
        self.strip_loading = false;
        self.trim = trim;
        self.player.seek(at);
        let start = std::time::Instant::now();
        while self.player.has_pending_still()
            && start.elapsed() < std::time::Duration::from_secs(20)
        {
            self.update(Msg::Frame);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        self.update(Msg::Frame);
    }

    /// Shows these marks (for previews).
    #[cfg(test)]
    pub fn show_marks(&mut self, marks: Vec<f64>) {
        self.markers = marks;
    }

    /// Shows these splits and cut-out parts (for previews).
    #[cfg(test)]
    pub fn show_cuts(&mut self, splits: Vec<f64>, removed: Vec<(f64, f64)>) {
        self.splits = splits;
        self.removed = removed;
    }

    /// Shows this text, one item picked (for previews; not saved with the clip).
    #[cfg(test)]
    pub fn show_stamps(&mut self, stamps: Vec<Item>, picked: Option<usize>) {
        self.stamps = stamps;
        self.stamp_picked = picked;
    }

    /// The first frame and the filmstrip are in.
    pub fn ready(&self) -> bool {
        self.frame.is_some() && !self.strip_loading && !self.strip.is_empty()
    }

    pub fn position(&self) -> f64 {
        self.player.position
    }

    pub fn is_playing(&self) -> bool {
        self.player.is_playing()
    }

    /// Whether the page needs a message every frame (playing, fetching a frame, exporting).
    pub fn busy(&self) -> bool {
        self.player.is_playing()
            || self.player.has_pending_still()
            || self.job.is_some()
            || self.caption_job.is_some()
    }

    /// Uses these keys and clicks for the overlay (the window self-test's).
    pub fn use_keys(&mut self, keys: Vec<kairo::keys::KeyEvent>) {
        self.keys = keys;
        self.show_keys = true;
    }

    /// Copies get something beyond a plain trim: parts cut out, a style, captions, text or the
    /// keys overlay.
    fn styled(&self) -> bool {
        self.has_cuts()
            || !self.effects.is_plain()
            || (self.burn_captions && !self.captions.is_empty())
            || (self.show_keys && !self.keys.is_empty())
            || !self.stamps.is_empty()
    }

    fn save_stamps(&self) {
        let _ = overlays::save(&crate::save::stamps_for(&self.clip), &self.stamps);
    }

    /// Where a new item shows: from the playhead (inside the trim) for three seconds.
    fn new_stamp_span(&self) -> (f64, f64) {
        let (a, b) = self.trim;
        let start = self.player.position.clamp(a, (b - 0.5).max(a));
        (start, (start + 3.0).min(b.max(start + 0.5)))
    }

    /// An item's picture for the monitor, drawn once for each look.
    fn stamp_picture(&self, item: &Item) -> Option<(image::Handle, (u32, u32))> {
        let key = format!("{}|{}|{:.4}", item.text, item.color, item.size);
        let mut cache = self.stamp_pictures.borrow_mut();
        if let Some(v) = cache.get(&key) {
            return Some(v.clone());
        }
        let aspect = self.player.info.width as f32 / self.player.info.height.max(1) as f32;
        let max_w = (STAMP_REF_H as f32 * aspect * 0.9) as u32;
        let p = item.picture(STAMP_REF_H, max_w, FONT_BOLD)?;
        let v = (
            image::Handle::from_rgba(p.width, p.height, p.rgba),
            (p.width, p.height),
        );
        if cache.len() > 64 {
            cache.clear();
        }
        cache.insert(key, v.clone());
        Some(v)
    }

    fn update_stamp(&mut self, change: impl FnOnce(&mut Item)) {
        if let Some(it) = self.stamp_picked.and_then(|i| self.stamps.get_mut(i)) {
            change(it);
            self.save_stamps();
        }
    }

    /// The keys copies show.
    fn keys_for_copies(&self) -> Vec<kairo::keys::KeyEvent> {
        if self.show_keys {
            self.keys.clone()
        } else {
            Vec::new()
        }
    }

    /// The overlay picture for the keys down at the playhead (drawn once per combination).
    fn keys_now(&self) -> Option<image::Handle> {
        if !self.show_keys || self.keys.is_empty() {
            return None;
        }
        let mask = kairo::keys::state_at(&self.keys, self.player.position);
        let mut cache = self.key_pictures.borrow_mut();
        let h = cache.entry(mask).or_insert_with(|| {
            let size = kairo::keys::size_for(720);
            let rgba = kairo::keys::render(
                mask,
                size,
                theme::accent_rgb(),
                include_bytes!("../../assets/fonts/Inter-Bold.ttf"),
            );
            image::Handle::from_rgba(size.0, size.1, rgba)
        });
        Some(h.clone())
    }

    /// The captions copies get.
    fn captions_for_copies(&self) -> Vec<kairo::captions::Caption> {
        if self.burn_captions {
            self.captions.clone()
        } else {
            Vec::new()
        }
    }

    /// Writes the captions beside the clip (or removes the file when there are none).
    fn save_captions(&self) {
        let path = self.clip.with_extension("srt");
        if self.captions.is_empty() {
            let _ = std::fs::remove_file(path);
        } else {
            let _ = std::fs::write(path, kairo::captions::srt(&self.captions));
        }
    }

    fn poll_captions(&mut self) -> Effect {
        if !self.caption_job.as_ref().is_some_and(|j| j.finished()) {
            return Effect::None;
        }
        let job = self.caption_job.take().expect("finished");
        let result = job.status.lock().unwrap().result.take();
        match result {
            Some(Ok(caps)) if caps.is_empty() => {
                Effect::Note("No speech heard in this clip.".into(), false)
            }
            Some(Ok(caps)) => {
                self.captions = caps;
                self.save_captions();
                Effect::None
            }
            Some(Err(e)) if e == "Cancelled" => Effect::None,
            Some(Err(e)) => Effect::Note(format!("Couldn't make captions: {e}"), false),
            None => Effect::None,
        }
    }

    /// The caption on screen at the playhead.
    fn caption_now(&self) -> Option<&str> {
        let t = self.player.position;
        self.captions
            .iter()
            .find(|c| c.start <= t && t < c.end)
            .map(|c| c.text.as_str())
    }

    /// Parts inside the trim are cut out.
    fn has_cuts(&self) -> bool {
        kairo::cuts::cuts_into(self.trim, &self.removed)
    }

    /// How long a copy runs before any speed change.
    fn kept_length(&self) -> f64 {
        kairo::cuts::kept_length(self.trim, &self.removed)
    }

    fn trimmed(&self) -> bool {
        self.trim.0 > 0.01 || self.trim.1 < self.player.duration() - 0.01
    }

    fn toggle_play(&mut self) {
        let step = self.player.frame_step();
        if !self.player.is_playing()
            && (self.player.position < self.trim.0 || self.player.position >= self.trim.1 - step)
        {
            self.player.position = self.trim.0;
        }
        if self.player.is_playing() {
            self.player.pause();
        } else {
            self.player.play();
        }
    }

    fn set_in(&mut self, t: f64) {
        self.trim.0 = t.clamp(0.0, (self.trim.1 - 0.5).max(0.0));
    }

    fn set_out(&mut self, t: f64) {
        self.trim.1 = t.clamp(
            (self.trim.0 + 0.5).min(self.player.duration()),
            self.player.duration(),
        );
    }

    /// Stops reading the file (before it's renamed or deleted elsewhere).
    pub fn release(&mut self) {
        self.player.release_file();
    }

    pub fn key(&mut self, key: &Key, modifiers: keyboard::Modifiers) -> bool {
        let jump = if modifiers.shift() { 5.0 } else { 1.0 };
        let pos = self.player.position;
        if modifiers.command() {
            match key.as_ref() {
                Key::Character("z") | Key::Character("Z") if modifiers.shift() => {
                    self.update(Msg::Redo);
                }
                Key::Character("z") | Key::Character("Z") => {
                    self.update(Msg::Undo);
                }
                Key::Character("y") | Key::Character("Y") => {
                    self.update(Msg::Redo);
                }
                Key::Character("b")
                | Key::Character("B")
                | Key::Character("k")
                | Key::Character("K") => {
                    self.update(Msg::Split);
                }
                _ => return false,
            }
            return true;
        }
        match key.as_ref() {
            Key::Named(Named::Space) | Key::Character("k") | Key::Character("K") => {
                self.toggle_play()
            }
            Key::Character("j") | Key::Character("J") => self.player.seek(pos - jump),
            Key::Character("l") | Key::Character("L") => self.player.seek(pos + jump),
            Key::Named(Named::ArrowLeft) => self.player.seek(pos - jump),
            Key::Named(Named::ArrowRight) => self.player.seek(pos + jump),
            Key::Named(Named::Home) => self.player.seek(self.trim.0),
            Key::Named(Named::End) => self.player.seek(self.trim.1),
            Key::Character(",") | Key::Character(".") => {
                self.player.pause();
                let step = self.player.frame_step();
                let back = matches!(key.as_ref(), Key::Character(","));
                self.player.seek(if back { pos - step } else { pos + step });
            }
            Key::Character("i")
            | Key::Character("I")
            | Key::Character("q")
            | Key::Character("Q") => {
                self.update(Msg::SetIn);
            }
            Key::Character("o")
            | Key::Character("O")
            | Key::Character("w")
            | Key::Character("W") => {
                self.update(Msg::SetOut);
            }
            Key::Character("m") | Key::Character("M") => {
                self.update(Msg::Mark);
            }
            Key::Character("s") | Key::Character("S") => {
                self.update(Msg::Split);
            }
            Key::Named(Named::Delete)
            | Key::Named(Named::Backspace)
            | Key::Character("x")
            | Key::Character("X") => {
                let effect = self.update(Msg::CutPart);
                if let Effect::Note(..) = effect {
                    return true;
                }
            }
            Key::Character("n") | Key::Character("N") => self.jump_mark(!modifiers.shift()),
            _ => return false,
        }
        true
    }

    /// Marks the moment at the playhead, or takes the mark there away; kept with the clip.
    fn toggle_mark(&mut self) {
        let mut lib = library::Library::load();
        self.markers = lib.toggle_marker(&self.clip, self.player.position);
        let _ = lib.save();
    }

    fn set_marks(&mut self, marks: Vec<f64>) {
        let mut lib = library::Library::load();
        lib.set_markers(&self.clip, marks);
        self.markers = lib.markers(&self.clip).to_vec();
        let _ = lib.save();
    }

    /// Goes to the next mark after the playhead (or the one before), round to the first.
    fn jump_mark(&mut self, forward: bool) {
        let pos = self.player.position;
        let found = if forward {
            self.markers
                .iter()
                .find(|m| **m > pos + 0.05)
                .or(self.markers.first())
        } else {
            self.markers
                .iter()
                .rev()
                .find(|m| **m < pos - 0.05)
                .or(self.markers.last())
        };
        if let Some(m) = found.copied() {
            self.player.seek(m);
        }
    }

    /// The mark nearest the playhead.
    fn nearest_mark(&self) -> Option<f64> {
        let pos = self.player.position;
        self.markers
            .iter()
            .copied()
            .min_by(|a, b| (a - pos).abs().total_cmp(&(b - pos).abs()))
    }

    /// The editable state as it is now.
    fn doc(&self) -> Doc {
        Doc {
            trim: self.trim,
            splits: self.splits.clone(),
            removed: self.removed.clone(),
            markers: self.markers.clone(),
            effects: self.effects.clone(),
            captions: self.captions.clone(),
            burn_captions: self.burn_captions,
            show_keys: self.show_keys,
            stamps: self.stamps.clone(),
        }
    }

    /// Puts back an earlier state, saving what's kept beside the clip.
    fn restore(&mut self, d: Doc) {
        if d.markers != self.markers {
            self.set_marks(d.markers.clone());
        }
        let captions_changed = d.captions != self.captions;
        let stamps_changed = d.stamps != self.stamps;
        self.trim = d.trim;
        self.splits = d.splits;
        self.removed = d.removed;
        self.effects = d.effects;
        self.captions = d.captions;
        self.burn_captions = d.burn_captions;
        self.show_keys = d.show_keys;
        self.stamps = d.stamps;
        if captions_changed {
            self.save_captions();
        }
        if stamps_changed {
            self.save_stamps();
        }
        if self.stamp_picked.is_some_and(|i| i >= self.stamps.len()) {
            self.stamp_picked = None;
        }
        self.last_edit = None;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Handles a message, keeping what it changed for undo.
    pub fn update(&mut self, msg: Msg) -> Effect {
        match msg {
            // Playback and loading change nothing undoable, and come often.
            Msg::Frame | Msg::Strip(_) => return self.handle(msg),
            Msg::Undo => {
                if let Some(d) = self.undo.pop() {
                    self.redo.push(self.doc());
                    self.restore(d);
                }
                return Effect::None;
            }
            Msg::Redo => {
                if let Some(d) = self.redo.pop() {
                    self.undo.push(self.doc());
                    self.restore(d);
                }
                return Effect::None;
            }
            _ => {}
        }
        let key = merge_key(&msg);
        let before = self.doc();
        let effect = self.handle(msg);
        if self.doc() != before {
            let now = std::time::Instant::now();
            let merge = matches!(
                (&key, &self.last_edit),
                (Some(k), Some((last, at))) if k == last && now.duration_since(*at) < MERGE_WITHIN
            );
            if !merge {
                self.undo.push(before);
                if self.undo.len() > UNDO_STEPS {
                    self.undo.remove(0);
                }
            }
            self.redo.clear();
            self.last_edit = key.map(|k| (k, now));
        }
        effect
    }

    fn handle(&mut self, msg: Msg) -> Effect {
        match msg {
            Msg::Undo | Msg::Redo => {}
            Msg::Split => {
                let t = self.player.position;
                if t > self.trim.0 + 0.05 && t < self.trim.1 - 0.05 {
                    kairo::cuts::split(&mut self.splits, t);
                }
            }
            Msg::CutPart => {
                let part = kairo::cuts::part_at(self.trim, &self.splits, self.player.position);
                if kairo::cuts::kept_length(self.trim, &self.removed)
                    - kairo::cuts::kept_length(part, &self.removed)
                    < 0.3
                {
                    return Effect::Note("That would leave nothing to keep.".into(), false);
                }
                kairo::cuts::remove(&mut self.removed, part);
                // On to what's left after it.
                let next = kairo::cuts::kept(self.trim, &self.removed)
                    .into_iter()
                    .find(|(a, _)| *a >= part.1 - 0.001)
                    .or_else(|| kairo::cuts::kept(self.trim, &self.removed).last().copied());
                if let Some((a, _)) = next {
                    self.player.seek(a);
                }
            }
            Msg::Frame => {
                let end = self.trim.1;
                let was_playing = self.player.is_playing();
                self.player.tick(end);
                // Playback skips the parts cut out.
                if self.player.is_playing()
                    && let Some((_, b)) =
                        kairo::cuts::removed_at(&self.removed, self.player.position)
                {
                    if b >= end - 0.01 {
                        // The rest is cut out: that's the end, as if playback reached it (so
                        // Play starts over, and looping does).
                        self.player.pause();
                        self.player.position = end;
                    } else {
                        self.player.seek(b);
                    }
                }
                if let Some(f) = self.player.frame.take() {
                    self.frame = Some(Picture::new(f.size, &f.rgba));
                    self.frames_shown += 1;
                }
                if was_playing && !self.player.is_playing() && self.looping && self.grab.is_none() {
                    self.player.position = self.trim.0;
                    self.player.play();
                }
                if let Some(e) = self.player.error.take() {
                    return Effect::Note(e, false);
                }
                if let Effect::Note(t, ok) = self.poll_captions() {
                    return Effect::Note(t, ok);
                }
                return self.poll_job();
            }
            Msg::MakeCaptions => {
                if self.caption_job.is_none() {
                    let tracks = &self.player.info.audio_tracks;
                    // The microphone alone hears the player best; else the whole mix.
                    let track = tracks.iter().position(|t| t == "Microphone").unwrap_or(0);
                    self.caption_job = Some(export::start_captions(
                        self.ff.clone(),
                        self.clip.clone(),
                        track,
                    ));
                }
            }
            Msg::CancelCaptions => {
                if let Some(j) = &self.caption_job {
                    j.cancel();
                }
            }
            Msg::CaptionText(i, t) => {
                if let Some(c) = self.captions.get_mut(i) {
                    c.text = t;
                    self.save_captions();
                }
            }
            Msg::DeleteCaption(i) => {
                if i < self.captions.len() {
                    self.captions.remove(i);
                    self.save_captions();
                }
            }
            Msg::BurnCaptions(on) => self.burn_captions = on,
            Msg::ShowKeys(on) => self.show_keys = on,
            Msg::RevealSrt => {
                let p = self.clip.with_extension("srt");
                if p.exists() {
                    crate::app::reveal(&p);
                }
            }
            Msg::Strip(s) => {
                self.strip = s;
                self.strip_loading = false;
            }
            Msg::TogglePlay => self.toggle_play(),
            Msg::Seek(t) => self.player.seek(t),
            Msg::Grab(g, t) => {
                self.grab = Some(g);
                self.resume_after_grab = self.player.is_playing();
                self.player.pause();
                self.last_scrub = -1.0;
                self.drag(t);
            }
            Msg::Drag(t) => self.drag(t),
            Msg::Release => {
                self.grab = None;
                if self.resume_after_grab {
                    self.resume_after_grab = false;
                    self.toggle_play();
                }
            }
            Msg::SetIn => {
                let p = self.player.position;
                self.set_in(p);
            }
            Msg::SetOut => {
                let p = self.player.position;
                self.set_out(p);
            }
            Msg::ResetTrim => {
                self.trim = (0.0, self.player.duration());
                self.splits.clear();
                self.removed.clear();
            }
            Msg::Mark => self.toggle_mark(),
            Msg::ClearMarks => self.set_marks(Vec::new()),
            Msg::TrimAroundMark => {
                if let Some(m) = self.nearest_mark() {
                    let end = self.player.duration();
                    self.trim = ((m - MARK_SPAN).max(0.0), (m + MARK_SPAN).min(end));
                    self.player.seek(self.trim.0);
                }
            }
            Msg::Loop => self.looping = !self.looping,
            Msg::Volume(v) => self.player.set_volume(v),
            Msg::Speed(v) => self.effects.speed = v,
            Msg::Look(l) => self.effects.look = l,
            Msg::Fade(on) => self.effects.fade = on,
            Msg::PickMusic => {
                let picked = rfd::FileDialog::new()
                    .set_title("Music for your clip")
                    .add_filter(
                        "Music",
                        &["mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "mp4"],
                    )
                    .pick_file();
                if let Some(path) = picked {
                    let volume = self.effects.music.as_ref().map_or(0.35, |m| m.volume);
                    self.effects.music = Some(media::Music { path, volume });
                }
            }
            Msg::MusicVolume(v) => {
                if let Some(m) = &mut self.effects.music {
                    m.volume = v;
                }
            }
            Msg::ClearMusic => self.effects.music = None,
            Msg::GameVolume(v) => self.effects.game_volume = v,
            Msg::MicVolume(v) => self.effects.mic_volume = v,
            Msg::ChatVolume(v) => self.effects.chat_volume = v,
            Msg::MicCleanup(on) => self.effects.mic_cleanup = on,
            Msg::EvenLoudness(on) => self.effects.even_loudness = on,
            Msg::ResetStyle => self.effects = media::Effects::default(),
            Msg::StampText(t) => self.stamp_text = t,
            Msg::AddText => {
                let words = self.stamp_text.trim().to_string();
                if !words.is_empty() {
                    let (a, b) = self.new_stamp_span();
                    self.stamps.push(Item::text(&words, a, b));
                    self.stamp_picked = Some(self.stamps.len() - 1);
                    self.stamp_text.clear();
                    self.save_stamps();
                }
            }
            Msg::PickStamp(i) => self.stamp_picked = i.filter(|i| *i < self.stamps.len()),
            Msg::MoveStamp(i, x, y) => {
                if let Some(it) = self.stamps.get_mut(i) {
                    it.x = x.clamp(0.0, 1.0);
                    it.y = y.clamp(0.0, 1.0);
                }
            }
            Msg::DropStamp => self.save_stamps(),
            Msg::StampWords(t) => self.update_stamp(|it| it.text = t),
            Msg::StampSize(v) => self.update_stamp(|it| it.size = v),
            Msg::StampColor(c) => self.update_stamp(|it| it.color = c),
            Msg::StampAnim(a) => self.update_stamp(|it| it.anim = a.to_string()),
            Msg::StampStart => {
                let t = self.player.position;
                self.update_stamp(|it| it.start = t.min(it.end - 0.2).max(0.0));
            }
            Msg::StampEnd => {
                let (t, d) = (self.player.position, self.player.duration());
                self.update_stamp(|it| it.end = t.max(it.start + 0.2).min(d.max(it.start + 0.2)));
            }
            Msg::DeleteStamp(i) => {
                if i < self.stamps.len() {
                    self.stamps.remove(i);
                    self.stamp_picked = None;
                    self.save_stamps();
                }
            }
            Msg::Track(t) => self.player.set_track(t.index),
            Msg::Name(n) => self.name = n,
            Msg::Rename => return self.rename(),
            Msg::Export(k) => return self.start_export(k),
            Msg::CancelExport => {
                if let Some(j) = &self.job {
                    j.cancel();
                }
            }
            Msg::ShowExport => {
                if let Some((p, _)) = &self.last_export {
                    crate::app::reveal(p);
                }
            }
            Msg::Copy => {
                return match platform::copy_files(std::slice::from_ref(&self.clip)) {
                    Ok(()) => Effect::Note(
                        "Copied. Paste it into Discord, a chat or a folder.".into(),
                        true,
                    ),
                    Err(e) => Effect::Note(e, false),
                };
            }
            Msg::Reveal => crate::app::reveal(&self.clip),
            Msg::External => crate::app::open(&self.clip),
            Msg::Favorite => return Effect::Favorite(self.clip.clone()),
            Msg::AskDelete => return Effect::AskDelete(self.clip.clone()),
            Msg::Back => return Effect::Back,
            Msg::SendToFriends => return Effect::SendToFriends(self.clip.clone()),
            Msg::GetLink => {
                // With a style or captions, share the styled copy: make it, then upload it.
                if self.styled() {
                    let e = self.start_export(ExportKind::Trim);
                    if self.job.is_some() {
                        self.share_after_export = true;
                    }
                    return e;
                }
                // A trimmed clip shares just the trimmed part (the link copy is cut from it).
                self.shared_file = None;
                return Effect::Share {
                    file: self.clip.clone(),
                    clip: self.clip.clone(),
                    range: self.trimmed().then_some(self.trim),
                };
            }
            Msg::ShareExport => {
                if let Some((p, _)) = &self.last_export {
                    self.shared_file = Some(p.clone());
                    return Effect::Share {
                        file: p.clone(),
                        clip: p.clone(),
                        range: None,
                    };
                }
            }
            Msg::CancelLink => return Effect::CancelUpload(self.share_target()),
            Msg::SendToChannel => {
                let e = self.start_export(ExportKind::Share);
                if self.job.is_some() {
                    self.send_after_export = true;
                }
                return e;
            }
            Msg::CancelSend => {
                if self.send_after_export
                    && let Some(j) = &self.job
                {
                    j.cancel();
                }
                return Effect::CancelSend(self.clip.clone());
            }
            Msg::TagText(t) => self.tag_text = t,
            Msg::AddTag => {
                let tag = library::clean_tag(&std::mem::take(&mut self.tag_text));
                if !tag.is_empty() {
                    return Effect::Tag {
                        clip: self.clip.clone(),
                        tag,
                        add: true,
                    };
                }
            }
            Msg::AddTagNamed(tag) => {
                self.tag_text.clear();
                return Effect::Tag {
                    clip: self.clip.clone(),
                    tag,
                    add: true,
                };
            }
            Msg::SetFolder(pick) => {
                return Effect::Folder {
                    clip: self.clip.clone(),
                    folder: pick.0,
                };
            }
            Msg::RemoveTag(tag) => {
                return Effect::Tag {
                    clip: self.clip.clone(),
                    tag,
                    add: false,
                };
            }
            Msg::CopyLink(u) => return Effect::CopyText(u),
            Msg::OpenLink(u) => return Effect::OpenUrl(u),
            Msg::ShowQr => return Effect::ShowLink(self.share_target()),
        }
        Effect::None
    }

    fn drag(&mut self, t: f64) {
        let Some(g) = self.grab else { return };
        match g {
            Grab::Seek => {}
            Grab::In => self.set_in(t),
            Grab::Out => self.set_out(t),
        }
        let show = match g {
            Grab::Seek => t.clamp(0.0, self.player.duration()),
            Grab::In => self.trim.0,
            Grab::Out => self.trim.1,
        };
        // Fetching a frame per pixel would pile up; only follow real moves.
        if (show - self.last_scrub).abs() >= self.player.frame_step() {
            self.last_scrub = show;
            self.player.seek(show);
        }
    }

    /// Renames the clip to the name in the title field, if it changed.
    pub fn rename(&mut self) -> Effect {
        let current = stem(&self.clip);
        if self.name.trim() == current || self.name.trim().is_empty() {
            self.name = current;
            return Effect::None;
        }
        let to = match library::rename_target(&self.clip, &self.name) {
            Ok(t) => t,
            Err(e) => {
                self.name = current;
                return Effect::Note(e, false);
            }
        };
        let was_playing = self.player.is_playing();
        self.player.release_file();
        let effect = match std::fs::rename(&self.clip, &to) {
            Ok(()) => {
                crate::save::move_sidecars(&self.clip, &to);
                let from = std::mem::replace(&mut self.clip, to.clone());
                self.player.clip = to.clone();
                self.name = stem(&to);
                Effect::Renamed { from, to }
            }
            Err(e) => {
                self.name = current;
                Effect::Note(format!("Couldn't rename: {e}"), false)
            }
        };
        let pos = self.player.position;
        self.player.seek(pos);
        if was_playing {
            self.player.play();
        }
        effect
    }

    fn start_export(&mut self, k: ExportKind) -> Effect {
        if self.job.is_some() {
            return Effect::None;
        }
        let kind = match k {
            ExportKind::Trim => Kind::Trim,
            ExportKind::Copy => Kind::Copy,
            ExportKind::Share => Kind::Share(self.discord_mb as u64 * 1_000_000),
            ExportKind::Gif => Kind::Gif,
            ExportKind::Vertical => Kind::Vertical,
            ExportKind::Square => Kind::Square,
            ExportKind::Portrait => Kind::Portrait,
            ExportKind::Slow(f) => Kind::Slow(f),
        };
        self.share_after_export = false;
        self.send_after_export = false;
        // The trimmed part; untrimmed, that's the whole clip.
        let range = if self.trimmed() {
            self.trim
        } else {
            (0.0, self.player.duration())
        };
        let length = self
            .effects
            .length(0.0, kairo::cuts::kept_length(range, &self.removed));
        if let Kind::Share(limit) = kind
            && let Err(e) = media::share_plan(limit, length, &self.player.info, 1.0)
        {
            return Effect::Note(e, false);
        }
        if kind == Kind::Gif && length > export::GIF_MAX_SECONDS + 0.05 {
            return Effect::Note(
                format!(
                    "Trim it to {:.0} seconds or less to make a GIF.",
                    export::GIF_MAX_SECONDS
                ),
                false,
            );
        }
        self.last_export = None;
        self.job = Some(export::start(
            kind,
            self.ff.clone(),
            self.clip.clone(),
            self.player.info.clone(),
            range,
            self.encoders.clone(),
            media::Effects {
                stamps: self.stamps.clone(),
                ..self.effects.clone()
            },
            self.captions_for_copies(),
            self.keys_for_copies(),
            self.removed.clone(),
        ));
        Effect::None
    }

    fn poll_job(&mut self) -> Effect {
        let Some(job) = &self.job else {
            return Effect::None;
        };
        if !job.finished() {
            return Effect::None;
        }
        let kind = job.kind;
        let result = job.status.lock().unwrap().result.take();
        self.job = None;
        match result {
            Some(Ok(path)) => {
                let send = std::mem::take(&mut self.send_after_export);
                let copied = matches!(kind, Kind::Share(_))
                    && !send
                    && platform::copy_files(std::slice::from_ref(&path)).is_ok();
                self.last_export = Some((path.clone(), copied));
                if send {
                    return Effect::SendToChannel {
                        file: path,
                        clip: self.clip.clone(),
                        message: self.name.clone(),
                    };
                }
                if std::mem::take(&mut self.share_after_export) {
                    self.shared_file = Some(path.clone());
                    return Effect::Share {
                        file: path.clone(),
                        clip: path,
                        range: None,
                    };
                }
                Effect::Exported
            }
            Some(Err(e)) if e == "Cancelled" => {
                self.share_after_export = false;
                self.send_after_export = false;
                Effect::Note("Export cancelled.".into(), true)
            }
            Some(Err(e)) => {
                self.share_after_export = false;
                self.send_after_export = false;
                Effect::Note(e, false)
            }
            None => Effect::None,
        }
    }

    // --- View -------------------------------------------------------------------------------

    /// The file the last export made.
    pub fn last_export(&self) -> Option<&Path> {
        self.last_export.as_ref().map(|(p, _)| p.as_path())
    }

    /// The file the Share panel shows: a trimmed copy made for sharing, or the clip.
    pub fn share_target(&self) -> PathBuf {
        self.shared_file
            .clone()
            .unwrap_or_else(|| self.clip.clone())
    }

    pub fn view<'a>(
        &'a self,
        item: Option<&'a ClipItem>,
        mut share: ShareView,
    ) -> Element<'a, Msg> {
        let known_tags = std::mem::take(&mut share.tags);
        let folders = std::mem::take(&mut share.folders);
        let folder = item.and_then(|c| c.folder.clone());
        let tags = item.map_or(&[][..], |c| c.tags.as_slice());
        let favorite = item.is_some_and(|c| c.favorite);
        let star = if favorite {
            icon_filled(Icon::Star, 16.0, theme::GOLD)
        } else {
            icon(Icon::Star, 16.0, theme::ink_2())
        };
        let head = row![
            tip(
                button(
                    row![
                        icon(Icon::Back, 16.0, theme::ink_2()),
                        text("Clips").size(14).font(MEDIUM)
                    ]
                    .spacing(7)
                    .align_y(Alignment::Center)
                )
                .padding(Padding::from([8, 12]))
                .style(theme::ghost)
                .on_press(Msg::Back),
                "Back to your clips (Esc)",
            ),
            text_input("Clip name", &self.name)
                .on_input(Msg::Name)
                .on_submit(Msg::Rename)
                .font(BOLD)
                .size(20)
                .padding(Padding::from([6, 10]))
                .style(theme::title_input)
                .width(Fill),
            tip(
                button(star)
                    .padding(8)
                    .style(theme::ghost)
                    .on_press(Msg::Favorite),
                if favorite {
                    "Remove from favourites"
                } else {
                    "Favourite: never cleaned up"
                },
            ),
            tip(
                icon_only(Icon::Undo, theme::ink_2())
                    .style(theme::ghost)
                    .on_press_maybe(self.can_undo().then_some(Msg::Undo)),
                "Undo (Ctrl+Z)",
            ),
            tip(
                icon_only(Icon::Redo, theme::ink_2())
                    .style(theme::ghost)
                    .on_press_maybe(self.can_redo().then_some(Msg::Redo)),
                "Redo (Ctrl+Shift+Z)",
            ),
            icon_button(Icon::Copy, "Copy", theme::ink_2())
                .style(theme::ghost)
                .on_press(Msg::Copy),
            icon_button(Icon::Folder, "Show", theme::ink_2())
                .style(theme::ghost)
                .on_press(Msg::Reveal),
            tip(
                icon_only(Icon::External, theme::ink_2())
                    .style(theme::ghost)
                    .on_press(Msg::External),
                "Open in your video player"
            ),
            tip(
                icon_only(Icon::Trash, theme::ink_2())
                    .style(theme::danger_ghost)
                    .on_press(Msg::AskDelete),
                "Delete"
            ),
        ]
        .spacing(4)
        .align_y(Alignment::Center);

        let picture: Element<'a, Msg> = match &self.frame {
            Some(p) => p.view(),
            None => text("Loading...").color(theme::muted()).into(),
        };
        let mut monitor = stack![container(picture).center(Fill)];
        if let Some(k) = self.keys_now() {
            monitor = monitor.push(
                container(image(k).width(206))
                    .width(Fill)
                    .height(Fill)
                    .align_x(iced::alignment::Horizontal::Left)
                    .align_y(iced::alignment::Vertical::Bottom)
                    .padding(16),
            );
        }
        if let Some(line) = self.caption_now() {
            monitor = monitor.push(
                container(
                    container(
                        text(line.to_string())
                            .size(20)
                            .font(BOLD)
                            .color(Color::WHITE),
                    )
                    .padding(Padding::from([6, 14]))
                    .style(|_| container::Style {
                        background: Some(theme::alpha(Color::BLACK, 0.62).into()),
                        border: iced::Border {
                            radius: 8.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
                )
                .width(Fill)
                .height(Fill)
                .align_x(iced::alignment::Horizontal::Center)
                .align_y(iced::alignment::Vertical::Bottom)
                .padding(26),
            );
        }
        if self.frame.is_some() && !self.stamps.is_empty() {
            let t = self.player.position;
            let items = self
                .stamps
                .iter()
                .enumerate()
                .filter(|(i, it)| it.shows_at(t) || self.stamp_picked == Some(*i))
                .filter_map(|(i, it)| {
                    let (handle, size) = self.stamp_picture(it)?;
                    Some(Shown {
                        index: i,
                        handle,
                        size,
                        x: it.x,
                        y: it.y,
                        pose: it.pose_at(t),
                        dim: !it.shows_at(t),
                    })
                })
                .collect();
            monitor = monitor.push(
                Canvas::new(StampLayer {
                    items,
                    video: (
                        self.player.info.width.max(1) as f32,
                        self.player.info.height.max(1) as f32,
                    ),
                    picked: self.stamp_picked,
                })
                .width(Fill)
                .height(Fill),
            );
        }
        if !self.player.is_playing() && self.frame.is_some() {
            monitor = monitor.push(
                container(
                    container(icon_filled(Icon::Play, 26.0, Color::WHITE))
                        .center(Length::Fixed(66.0))
                        .style(|_| container::Style {
                            background: Some(theme::alpha(Color::BLACK, 0.55).into()),
                            border: iced::Border {
                                color: theme::alpha(Color::WHITE, 0.18),
                                width: 1.0,
                                radius: 33.0.into(),
                            },
                            ..Default::default()
                        }),
                )
                .center(Fill),
            );
        }
        let monitor = mouse_area(
            container(monitor)
                .width(Fill)
                .height(Fill)
                .style(|_| container::Style {
                    background: Some(Color::BLACK.into()),
                    border: iced::Border {
                        color: theme::edge(),
                        width: 1.0,
                        radius: 12.0.into(),
                    },
                    ..Default::default()
                })
                .clip(true),
        )
        .on_press(Msg::TogglePlay)
        .interaction(mouse::Interaction::Pointer);

        let main = column![
            monitor,
            self.transport(),
            // Two layers: iced draws a canvas's pictures above its shapes, so the marks go on a
            // canvas of their own on top of the filmstrip.
            stack![
                Canvas::new(Strip {
                    strip: &self.strip,
                    aspect: self.player.info.width as f32 / self.player.info.height.max(1) as f32,
                })
                .width(Fill)
                .height(TIMELINE_H),
                Canvas::new(Timeline {
                    duration: self.player.duration(),
                    trim: self.trim,
                    position: self.player.position,
                    grab: self.grab,
                    markers: self.markers.clone(),
                    splits: self.splits.clone(),
                    removed: self.removed.clone(),
                })
                .width(Fill)
                .height(TIMELINE_H),
            ],
            row![
                hint_key("Space"),
                text("play").size(12).color(theme::muted()),
                Space::new().width(6),
                hint_key("I"),
                hint_key("O"),
                text("start and end").size(12).color(theme::muted()),
                Space::new().width(6),
                hint_key(","),
                hint_key("."),
                text("one frame").size(12).color(theme::muted()),
                Space::new().width(6),
                hint_key("M"),
                text("mark").size(12).color(theme::muted()),
                Space::new().width(6),
                hint_key("S"),
                text("split").size(12).color(theme::muted()),
                Space::new().width(6),
                hint_key("Ctrl+Z"),
                text("undo").size(12).color(theme::muted()),
            ]
            .spacing(5)
            .align_y(Alignment::Center),
        ]
        .spacing(12)
        .width(Fill);

        let side = scrollable(
            column![
                self.share_panel(share),
                self.tags_panel(tags, known_tags, folders, folder),
                self.trim_panel(),
                self.marks_panel(),
                self.stamps_panel(),
                self.captions_panel(),
                self.style_panel(),
                self.export_panel(),
                self.details_panel()
            ]
            .spacing(12),
        )
        .width(310)
        .style(theme::scroll);

        column![head, row![main, side].spacing(20).height(Fill)]
            .spacing(14)
            .padding(Padding {
                top: 0.0,
                right: 24.0,
                bottom: 20.0,
                left: 24.0,
            })
            .into()
    }

    fn transport(&self) -> Element<'_, Msg> {
        let playing = self.player.is_playing();
        let play = button(
            container(icon_filled(
                if playing { Icon::Pause } else { Icon::Play },
                18.0,
                theme::on_accent(),
            ))
            .center(Length::Fixed(22.0)),
        )
        .padding(10)
        .style(|t, s| {
            let mut st = theme::primary(t, s);
            st.border.radius = 999.0.into();
            st
        })
        .on_press(Msg::TogglePlay);
        let time = row![
            text(format_time(self.player.position))
                .font(Font::MONOSPACE)
                .size(17)
                .color(theme::ink()),
            text(format!(" / {}", format_time(self.player.duration())))
                .font(Font::MONOSPACE)
                .size(14)
                .color(theme::muted()),
        ]
        .align_y(Alignment::Center);

        let mut right = row![
            tip(
                button(icon(
                    Icon::Repeat,
                    16.0,
                    if self.looping {
                        theme::accent()
                    } else {
                        theme::ink_2()
                    }
                ))
                .padding(8)
                .style(if self.looping {
                    theme::segment(true)
                } else {
                    theme::segment(false)
                })
                .on_press(Msg::Loop),
                "Loop the trimmed part",
            ),
            Space::new().width(6),
            icon(
                if self.player.volume <= 0.0 {
                    Icon::Mute
                } else {
                    Icon::Volume
                },
                16.0,
                theme::ink_2()
            ),
            slider(0.0..=1.5, self.player.volume, Msg::Volume)
                .step(0.01_f32)
                .width(110)
                .style(theme::volume_slider),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let tracks = &self.player.info.audio_tracks;
        if tracks.len() > 1 {
            let options: Vec<Track> = tracks
                .iter()
                .enumerate()
                .map(|(i, n)| Track {
                    index: i,
                    name: if n.is_empty() {
                        format!("Track {}", i + 1)
                    } else {
                        n.clone()
                    },
                })
                .collect();
            let current = options.get(self.player.track).cloned();
            right = right.push(
                pick_list(options, current, Msg::Track)
                    .text_size(13)
                    .padding(Padding::from([6, 10]))
                    .style(theme::pick)
                    .menu_style(theme::menu),
            );
        }
        row![play, time, space::horizontal(), right]
            .spacing(14)
            .align_y(Alignment::Center)
            .into()
    }

    fn trim_panel(&self) -> Element<'_, Msg> {
        let readout = |label: &'static str, value: String, color: Color, on: Option<Msg>| {
            let body = column![
                text(label).size(11).color(theme::muted()),
                text(value).font(Font::MONOSPACE).size(14).color(color)
            ]
            .spacing(2);
            button(body)
                .padding(Padding::from([7, 9]))
                .width(Fill)
                .style(|t, s| {
                    let mut st = theme::segment(false)(t, s);
                    st.background = Some(
                        match s {
                            button::Status::Hovered => theme::surface_hi(),
                            _ => color_well(),
                        }
                        .into(),
                    );
                    st.border = iced::Border {
                        color: theme::edge(),
                        width: 1.0,
                        radius: 8.0.into(),
                    };
                    st
                })
                .on_press_maybe(on)
        };
        let len = self.kept_length();
        let cut_out = self.trim.1 - self.trim.0 - len;
        let body = column![
            row![
                readout(
                    "Start",
                    format_time(self.trim.0),
                    theme::GOLD,
                    Some(Msg::Seek(self.trim.0))
                ),
                readout(
                    "End",
                    format_time(self.trim.1),
                    theme::GOLD,
                    Some(Msg::Seek(self.trim.1))
                ),
                readout("Keeps", format!("{len:.1} s"), theme::ink(), None),
            ]
            .spacing(6),
            row![
                button(
                    row![text("Start here").size(12).font(MEDIUM), hint_key("I")]
                        .spacing(6)
                        .align_y(Alignment::Center)
                )
                .padding(Padding::from([6, 9]))
                .style(theme::secondary)
                .on_press(Msg::SetIn),
                button(
                    row![text("End here").size(12).font(MEDIUM), hint_key("O")]
                        .spacing(6)
                        .align_y(Alignment::Center)
                )
                .padding(Padding::from([6, 9]))
                .style(theme::secondary)
                .on_press(Msg::SetOut),
                button(text("Reset").size(12).font(MEDIUM))
                    .padding(Padding::from([6, 9]))
                    .style(theme::ghost)
                    .on_press_maybe(
                        (self.trimmed() || !self.splits.is_empty()).then_some(Msg::ResetTrim)
                    ),
            ]
            .spacing(6),
            row![
                button(
                    row![
                        icon(Icon::Scissors, 13.0, theme::ink_2()),
                        text("Split").size(12).font(MEDIUM),
                        hint_key("S")
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center)
                )
                .padding(Padding::from([6, 9]))
                .style(theme::secondary)
                .on_press(Msg::Split),
                button(
                    row![text("Cut this part").size(12).font(MEDIUM), hint_key("Del")]
                        .spacing(6)
                        .align_y(Alignment::Center)
                )
                .padding(Padding::from([6, 9]))
                .style(theme::secondary)
                .on_press_maybe((!self.splits.is_empty()).then_some(Msg::CutPart)),
            ]
            .spacing(6),
            text(if cut_out > 0.05 {
                format!(
                    "{} cut out from the middle. Ctrl+Z brings it back.",
                    seconds_text(cut_out)
                )
            } else {
                "Split at the playhead, then cut out the part you don't want.".to_string()
            })
            .size(11)
            .color(theme::muted()),
        ]
        .spacing(10);
        panel("Trim", body)
    }

    /// What's said in the clip, as captions: make them, fix them, burn them into copies.
    fn captions_panel(&self) -> Element<'_, Msg> {
        if let Some(job) = &self.caption_job {
            let (stage, p) = {
                let s = job.status.lock().unwrap();
                (s.stage.clone(), s.progress)
            };
            return panel(
                "Captions",
                column![
                    row![
                        text(if stage.is_empty() {
                            "Starting".into()
                        } else {
                            stage
                        })
                        .size(13)
                        .font(MEDIUM),
                        space::horizontal(),
                        text(format!("{:.0}%", p * 100.0))
                            .size(13)
                            .color(theme::ink_2())
                    ],
                    progress_bar(0.0..=1.0, p).girth(6).style(theme::progress),
                    button(text("Cancel").size(12).font(MEDIUM))
                        .padding(Padding::from([6, 10]))
                        .style(theme::ghost)
                        .on_press(Msg::CancelCaptions),
                ]
                .spacing(10),
            );
        }
        let make = |label: &'static str| {
            button(
                row![
                    icon(Icon::Sparkles, 14.0, theme::accent()),
                    text(label).size(12).font(MEDIUM)
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([6, 9]))
            .style(theme::secondary)
            .on_press(Msg::MakeCaptions)
        };
        if self.captions.is_empty() {
            let mut c = column![
                text("Subtitles of what's said, made on this computer (English).")
                    .size(12)
                    .color(theme::ink_2()),
                make("Make captions"),
            ]
            .spacing(8);
            if !crate::voice::installed() {
                c = c.push(
                    text("The first time, it downloads the speech files (about 55 MB).")
                        .size(11)
                        .color(theme::muted()),
                );
            }
            return panel("Captions", c);
        }
        let mut list = column![].spacing(5);
        for (i, cap) in self.captions.iter().enumerate() {
            list = list.push(
                row![
                    button(
                        text(format_time(cap.start))
                            .font(Font::MONOSPACE)
                            .size(11)
                            .color(theme::muted())
                    )
                    .padding(Padding::from([4, 4]))
                    .style(theme::ghost)
                    .on_press(Msg::Seek(cap.start)),
                    text_input("", &cap.text)
                        .on_input(move |t| Msg::CaptionText(i, t))
                        .size(12)
                        .padding(Padding::from([5, 7]))
                        .style(theme::input),
                    button(icon(Icon::Close, 12.0, theme::ink_2()))
                        .padding(5)
                        .style(theme::ghost)
                        .on_press(Msg::DeleteCaption(i)),
                ]
                .spacing(4)
                .align_y(Alignment::Center),
            );
        }
        panel(
            "Captions",
            column![
                list,
                row![
                    text("Burn into copies")
                        .size(12)
                        .color(theme::ink_2())
                        .width(Fill),
                    toggler(self.burn_captions)
                        .on_toggle(Msg::BurnCaptions)
                        .size(18)
                        .style(theme::toggle)
                ]
                .align_y(Alignment::Center),
                row![
                    make("Redo"),
                    button(text("Show .srt").size(12).font(MEDIUM))
                        .padding(Padding::from([6, 9]))
                        .style(theme::ghost)
                        .on_press(Msg::RevealSrt),
                ]
                .spacing(6),
            ]
            .spacing(10),
        )
    }

    /// Text: add it at the playhead, drag it on the picture, pick how it comes in and goes out.
    fn stamps_panel(&self) -> Element<'_, Msg> {
        let add = row![
            text_input("Type some text", &self.stamp_text)
                .on_input(Msg::StampText)
                .on_submit(Msg::AddText)
                .size(12)
                .padding(Padding::from([6, 8]))
                .style(theme::input),
            button(text("Add").size(12).font(MEDIUM))
                .padding(Padding::from([6, 10]))
                .style(theme::secondary)
                .on_press_maybe((!self.stamp_text.trim().is_empty()).then_some(Msg::AddText)),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let mut body = column![add].spacing(10);
        if !self.stamps.is_empty() {
            let mut list = column![].spacing(4);
            for (i, it) in self.stamps.iter().enumerate() {
                let picked = self.stamp_picked == Some(i);
                list = list.push(
                    row![
                        button(
                            row![
                                text(it.label())
                                    .size(12)
                                    .font(if picked { SEMIBOLD } else { MEDIUM })
                                    .wrapping(text::Wrapping::None)
                                    .width(Fill),
                                text(format!("{}-{}", format_time(it.start), format_time(it.end)))
                                    .font(Font::MONOSPACE)
                                    .size(10)
                                    .color(theme::muted()),
                            ]
                            .spacing(6)
                            .align_y(Alignment::Center),
                        )
                        .padding(Padding::from([5, 8]))
                        .width(Fill)
                        .style(if picked {
                            theme::secondary
                        } else {
                            theme::ghost
                        })
                        .on_press(Msg::PickStamp(if picked { None } else { Some(i) })),
                        button(icon(Icon::Close, 12.0, theme::ink_2()))
                            .padding(5)
                            .style(theme::ghost)
                            .on_press(Msg::DeleteStamp(i)),
                    ]
                    .spacing(2)
                    .align_y(Alignment::Center),
                );
            }
            body = body.push(list);
        }
        if let Some(it) = self.stamp_picked.and_then(|i| self.stamps.get(i)) {
            let mut edit = column![
                text_input("Text", &it.text)
                    .on_input(Msg::StampWords)
                    .size(12)
                    .padding(Padding::from([6, 8]))
                    .style(theme::input)
            ]
            .spacing(8);
            let small = |label: &'static str, m: Msg| {
                button(text(label).size(11).font(MEDIUM))
                    .padding(Padding::from([5, 8]))
                    .style(theme::secondary)
                    .on_press(m)
            };
            edit = edit.push(
                row![
                    small("Start here", Msg::StampStart),
                    small("End here", Msg::StampEnd),
                ]
                .spacing(6),
            );
            edit = edit.push(
                row![
                    text("Size").size(12).color(theme::ink_2()).width(44),
                    slider(0.03..=0.4, it.size, Msg::StampSize)
                        .step(0.005_f32)
                        .style(theme::volume_slider),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            );
            let mut swatches = row![].spacing(6);
            for (i, (name, rgb)) in COLORS.iter().enumerate() {
                let chosen = it.color == i;
                let c = Color::from_rgb8(rgb[0], rgb[1], rgb[2]);
                swatches = swatches.push(tip(
                    button(Space::new().width(18).height(18))
                        .padding(0)
                        .style(move |_, st| button::Style {
                            background: Some(c.into()),
                            border: iced::Border {
                                color: if chosen {
                                    theme::ink()
                                } else if matches!(st, button::Status::Hovered) {
                                    theme::alpha(theme::ink(), 0.5)
                                } else {
                                    theme::edge()
                                },
                                width: if chosen { 2.5 } else { 1.0 },
                                radius: 999.0.into(),
                            },
                            ..Default::default()
                        })
                        .on_press(Msg::StampColor(i)),
                    name,
                ));
            }
            edit = edit.push(swatches);
            let anim = ANIMS
                .iter()
                .map(|(id, _)| *id)
                .find(|id| *id == it.anim)
                .unwrap_or("pop");
            edit = edit.push(
                column![
                    text("Coming in and going out")
                        .size(12)
                        .color(theme::ink_2()),
                    segmented(
                        ANIMS.iter().map(|(id, n)| (*id, n.to_string())).collect(),
                        anim,
                        Msg::StampAnim,
                    ),
                ]
                .spacing(6),
            );
            body = body.push(container(edit).padding(Padding {
                top: 4.0,
                ..Default::default()
            }));
        }
        body = body.push(
            text(if self.stamps.is_empty() {
                "Added at the playhead for 3 seconds."
            } else {
                "Drag them on the picture to move them."
            })
            .size(11)
            .color(theme::muted()),
        );
        panel("Text", body)
    }

    /// Speed, look, fades, music and the mix: applied to every copy made below.
    fn style_panel(&self) -> Element<'_, Msg> {
        let eff = &self.effects;
        let small = |t: &'static str| text(t).size(12).color(theme::ink_2());
        let speeds = [0.25, 0.5, 1.0, 1.5, 2.0, 4.0];
        let speed_row = speeds.iter().fold(row![].spacing(4), |r, v| {
            let on = (eff.speed - v).abs() < 1e-6;
            r.push(
                button(text(format!("{v}x")).size(12).font(MEDIUM))
                    .padding(Padding::from([5, 7]))
                    .style(theme::segment(on))
                    .on_press(Msg::Speed(*v)),
            )
        });
        let look = pick_list(media::Look::ALL, Some(eff.look), Msg::Look)
            .text_size(13)
            .padding(Padding::from([6, 10]))
            .width(Fill)
            .style(theme::pick)
            .menu_style(theme::menu);
        let music: Element<'_, Msg> = match &eff.music {
            Some(m) => column![
                row![
                    icon(Icon::Music, 14.0, theme::accent()),
                    container(
                        text(
                            m.path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default()
                        )
                        .size(12)
                        .wrapping(text::Wrapping::None)
                    )
                    .width(Fill)
                    .clip(true),
                    button(icon(Icon::Close, 13.0, theme::ink_2()))
                        .padding(5)
                        .style(theme::ghost)
                        .on_press(Msg::ClearMusic),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
                slider(0.0..=1.0, m.volume, Msg::MusicVolume)
                    .step(0.01_f32)
                    .style(theme::volume_slider),
            ]
            .spacing(6)
            .into(),
            None => button(
                row![
                    icon(Icon::Music, 14.0, theme::ink_2()),
                    text("Add music").size(12).font(MEDIUM)
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([6, 9]))
            .style(theme::secondary)
            .on_press(Msg::PickMusic)
            .into(),
        };
        let toggle = |on: bool, label: &'static str, m: fn(bool) -> Msg| {
            row![
                text(label).size(12).color(theme::ink_2()).width(Fill),
                toggler(on).on_toggle(m).size(18).style(theme::toggle)
            ]
            .align_y(Alignment::Center)
        };
        let mut body = column![
            small("Speed"),
            speed_row,
            small("Look"),
            look,
            small("Music"),
            music,
            toggle(eff.fade, "Fade in and out", Msg::Fade),
            toggle(eff.even_loudness, "Even out loudness", Msg::EvenLoudness),
        ]
        .spacing(8);
        let tracks = &self.player.info.audio_tracks;
        if tracks.iter().any(|t| t == "Game") && tracks.iter().any(|t| t == "Microphone") {
            body = body
                .push(
                    row![
                        small("Game").width(70),
                        slider(0.0..=2.0, eff.game_volume, Msg::GameVolume)
                            .step(0.05_f32)
                            .style(theme::volume_slider)
                    ]
                    .align_y(Alignment::Center),
                )
                .push(
                    row![
                        small("Mic").width(70),
                        slider(0.0..=2.0, eff.mic_volume, Msg::MicVolume)
                            .step(0.05_f32)
                            .style(theme::volume_slider)
                    ]
                    .align_y(Alignment::Center),
                )
                .push(toggle(eff.mic_cleanup, "Clean up the mic", Msg::MicCleanup));
        }
        if tracks.iter().any(|t| t == "Voice chat") {
            body = body.push(
                row![
                    small("Chat").width(70),
                    slider(0.0..=2.0, eff.chat_volume, Msg::ChatVolume)
                        .step(0.05_f32)
                        .style(theme::volume_slider)
                ]
                .align_y(Alignment::Center),
            );
        }
        if !self.keys.is_empty() {
            body = body.push(
                row![
                    text("Show keys and clicks")
                        .size(12)
                        .color(theme::ink_2())
                        .width(Fill),
                    toggler(self.show_keys)
                        .on_toggle(Msg::ShowKeys)
                        .size(18)
                        .style(theme::toggle)
                ]
                .align_y(Alignment::Center),
            );
        }
        body = body.push(
            row![
                text(if eff.is_plain() {
                    "Applies to every copy you make below."
                } else {
                    "Every copy below gets this style."
                })
                .size(11)
                .color(theme::muted())
                .width(Fill),
                button(text("Reset").size(12).font(MEDIUM))
                    .padding(Padding::from([5, 8]))
                    .style(theme::ghost)
                    .on_press_maybe((!eff.is_plain()).then_some(Msg::ResetStyle)),
            ]
            .align_y(Alignment::Center),
        );
        panel("Style", body)
    }

    /// The marked moments: jump to one, keep the seconds around it.
    fn marks_panel(&self) -> Element<'_, Msg> {
        if self.markers.is_empty() {
            let while_playing = if self.mark_key.is_empty() {
                String::new()
            } else {
                format!(
                    " While you play, {} marks one for the next clip.",
                    self.mark_key
                )
            };
            return panel(
                "Moments",
                column![
                    text(format!(
                        "Mark the moments worth finding again.{while_playing}"
                    ))
                    .size(12)
                    .color(theme::muted()),
                    mark_button(),
                ]
                .spacing(10),
            );
        }
        let chips = self
            .markers
            .iter()
            .fold(row![].spacing(6), |r, t| {
                r.push(
                    button(
                        row![
                            icon(Icon::Flag, 12.0, theme::GOLD),
                            text(format_time(*t)).font(Font::MONOSPACE).size(12)
                        ]
                        .spacing(5)
                        .align_y(Alignment::Center),
                    )
                    .padding(Padding::from([5, 8]))
                    .style(theme::secondary)
                    .on_press(Msg::Seek(*t)),
                )
            })
            .wrap()
            .vertical_spacing(6);
        let body = column![
            chips,
            row![
                mark_button(),
                button(
                    text(format!("Keep {:.0} s around it", MARK_SPAN * 2.0))
                        .size(12)
                        .font(MEDIUM)
                )
                .padding(Padding::from([6, 9]))
                .style(theme::secondary)
                .on_press(Msg::TrimAroundMark),
                button(text("Clear").size(12).font(MEDIUM))
                    .padding(Padding::from([6, 9]))
                    .style(theme::ghost)
                    .on_press(Msg::ClearMarks),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        ]
        .spacing(10);
        panel("Moments", body)
    }

    /// Tags: the clip's, to take off, a field to add one, and tags from other clips to add with
    /// a click.
    fn tags_panel<'a>(
        &'a self,
        tags: &'a [String],
        known: Vec<String>,
        folders: Vec<String>,
        folder: Option<String>,
    ) -> Element<'a, Msg> {
        let chip = |t: &str, on: Msg, add: bool| {
            button(
                row![
                    icon(
                        if add { Icon::Tag } else { Icon::Close },
                        12.0,
                        if add { theme::muted() } else { theme::ink_2() }
                    ),
                    text(t.to_string()).size(12).font(MEDIUM),
                ]
                .spacing(5)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([4, 9]))
            .style(if add { theme::ghost } else { theme::secondary })
            .on_press(on)
        };
        let mut c = column![].spacing(10);
        if !tags.is_empty() {
            let mut on = row![].spacing(6);
            for t in tags {
                on = on.push(chip(t, Msg::RemoveTag(t.clone()), false));
            }
            c = c.push(on.wrap().vertical_spacing(6));
        }
        c = c.push(
            text_input("Add a tag, then Enter", &self.tag_text)
                .on_input(Msg::TagText)
                .on_submit(Msg::AddTag)
                .size(13)
                .padding(Padding::from([7, 9]))
                .style(theme::input),
        );
        let typed = self.tag_text.trim().to_lowercase();
        let others: Vec<String> = known
            .into_iter()
            .filter(|t| !tags.contains(t) && t.to_lowercase().contains(&typed))
            .take(8)
            .collect();
        if !others.is_empty() {
            let mut more = row![].spacing(6);
            for t in others {
                more = more.push(chip(&t, Msg::AddTagNamed(t.clone()), true));
            }
            c = c.push(more.wrap().vertical_spacing(6));
        }
        let picks: Vec<super::FolderPick> = std::iter::once(super::FolderPick(None))
            .chain(folders.into_iter().map(|f| super::FolderPick(Some(f))))
            .collect();
        c = c.push(
            row![
                icon(Icon::Folder, 14.0, theme::muted()),
                text("Folder").size(13).color(theme::ink_2()),
                Space::new().width(Fill),
                pick_list(picks, Some(super::FolderPick(folder)), Msg::SetFolder)
                    .text_size(13)
                    .padding(Padding::from([6, 10]))
                    .style(theme::pick)
                    .menu_style(theme::menu),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
        panel("Tags and folder", c)
    }

    /// Sending to the connected Discord channel.
    fn channel_view(&self, name: String, sending: Option<f32>) -> Element<'_, Msg> {
        if let Some(p) = sending {
            return column![
                row![
                    text("Sending to the channel").size(13).font(MEDIUM),
                    space::horizontal(),
                    text(format!("{:.0}%", p * 100.0))
                        .size(12)
                        .color(theme::muted())
                ],
                progress_bar(0.0..=1.0, p).girth(6).style(theme::progress),
                button(text("Cancel").size(12).font(MEDIUM))
                    .padding(Padding::from([6, 10]))
                    .style(theme::ghost)
                    .on_press(Msg::CancelSend),
            ]
            .spacing(10)
            .into();
        }
        if self.send_after_export && self.job.is_some() {
            return text("Making the copy for the channel...")
                .size(13)
                .color(theme::ink_2())
                .into();
        }
        let span = if self.trimmed() {
            self.trim.1 - self.trim.0
        } else {
            self.player.duration()
        };
        let fits = media::share_plan(
            self.discord_mb as u64 * 1_000_000,
            span,
            &self.player.info,
            1.0,
        );
        column![
            button(
                row![
                    icon(Icon::Send, 16.0, theme::ink()),
                    text("Send to Discord channel").font(SEMIBOLD).size(14)
                ]
                .spacing(8)
                .align_y(Alignment::Center)
            )
            .width(Fill)
            .padding(Padding::from([10, 12]))
            .style(theme::secondary)
            .on_press_maybe((fits.is_ok() && self.job.is_none()).then_some(Msg::SendToChannel)),
            text(match &fits {
                Ok(_) => format!(
                    "Posts a copy under {} MB{} as {name}.",
                    self.discord_mb,
                    if self.trimmed() {
                        " of the trimmed part"
                    } else {
                        ""
                    }
                ),
                Err(e) => e.clone(),
            })
            .size(12)
            .color(theme::muted()),
        ]
        .spacing(8)
        .into()
    }

    fn share_panel<'a>(&'a self, share: ShareView) -> Element<'a, Msg> {
        let channel = share
            .channel
            .clone()
            .map(|name| self.channel_view(name, share.sending));
        let preparing = self.share_after_export && self.job.is_some();
        let body: Element<'a, Msg> = if let Some((label, p)) = share.progress {
            column![
                row![
                    text(label).size(13).font(MEDIUM),
                    space::horizontal(),
                    text(if p < 0.34 {
                        "Making a copy for the link"
                    } else {
                        "Sending"
                    })
                    .size(12)
                    .color(theme::muted())
                ],
                progress_bar(0.0..=1.0, p).girth(6).style(theme::progress),
                button(text("Cancel").size(12).font(MEDIUM))
                    .padding(Padding::from([6, 10]))
                    .style(theme::ghost)
                    .on_press(Msg::CancelLink),
            ]
            .spacing(10)
            .into()
        } else if preparing {
            text("Making the trimmed part to share...")
                .size(13)
                .color(theme::ink_2())
                .into()
        } else if let Some(link) = share.link {
            let url = link.url.clone();
            column![
                container(
                    text(link.url.clone())
                        .size(13)
                        .font(MEDIUM)
                        .wrapping(text::Wrapping::None)
                )
                .padding(Padding::from([8, 10]))
                .width(Fill)
                .clip(true)
                .style(theme::well),
                row![
                    button(
                        row![
                            icon(Icon::Copy, 15.0, Color::WHITE),
                            text("Copy link").size(13).font(SEMIBOLD)
                        ]
                        .spacing(7)
                        .align_y(Alignment::Center)
                    )
                    .padding(Padding::from([8, 12]))
                    .style(theme::primary)
                    .on_press(Msg::CopyLink(url.clone())),
                    tip(
                        icon_only(Icon::Qr, theme::ink_2())
                            .style(theme::secondary)
                            .on_press(Msg::ShowQr),
                        "QR code"
                    ),
                    tip(
                        icon_only(Icon::Globe, theme::ink_2())
                            .style(theme::secondary)
                            .on_press(Msg::OpenLink(url)),
                        "Open in browser"
                    ),
                ]
                .spacing(6),
                text(match link.expires {
                    None => "Anyone with the link can watch.".to_string(),
                    Some(t) => format!(
                        "Anyone with the link can watch, for {} more.",
                        human_hours(t.saturating_sub(crate::share::now_secs()))
                    ),
                })
                .size(12)
                .color(theme::muted()),
            ]
            .spacing(10)
            .into()
        } else {
            let lasts = "It works for 2 days.";
            column![
                button(
                    row![
                        icon(Icon::Share, 16.0, Color::WHITE),
                        text("Get a share link").font(SEMIBOLD).size(14)
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center)
                )
                .width(Fill)
                .padding(Padding::from([10, 12]))
                .style(theme::primary)
                .on_press_maybe(self.job.is_none().then_some(Msg::GetLink)),
                text(format!(
                    "Uploads {} and copies a link anyone can watch in their browser. {lasts}",
                    if self.trimmed() {
                        "the trimmed part"
                    } else {
                        "the clip"
                    }
                ))
                .size(12)
                .color(theme::muted()),
            ]
            .spacing(8)
            .into()
        };
        let body: Element<'a, Msg> = if share.friends {
            column![
                body,
                button(
                    row![
                        icon(Icon::Send, 15.0, theme::ink()),
                        text("Send to friends").font(SEMIBOLD).size(14)
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center)
                )
                .width(Fill)
                .padding(Padding::from([9, 12]))
                .style(theme::secondary)
                .on_press(Msg::SendToFriends),
            ]
            .spacing(10)
            .into()
        } else {
            body
        };
        match channel {
            Some(c) => panel("Share", column![body, divider(), c].spacing(14)),
            None => panel("Share", body),
        }
    }

    fn export_panel(&self) -> Element<'_, Msg> {
        let body: Element<'_, Msg> = if let Some(job) = &self.job {
            let (p, stage) = {
                let s = job.status.lock().unwrap();
                (s.progress, s.stage.clone())
            };
            column![
                row![
                    text(stage).size(13).font(MEDIUM),
                    space::horizontal(),
                    text(format!("{:.0}%", p * 100.0))
                        .size(13)
                        .color(theme::ink_2())
                ],
                progress_bar(0.0..=1.0, p).girth(6).style(theme::progress),
                button(text("Cancel").size(12).font(MEDIUM))
                    .padding(Padding::from([6, 10]))
                    .style(theme::ghost)
                    .on_press(Msg::CancelExport),
            ]
            .spacing(10)
            .into()
        } else {
            let span = self.kept_length();
            let limit = self.discord_mb as u64 * 1_000_000;
            let fits = media::share_plan(limit, span, &self.player.info, 1.0);
            let share_hint = match &fits {
                Ok(_) => format!(
                    "A copy under {} MB{}, copied so you can paste it straight into a chat.",
                    self.discord_mb,
                    if self.trimmed() {
                        " of the trimmed part"
                    } else {
                        ""
                    }
                ),
                Err(e) => e.clone(),
            };
            let wide = |b: iced::widget::Button<'static, Msg>| {
                b.width(Fill).padding(Padding::from([10, 12]))
            };
            let mut c = column![
                wide(button(
                    row![
                        icon(Icon::Send, 16.0, Color::WHITE),
                        text("Share to Discord").font(SEMIBOLD).size(14)
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center)
                ))
                .style(theme::discord)
                .on_press_maybe(fits.is_ok().then_some(Msg::Export(ExportKind::Share))),
                text(share_hint).size(12).color(theme::muted()),
                Space::new().height(4),
                wide(button(
                    row![
                        icon(Icon::Scissors, 16.0, theme::ink()),
                        text(if !self.styled() {
                            "Save trimmed copy"
                        } else {
                            "Save edited copy"
                        })
                        .font(SEMIBOLD)
                        .size(14)
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center)
                ))
                .style(theme::secondary)
                .on_press_maybe(
                    (self.trimmed() || self.styled()).then_some(Msg::Export(ExportKind::Trim))
                ),
                text(if self.styled() {
                    "A copy with your style, saved next to this clip."
                } else if self.trimmed() {
                    "An exact cut at full quality, saved next to this clip."
                } else {
                    "Drag the handles under the video to choose the part to keep."
                })
                .size(12)
                .color(theme::muted()),
            ]
            .spacing(8);
            if self.trimmed() && !self.styled() {
                c = c.push(
                    row![
                        button(
                            row![
                                icon(Icon::Sparkles, 14.0, theme::accent()),
                                text("Instant copy").size(12).font(MEDIUM)
                            ]
                            .spacing(6)
                            .align_y(Alignment::Center)
                        )
                        .padding(Padding::from([6, 9]))
                        .style(theme::secondary)
                        .on_press(Msg::Export(ExportKind::Copy)),
                        text("No encoding, done in a second. May start up to 1 s early.")
                            .size(11)
                            .color(theme::muted())
                            .width(Fill),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                );
            }
            c = c.push(
                column![
                    Space::new().height(4),
                    text("More formats")
                        .size(12)
                        .font(SEMIBOLD)
                        .color(theme::ink_2()),
                    row![
                        format_button(
                            Icon::Image,
                            "GIF",
                            "A looping GIF, 480 px wide (15 s at most)",
                            Msg::Export(ExportKind::Gif)
                        ),
                        format_button(
                            Icon::Phone,
                            "9:16",
                            "Vertical for TikTok, Shorts and Reels",
                            Msg::Export(ExportKind::Vertical)
                        ),
                        format_button(
                            Icon::Square,
                            "1:1",
                            "Square for feeds",
                            Msg::Export(ExportKind::Square)
                        ),
                        format_button(
                            Icon::Portrait,
                            "4:5",
                            "Tall for Instagram and X feeds",
                            Msg::Export(ExportKind::Portrait)
                        ),
                        format_button(
                            Icon::Snail,
                            "0.5x",
                            "Slow motion, half speed",
                            Msg::Export(ExportKind::Slow(2))
                        ),
                        format_button(
                            Icon::Snail,
                            "0.25x",
                            "Slow motion, quarter speed",
                            Msg::Export(ExportKind::Slow(4))
                        ),
                    ]
                    .spacing(6)
                    .wrap()
                    .vertical_spacing(6),
                ]
                .spacing(8),
            );
            if let Some((path, copied)) = &self.last_export {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                c = c.push(
                    container(
                        row![
                            icon(Icon::Check, 16.0, theme::GREEN),
                            column![
                                text(if *copied {
                                    "Copied to the clipboard"
                                } else {
                                    "Saved"
                                })
                                .size(13)
                                .font(SEMIBOLD),
                                text(name).size(11).color(theme::ink_2()),
                            ]
                            .spacing(2)
                            .width(Fill),
                            tip(
                                icon_only(Icon::Share, theme::ink_2())
                                    .style(theme::ghost)
                                    .on_press(Msg::ShareExport),
                                "Get a share link"
                            ),
                            button(text("Show").size(12).font(MEDIUM))
                                .padding(Padding::from([5, 9]))
                                .style(theme::ghost)
                                .on_press(Msg::ShowExport),
                        ]
                        .spacing(10)
                        .align_y(Alignment::Center),
                    )
                    .padding(10)
                    .style(theme::chip_box(theme::GREEN)),
                );
            }
            c.into()
        };
        panel("Export", body)
    }

    fn details_panel(&self) -> Element<'_, Msg> {
        let i: &Info = &self.player.info;
        let size = std::fs::metadata(&self.clip)
            .map(|m| human_size(m.len()))
            .unwrap_or_default();
        let sound = if i.audio_tracks.is_empty() {
            "None".to_string()
        } else {
            i.audio_tracks
                .iter()
                .enumerate()
                .map(|(n, t)| {
                    if t.is_empty() {
                        format!("Track {}", n + 1)
                    } else {
                        t.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let line = |k: &'static str, v: String| {
            row![
                text(k).size(12).color(theme::muted()).width(64),
                text(v).size(12).color(theme::ink_2())
            ]
            .spacing(8)
        };
        panel(
            "Details",
            column![
                line(
                    "Picture",
                    format!("{} x {}, {:.0} fps", i.width, i.height, i.fps)
                ),
                line(
                    "Quality",
                    i.video_bps
                        .map_or("-".into(), |b| format!("{:.1} Mbit/s", b as f64 / 1e6))
                ),
                line("Sound", sound),
                line("File", size),
            ]
            .spacing(6),
        )
    }
}

/// The recessed fields of the editor's panels (trim times and the like).
fn color_well() -> Color {
    if theme::is_light() {
        theme::surface_hi()
    } else {
        iced::color!(0x0f1013)
    }
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// A video frame on screen, as horizontal bands of under 2 MB each. iced puts a picture on the
/// GPU right away only when it's under 2 MB; bigger ones (a 720p frame is 3.7 MB) arrive a frame
/// late, so every new frame first drew as nothing and playback flickered. Bands go up at once.
struct Picture {
    size: (u32, u32),
    /// Each band and how many rows it has.
    bands: Vec<(image::Handle, u32)>,
}

/// Under iced's limit for pictures it uploads at once (2 MiB).
const BAND_BYTES: usize = 2_000_000;

impl Picture {
    fn new((w, h): (u32, u32), rgba: &[u8]) -> Picture {
        let row = (w as usize * 4).max(1);
        let rows_per = (BAND_BYTES / row).max(1);
        let n = (h as usize).div_ceil(rows_per).max(1);
        // Even bands, so none is a sliver.
        let per = (h as usize).div_ceil(n).max(1);
        let bands = rgba
            .chunks(per * row)
            .map(|c| {
                let rows = (c.len() / row) as u32;
                (image::Handle::from_rgba(w, rows, c.to_vec()), rows)
            })
            .collect();
        Picture {
            size: (w, h),
            bands,
        }
    }

    /// The bands stacked into one picture, as big as fits (like `ContentFit::Contain`).
    fn view<'a>(&self) -> Element<'a, Msg> {
        let (w, h) = (self.size.0.max(1) as f32, self.size.1.max(1) as f32);
        let bands = self.bands.clone();
        iced::widget::responsive(move |space| {
            let k = (space.width / w).min(space.height / h);
            let width = (w * k).floor();
            let mut col = column![].width(width);
            for (handle, rows) in &bands {
                col = col.push(
                    image(handle.clone())
                        .content_fit(ContentFit::Fill)
                        .width(width)
                        .height(*rows as f32 * k),
                );
            }
            container(col).center(Fill).into()
        })
        .into()
    }
}

/// "4.2 s", "1 min 05 s".
fn seconds_text(s: f64) -> String {
    if s >= 60.0 {
        format!("{} min {:02.0} s", (s / 60.0).floor(), s % 60.0)
    } else {
        format!("{s:.1} s")
    }
}

/// "3 days", "5 hours".
fn human_hours(secs: u64) -> String {
    let h = secs.div_ceil(3600);
    if h > 36 {
        format!("{} days", h.div_ceil(24))
    } else {
        format!("{h} hour{}", if h == 1 { "" } else { "s" })
    }
}

/// A small button for one export format, with an explanation on hover.
fn format_button<'a>(i: Icon, label: &'a str, explain: &'a str, m: Msg) -> Element<'a, Msg> {
    tip(
        button(
            row![
                icon(i, 14.0, theme::ink_2()),
                text(label).size(12).font(SEMIBOLD)
            ]
            .spacing(5)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([6, 8]))
        .style(theme::secondary)
        .on_press(m),
        explain,
    )
}

pub(super) fn filmstrip(
    ff: &Path,
    clip: &Path,
    size: (u32, u32),
    duration: f64,
) -> Vec<image::Handle> {
    let Ok(mut child) = ffmpeg::command(ff)
        .args(media::filmstrip_args(
            clip,
            size,
            media::strip_step(duration),
        ))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    // Deleting the clip stops it (it reads the whole file, which takes a while for a big one).
    let out = child.stdout.take();
    let reading = ffmpeg::Reading::new(clip, child);
    let mut bytes = Vec::new();
    if let Some(mut out) = out {
        let _ = out.read_to_end(&mut bytes);
    }
    let _ = reading.wait();
    let n = size.0 as usize * size.1 as usize * 4;
    bytes
        .chunks_exact(n)
        .map(|c| image::Handle::from_rgba(size.0, size.1, c.to_vec()))
        .collect()
}

/// The part of the timeline the filmstrip fills, in the canvas's coordinates.
fn track(bounds: Rectangle) -> Rectangle {
    Rectangle {
        x: HANDLE_W,
        y: EDGE,
        width: (bounds.width - 2.0 * HANDLE_W).max(1.0),
        height: (bounds.height - 2.0 * EDGE).max(1.0),
    }
}

/// The timeline's pictures: as many keyframes as fit at the video's shape.
struct Strip<'a> {
    strip: &'a [image::Handle],
    aspect: f32,
}

impl canvas::Program<Msg> for Strip<'_> {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut f = Frame::new(renderer, bounds.size());
        let track = track(bounds);
        f.fill(
            &Shape::rounded_rectangle(track.position(), track.size(), 8.0.into()),
            color_well(),
        );
        if !self.strip.is_empty() {
            let tile_w = (track.height * self.aspect).max(8.0);
            let n = ((track.width / tile_w).round() as usize).max(1);
            let w = track.width / n as f32;
            for i in 0..n {
                let src = &self.strip[(i * self.strip.len() / n).min(self.strip.len() - 1)];
                let r = Rectangle {
                    x: track.x + i as f32 * w,
                    y: track.y,
                    width: w + 0.5,
                    height: track.height,
                };
                let mut img = iced::advanced::image::Image::from(src);
                if i == 0 {
                    img.border_radius = iced::border::left(8.0);
                }
                if i == n - 1 {
                    img.border_radius = img.border_radius.right(8.0);
                }
                f.draw_image(r, img);
            }
        }
        vec![f.into_geometry()]
    }
}

/// Text on the monitor.
struct Shown {
    index: usize,
    handle: image::Handle,
    /// Its picture's size, drawn for a `STAMP_REF_H`-tall picture.
    size: (u32, u32),
    x: f32,
    y: f32,
    pose: overlays::Pose,
    /// Picked but not showing at the playhead.
    dim: bool,
}

/// Text over the picture, where it can be picked and dragged.
struct StampLayer {
    items: Vec<Shown>,
    video: (f32, f32),
    picked: Option<usize>,
}

impl StampLayer {
    /// Where the picture is within the monitor (it's fitted in the middle).
    fn picture(&self, bounds: Rectangle) -> Rectangle {
        let k = (bounds.width / self.video.0).min(bounds.height / self.video.1);
        let (w, h) = ((self.video.0 * k).floor(), self.video.1 * k);
        Rectangle {
            x: (bounds.width - w) / 2.0,
            y: (bounds.height - h) / 2.0,
            width: w,
            height: h,
        }
    }

    /// An item's place on the monitor: settled, or as its pose has it.
    fn rect(&self, pic: Rectangle, s: &Shown, posed: bool) -> Rectangle {
        let k = pic.height / STAMP_REF_H as f32;
        let p = if posed {
            s.pose
        } else {
            overlays::Pose::SETTLED
        };
        let (w, h) = (s.size.0 as f32 * k * p.scale, s.size.1 as f32 * k * p.scale);
        let (cx, cy) = (
            pic.x + (s.x + p.shift) * pic.width,
            pic.y + s.y * pic.height,
        );
        Rectangle {
            x: cx - w / 2.0,
            y: cy - h / 2.0,
            width: w,
            height: h,
        }
    }

    fn under(&self, bounds: Rectangle, p: Point) -> Option<&Shown> {
        let pic = self.picture(bounds);
        self.items
            .iter()
            .rev()
            .find(|s| self.rect(pic, s, false).contains(p))
    }
}

impl canvas::Program<Msg> for StampLayer {
    /// The item being dragged, and where in it it was grabbed (shares of the picture).
    type State = Option<(usize, f32, f32)>;

    fn update(
        &self,
        state: &mut Self::State,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Msg>> {
        let pic = self.picture(bounds);
        match event {
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(bounds)?;
                let s = self.under(bounds, p)?;
                *state = Some((
                    s.index,
                    (p.x - pic.x) / pic.width - s.x,
                    (p.y - pic.y) / pic.height - s.y,
                ));
                Some(Action::publish(Msg::PickStamp(Some(s.index))).and_capture())
            }
            iced::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let (i, ox, oy) = (*state)?;
                let p = cursor.position()?;
                let (x, y) = (p.x - bounds.x, p.y - bounds.y);
                Some(
                    Action::publish(Msg::MoveStamp(
                        i,
                        (x - pic.x) / pic.width - ox,
                        (y - pic.y) / pic.height - oy,
                    ))
                    .and_capture(),
                )
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.take()?;
                Some(Action::publish(Msg::DropStamp).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _: &Self::State,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut f = Frame::new(renderer, bounds.size());
        let pic = self.picture(bounds);
        for s in &self.items {
            let mut img = iced::advanced::image::Image::from(&s.handle);
            img.opacity = s.pose.alpha * if s.dim { 0.45 } else { 1.0 };
            if img.opacity > 0.0 && s.pose.scale > 0.0 {
                f.draw_image(self.rect(pic, s, true), img);
            }
            if self.picked == Some(s.index) {
                let r = self.rect(pic, s, false);
                f.stroke(
                    &Shape::rectangle(
                        Point::new(r.x - 4.0, r.y - 4.0),
                        Size::new(r.width + 8.0, r.height + 8.0),
                    ),
                    Stroke {
                        line_dash: canvas::LineDash {
                            segments: &[5.0, 4.0],
                            offset: 0,
                        },
                        ..Stroke::default()
                            .with_color(theme::accent())
                            .with_width(1.5)
                    },
                );
            }
        }
        vec![f.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.is_some() {
            return mouse::Interaction::Grabbing;
        }
        match cursor.position_in(bounds) {
            Some(p) if self.under(bounds, p).is_some() => mouse::Interaction::Grab,
            _ => mouse::Interaction::default(),
        }
    }
}

/// The timeline's marks: the trim handles, the dimmed parts that are cut, the playhead and
/// the time under the pointer. Drag anywhere to scrub, drag a handle to trim.
struct Timeline {
    duration: f64,
    trim: (f64, f64),
    position: f64,
    grab: Option<Grab>,
    markers: Vec<f64>,
    splits: Vec<f64>,
    removed: Vec<(f64, f64)>,
}

impl Timeline {
    fn track(&self, bounds: Rectangle) -> Rectangle {
        track(bounds)
    }

    fn x_at(&self, track: Rectangle, t: f64) -> f32 {
        track.x + (t / self.duration.max(0.001)).clamp(0.0, 1.0) as f32 * track.width
    }

    fn t_at(&self, track: Rectangle, x: f32) -> f64 {
        ((x - track.x) / track.width).clamp(0.0, 1.0) as f64 * self.duration
    }

    fn near(&self, track: Rectangle, x: f32) -> Option<Grab> {
        let (xi, xo) = (self.x_at(track, self.trim.0), self.x_at(track, self.trim.1));
        if (x - (xi - HANDLE_W / 2.0)).abs() <= HANDLE_W {
            Some(Grab::In)
        } else if (x - (xo + HANDLE_W / 2.0)).abs() <= HANDLE_W {
            Some(Grab::Out)
        } else {
            None
        }
    }
}

impl canvas::Program<Msg> for Timeline {
    type State = ();

    fn update(
        &self,
        _: &mut (),
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Msg>> {
        let track = self.track(bounds);
        match event {
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(bounds)?;
                let grab = self.near(track, p.x).unwrap_or(Grab::Seek);
                Some(Action::publish(Msg::Grab(grab, self.t_at(track, p.x))).and_capture())
            }
            iced::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if self.grab.is_some() {
                    let p = cursor.position()?;
                    Some(Action::publish(Msg::Drag(self.t_at(track, p.x - bounds.x))).and_capture())
                } else if cursor.is_over(bounds) {
                    Some(Action::request_redraw())
                } else {
                    None
                }
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if self.grab.is_some() =>
            {
                Some(Action::publish(Msg::Release).and_capture())
            }
            iced::Event::Mouse(mouse::Event::CursorLeft) => Some(Action::request_redraw()),
            _ => None,
        }
    }

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut f = Frame::new(renderer, bounds.size());
        let track = self.track(bounds);
        let (xi, xo) = (self.x_at(track, self.trim.0), self.x_at(track, self.trim.1));
        let shade = theme::alpha(Color::BLACK, 0.68);
        if xi > track.x {
            f.fill_rectangle(
                Point::new(track.x, track.y),
                Size::new(xi - track.x, track.height),
                shade,
            );
        }
        if xo < track.x + track.width {
            f.fill_rectangle(
                Point::new(xo, track.y),
                Size::new(track.x + track.width - xo, track.height),
                shade,
            );
        }
        // The kept part: framed in the accent, with a handle at each end.
        let frame_top = Shape::rectangle(Point::new(xi, track.y - 3.0), Size::new(xo - xi, 3.0));
        let frame_bottom = Shape::rectangle(
            Point::new(xi, track.y + track.height),
            Size::new(xo - xi, 3.0),
        );
        f.fill(&frame_top, theme::accent());
        f.fill(&frame_bottom, theme::accent());
        for (x, left) in [(xi - HANDLE_W, true), (xo, false)] {
            let r = Rectangle {
                x,
                y: track.y - 3.0,
                width: HANDLE_W,
                height: track.height + 6.0,
            };
            let radius = if left {
                iced::border::left(6.0)
            } else {
                iced::border::right(6.0)
            };
            f.fill(
                &Shape::rounded_rectangle(r.position(), r.size(), radius),
                theme::accent(),
            );
            let cx = r.x + r.width / 2.0;
            for dx in [-2.0, 2.0] {
                let grip = Shape::line(
                    Point::new(cx + dx, r.center_y() - 7.0),
                    Point::new(cx + dx, r.center_y() + 7.0),
                );
                f.stroke(
                    &grip,
                    Stroke::default()
                        .with_color(theme::alpha(Color::WHITE, 0.85))
                        .with_width(1.5),
                );
            }
        }

        // Marked moments: a gold pennant on a thin line.
        for m in &self.markers {
            let x = self.x_at(track, *m);
            f.fill_rectangle(
                Point::new(x - 1.0, track.y),
                Size::new(2.0, track.height),
                theme::alpha(theme::GOLD, 0.85),
            );
            let flag = Shape::new(|p| {
                p.move_to(Point::new(x, track.y + 2.0));
                p.line_to(Point::new(x + 10.0, track.y + 6.5));
                p.line_to(Point::new(x, track.y + 11.0));
                p.close();
            });
            f.fill(&flag, theme::GOLD);
        }

        // Where the pointer is, and its time.
        if self.grab.is_none()
            && let Some(p) = cursor.position_in(bounds)
            && p.x >= track.x
            && p.x <= track.x + track.width
            && self.near(track, p.x).is_none()
        {
            f.fill_rectangle(
                Point::new(p.x - 0.5, track.y),
                Size::new(1.0, track.height),
                theme::alpha(Color::WHITE, 0.45),
            );
            let label = format_time(self.t_at(track, p.x));
            let w = 8.0 + label.len() as f32 * 7.0;
            let x = (p.x - w / 2.0).clamp(track.x, track.x + track.width - w);
            f.fill(
                &Shape::rounded_rectangle(
                    Point::new(x, track.y + 4.0),
                    Size::new(w, 18.0),
                    5.0.into(),
                ),
                theme::alpha(Color::BLACK, 0.8),
            );
            f.fill_text(canvas::Text {
                content: label,
                position: Point::new(x + w / 2.0, track.y + 13.0),
                color: theme::ink(),
                size: 11.0.into(),
                font: Font::MONOSPACE,
                align_x: iced::widget::text::Alignment::Center,
                align_y: iced::alignment::Vertical::Center,
                ..Default::default()
            });
        }

        // Parts cut out: darker, crossed through; splits: a thin white line.
        for &(a, b) in &self.removed {
            let (xa, xb) = (self.x_at(track, a), self.x_at(track, b));
            if xb - xa < 1.0 {
                continue;
            }
            f.fill_rectangle(
                Point::new(xa, track.y),
                Size::new(xb - xa, track.height),
                theme::alpha(Color::BLACK, 0.78),
            );
            for (from, to) in [
                (
                    Point::new(xa, track.y),
                    Point::new(xb, track.y + track.height),
                ),
                (
                    Point::new(xa, track.y + track.height),
                    Point::new(xb, track.y),
                ),
            ] {
                f.stroke(
                    &Shape::line(from, to),
                    Stroke::default()
                        .with_color(theme::alpha(Color::WHITE, 0.35))
                        .with_width(1.0),
                );
            }
        }
        for &s in &self.splits {
            if s <= self.trim.0 || s >= self.trim.1 {
                continue;
            }
            let x = self.x_at(track, s);
            f.fill_rectangle(
                Point::new(x - 1.0, track.y),
                Size::new(2.0, track.height),
                theme::alpha(Color::WHITE, 0.9),
            );
        }

        // The playhead.
        let xp = self.x_at(track, self.position);
        f.fill_rectangle(
            Point::new(xp - 1.0, 1.0),
            Size::new(2.0, bounds.height - 2.0),
            Color::WHITE,
        );
        f.fill(&Shape::circle(Point::new(xp, 4.0), 4.5), Color::WHITE);
        vec![f.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _: &(),
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        match self.grab {
            Some(Grab::In | Grab::Out) => return mouse::Interaction::ResizingHorizontally,
            Some(Grab::Seek) => return mouse::Interaction::Grabbing,
            None => {}
        }
        match cursor.position_in(bounds) {
            Some(p) if self.near(self.track(bounds), p.x).is_some() => {
                mouse::Interaction::ResizingHorizontally
            }
            Some(_) => mouse::Interaction::Pointer,
            None => mouse::Interaction::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_split_into_bands_iced_uploads_at_once() {
        for (w, h) in [
            (1280, 720),
            (1920, 1080),
            (2560, 1440),
            (640, 360),
            (3, 1),
            (0, 0),
        ] {
            let rgba = vec![7u8; w as usize * h as usize * 4];
            let p = Picture::new((w, h), &rgba);
            let rows: u32 = p.bands.iter().map(|b| b.1).sum();
            assert_eq!(rows, h, "{w}x{h}");
            for (_, r) in &p.bands {
                assert!(w as usize * *r as usize * 4 <= BAND_BYTES, "{w}x{h}");
            }
        }
        // A 720p frame (3.7 MB) is two even halves.
        let p = Picture::new((1280, 720), &vec![0; 1280 * 720 * 4]);
        assert_eq!(p.bands.iter().map(|b| b.1).collect::<Vec<_>>(), [360, 360]);
    }
}
