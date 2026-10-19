//! The settings page.

use std::path::PathBuf;
use std::sync::Arc;

use iced::keyboard::{self, Key, key};
use iced::widget::{
    Space, button, column, container, pick_list, row, scrollable, text, text_input, toggler,
};
use iced::{Alignment, Element, Fill, Length, Padding, Task};

use rewindr::args::{self, Encoder};
use rewindr::config::{MAX_CLIP_SECONDS, MIN_CLIP_SECONDS, Quality, Resolution, Settings};

use super::icons::{Icon, icon};
use super::theme::{self, BOLD, MEDIUM, MUTED, SEMIBOLD, TEXT, TEXT_2};
use super::widgets::{divider, icon_button, keycap, label, segmented};
use super::{background, human_size, seconds_label};
use crate::daemon::{self, Request};
use crate::platform::{self, Monitor};
use crate::{autostart, hotkey, updater};

/// A choice in a pick list: what's stored and what's shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    value: String,
    label: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

pub struct State {
    monitors: Vec<Monitor>,
    mics: Vec<String>,
    encoders: Option<Vec<Encoder>>,
    launcher: Option<auto_launch::AutoLaunch>,
    autostart: bool,
    pub capturing_key: bool,
    hotkey_problem: Option<String>,
    clip_text: String,
    gb_text: String,
    pub update: Updates,
}

#[derive(Default)]
pub struct Updates {
    pub checking: bool,
    pub installing: bool,
    pub available: Option<Arc<updater::Update>>,
    pub message: Option<(String, bool)>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Capture,
    ClipSeconds(u32),
    ClipText(String),
    Monitor(Choice),
    Fps(u32),
    Resolution(Resolution),
    Quality(Quality),
    Encoder(Choice),
    Encoders(Vec<Encoder>),
    DesktopAudio(bool),
    Microphone(bool),
    Mic(Choice),
    PickFolder,
    Folder(Option<PathBuf>),
    DefaultFolder,
    Notify(bool),
    Sound(bool),
    Cleanup(bool),
    GbText(String),
    Discord(u32),
    Autostart(bool),
    CheckUpdates(bool),
    CheckNow,
    Checked(Result<Option<Arc<updater::Update>>, String>),
    Install,
    Installed(Result<(), String>),
    OpenLog,
    Quit,
}

/// What the settings ask of the window.
pub enum Effect {
    None,
    /// The settings changed; save them soon.
    Changed,
    /// An update was installed: restart.
    Restart,
}

impl State {
    pub fn new(settings: &Settings) -> (State, Task<Msg>) {
        let launcher = autostart::launcher();
        let autostart = launcher.as_ref().is_some_and(autostart::is_enabled);
        let encoders = Task::perform(
            background(|| {
                crate::ffmpeg::path()
                    .map(|f| crate::ffmpeg::working_encoders(&f))
                    .unwrap_or_default()
            }),
            Msg::Encoders,
        );
        let mut tasks = vec![encoders];
        let mut update = Updates::default();
        if settings.check_updates && std::env::var_os("REWINDR_NO_UPDATE_CHECK").is_none() {
            update.checking = true;
            tasks.push(check());
        }
        (
            State {
                monitors: platform::monitors(),
                mics: crate::audio::microphones(),
                encoders: None,
                launcher,
                autostart,
                capturing_key: false,
                hotkey_problem: None,
                clip_text: settings.clip_seconds.to_string(),
                gb_text: settings.max_storage_gb.max(1).to_string(),
                update,
            },
            Task::batch(tasks),
        )
    }

    /// A key press while waiting for the new shortcut.
    pub fn key(
        &mut self,
        s: &mut Settings,
        k: &Key,
        physical: &key::Physical,
        m: keyboard::Modifiers,
    ) -> Effect {
        if matches!(k.as_ref(), Key::Named(key::Named::Escape)) {
            self.capturing_key = false;
            return Effect::None;
        }
        if matches!(
            k.as_ref(),
            Key::Named(
                key::Named::Control | key::Named::Shift | key::Named::Alt | key::Named::Super
            )
        ) {
            return Effect::None;
        }
        let key::Physical::Code(code) = physical else {
            return Effect::None;
        };
        let name = format!("{code:?}");
        let name = name
            .strip_prefix("Key")
            .or_else(|| name.strip_prefix("Digit"))
            .unwrap_or(&name)
            .to_string();
        let mut parts: Vec<String> = Vec::new();
        if m.control() {
            parts.push("Ctrl".into());
        }
        if m.logo() && cfg!(target_os = "macos") {
            parts.push("Cmd".into());
        }
        if m.alt() {
            parts.push("Alt".into());
        }
        if m.shift() {
            parts.push("Shift".into());
        }
        parts.push(name);
        let spec = parts.join("+");
        self.capturing_key = false;
        match hotkey::parse(&spec) {
            Ok(_) => {
                s.hotkey = spec;
                self.hotkey_problem = None;
                Effect::Changed
            }
            Err(_) => {
                self.hotkey_problem = Some(format!("{spec} can't be used as a shortcut."));
                Effect::None
            }
        }
    }

