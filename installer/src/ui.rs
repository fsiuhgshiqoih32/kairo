//! The installer's window: one small page at a time (choices, progress, done), in Kairo's look.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use iced::font::Weight;
use iced::widget::{
    Space, button, column, container, image, progress_bar, row, space, svg, text, text_input,
    toggler,
};
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Font, Length, Padding, Shadow, Size,
    Subscription, Task, Theme, Vector, color, window,
};

use crate::install::{self, Options, Progress, Removal, Shared, VERSION};
use crate::win;

const BG: Color = color!(0x141519);
const SURFACE: Color = color!(0x1e2026);
const RAISED: Color = color!(0x2c2f37);
const BORDER: Color = color!(0x2a2d35);
const TEXT: Color = color!(0xeceef1);
const TEXT_2: Color = color!(0xa6abb5);
const MUTED: Color = color!(0x737884);
const ACCENT: Color = color!(0xff7a1a);
const ACCENT_HI: Color = color!(0xff913f);
const GREEN: Color = color!(0x3dd68c);

const FONT: Font = Font::with_name("Inter");
const MEDIUM: Font = Font {
    weight: Weight::Medium,
    ..FONT
};
const SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..FONT
};
const BOLD: Font = Font {
    weight: Weight::Bold,
    ..FONT
};
const FONTS: [&[u8]; 4] = [
    include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/Inter-Bold.ttf"),
];
const LOGO: &[u8] = include_bytes!("../../assets/icon.png");
const CHECK: &[u8] = include_bytes!("../../assets/icons/check.svg");

pub enum Mode {
    Install(Arc<Vec<u8>>),
    Uninstall(PathBuf),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Page {
    Ready,
    Working,
    Done,
    Failed(String),
}

pub struct Setup {
    mode: Mode,
    pub page: Page,
    dir_text: String,
    /// The version installed now, if any.
    installed: Option<String>,
    /// A Rewindr (Kairo's old name) this install replaces.
    old: Option<PathBuf>,
    desktop: bool,
    autostart: bool,
    launch: bool,
    remove_settings: bool,
    progress: Shared,
    clips: PathBuf,
}

#[derive(Debug, Clone)]
pub enum Msg {
    DirText(String),
    PickDir,
    Picked(Option<PathBuf>),
    Desktop(bool),
    Autostart(bool),
    Launch(bool),
    RemoveSettings(bool),
    Go,
    Tick,
    Finish,
    Close,
    CloseRequested,
}

impl Setup {
    pub fn new(mode: Mode) -> Setup {
        let installed = win::installed_version();
        let (dir, old) = match &mode {
            Mode::Install(_) => (install::default_dir(), install::old_rewindr()),
            Mode::Uninstall(d) => (d.clone(), None),
        };
        Setup {
            mode,
            page: Page::Ready,
            dir_text: dir.display().to_string(),
            // A fresh install starts with Windows (a replay only works while Kairo runs); an
            // update keeps what the user chose.
            autostart: installed.is_none() || win::starts_with_windows(),
            old,
            installed,
            desktop: true,
            launch: true,
            remove_settings: false,
            progress: Shared::default(),
            clips: install::clips_dir(),
        }
    }

    fn installing(&self) -> bool {
        matches!(self.mode, Mode::Install(_))
    }

