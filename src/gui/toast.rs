//! The "clip saved" card in the corner of the screen: a thumbnail, the clip's name, and buttons
//! to open it, get a share link, or copy it. It runs as its own short-lived process
//! (`kairo notify`), so the recorder never waits on a window. It appears without taking focus
//! from a game.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::widget::{
    Space, button, column, container, image, mouse_area, progress_bar, row, stack, text,
};
use iced::window;
use iced::{
    Alignment, Color, ContentFit, Element, Fill, Length, Padding, Point, Size, Subscription, Task,
};

use super::icons::{Icon, icon, icon_filled};
use super::theme::{self, BOLD, MEDIUM};
use crate::{platform, share};

/// Wide enough for the picture and three buttons ("Copy" was cut off at 380).
pub(super) const W: f32 = 400.0;
pub(super) const H: f32 = 112.0;
const MARGIN: f32 = 16.0;
const SHOW_FOR: Duration = Duration::from_secs(6);
/// How long a press of the close button waits for the second press that stops an upload.
const STOP_WITHIN: Duration = Duration::from_secs(5);
const STOP_ASK: &str = "Close again to stop the upload.";

pub enum Message {
    /// With `share`, the clip is uploaded straight away and its link copied.
    Saved {
        clip: PathBuf,
        seconds: f64,
        share: bool,
    },
    Screenshot(PathBuf),
    Failed(String),
    /// A newer version is out; `about` is one line on what's in it.
    Update {
        version: String,
        about: Option<String>,
    },
    /// A friend sent a message or a clip (`link`), or asked to be friends (`request`).
    Friend {
        from: String,
        text: String,
        link: Option<String>,
        request: bool,
    },
}

pub(super) struct Toast {
    msg: Message,
    thumb: Option<image::Handle>,
    size: String,
    shown: Instant,
    hovered: bool,
    status: Option<(String, bool)>,
    /// Runs in this process: closing the card ends it, so the card stays until it's done.
    upload: Option<share::Upload>,
    /// When close was pressed during the upload; a second press soon after stops it.
    stopping: Option<Instant>,
    id: Option<window::Id>,
}

#[derive(Debug, Clone)]
pub(super) enum Msg {
    Opened(window::Id),
    Scale(f32),
    Raw(u64),
    Tick,
    Hover(bool),
    Open,
    Link,
    Copy,
    /// Open the window on the update and install it.
    UpdateNow,
    /// Play a friend's clip in the browser.
    Watch,
    Close,
}

pub fn run(msg: Message) -> Result<(), String> {
    if platform::is_wayland() {
        return system_notification(&msg);
    }
    if let Ok(s) = kairo::config::load(&kairo::config::default_path()) {
        theme::set_accent(&s.accent);
        theme::apply_look(&s.look);
    }
    let mut settings = window::Settings {
        size: Size::new(W, H),
        resizable: false,
        decorations: false,
        level: window::Level::AlwaysOnTop,
        visible: false,
        exit_on_close_request: true,
        icon: window::icon::from_rgba(kairo::art::app_icon(64), 64, 64).ok(),
        ..Default::default()
    };
    #[cfg(windows)]
    {
        settings.platform_specific.skip_taskbar = true;
    }
    #[cfg(not(windows))]
    {
        let _ = &mut settings;
    }
    let msg = std::cell::Cell::new(Some(msg));
    let mut app = iced::application(
        move || {
            let m = msg.take().expect("toast built once");
            (Toast::new(m), Task::none())
        },
        Toast::update,
        Toast::view,
    )
    .title("Kairo")
    .theme(|_: &Toast| theme::theme())
    .subscription(Toast::subscription)
    .default_font(theme::FONT)
    .window(settings);
    for f in theme::FONTS {
        app = app.font(f);
    }
    app.run().map_err(|e| e.to_string())
}

/// A picture for the card, decoded now (small, so it's on screen at once).
fn picture(path: &Path) -> Option<image::Handle> {
    let i = ::image::open(path).ok()?.thumbnail(480, 270).to_rgba8();
    Some(image::Handle::from_rgba(
        i.width(),
        i.height(),
        i.into_raw(),
    ))
}

impl Toast {
    pub(super) fn new(msg: Message) -> Toast {
        let size_of = |p: &Path| {
            std::fs::metadata(p)
                .map(|m| super::human_size(m.len()))
                .unwrap_or_default()
        };
        let (thumb, size) = match &msg {
            Message::Saved { clip, .. } => (picture(&crate::save::thumb_for(clip)), size_of(clip)),
            Message::Screenshot(p) => (picture(p), size_of(p)),
            Message::Failed(_) | Message::Update { .. } | Message::Friend { .. } => {
                (None, String::new())
            }
        };
        let share = matches!(msg, Message::Saved { share: true, .. });
        let mut toast = Toast {
            msg,
            thumb,
            size,
            shown: Instant::now(),
            hovered: false,
            status: None,
            upload: None,
            stopping: None,
            id: None,
        };
        if share {
            toast.start_upload();
        }
        toast
    }

