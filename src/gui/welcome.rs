//! The first start: a few steps to pick the save key, length, quality, sound and look. Every
//! choice goes through the same messages as Settings, so it's saved the same way.

use iced::widget::{Space, button, column, container, image, row, text};
use iced::{Alignment, Element, Fill, Length, Padding};
use kairo::config::{ACCENTS, Quality};

use super::icons::{Icon, icon};
use super::settings::{self, Target};
use super::theme::{self, BOLD, MEDIUM, SEMIBOLD};
use super::widgets::segmented;
use super::{App, Message, seconds_label};

/// How many steps there are, the last one being "you're set".
pub const STEPS: usize = 5;

fn title<'a>(s: String) -> Element<'a, Message> {
    text(s).font(BOLD).size(26).color(theme::ink()).into()
}

fn line<'a>(s: String) -> Element<'a, Message> {
    text(s).size(15).color(theme::ink_2()).into()
}

/// A labelled control, the label on the left.
fn choice<'a>(
    label: &str,
    note: Option<&str>,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut words = column![
        text(label.to_string())
            .size(15)
            .font(MEDIUM)
            .color(theme::ink())
    ];
    if let Some(n) = note {
        words = words.push(text(n.to_string()).size(13).color(theme::muted()));
    }
    row![
        words.spacing(3).width(Fill),
        container(control.into()).align_x(Alignment::End)
    ]
    .spacing(20)
    .align_y(Alignment::Center)
    .into()
}