    /// The folder Kairo goes in, from what's typed.
    fn target(&self) -> PathBuf {
        match &self.mode {
            Mode::Install(_) => install::install_dir(Path::new(self.dir_text.trim())),
            Mode::Uninstall(d) => d.clone(),
        }
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::DirText(t) => self.dir_text = t,
            Msg::PickDir => {
                let start = PathBuf::from(self.dir_text.trim());
                return Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_title("Install Kairo in")
                            .set_directory(start.parent().unwrap_or(&start))
                            .pick_folder()
                            .await
                            .map(|f| f.path().to_path_buf())
                    },
                    Msg::Picked,
                );
            }
            Msg::Picked(Some(p)) => self.dir_text = install::install_dir(&p).display().to_string(),
            Msg::Picked(None) => {}
            Msg::Desktop(on) => self.desktop = on,
            Msg::Autostart(on) => self.autostart = on,
            Msg::Launch(on) => self.launch = on,
            Msg::RemoveSettings(on) => self.remove_settings = on,
            Msg::Go => {
                if self.page == Page::Working || self.dir_text.trim().is_empty() {
                    return Task::none();
                }
                self.page = Page::Working;
                self.progress = Shared::default();
                let progress = self.progress.clone();
                let dir = self.target();
                match &self.mode {
                    Mode::Install(zip) => {
                        let zip = zip.clone();
                        let o = Options {
                            dir,
                            start_menu: true,
                            desktop: self.desktop,
                            autostart: Some(self.autostart),
                            register: true,
                            stop_running: true,
                            replace: self.old.clone(),
                        };
                        std::thread::spawn(move || {
                            let r = std::env::current_exe()
                                .map_err(|e| e.to_string())
                                .and_then(|me| install::install(&zip, &me, &o, &progress));
                            progress.lock().unwrap().result = Some(r);
                        });
                    }
                    Mode::Uninstall(_) => {
                        let r = Removal {
                            dir,
                            settings: self.remove_settings,
                            system: true,
                        };
                        std::thread::spawn(move || {
                            let result = install::uninstall(&r, &progress);
                            install::remove_uninstaller(&r.dir);
                            progress.lock().unwrap().result = Some(result);
                        });
                    }
                }
            }
            Msg::Tick => {
                let result = self.progress.lock().unwrap().result.clone();
                match result {
                    Some(Ok(())) => self.page = Page::Done,
                    Some(Err(e)) => self.page = Page::Failed(e),
                    None => {}
                }
            }
            Msg::Finish => {
                if self.installing() && self.launch {
                    let _ = win::launch(&self.target().join(install::PROGRAM), &[]);
                }
                return iced::exit();
            }
            Msg::Close => return iced::exit(),
            // Closing mid-way would leave half an install.
            Msg::CloseRequested if self.page != Page::Working => return iced::exit(),
            Msg::CloseRequested => {}
        }
        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        let close = window::close_requests().map(|_| Msg::CloseRequested);
        if self.page == Page::Working {
            Subscription::batch([
                close,
                iced::time::every(Duration::from_millis(60)).map(|_| Msg::Tick),
            ])
        } else {
            close
        }
    }

    pub fn title(&self) -> String {
        if self.installing() {
            "Kairo Setup".into()
        } else {
            "Uninstall Kairo".into()
        }
    }

    pub fn theme(&self) -> Theme {
        Theme::custom(
            "Kairo",
            iced::theme::Palette {
                background: BG,
                text: TEXT,
                primary: ACCENT,
                success: GREEN,
                warning: color!(0xffc53d),
                danger: color!(0xe5484d),
            },
        )
    }

    fn subtitle(&self) -> String {
        match (&self.mode, self.installed.as_deref()) {
            (Mode::Install(_), None) if self.old.is_some() => {
                format!("Version {VERSION}. Replaces Rewindr; your clips and settings carry over.")
            }
            (Mode::Install(_), None) => format!("Version {VERSION}"),
            (Mode::Install(_), Some(v)) if v == VERSION => {
                format!("Version {VERSION} is installed. Installing it again repairs it.")
            }
            (Mode::Install(_), Some(v)) => format!("Updates version {v} to {VERSION}"),
            (Mode::Uninstall(_), v) => format!("Version {}", v.unwrap_or(VERSION)),
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let head = row![
            image(image::Handle::from_bytes(LOGO)).width(52).height(52),
            column![
                text(if self.installing() {
                    "Kairo"
                } else {
                    "Uninstall Kairo"
                })
                .font(BOLD)
                .size(24),
                text(self.subtitle()).size(13).color(TEXT_2),
            ]
            .spacing(2),
        ]
        .spacing(16)
        .align_y(Alignment::Center);

        let (body, footer) = match (&self.page, self.installing()) {
            (Page::Ready, true) => self.install_choices(),
            (Page::Ready, false) => self.uninstall_choices(),
            (Page::Working, _) => (self.working(), row![].into()),
            (Page::Done, _) => self.done(),
            (Page::Failed(e), _) => (
                column![
                    text(if self.installing() {
                        "Kairo couldn't be installed"
                    } else {
                        "Kairo couldn't be fully removed"
                    })
                    .font(SEMIBOLD)
                    .size(16)
                    .color(ACCENT),
                    text(e.clone()).size(13).color(TEXT_2),
                ]
                .spacing(8)
                .into(),
                row![
                    space::horizontal(),
                    button(text("Close").size(13).font(MEDIUM))
                        .padding(Padding::from([9, 16]))
                        .style(ghost)
                        .on_press(Msg::Close),
                    button(text("Try again").size(13).font(SEMIBOLD))
                        .padding(Padding::from([9, 18]))
                        .style(primary)
                        .on_press(Msg::Go),
                ]
                .spacing(8)
                .into(),
            ),
        };

        container(
            column![
                head,
                Space::new().height(26),
                body,
                space::vertical(),
                footer
            ]
            .height(Fill),
        )
        .padding(Padding::from([28, 32]))
        .width(Fill)
        .height(Fill)
        .style(|_| container::Style {
            background: Some(BG.into()),
            text_color: Some(TEXT),
            ..Default::default()
        })
        .into()
    }

    fn install_choices(&self) -> (Element<'_, Msg>, Element<'_, Msg>) {
        let target = self.target();
        let typed = PathBuf::from(self.dir_text.trim());
        let mut folder = column![
            text("Install folder").size(13).font(SEMIBOLD).color(TEXT_2),
            row![
                text_input("Folder", &self.dir_text)
                    .on_input(Msg::DirText)
                    .on_submit(Msg::Go)
                    .size(13)
                    .padding(Padding::from([9, 11]))
                    .style(input)
                    .width(Fill),
                button(text("Change").size(13).font(MEDIUM))
                    .padding(Padding::from([9, 14]))
                    .style(secondary)
                    .on_press(Msg::PickDir),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(8);
        if target != typed {
            folder = folder.push(
                text(format!(
                    "Kairo gets a folder of its own: {}",
                    target.display()
                ))
                .size(12)
                .color(MUTED),
            );
        }
        let body = column![
            folder,
            Space::new().height(10),
            choice("Desktop shortcut", None, self.desktop, Msg::Desktop),
            choice(
                "Start with Windows",
                Some("Keeps the replay ready from the moment you sign in."),
                self.autostart,
                Msg::Autostart
            ),
        ]
        .spacing(10);
        let action = match self.installed.as_deref() {
            None => "Install",
            Some(v) if v == VERSION => "Reinstall",
            Some(_) => "Update",
        };
        let footer = row![
            column![
                text("No administrator rights needed.")
                    .size(12)
                    .color(MUTED),
                text(format!("Clips save to {}.", self.clips.display()))
                    .size(12)
                    .color(MUTED),
            ]
            .spacing(2)
            .width(Fill),
            button(text("Cancel").size(13).font(MEDIUM))
                .padding(Padding::from([9, 16]))
                .style(ghost)
                .on_press(Msg::Close),
            button(text(action).size(13).font(SEMIBOLD))
                .padding(Padding::from([9, 20]))
                .style(primary)
                .on_press_maybe((!self.dir_text.trim().is_empty()).then_some(Msg::Go)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        (body.into(), footer.into())
    }

    fn uninstall_choices(&self) -> (Element<'_, Msg>, Element<'_, Msg>) {
        let body = column![
            text(format!(
                "Removes Kairo from {} along with its shortcuts.",
                self.target().display()
            ))
            .size(14)
            .color(TEXT_2),
            text(format!("Your clips stay in {}.", self.clips.display()))
                .size(14)
                .color(TEXT_2),
            Space::new().height(8),
            choice(
                "Also remove settings",
                Some("Settings, thumbnails, logs and the connected Discord channel."),
                self.remove_settings,
                Msg::RemoveSettings
            ),
        ]
        .spacing(8);
        let footer = row![
            space::horizontal(),
            button(text("Cancel").size(13).font(MEDIUM))
                .padding(Padding::from([9, 16]))
                .style(ghost)
                .on_press(Msg::Close),
            button(text("Uninstall").size(13).font(SEMIBOLD))
                .padding(Padding::from([9, 20]))
                .style(primary)
                .on_press(Msg::Go),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        (body.into(), footer.into())
    }

    fn working(&self) -> Element<'_, Msg> {
        let Progress { fraction, step, .. } = &*self.progress.lock().unwrap();
        let step = if step.is_empty() {
            "Starting".to_string()
        } else {
            step.clone()
        };
        column![
            row![
                text(step).size(14).font(MEDIUM),
                space::horizontal(),
                text(format!("{:.0}%", fraction * 100.0))
                    .size(13)
                    .color(MUTED),
            ],
            progress_bar(0.0..=1.0, *fraction)
                .girth(8)
                .style(|_| progress_bar::Style {
                    background: RAISED.into(),
                    bar: ACCENT.into(),
                    border: Border {
                        radius: 4.0.into(),
                        ..Default::default()
                    },
                }),
        ]
        .spacing(12)
        .into()
    }

    fn done(&self) -> (Element<'_, Msg>, Element<'_, Msg>) {
        let mark = container(
            svg(svg::Handle::from_memory(CHECK))
                .width(22)
                .height(22)
                .style(|_, _| svg::Style { color: Some(GREEN) }),
        )
        .center(Length::Fixed(44.0))
        .style(|_| container::Style {
            background: Some(Color { a: 0.14, ..GREEN }.into()),
            border: Border {
                radius: 22.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
        let (title, note) = if self.installing() {
            (
                "Kairo is installed",
                "It's in the Start menu. It runs in the tray and keeps your replay ready: \
                 press F8 to save the last 30 seconds.",
            )
        } else {
            (
                "Kairo is uninstalled",
                "Your clips are still where they were.",
            )
        };
        let mut body = column![
            row![
                mark,
                column![
                    text(title).font(SEMIBOLD).size(17),
                    text(note).size(13).color(TEXT_2),
                ]
                .spacing(4)
                .width(Fill),
            ]
            .spacing(14)
            .align_y(Alignment::Center),
        ]
        .spacing(18);
        if self.installing() {
            body = body.push(choice("Open Kairo now", None, self.launch, Msg::Launch));
        }
        let footer = row![
            space::horizontal(),
            button(
                text(if self.installing() { "Finish" } else { "Close" })
                    .size(13)
                    .font(SEMIBOLD)
            )
            .padding(Padding::from([9, 20]))
            .style(primary)
            .on_press(Msg::Finish),
        ];
        (body.into(), footer.into())
    }
}

/// An option: its name (and a line about it) with a switch.
fn choice<'a>(
    title: &'a str,
    note: Option<&'a str>,
    on: bool,
    msg: fn(bool) -> Msg,
) -> Element<'a, Msg> {
    let mut words = column![text(title).size(14).font(MEDIUM)].spacing(2);
    if let Some(n) = note {
        words = words.push(text(n).size(12).color(MUTED));
    }
    container(
        row![
            words.width(Fill),
            toggler(on).on_toggle(msg).size(22).style(toggle)
        ]
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([12, 14]))
    .style(|_| container::Style {
        background: Some(SURFACE.into()),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn base_button(bg: Option<Color>, text: Color) -> button::Style {
    button::Style {
        background: bg.map(Background::Color),
        text_color: text,
        border: Border {
            radius: 9.0.into(),
            ..Default::default()
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

fn primary(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered => button::Style {
            shadow: Shadow {
                color: Color { a: 0.45, ..ACCENT },
                offset: Vector::new(0.0, 4.0),
                blur_radius: 18.0,
            },
            ..base_button(Some(ACCENT_HI), Color::WHITE)
        },
        button::Status::Disabled => base_button(Some(RAISED), MUTED),
        _ => base_button(Some(ACCENT), Color::WHITE),
    }
}

fn secondary(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => {
            base_button(Some(color!(0x363a43)), TEXT)
        }
        _ => base_button(Some(RAISED), TEXT),
    }
}

fn ghost(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => base_button(Some(RAISED), TEXT),
        _ => base_button(None, TEXT_2),
    }
}

fn input(_: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: SURFACE.into(),
        border: Border {
            color: if focused {
                Color { a: 0.7, ..ACCENT }
            } else {
                BORDER
            },
            width: 1.0,
            radius: 9.0.into(),
        },
        icon: MUTED,
        placeholder: MUTED,
        value: TEXT,
        selection: Color { a: 0.35, ..ACCENT },
    }
}

fn toggle(_: &Theme, status: toggler::Status) -> toggler::Style {
    let (on, hovered) = match status {
        toggler::Status::Active { is_toggled } => (is_toggled, false),
        toggler::Status::Hovered { is_toggled } => (is_toggled, true),
        toggler::Status::Disabled { is_toggled } => (is_toggled, false),
    };
    toggler::Style {
        background: if on {
            if hovered { ACCENT_HI } else { ACCENT }
        } else if hovered {
            color!(0x3b3f49)
        } else {
            RAISED
        }
        .into(),
        background_border_width: 0.0,
        background_border_color: Color::TRANSPARENT,
        foreground: if on { Color::WHITE } else { TEXT_2 }.into(),
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
        text_color: None,
        border_radius: None,
        padding_ratio: 0.12,
    }
}

/// The window's icon: Kairo's logo.
fn icon() -> Option<window::Icon> {
    let img = ::image::load_from_memory(LOGO)
        .ok()?
        .resize(64, 64, ::image::imageops::Lanczos3);
    let rgba = img.to_rgba8();
    window::icon::from_rgba(rgba.into_raw(), 64, 64).ok()
}

pub fn run(mode: Mode) -> Result<(), String> {
    let start = std::cell::Cell::new(Some(mode));
    let mut app = iced::application(
        move || (Setup::new(start.take().expect("booted once")), Task::none()),
        Setup::update,
        Setup::view,
    )
    .title(Setup::title)
    .theme(Setup::theme)
    .subscription(Setup::subscription)
    .default_font(FONT)
    .window(window::Settings {
        size: Size::new(600.0, 440.0),
        resizable: false,
        position: window::Position::Centered,
        icon: icon(),
        exit_on_close_request: false,
        ..Default::default()
    });
    for f in FONTS {
        app = app.font(f);
    }
    app.run().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    //! Renders each page to a PNG without opening a window:
    //! `KAIRO_SNAPSHOTS=<dir> cargo test -p kairo-setup -- --ignored preview`
    use super::*;
    use iced_test::Simulator;

    fn shoot(setup: &Setup, name: &str) {
        let dir = PathBuf::from(std::env::var("KAIRO_SNAPSHOTS").expect("KAIRO_SNAPSHOTS"));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = iced::Settings {
            fonts: FONTS
                .iter()
                .map(|f| std::borrow::Cow::Borrowed(*f))
                .collect(),
            default_font: FONT,
            ..Default::default()
        };
        let mut ui = Simulator::with_size(settings, Size::new(600.0, 440.0), setup.view());
        let shot = ui.snapshot(&setup.theme()).unwrap();
        let _ = shot.matches_image(dir.join(format!("{name}.png")));
    }

    #[test]
    #[ignore = "writes PNGs to KAIRO_SNAPSHOTS"]
    fn preview() {
        let mut s = Setup::new(Mode::Install(Arc::new(Vec::new())));
        s.installed = None;
        s.old = None;
        s.autostart = true;
        s.dir_text = r"C:\Users\you\AppData\Local\Programs\Kairo".into();
        s.clips = PathBuf::from(r"C:\Users\you\Videos\Kairo");
        shoot(&s, "setup-ready");
        s.old = Some(PathBuf::from(
            r"C:\Users\you\AppData\Local\Programs\Rewindr",
        ));
        s.clips = PathBuf::from(r"C:\Users\you\Videos\Rewindr");
        shoot(&s, "setup-replaces-rewindr");
        s.old = None;
        s.dir_text = r"D:\Games".into();
        s.installed = Some("0.2.0".into());
        shoot(&s, "setup-update");
        s.page = Page::Working;
        *s.progress.lock().unwrap() = Progress {
            fraction: 0.62,
            step: "Copying files".into(),
            result: None,
        };
        shoot(&s, "setup-working");
        s.page = Page::Done;
        shoot(&s, "setup-done");
        s.page = Page::Failed(
            r"Couldn't make C:\Program Files\Kairo (Access is denied). Choose a folder you can write to."
                .into(),
        );
        shoot(&s, "setup-failed");
        let mut u = Setup::new(Mode::Uninstall(PathBuf::from(
            r"C:\Users\you\AppData\Local\Programs\Kairo",
        )));
        u.clips = PathBuf::from(r"C:\Users\you\Videos\Kairo");
        u.installed = Some(VERSION.into());
        shoot(&u, "uninstall-ready");
        u.page = Page::Done;
        shoot(&u, "uninstall-done");
    }
}