    pub fn update(&mut self, s: &mut Settings, msg: Msg) -> (Task<Msg>, Effect) {
        let changed = (Task::none(), Effect::Changed);
        match msg {
            Msg::Capture => self.capturing_key = !self.capturing_key,
            Msg::ClipSeconds(v) => {
                s.clip_seconds = v;
                self.clip_text = v.to_string();
                return changed;
            }
            Msg::ClipText(t) => {
                self.clip_text = t.chars().filter(char::is_ascii_digit).take(3).collect();
                if let Ok(v) = self.clip_text.parse::<u32>()
                    && (MIN_CLIP_SECONDS..=MAX_CLIP_SECONDS).contains(&v)
                {
                    s.clip_seconds = v;
                    return changed;
                }
            }
            Msg::Monitor(c) => {
                s.monitor = c.value;
                return changed;
            }
            Msg::Fps(f) => {
                s.fps = f;
                return changed;
            }
            Msg::Resolution(r) => {
                s.resolution = r;
                return changed;
            }
            Msg::Quality(q) => {
                s.quality = q;
                return changed;
            }
            Msg::Encoder(c) => {
                s.encoder = c.value;
                return changed;
            }
            Msg::Encoders(e) => self.encoders = Some(e),
            Msg::DesktopAudio(v) => {
                s.desktop_audio = v;
                return changed;
            }
            Msg::Microphone(v) => {
                s.microphone = v;
                return changed;
            }
            Msg::Mic(c) => {
                s.microphone_device = c.value;
                return changed;
            }
            Msg::PickFolder => {
                let start = s.save_dir();
                return (
                    Task::perform(
                        background(move || {
                            rfd::FileDialog::new().set_directory(start).pick_folder()
                        }),
                        Msg::Folder,
                    ),
                    Effect::None,
                );
            }
            Msg::Folder(Some(d)) => {
                s.save_dir = d.display().to_string();
                return changed;
            }
            Msg::Folder(None) => {}
            Msg::DefaultFolder => {
                s.save_dir.clear();
                return changed;
            }
            Msg::Notify(v) => {
                s.notify = v;
                return changed;
            }
            Msg::Sound(v) => {
                s.sound = v;
                return changed;
            }
            Msg::Cleanup(on) => {
                s.max_storage_gb = if on {
                    self.gb_text.parse().unwrap_or(50).max(1)
                } else {
                    0
                };
                return changed;
            }
            Msg::GbText(t) => {
                self.gb_text = t.chars().filter(char::is_ascii_digit).take(5).collect();
                if let Ok(v) = self.gb_text.parse::<u32>()
                    && v >= 1
                    && s.max_storage_gb > 0
                {
                    s.max_storage_gb = v;
                    return changed;
                }
            }
            Msg::Discord(mb) => {
                s.discord_mb = mb;
                return changed;
            }
            Msg::Autostart(on) => {
                if let Some(l) = &self.launcher {
                    autostart::set_enabled(l, on);
                    self.autostart = autostart::is_enabled(l);
                }
            }
            Msg::CheckUpdates(v) => {
                s.check_updates = v;
                return changed;
            }
            Msg::CheckNow => {
                if !self.update.checking {
                    self.update.checking = true;
                    self.update.message = None;
                    return (check(), Effect::None);
                }
            }
            Msg::Checked(r) => {
                self.update.checking = false;
                match r {
                    Ok(Some(u)) => self.update.available = Some(u),
                    Ok(None) => {
                        self.update.message = Some((
                            format!(
                                "Rewindr {} is the latest version.",
                                updater::current_version()
                            ),
                            true,
                        ))
                    }
                    Err(e) => {
                        self.update.message =
                            Some((format!("Couldn't check for updates: {e}"), false))
                    }
                }
            }
            Msg::Install => {
                if let Some(u) = self.update.available.clone()
                    && !self.update.installing
                {
                    self.update.installing = true;
                    return (
                        Task::perform(background(move || updater::install(&u)), Msg::Installed),
                        Effect::None,
                    );
                }
            }
            Msg::Installed(Ok(())) => return (Task::none(), Effect::Restart),
            Msg::Installed(Err(e)) => {
                self.update.installing = false;
                self.update.message = Some((format!("Update failed: {e}"), false));
            }
            Msg::OpenLog => crate::app::open(&crate::log::path()),
            Msg::Quit => {
                let _ = daemon::request(Request::Quit);
            }
        }
        (Task::none(), Effect::None)
    }

