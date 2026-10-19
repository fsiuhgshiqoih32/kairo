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
    slider, space, stack, text, text_input,
};
use iced::{
    Alignment, Color, ContentFit, Element, Fill, Font, Length, Padding, Point, Rectangle, Renderer,
    Size, Task, Theme,
};

use rewindr::args::Encoder;
use rewindr::library;
use rewindr::media::{self, Info, format_time};

use super::icons::{Icon, icon, icon_filled};
use super::library::{ClipItem, tip};
use super::theme::{self, BOLD, MEDIUM, MUTED, SEMIBOLD, TEXT, TEXT_2};
use super::widgets::{hint_key, icon_button, icon_only, panel};
use super::{background, human_size};
use crate::export::{self, Job, Kind};
use crate::player::Player;
use crate::{ffmpeg, platform};

/// Frames are decoded for playback at most this big.
const PLAYBACK_MAX: (u32, u32) = (1280, 720);
/// Height of the filmstrip pictures, in pixels.
const STRIP_H: u32 = 60;
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
    ShareExport,
    CancelLink,
    CopyLink(String),
    OpenLink(String),
    ShowQr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    Trim,
    Share,
    Gif,
    Vertical,
    Slow(u32),
}

/// The window's view of the clip's share link, for the Share panel.
pub struct ShareView {
    pub progress: Option<(String, f32)>,
    pub link: Option<rewindr::library::Link>,
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
    share_expiry: String,
    /// Upload the trimmed copy once it's made.
    share_after_export: bool,
    /// The file the Share panel is about: the clip, or a trimmed copy made for sharing.
    shared_file: Option<PathBuf>,
}