    /// Uploads the file and copies its link when it's done.
    fn start_upload(&mut self) {
        if self.upload.is_none()
            && let Some(f) = self.file()
        {
            self.upload = Some(share::start(f, f, None));
            self.status = None;
        }
    }

    /// The file the card is about.
    fn file(&self) -> Option<&Path> {
        match &self.msg {
            Message::Saved { clip, .. } => Some(clip),
            Message::Screenshot(p) => Some(p),
            Message::Failed(_) | Message::Update { .. } | Message::Friend { .. } => None,
        }
    }

    fn subscription(&self) -> Subscription<Msg> {
        let every = if self.upload.is_some() { 100 } else { 250 };
        Subscription::batch([
            window::open_events().map(Msg::Opened),
            iced::time::every(Duration::from_millis(every)).map(|_| Msg::Tick),
        ])
    }

    fn update(&mut self, m: Msg) -> Task<Msg> {
        match m {
            Msg::Opened(id) => {
                self.id = Some(id);
                return window::scale_factor(id).map(Msg::Scale);
            }
            Msg::Scale(k) => {
                let Some(id) = self.id else {
                    return Task::none();
                };
                // The bottom-right corner of the main screen's work area (above the taskbar).
                let (x, y) = match platform::work_area() {
                    Some((ax, ay, aw, ah)) => (
                        (ax as f32 + aw as f32) / k - W - MARGIN,
                        (ay as f32 + ah as f32) / k - H - MARGIN,
                    ),
                    None => (0.0, 0.0),
                };
                let place = window::move_to(id, Point::new(x, y));
                return place.chain(window::raw_id::<Msg>(id).map(Msg::Raw));
            }
            Msg::Raw(raw) => {
                self.shown = Instant::now();
                if !platform::show_without_focus(raw)
                    && let Some(id) = self.id
                {
                    return window::set_mode(id, window::Mode::Windowed);
                }
            }
            Msg::Tick => {
                if let Some(up) = &self.upload {
                    self.shown = Instant::now();
                    if self.stopping.is_some_and(|t| t.elapsed() > STOP_WITHIN) {
                        self.stopping = None;
                        if self.status.as_ref().is_some_and(|(s, _)| s == STOP_ASK) {
                            self.status = None;
                        }
                    }
                    if let Some(result) = up.take() {
                        self.upload = None;
                        self.stopping = None;
                        match result {
                            Ok(link) => {
                                self.status =
                                    Some(("Link copied! Paste it anywhere.".into(), true));
                                return iced::clipboard::write(link.url);
                            }
                            // The whole reason is in the log; the card has room for a line.
                            Err(e) => self.status = Some((share::brief(&e), false)),
                        }
                    }
                    return Task::none();
                }
                // A failed share stays longer, with Get link to try again.
                let limit = if matches!(self.msg, Message::Update { .. }) {
                    SHOW_FOR * 4
                } else if matches!(self.msg, Message::Failed(_))
                    || matches!(self.status, Some((_, false)))
                {
                    SHOW_FOR * 2
                } else {
                    SHOW_FOR
                };
                if self.hovered {
                    self.shown = Instant::now();
                } else if self.shown.elapsed() > limit {
                    return iced::exit();
                }
            }
            Msg::Hover(h) => self.hovered = h,
            Msg::Open => {
                match &self.msg {
                    // The link is made in this process: the card stays until it's done.
                    Message::Saved { clip, .. } if self.upload.is_some() => {
                        crate::app::open_clip(clip);
                        self.status = Some(("Opened. The link is on its way.".into(), true));
                        return Task::none();
                    }
                    Message::Screenshot(p) if self.upload.is_some() => {
                        crate::app::open(p);
                        self.status = Some(("Opened. The link is on its way.".into(), true));
                        return Task::none();
                    }
                    Message::Saved { clip, .. } => crate::app::open_clip(clip),
                    Message::Screenshot(p) => crate::app::open(p),
                    Message::Failed(_) => {}
                    Message::Update { .. } => return self.update(Msg::UpdateNow),
                    Message::Friend { .. } => {
                        if let Err(e) = crate::app::spawn_self(&["window", "--friends"]) {
                            crate::log::warning!("{e}");
                        }
                    }
                }
                return iced::exit();
            }
            Msg::Watch => {
                if let Message::Friend {
                    link: Some(url), ..
                } = &self.msg
                    && crate::friends::is_clip_link(url)
                {
                    let _ = opener::open_browser(url);
                }
                return iced::exit();
            }
            Msg::Link => self.start_upload(),
            Msg::UpdateNow => {
                if let Err(e) = crate::app::spawn_self(&["window", "--update"]) {
                    crate::log::warning!("{e}");
                }
                return iced::exit();
            }
            Msg::Copy => {
                if let Some(f) = self.file() {
                    self.status = Some(match platform::copy_files(&[f.to_path_buf()]) {
                        Ok(()) => ("Copied. Paste it anywhere.".into(), true),
                        Err(e) => (e, false),
                    });
                    self.shown = Instant::now();
                }
            }
            Msg::Close => {
                // Closing ends the upload with this process: the first press says so.
                if let Some(up) = &self.upload {
                    if !self.stopping.is_some_and(|t| t.elapsed() <= STOP_WITHIN) {
                        self.stopping = Some(Instant::now());
                        self.status = Some((STOP_ASK.into(), false));
                        return Task::none();
                    }
                    up.cancel();
                }
                return iced::exit();
            }
        }
        Task::none()
    }