    fn memory_estimate(&self, s: &Settings) -> String {
        let m = platform::choose_monitor(&s.monitor);
        let (w, h) = m.map_or((1920, 1080), |m| (m.w, m.h));
        let (ow, oh) = args::output_size((w, h), s.resolution.max_height());
        let bps = s.quality.bitrate(ow as u64 * oh as u64, s.fps);
        let bytes = bps as f64 / 8.0 * s.buffer_seconds() as f64;
        format!(
            "{ow} x {oh} at about {:.0} Mbit/s. The replay lives in memory (about {}), never on disk.",
            bps as f64 / 1e6,
            human_size(bytes as u64)
        )
    }

    pub fn view<'a>(
        &'a self,
        s: &'a Settings,
        hotkey_error: Option<String>,
        used: u64,
    ) -> Element<'a, Msg> {
        let mut sections = column![].spacing(26).max_width(780);

        // Replay.
        let key_button = button(
            if self.capturing_key {
                row![
                    text("Press a key")
                        .font(SEMIBOLD)
                        .size(14)
                        .color(theme::ACCENT)
                ]
            } else {
                s.hotkey
                    .split('+')
                    .fold(row![].spacing(4), |r, k| r.push(keycap(k)))
            }
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([8, 14]))
        .style({
            let capturing = self.capturing_key;
            move |t, st| {
                if capturing {
                    theme::segment(true)(t, st)
                } else {
                    theme::secondary(t, st)
                }
            }
        })
        .on_press(Msg::Capture);
        let key_note = if self.capturing_key {
            Some("Add Ctrl, Alt or Shift if you like. Esc cancels.".to_string())
        } else {
            self.hotkey_problem.clone().or(hotkey_error)
        };
        let mut key_col = column![key_button].spacing(6).align_x(Alignment::End);
        if let Some(n) = key_note {
            let bad = !self.capturing_key;
            key_col = key_col.push(
                text(n)
                    .size(12)
                    .color(if bad { theme::ACCENT } else { MUTED }),
            );
        }
        sections = sections.push(section(
            "Replay",
            vec![
                setting(
                    "Save shortcut",
                    Some("Works everywhere, even in full-screen games.".into()),
                    key_col,
                ),
                setting(
                    "Clip length",
                    Some(format!(
                        "Rewindr keeps the last {} ready to save.",
                        seconds_label(s.clip_seconds)
                    )),
                    row![
                        segmented(
                            [15u32, 30, 60, 120]
                                .iter()
                                .map(|v| (*v, seconds_label(*v)))
                                .collect(),
                            s.clip_seconds,
                            Msg::ClipSeconds
                        ),
                        text_input("30", &self.clip_text)
                            .on_input(Msg::ClipText)
                            .width(56)
                            .padding(Padding::from([7, 9]))
                            .size(13)
                            .style(theme::input),
                        text("s").size(13).color(MUTED),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                ),
            ],
        ));

        // Video.
        let mut screens = vec![Choice {
            value: "primary".into(),
            label: "Main screen".into(),
        }];
        for (i, m) in self.monitors.iter().enumerate() {
            screens.push(Choice {
                value: (i + 1).to_string(),
                label: m.label(),
            });
        }
        let screen = screens.iter().find(|c| c.value == s.monitor).cloned();
        let mut encoders = vec![Choice {
            value: "auto".into(),
            label: "Automatic (graphics card first)".into(),
        }];
        for e in self.encoders.clone().unwrap_or_default() {
            encoders.push(Choice {
                value: e.id().into(),
                label: e.label().into(),
            });
        }
        if let Some(e) = Encoder::from_id(&s.encoder)
            && !encoders.iter().any(|c| c.value == s.encoder)
        {
            encoders.push(Choice {
                value: e.id().into(),
                label: e.label().into(),
            });
        }
        let encoder = encoders.iter().find(|c| c.value == s.encoder).cloned();
        let mut encoder_row = row![pick(encoders, encoder, Msg::Encoder)]
            .spacing(8)
            .align_y(Alignment::Center);
        if self.encoders.is_none() {
            encoder_row = encoder_row.push(text("checking...").size(12).color(MUTED));
        }
        sections = sections.push(section(
            "Video",
            vec![
                setting("Screen", None, pick(screens, screen, Msg::Monitor)),
                setting(
                    "Frame rate",
                    None,
                    segmented(
                        [30u32, 60, 120]
                            .iter()
                            .map(|f| (*f, format!("{f} fps")))
                            .collect(),
                        s.fps,
                        Msg::Fps,
                    ),
                ),
                setting(
                    "Resolution",
                    None,
                    segmented(
                        Resolution::ALL
                            .iter()
                            .map(|r| (*r, r.label().to_string()))
                            .collect(),
                        s.resolution,
                        Msg::Resolution,
                    ),
                ),
                setting(
                    "Quality",
                    Some(self.memory_estimate(s)),
                    segmented(
                        Quality::ALL
                            .iter()
                            .map(|q| (*q, q.label().to_string()))
                            .collect(),
                        s.quality,
                        Msg::Quality,
                    ),
                ),
                setting("Encoder", None, encoder_row),
            ],
        ));

        // Sound.
        let mut mics = vec![Choice {
            value: String::new(),
            label: "Default microphone".into(),
        }];
        for m in &self.mics {
            mics.push(Choice {
                value: m.clone(),
                label: m.clone(),
            });
        }
        let mic = mics
            .iter()
            .find(|c| c.value == s.microphone_device)
            .cloned();
        let mic_pick = pick_list(mics, mic, Msg::Mic)
            .text_size(13)
            .padding(Padding::from([7, 12]))
            .width(260)
            .style(theme::pick)
            .menu_style(theme::menu);
        sections = sections.push(section(
            "Sound",
            vec![
                setting(
                    "Desktop audio",
                    Some(if cfg!(target_os = "macos") {
                        "Game and app sound. Needs macOS 13 or later and the Screen Recording permission.".into()
                    } else {
                        "Game and app sound: whatever you hear.".into()
                    }),
                    switch(s.desktop_audio, Msg::DesktopAudio),
                ),
                setting(
                    "Microphone",
                    Some("Mixed into the clip and kept on its own track too.".into()),
                    row![if s.microphone { Element::from(mic_pick) } else { Space::new().into() }, switch(s.microphone, Msg::Microphone)]
                        .spacing(12)
                        .align_y(Alignment::Center),
                ),
            ],
        ));

        // Saving.
        let mut folder = row![
            icon_button(Icon::Folder, "Change", TEXT_2)
                .style(theme::secondary)
                .on_press(Msg::PickFolder)
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if !s.save_dir.is_empty() {
            folder = folder.push(
                button(text("Use default").size(13).font(MEDIUM))
                    .padding(Padding::from([7, 10]))
                    .style(theme::ghost)
                    .on_press(Msg::DefaultFolder),
            );
        }
        let cleanup_on = s.max_storage_gb > 0;
        let mut cleanup = row![].spacing(8).align_y(Alignment::Center);
        if cleanup_on {
            cleanup = cleanup
                .push(text("Keep under").size(13).color(TEXT_2))
                .push(
                    text_input("50", &self.gb_text)
                        .on_input(Msg::GbText)
                        .width(64)
                        .padding(Padding::from([7, 9]))
                        .size(13)
                        .style(theme::input),
                )
                .push(text("GB").size(13).color(MUTED));
        }
        cleanup = cleanup.push(switch(cleanup_on, Msg::Cleanup));
        sections = sections.push(section(
            "Saving",
            vec![
                setting("Clips folder", Some(s.save_dir().display().to_string()), folder),
                setting(
                    "Clean up old clips",
                    Some(format!(
                        "Clips use {} now. Over the limit, the oldest go first; favourites always stay.",
                        human_size(used)
                    )),
                    cleanup,
                ),
                setting(
                    "Discord size",
                    Some("The limit for Share to Discord.".into()),
                    segmented(
                        vec![(10u32, "10 MB".into()), (50, "50 MB".into()), (500, "500 MB".into())],
                        s.discord_mb,
                        Msg::Discord,
                    ),
                ),
                setting("Notification", Some("A small card in the corner when a clip is saved.".into()), switch(s.notify, Msg::Notify)),
                setting("Sound", Some("A short tone when a clip is saved.".into()), switch(s.sound, Msg::Sound)),
            ],
        ));

        // Rewindr.
        let mut updates = row![].spacing(8).align_y(Alignment::Center);
        if let Some(u) = &self.update.available {
            updates = updates.push(
                button(
                    row![
                        icon(Icon::Download, 15.0, iced::Color::WHITE),
                        text(if self.update.installing {
                            "Updating...".to_string()
                        } else {
                            format!("Update to {}", u.version)
                        })
                        .size(13)
                        .font(SEMIBOLD)
                    ]
                    .spacing(7)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([7, 12]))
                .style(theme::primary)
                .on_press_maybe((!self.update.installing).then_some(Msg::Install)),
            );
        } else {
            updates = updates.push(
                icon_button(
                    Icon::Refresh,
                    if self.update.checking {
                        "Checking..."
                    } else {
                        "Check now"
                    },
                    TEXT_2,
                )
                .style(theme::secondary)
                .on_press_maybe((!self.update.checking).then_some(Msg::CheckNow)),
            );
        }
        let update_note = match (&self.update.message, &self.update.available) {
            (Some((m, _)), _) => m.clone(),
            (None, Some(u)) => format!(
                "Rewindr {} is available. You have {}.",
                u.version,
                updater::current_version()
            ),
            (None, None) => format!("You have Rewindr {}.", updater::current_version()),
        };
        sections = sections.push(section(
            "Rewindr",
            vec![
                setting(
                    "Start with the computer",
                    None,
                    if self.launcher.is_some() {
                        switch(self.autostart, Msg::Autostart)
                    } else {
                        text("Not available here").size(12).color(MUTED).into()
                    },
                ),
                setting("Updates", Some(update_note), updates),
                setting(
                    "Check for updates automatically",
                    None,
                    switch(s.check_updates, Msg::CheckUpdates),
                ),
                setting(
                    "Troubleshooting",
                    None,
                    row![
                        icon_button(Icon::Log, "Open the log", TEXT_2)
                            .style(theme::ghost)
                            .on_press(Msg::OpenLog),
                        icon_button(Icon::Power, "Quit Rewindr", TEXT_2)
                            .style(theme::danger_ghost)
                            .on_press(Msg::Quit),
                    ]
                    .spacing(6),
                ),
            ],
        ));

        scrollable(container(sections).padding(Padding {
            top: 0.0,
            right: 28.0,
            bottom: 36.0,
            left: 28.0,
        }))
        .height(Fill)
        .style(theme::scroll)
        .into()
    }
}