impl App {
    pub(super) fn welcome_view(&self, step: usize) -> Element<'_, Message> {
        let s = &self.settings;
        let set = |m: settings::Msg| Message::Settings(m);
        let body: Element<'_, Message> = match step {
            0 => column![
                image(super::widgets::app_logo(128)).width(72).height(72),
                title("Welcome to Kairo".into()),
                line(
                    "Kairo quietly keeps the last moments of your game. When something happens, \
                     press a key and they're saved as a clip, sound included."
                        .into()
                ),
                line("This takes a minute. You can change all of it later in Settings.".into()),
            ]
            .spacing(14)
            .into(),
            1 => {
                let lengths = [15u32, 30, 60, 120];
                let picked = lengths
                    .iter()
                    .copied()
                    .min_by_key(|l| l.abs_diff(s.clip_seconds))
                    .unwrap_or(30);
                let save_error = self.report.as_ref().and_then(|r| r.hotkey_error.clone());
                column![
                    title("Your save key".into()),
                    line(
                        "Press it in any game, even full screen, to keep what just happened."
                            .into()
                    ),
                    Space::new().height(6),
                    choice(
                        "Save key",
                        Some("Click, then press the key you want."),
                        self.setup
                            .key_capture(Target::Save, &s.hotkey, save_error)
                            .map(Message::Settings),
                    ),
                    choice(
                        "How much to keep",
                        None,
                        segmented(
                            lengths.iter().map(|l| (*l, seconds_label(*l))).collect(),
                            picked,
                            move |v| set(settings::Msg::ClipSeconds(v)),
                        ),
                    ),
                    choice(
                        "Quality",
                        Some("High looks best; Low makes the smallest files."),
                        segmented(
                            vec![
                                (Quality::Low, "Low".to_string()),
                                (Quality::Medium, "Medium".to_string()),
                                (Quality::High, "High".to_string()),
                            ],
                            s.quality,
                            move |q| set(settings::Msg::Quality(q)),
                        ),
                    ),
                ]
                .spacing(18)
                .into()
            }
            2 => column![
                title("Sound".into()),
                line(
                    "Each sound is also kept on its own track, so you can balance them later."
                        .into()
                ),
                Space::new().height(6),
                choice(
                    "Game and app sound",
                    Some("Whatever you hear."),
                    settings::switch(s.desktop_audio, settings::Msg::DesktopAudio)
                        .map(Message::Settings),
                ),
                choice(
                    "Your microphone",
                    Some("Your voice, mixed into the clip."),
                    settings::switch(s.microphone, settings::Msg::Microphone)
                        .map(Message::Settings),
                ),
            ]
            .spacing(18)
            .into(),
            3 => {
                let swatches = ACCENTS.iter().zip(theme::ACCENT_COLORS).fold(
                    row![].spacing(9),
                    |r, (name, rgb)| {
                        let chosen = s.accent == *name;
                        let c = theme::rgb(rgb);
                        r.push(
                            button(Space::new().width(22).height(22))
                                .padding(0)
                                .style(move |_, st| button::Style {
                                    background: Some(c.into()),
                                    border: iced::Border {
                                        color: if chosen {
                                            theme::ink()
                                        } else if matches!(st, button::Status::Hovered) {
                                            theme::alpha(theme::ink(), 0.45)
                                        } else {
                                            iced::Color::TRANSPARENT
                                        },
                                        width: if chosen { 3.0 } else { 2.0 },
                                        radius: 999.0.into(),
                                    },
                                    ..Default::default()
                                })
                                .on_press(set(settings::Msg::Accent(name))),
                        )
                    },
                );
                let mut col = column![
                    title("Make it yours".into()),
                    Space::new().height(6),
                    choice(
                        "Look",
                        None,
                        segmented(
                            vec![
                                ("dark", "Dark".to_string()),
                                ("light", "Light".to_string()),
                                ("system", "Match system".to_string()),
                            ],
                            match s.look.as_str() {
                                "light" => "light",
                                "system" => "system",
                                _ => "dark",
                            },
                            move |v| set(settings::Msg::Look(v)),
                        ),
                    ),
                    choice("Colour", None, swatches),
                ]
                .spacing(18);
                if let Some(on) = self.setup.autostart_state() {
                    col = col.push(choice(
                        "Start with the computer",
                        Some("Kairo waits in the tray, ready when you play."),
                        settings::switch(on, settings::Msg::Autostart).map(Message::Settings),
                    ));
                }
                col.into()
            }
            _ => {
                let key = self.hotkey();
                column![
                    container(icon(Icon::Check, 30.0, theme::on_accent()))
                        .center(Length::Fixed(56.0))
                        .style(|_| container::Style {
                            background: Some(theme::accent().into()),
                            border: iced::Border {
                                radius: 28.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                    title("You're all set".into()),
                    line(format!(
                        "Play as usual. When something worth keeping happens, press {key}. \
                         Your clips show up in the library, named after the game."
                    )),
                    line(
                        "F9 saves a screenshot and F10 marks a moment. Find more in Settings."
                            .into()
                    ),
                ]
                .spacing(14)
                .into()
            }
        };

        let dots = (0..STEPS).fold(row![].spacing(8), |r, i| {
            let on = i == step;
            r.push(
                container(Space::new().width(if on { 22 } else { 8 }).height(8)).style(move |_| {
                    container::Style {
                        background: Some(if on { theme::accent() } else { theme::raised() }.into()),
                        border: iced::Border {
                            radius: 4.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                }),
            )
        });
        let last = step + 1 >= STEPS;
        let back = (step > 0).then(|| {
            button(text("Back").size(14).font(MEDIUM))
                .padding(Padding::from([10, 18]))
                .style(theme::ghost)
                .on_press(Message::WelcomeStep(step - 1))
        });
        let next = button(
            text(match step {
                0 => "Get started",
                _ if last => "Open my library",
                _ => "Next",
            })
            .size(14)
            .font(SEMIBOLD),
        )
        .padding(Padding::from([10, 20]))
        .style(theme::primary)
        .on_press(if last {
            Message::WelcomeDone
        } else {
            Message::WelcomeStep(step + 1)
        });
        let mut nav = row![].spacing(10).align_y(Alignment::Center);
        if step > 0 && !last {
            nav = nav.push(
                button(text("Skip").size(14).font(MEDIUM).color(theme::muted()))
                    .padding(Padding::from([10, 14]))
                    .style(theme::ghost)
                    .on_press(Message::WelcomeDone),
            );
        }
        nav = nav.push(Space::new().width(Fill));
        if let Some(b) = back {
            nav = nav.push(b);
        }
        nav = nav.push(next);

        let card = container(column![body, Space::new().height(28), nav].width(Fill))
            .padding(32)
            .max_width(620)
            .style(theme::well);
        container(column![dots, card].spacing(20).align_x(Alignment::Center))
            .center(Fill)
            .padding(24)
            .style(|_| container::Style {
                background: Some(theme::bg().into()),
                ..Default::default()
            })
            .into()
    }
}
