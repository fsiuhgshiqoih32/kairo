//! The "clip saved" card in the corner of the screen: a thumbnail, the clip's name, and buttons
//! to open it, get a share link, or copy it. It runs as its own short-lived process
//! (`rewindr notify`), so the recorder never waits on a window. It appears without taking focus
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
use super::theme::{self, BOLD, MEDIUM, MUTED, TEXT, TEXT_2};
use crate::{platform, share};

const W: f32 = 380.0;
const H: f32 = 112.0;
const MARGIN: f32 = 16.0;
const SHOW_FOR: Duration = Duration::from_secs(6);

pub enum Message {
    Saved { clip: PathBuf, seconds: f64 },
    Screenshot(PathBuf),
    Failed(String),
}

pub(super) struct Toast {
    msg: Message,
    thumb: Option<image::Handle>,
    size: String,
    shown: Instant,
    hovered: bool,
    status: Option<(String, bool)>,
    upload: Option<share::Upload>,
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
    Close,
}

pub fn run(msg: Message) -> Result<(), String> {
    if platform::is_wayland() {
        return system_notification(&msg);
    }
    let mut settings = window::Settings {
        size: Size::new(W, H),
        resizable: false,
        decorations: false,
        level: window::Level::AlwaysOnTop,
        visible: false,
        exit_on_close_request: true,
        icon: window::icon::from_rgba(rewindr::art::app_icon(64), 64, 64).ok(),
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
    .title("Rewindr")
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
            Message::Failed(_) => (None, String::new()),
        };
        Toast {
            msg,
            thumb,
            size,
            shown: Instant::now(),
            hovered: false,
            status: None,
            upload: None,
            id: None,
        }
    }

    /// The file the card is about.
    fn file(&self) -> Option<&Path> {
        match &self.msg {
            Message::Saved { clip, .. } => Some(clip),
            Message::Screenshot(p) => Some(p),
            Message::Failed(_) => None,
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
                    if let Some(result) = up.take() {
                        self.upload = None;
                        match result {
                            Ok(link) => {
                                self.status =
                                    Some(("Link copied! Paste it anywhere.".into(), true));
                                return iced::clipboard::write(link.url);
                            }
                            Err(e) => self.status = Some((e, false)),
                        }
                    }
                    return Task::none();
                }
                let limit = if matches!(self.msg, Message::Failed(_)) {
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
                    Message::Saved { clip, .. } => crate::app::open_clip(clip),
                    Message::Screenshot(p) => crate::app::open(p),
                    Message::Failed(_) => {}
                }
                return iced::exit();
            }
            Msg::Link => {
                if self.upload.is_none()
                    && let Some(f) = self.file()
                {
                    let (settings, _) =
                        rewindr::config::load_or_create(&rewindr::config::default_path());
                    self.upload = Some(share::start(f, f, &settings.share_expiry, None));
                    self.status = None;
                }
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
                if let Some(up) = &self.upload {
                    up.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                return iced::exit();
            }
        }
        Task::none()
    }

    pub(super) fn view(&self) -> Element<'_, Msg> {
        let close = button(icon(Icon::Close, 14.0, MUTED))
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
            Message::Failed(e) => column![
                row![
                    dot(theme::ACCENT),
                    text("Couldn't save").font(BOLD).size(14),
                    Space::new().width(Fill),
                    close
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                text(e.clone()).size(12).color(TEXT_2),
            ]
            .spacing(6)
            .into(),
            m => {
                let (title, name, detail) = match m {
                    Message::Saved { clip, seconds } => (
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
                    Message::Failed(_) => unreachable!(),
                };
                let picture: Element<'_, Msg> = match &self.thumb {
                    Some(h) => image(h.clone())
                        .content_fit(ContentFit::Cover)
                        .width(Fill)
                        .height(Fill)
                        .border_radius(8.0)
                        .into(),
                    None => container(icon(Icon::Film, 22.0, MUTED))
                        .center(Fill)
                        .style(theme::well)
                        .into(),
                };
                let thumb = mouse_area(stack![
                    picture,
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
                            })
                    )
                    .center(Fill),
                ])
                .on_press(Msg::Open)
                .interaction(iced::mouse::Interaction::Pointer);
                let bottom: Element<'_, Msg> = if let Some(up) = &self.upload {
                    column![
                        text(up.label()).size(12).color(TEXT_2),
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
                                icon(i, 13.0, if primary { Color::WHITE } else { TEXT_2 }),
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
                                .color(TEXT)
                                .wrapping(text::Wrapping::None)
                        )
                        .clip(true),
                        text(line)
                            .size(11)
                            .color(if ok { MUTED } else { theme::ACCENT }),
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
                .style(theme::toast),
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
        Message::Saved { clip, seconds } => (
            "Clip saved".to_string(),
            format!("{} ({seconds:.0} s)", name(clip)),
            Some(crate::save::thumb_for(clip)),
        ),
        Message::Screenshot(p) => ("Screenshot saved".to_string(), name(p), Some(p.clone())),
        Message::Failed(e) => ("Couldn't save".to_string(), e.clone(), None),
    };
    let mut cmd = std::process::Command::new("notify-send");
    cmd.arg("--app-name=Rewindr");
    if let Some(i) = icon.filter(|p| p.exists()) {
        cmd.arg(format!("--icon={}", i.display()));
    }
    cmd.arg(title).arg(body);
    cmd.status()
        .map(|_| ())
        .map_err(|e| format!("notify-send: {e}"))
}