    pub(super) fn view(&self) -> Element<'_, Msg> {
        let close = button(icon(Icon::Close, 14.0, theme::muted()))
            .padding(4)
            .style(theme::ghost)
            .on_press(Msg::Close);
        let dot = |c: Color| {
            container(Space::new().width(8).height(8)).style(move |_| container::Style {
                background: Some(c.into()),
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
        };
        let body: Element<'_, Msg> = match &self.msg {
            Message::Update { version, about } => {
                let small = |label: &'static str, m: Msg, primary: bool| {
                    button(text(label).size(12).font(MEDIUM))
                        .padding(Padding::from([5, 10]))
                        .style(if primary {
                            theme::primary
                        } else {
                            theme::secondary
                        })
                        .on_press(m)
                };
                let logo = image(image::Handle::from_rgba(96, 96, kairo::art::app_icon(96)))
                    .width(48)
                    .height(48);
                row![
                    container(logo).center_y(Fill),
                    column![
                        row![
                            dot(theme::accent()),
                            text(format!("Kairo {version} is ready"))
                                .font(BOLD)
                                .size(14),
                            Space::new().width(Fill),
                            close
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                        text(match about {
                            Some(a) => format!("New: {a}"),
                            None => "A new version is out.".to_string(),
                        })
                        .size(12)
                        .color(theme::ink_2()),
                        Space::new().height(Fill),
                        row![
                            small("Update now", Msg::UpdateNow, true),
                            small("Later", Msg::Close, false),
                        ]
                        .spacing(5),
                    ]
                    .spacing(3)
                    .width(Fill),
                ]
                .spacing(14)
                .into()
            }
            Message::Friend {
                from,
                text: said,
                link,
                request,
            } => {
                let small = |label: &'static str, m: Msg, primary: bool| {
                    button(text(label).size(12).font(MEDIUM))
                        .padding(Padding::from([5, 10]))
                        .style(if primary {
                            theme::primary
                        } else {
                            theme::secondary
                        })
                        .on_press(m)
                };
                let badge = container(icon(
                    match (link.is_some(), *request) {
                        (true, _) => Icon::Film,
                        (false, true) => Icon::Users,
                        (false, false) => Icon::Send,
                    },
                    20.0,
                    theme::on_accent(),
                ))
                .center(Length::Fixed(48.0))
                .style(|_| container::Style {
                    background: Some(theme::accent().into()),
                    border: iced::Border {
                        radius: 24.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                });
                let buttons = match (link.is_some(), *request) {
                    (true, _) => row![
                        small("Watch", Msg::Watch, true),
                        small("Reply", Msg::Open, false),
                    ],
                    (false, true) => row![
                        small("Open Kairo", Msg::Open, true),
                        small("Later", Msg::Close, false),
                    ],
                    (false, false) => row![
                        small("Reply", Msg::Open, true),
                        small("Later", Msg::Close, false),
                    ],
                };
                row![
                    container(badge).center_y(Fill),
                    column![
                        row![
                            dot(theme::accent()),
                            text(from.clone()).font(BOLD).size(14),
                            Space::new().width(Fill),
                            close
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                        text(said.clone()).size(12).color(theme::ink_2()),
                        Space::new().height(Fill),
                        buttons.spacing(5),
                    ]
                    .spacing(3)
                    .width(Fill),
                ]
                .spacing(14)
                .into()
            }
            Message::Failed(e) => column![
                row![
                    dot(theme::accent()),
                    text("Couldn't save").font(BOLD).size(14),
                    Space::new().width(Fill),
                    close
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                text(e.clone()).size(12).color(theme::ink_2()),
            ]
            .spacing(6)
            .into(),
            m => {
                let (title, name, detail) = match m {
                    Message::Saved { clip, seconds, .. } => (
                        "Clip saved",
                        clip.file_stem()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        format!("{} long, {}", super::duration(Some(*seconds)), self.size),
                    ),
                    Message::Screenshot(p) => (
                        "Screenshot saved",
                        p.file_stem()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        self.size.clone(),
                    ),
                    Message::Failed(_) | Message::Update { .. } | Message::Friend { .. } => {
                        unreachable!()
                    }
                };
                let picture: Element<'_, Msg> = match &self.thumb {
                    Some(h) => image(h.clone())
                        .content_fit(ContentFit::Cover)
                        .width(Fill)
                        .height(Fill)
                        .border_radius(8.0)
                        .into(),
                    None => container(icon(Icon::Film, 22.0, theme::muted()))
                        .center(Fill)
                        .style(theme::well)
                        .into(),
                };
                // A play button on clips only: a screenshot just opens.
                let play = matches!(self.msg, Message::Saved { .. }).then(|| {
                    container(
                        container(icon_filled(Icon::Play, 14.0, Color::WHITE))
                            .center(Length::Fixed(30.0))
                            .style(|_| container::Style {
                                background: Some(theme::alpha(Color::BLACK, 0.55).into()),
                                border: iced::Border {
                                    radius: 15.0.into(),
                                    ..Default::default()
                                },
                                ..Default::default()
                            }),
                    )
                    .center(Fill)
                });
                let thumb = mouse_area(stack![picture].push(play))
                    .on_press(Msg::Open)
                    .interaction(iced::mouse::Interaction::Pointer);
                let bottom: Element<'_, Msg> = if let Some(up) = &self.upload {
                    column![
                        text(up.label()).size(12).color(theme::ink_2()),
                        progress_bar(0.0..=1.0, up.progress())
                            .girth(5)
                            .style(theme::progress),
                    ]
                    .spacing(5)
                    .into()
                } else {
                    let small = |i: Icon, label: &'static str, m: Msg, primary: bool| {
                        button(
                            row![
                                icon(
                                    i,
                                    13.0,
                                    if primary {
                                        Color::WHITE
                                    } else {
                                        theme::ink_2()
                                    }
                                ),
                                text(label).size(12).font(MEDIUM)
                            ]
                            .spacing(5)
                            .align_y(Alignment::Center),
                        )
                        .padding(Padding::from([5, 8]))
                        .style(if primary {
                            theme::primary
                        } else {
                            theme::secondary
                        })
                        .on_press(m)
                    };
                    row![
                        small(Icon::Play, "Open", Msg::Open, false),
                        small(Icon::Share, "Get link", Msg::Link, true),
                        small(Icon::Copy, "Copy", Msg::Copy, false),
                    ]
                    .spacing(5)
                    .into()
                };
                let (line, ok) = self.status.clone().unwrap_or((detail, true));
                row![
                    container(thumb).width(Length::Fixed(140.0)).height(Fill),
                    column![
                        row![
                            dot(theme::GREEN),
                            text(title).font(BOLD).size(14),
                            Space::new().width(Fill),
                            close
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                        container(
                            text(name)
                                .size(12)
                                .color(theme::ink())
                                .wrapping(text::Wrapping::None)
                        )
                        .clip(true),
                        // One line: the card's height is fixed, and a longer one would push the
                        // buttons out.
                        container(
                            text(line)
                                .size(11)
                                .wrapping(text::Wrapping::None)
                                .color(if ok { theme::muted() } else { theme::accent() })
                        )
                        .clip(true),
                        Space::new().height(Fill),
                        bottom,
                    ]
                    .spacing(3)
                    .width(Fill),
                ]
                .spacing(12)
                .into()
            }
        };
        mouse_area(
            container(body)
                .padding(10)
                .width(Fill)
                .height(Fill)
                .style(|t| {
                    // As round as the window: Windows 11 rounds it 8 px (`show_without_focus`).
                    let mut s = theme::toast(t);
                    s.border.radius = 8.0.into();
                    s
                }),
        )
        .on_enter(Msg::Hover(true))
        .on_exit(Msg::Hover(false))
        .into()
    }
}