impl Editor {
    pub fn open(
        clip: PathBuf,
        settings: &rewindr::config::Settings,
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
            let (ff, clip) = (ff.clone(), clip.clone());
            Task::perform(
                background(move || filmstrip(&ff, &clip, (w, STRIP_H))),
                Msg::Strip,
            )
        };
        let duration = info.duration;
        let name = stem(&clip);
        Ok((
            Editor {
                player: Player::new(ff.clone(), clip.clone(), info, PLAYBACK_MAX),
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
                share_expiry: settings.share_expiry.clone(),
                share_after_export: false,
                shared_file: None,
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
        self.strip = filmstrip(&self.ff, &self.clip, (w, STRIP_H));
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
        self.player.is_playing() || self.player.has_pending_still() || self.job.is_some()
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
        match key.as_ref() {
            Key::Named(Named::Space) => self.toggle_play(),
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
            Key::Character("i") | Key::Character("I") => self.set_in(pos),
            Key::Character("o") | Key::Character("O") => self.set_out(pos),
            _ => return false,
        }
        true
    }

    pub fn update(&mut self, msg: Msg) -> Effect {
        match msg {
            Msg::Frame => {
                let end = self.trim.1;
                let was_playing = self.player.is_playing();
                self.player.tick(end);
                if let Some(f) = self.player.frame.take() {
                    self.frame = Some(Picture::new(self.player.size, &f.rgba));
                    self.frames_shown += 1;
                }
                if was_playing && !self.player.is_playing() && self.looping && self.grab.is_none() {
                    self.player.position = self.trim.0;
                    self.player.play();
                }
                if let Some(e) = self.player.error.take() {
                    return Effect::Note(e, false);
                }
                return self.poll_job();
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
            Msg::ResetTrim => self.trim = (0.0, self.player.duration()),
            Msg::Loop => self.looping = !self.looping,
            Msg::Volume(v) => self.player.set_volume(v),
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
            Msg::GetLink => {
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
                let _ = std::fs::rename(
                    crate::save::thumb_for(&self.clip),
                    crate::save::thumb_for(&to),
                );
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
            ExportKind::Share => Kind::Share(self.discord_mb as u64 * 1_000_000),
            ExportKind::Gif => Kind::Gif,
            ExportKind::Vertical => Kind::Vertical,
            ExportKind::Slow(f) => Kind::Slow(f),
        };
        self.share_after_export = false;
        // The trimmed part; untrimmed, that's the whole clip.
        let range = if self.trimmed() {
            self.trim
        } else {
            (0.0, self.player.duration())
        };
        if let Kind::Share(limit) = kind
            && let Err(e) = media::share_plan(limit, range.1 - range.0, &self.player.info, 1.0)
        {
            return Effect::Note(e, false);
        }
        if kind == Kind::Gif && range.1 - range.0 > export::GIF_MAX_SECONDS + 0.05 {
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
                let copied = matches!(kind, Kind::Share(_))
                    && platform::copy_files(std::slice::from_ref(&path)).is_ok();
                self.last_export = Some((path.clone(), copied));
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
                Effect::Note("Export cancelled.".into(), true)
            }
            Some(Err(e)) => Effect::Note(e, false),
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

    pub fn view<'a>(&'a self, item: Option<&'a ClipItem>, share: ShareView) -> Element<'a, Msg> {
        let favorite = item.is_some_and(|c| c.favorite);
        let star = if favorite {
            icon_filled(Icon::Star, 16.0, theme::GOLD)
        } else {
            icon(Icon::Star, 16.0, TEXT_2)
        };
        let head = row![
            tip(
                button(
                    row![
                        icon(Icon::Back, 16.0, TEXT_2),
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
            icon_button(Icon::Copy, "Copy", TEXT_2)
                .style(theme::ghost)
                .on_press(Msg::Copy),
            icon_button(Icon::Folder, "Show", TEXT_2)
                .style(theme::ghost)
                .on_press(Msg::Reveal),
            tip(
                icon_only(Icon::External, TEXT_2)
                    .style(theme::ghost)
                    .on_press(Msg::External),
                "Open in your video player"
            ),
            tip(
                icon_only(Icon::Trash, TEXT_2)
                    .style(theme::danger_ghost)
                    .on_press(Msg::AskDelete),
                "Delete"
            ),
        ]
        .spacing(4)
        .align_y(Alignment::Center);

        let picture: Element<'a, Msg> = match &self.frame {
            Some(p) => p.view(),
            None => text("Loading...").color(MUTED).into(),
        };
        let mut monitor = stack![container(picture).center(Fill)];
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
                        color: theme::BORDER,
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
                })
                .width(Fill)
                .height(TIMELINE_H),
            ],
            row![
                hint_key("Space"),
                text("play").size(12).color(MUTED),
                Space::new().width(6),
                hint_key("I"),
                hint_key("O"),
                text("start and end").size(12).color(MUTED),
                Space::new().width(6),
                hint_key(","),
                hint_key("."),
                text("one frame").size(12).color(MUTED),
                Space::new().width(6),
                hint_key("Shift"),
                text("+ arrows: 5 s").size(12).color(MUTED),
            ]
            .spacing(5)
            .align_y(Alignment::Center),
        ]
        .spacing(12)
        .width(Fill);

        let side = scrollable(
            column![
                self.share_panel(share),
                self.trim_panel(),
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
                Color::WHITE,
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
                .color(TEXT),
            text(format!(" / {}", format_time(self.player.duration())))
                .font(Font::MONOSPACE)
                .size(14)
                .color(MUTED),
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
                        TEXT_2
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
                TEXT_2
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
                text(label).size(11).color(MUTED),
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
                            button::Status::Hovered => theme::SURFACE_HI,
                            _ => color_well(),
                        }
                        .into(),
                    );
                    st.border = iced::Border {
                        color: theme::BORDER,
                        width: 1.0,
                        radius: 8.0.into(),
                    };
                    st
                })
                .on_press_maybe(on)
        };
        let len = self.trim.1 - self.trim.0;
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
                readout("Keeps", format!("{len:.1} s"), TEXT, None),
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
                    .on_press_maybe(self.trimmed().then_some(Msg::ResetTrim)),
            ]
            .spacing(6),
        ]
        .spacing(10);
        panel("Trim", body)
    }

    fn share_panel<'a>(&'a self, share: ShareView) -> Element<'a, Msg> {
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
                    .color(MUTED)
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
                .color(TEXT_2)
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
                        icon_only(Icon::Qr, TEXT_2)
                            .style(theme::secondary)
                            .on_press(Msg::ShowQr),
                        "QR code"
                    ),
                    tip(
                        icon_only(Icon::Globe, TEXT_2)
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
                .color(MUTED),
            ]
            .spacing(10)
            .into()
        } else {
            let lasts = match self.share_expiry.as_str() {
                "forever" => "It stays up while people watch it.",
                "72h" => "It works for 3 days.",
                "24h" => "It works for a day.",
                "12h" => "It works for 12 hours.",
                _ => "It works for an hour.",
            };
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
                .color(MUTED),
            ]
            .spacing(8)
            .into()
        };
        panel("Share", body)
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
                    text(format!("{:.0}%", p * 100.0)).size(13).color(TEXT_2)
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
            let span = if self.trimmed() {
                self.trim.1 - self.trim.0
            } else {
                self.player.duration()
            };
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
                text(share_hint).size(12).color(MUTED),
                Space::new().height(4),
                wide(button(
                    row![
                        icon(Icon::Scissors, 16.0, TEXT),
                        text("Save trimmed copy").font(SEMIBOLD).size(14)
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center)
                ))
                .style(theme::secondary)
                .on_press_maybe(self.trimmed().then_some(Msg::Export(ExportKind::Trim))),
                text(if self.trimmed() {
                    "An exact cut at full quality, saved next to this clip."
                } else {
                    "Drag the handles under the video to choose the part to keep."
                })
                .size(12)
                .color(MUTED),
                Space::new().height(4),
                text("More formats").size(12).font(SEMIBOLD).color(TEXT_2),
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
                .spacing(6),
            ]
            .spacing(8);
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
                                text(name).size(11).color(TEXT_2),
                            ]
                            .spacing(2)
                            .width(Fill),
                            tip(
                                icon_only(Icon::Share, TEXT_2)
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
                text(k).size(12).color(MUTED).width(64),
                text(v).size(12).color(TEXT_2)
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

fn color_well() -> Color {
    iced::color!(0x0f1013)
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
            row![icon(i, 14.0, TEXT_2), text(label).size(12).font(SEMIBOLD)]
                .spacing(5)
                .align_y(Alignment::Center),
        )
        .padding(Padding::from([6, 8]))
        .style(theme::secondary)
        .on_press(m),
        explain,
    )
}

pub(super) fn filmstrip(ff: &Path, clip: &Path, size: (u32, u32)) -> Vec<image::Handle> {
    let Ok(mut child) = ffmpeg::command(ff)
        .args(media::filmstrip_args(clip, size))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    if let Some(out) = child.stdout.as_mut() {
        let _ = out.read_to_end(&mut bytes);
    }
    let _ = child.wait();
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

/// The timeline's marks: the trim handles, the dimmed parts that are cut, the playhead and
/// the time under the pointer. Drag anywhere to scrub, drag a handle to trim.
struct Timeline {
    duration: f64,
    trim: (f64, f64),
    position: f64,
    grab: Option<Grab>,
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
                color: TEXT,
                size: 11.0.into(),
                font: Font::MONOSPACE,
                align_x: iced::widget::text::Alignment::Center,
                align_y: iced::alignment::Vertical::Center,
                ..Default::default()
            });
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