fn check() -> Task<Msg> {
    Task::perform(
        background(|| updater::check().map(|u| u.map(Arc::new))),
        Msg::Checked,
    )
}

fn pick<'a>(
    options: Vec<Choice>,
    selected: Option<Choice>,
    on: impl Fn(Choice) -> Msg + 'a,
) -> Element<'a, Msg> {
    pick_list(options, selected, on)
        .text_size(13)
        .padding(Padding::from([7, 12]))
        .width(300)
        .style(theme::pick)
        .menu_style(theme::menu)
        .into()
}

fn switch<'a>(on: bool, msg: impl Fn(bool) -> Msg + 'a) -> Element<'a, Msg> {
    toggler(on)
        .on_toggle(msg)
        .size(22)
        .style(theme::toggle)
        .into()
}

fn setting<'a>(
    title: &str,
    note: Option<String>,
    control: impl Into<Element<'a, Msg>>,
) -> Element<'a, Msg> {
    row![
        container(label(title, note)).width(Length::FillPortion(5)),
        container(control.into())
            .width(Length::FillPortion(6))
            .align_x(iced::alignment::Horizontal::Right),
    ]
    .spacing(20)
    .align_y(Alignment::Center)
    .padding(Padding::from([14, 18]))
    .into()
}

fn section<'a>(title: &str, rows: Vec<Element<'a, Msg>>) -> Element<'a, Msg> {
    let mut body = column![];
    let n = rows.len();
    for (i, r) in rows.into_iter().enumerate() {
        body = body.push(r);
        if i + 1 < n {
            body = body.push(container(divider()).padding(Padding::from([0, 18])));
        }
    }
    column![
        text(title.to_string()).font(BOLD).size(16).color(TEXT),
        container(body).width(Fill).style(theme::panel),
    ]
    .spacing(10)
    .into()
}