/// Wayland won't let an app place a window in a corner; use the desktop's notifications.
fn system_notification(msg: &Message) -> Result<(), String> {
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    let (title, body, icon) = match msg {
        Message::Update { version, about } => (
            format!("Kairo {version} is ready"),
            about
                .clone()
                .map(|a| format!("New: {a}. Open Kairo to update."))
                .unwrap_or_else(|| "Open Kairo to update.".to_string()),
            None,
        ),
        Message::Saved { clip, seconds, .. } => (
            "Clip saved".to_string(),
            format!("{} ({seconds:.0} s)", name(clip)),
            Some(crate::save::thumb_for(clip)),
        ),
        Message::Screenshot(p) => ("Screenshot saved".to_string(), name(p), Some(p.clone())),
        Message::Failed(e) => ("Couldn't save".to_string(), e.clone(), None),
        Message::Friend { from, text, .. } => (from.clone(), text.clone(), None),
    };
    let mut cmd = std::process::Command::new("notify-send");
    cmd.arg("--app-name=Kairo");
    if let Some(i) = icon.filter(|p| p.exists()) {
        cmd.arg(format!("--icon={}", i.display()));
    }
    cmd.arg(title).arg(body);
    cmd.status()
        .map(|_| ())
        .map_err(|e| format!("notify-send: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved() -> Toast {
        Toast::new(Message::Saved {
            clip: PathBuf::from("VALORANT 2026-10-22 16-43-40.mp4"),
            seconds: 62.0,
            share: false,
        })
    }

    #[test]
    fn closing_during_an_upload_asks_first() {
        let mut t = saved();
        t.upload = Some(share::Upload::frozen(Path::new("a.mp4"), 300, 0, 1));
        let _ = t.update(Msg::Close);
        assert!(t.upload.is_some(), "the first press only warns");
        assert_eq!(t.status, Some((STOP_ASK.into(), false)));
        let _ = t.update(Msg::Close);
        let up = t.upload.as_ref().unwrap();
        assert_eq!(
            up.take(),
            Some(Err("Cancelled".into())),
            "the second stops it"
        );
    }

    /// A share that ends without a link says so in the card's one line, and the buttons come
    /// back for another go.
    #[test]
    fn a_stopped_share_says_so() {
        let mut t = saved();
        t.upload = Some(share::Upload::frozen(Path::new("a.mp4"), UPLOADING, 0, 1));
        t.upload.as_ref().unwrap().cancel();
        let _ = t.update(Msg::Tick);
        assert!(t.upload.is_none());
        assert_eq!(t.status, Some(("Stopped.".into(), false)));
    }

    /// What `share::Upload` keeps in its copy progress once the copy's done.
    const UPLOADING: u64 = u64::MAX;

    /// The card while a link is made, when it fails and when it's asked to close, drawn without
    /// a window: `KAIRO_SNAPSHOTS=<dir> cargo test -- --ignored toast::tests::sharing_pictures`.
    #[test]
    #[ignore]
    fn sharing_pictures() {
        let dir = PathBuf::from(std::env::var("KAIRO_SNAPSHOTS").expect("set KAIRO_SNAPSHOTS"));
        std::fs::create_dir_all(&dir).unwrap();
        let shoot = |t: &Toast, name: &str| {
            let settings = iced::Settings {
                fonts: theme::FONTS
                    .iter()
                    .map(|f| std::borrow::Cow::Borrowed(*f))
                    .collect(),
                default_font: theme::FONT,
                ..Default::default()
            };
            // A picture already there is compared, not replaced.
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                if e.file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{name}-"))
                {
                    let _ = std::fs::remove_file(e.path());
                }
            }
            let mut ui = iced_test::Simulator::with_size(settings, (W, H), t.view());
            let snap = ui.snapshot(&theme::theme()).expect("snapshot");
            snap.matches_image(dir.join(format!("{name}.png")))
                .expect("write png");
        };
        let mut t = saved();
        t.upload = Some(share::Upload::frozen(Path::new("a.mp4"), 420, 0, 1));
        shoot(&t, "toast-preparing");
        t.upload = Some(share::Upload::frozen(Path::new("a.mp4"), UPLOADING, 9, 10));
        shoot(&t, "toast-uploading");
        let _ = t.update(Msg::Close);
        shoot(&t, "toast-close-again");
        t.upload = None;
        t.stopping = None;
        t.status = Some((
            share::brief(
                "litterbox.catbox.moe isn't working right now (error 502). Kairo tried every \
                 file host it uses. Check your internet connection or try again in a few minutes.",
            ),
            false,
        ));
        shoot(&t, "toast-failed");
        // Whatever comes, the line stays one line.
        t.status = Some(("x ".repeat(80), false));
        shoot(&t, "toast-long-line");
    }
}
