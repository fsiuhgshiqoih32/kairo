//! The settings page.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use iced::keyboard::{self, Key, key};
use iced::widget::{
    Space, button, column, container, pick_list, row, scrollable, slider, text, text_input, toggler,
};
use iced::{Alignment, Element, Fill, FillPortion, Length, Padding, Task};

use kairo::args::{self, Encoder};
use kairo::config::{
    ACCENTS, GameRule, MAX_CLIP_SECONDS, MAX_GAME_RULES, MAX_QUICK_SAVES, MIN_CLIP_SECONDS,
    Quality, QuickSave, Resolution, SAVE_SOUNDS, Settings, Storage, VideoCodec, normalize_phrase,
};

use super::icons::{Icon, icon};
use super::theme::{self, BOLD, MEDIUM, SEMIBOLD};
use super::widgets::{divider, icon_button, keycap, label, segmented};
use super::{background, human_size, seconds_label};
use crate::daemon::{self, Report, Request};
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
    /// The cameras, once listed.
    cameras: Option<Vec<crate::webcam::Camera>>,
    /// The mic test while it runs, and its meter (0 to 1, falling back slowly).
    mic_test: Option<crate::mictest::MicTest>,
    mic_level: f32,
    encoders: Option<Vec<Encoder>>,
    launcher: Option<auto_launch::AutoLaunch>,
    autostart: bool,
    /// Which shortcut the next key press sets.
    pub capturing_key: Option<Target>,
    hotkey_problem: Option<(Target, String)>,
    clip_text: String,
    /// The quick saves' lengths as typed.
    quick_texts: Vec<String>,
    gb_text: String,
    phrase_text: String,
    /// Bytes done and total while the speech files download.
    voice_install: Option<Arc<[AtomicU64; 2]>>,
    voice_problem: Option<String>,
    /// The Discord webhook link as pasted (kept only until it's checked).
    webhook_text: String,
    webhook_busy: bool,
    webhook_problem: Option<String>,
    pub update: Updates,
    /// A game's name as typed, to add it to the games with their own settings.
    new_game: String,
}

/// A shortcut in the settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Save,
    Screenshot,
    Marker,
    Session,
    /// Save a clip and copy a share link.
    Share,
    /// A game's own save shortcut, by its place in the list.
    Game(usize),
    /// A quick save, by its place in the list.
    Quick(usize),
}

#[derive(Default)]
pub struct Updates {
    pub checking: bool,
    pub installing: bool,
    /// Install as soon as the check finds the update ("Update now" in a notice).
    pub auto_install: bool,
    pub available: Option<Arc<updater::Update>>,
    pub message: Option<(String, bool)>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Capture(Target),
    ScreenshotOff,
    MarkerOff,
    SessionOff,
    ShareOff,
    /// Open a version's release notes.
    OpenNotes(String),
    RecordWhen(&'static str),
    SaveSound(&'static str),
    PickSound,
    SoundPicked(Option<PathBuf>),
    PlaySound,
    NewGameText(String),
    /// Adds a game: the one named, or the typed name.
    AddGame(Option<String>),
    RemoveGame(usize),
    GameName(usize, String),
    GameSeconds(usize, u32),
    GameQuality(usize, Option<Quality>),
    GameKind(usize, &'static str),
    GameKeyOff(usize),
    SessionAuto(bool),
    ChatTrack(bool),
    ChatApp(String),
    InputOverlay(bool),
    Voice(bool),
    VoiceInstalled(Result<(), String>),
    Phrase(String),
    ClipSeconds(u32),
    ClipText(String),
    AddQuickSave,
    RemoveQuickSave(usize),
    QuickText(usize, String),
    PostRoll(u32),
    FreshAfterSave(bool),
    Codec(VideoCodec),
    Monitor(Choice),
    Fps(u32),
    Resolution(Resolution),
    Quality(Quality),
    Storage(Storage),
    Accent(&'static str),
    Look(&'static str),
    Motion(&'static str),
    Encoder(Choice),
    Encoders(Vec<Encoder>),
    DesktopAudio(bool),
    Microphone(bool),
    Mic(Choice),
    /// The microphone's volume in clips, in percent.
    MicVolume(u32),
    /// Starts or stops the mic test.
    MicTest(bool),
    /// Records a few seconds and plays them back.
    MicRecord,
    /// Moves the mic test's meter on.
    MicTick,
    Webcam(bool),
    Cameras(Vec<crate::webcam::Camera>),
    Camera(Choice),
    WebcamCorner(&'static str),
    WebcamSize(u32),
    PickFolder,
    Folder(Option<PathBuf>),
    DefaultFolder,
    Notify(bool),
    Sound(bool),
    Cleanup(bool),
    GbText(String),
    Discord(u32),
    WebhookText(String),
    ConnectWebhook,
    /// The webhook works (its name) and is stored, or why not.
    WebhookChecked(Result<String, String>),
    DisconnectWebhook,
    KeepDays(u32),
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
    /// Open a link in the browser.
    OpenUrl(String),
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
        let cameras = Task::perform(
            background(|| {
                crate::ffmpeg::path()
                    .map(|f| crate::webcam::cameras(&f))
                    .unwrap_or_default()
            }),
            Msg::Cameras,
        );
        let mut tasks = vec![encoders, cameras];
        let mut update = Updates::default();
        if settings.check_updates && std::env::var_os("KAIRO_NO_UPDATE_CHECK").is_none() {
            update.checking = true;
            tasks.push(check());
        }
        (
            State {
                monitors: platform::monitors(),
                mics: crate::audio::microphones(),
                cameras: None,
                mic_test: None,
                mic_level: 0.0,
                encoders: None,
                launcher,
                autostart,
                capturing_key: None,
                hotkey_problem: None,
                clip_text: settings.clip_seconds.to_string(),
                new_game: String::new(),
                quick_texts: settings
                    .quick_saves
                    .iter()
                    .map(|q| q.seconds.to_string())
                    .collect(),
                gb_text: settings.max_storage_gb.max(1).to_string(),
                phrase_text: settings.voice_phrase.clone(),
                voice_install: None,
                voice_problem: None,
                webhook_text: String::new(),
                webhook_busy: false,
                webhook_problem: None,
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
        let Some(target) = self.capturing_key else {
            return Effect::None;
        };
        if matches!(k.as_ref(), Key::Named(key::Named::Escape)) {
            self.capturing_key = None;
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
        let alone = parts.is_empty();
        parts.push(name.clone());
        let spec = parts.join("+");
        self.capturing_key = None;
        if alone && !hotkey::ok_alone(&name) {
            self.hotkey_problem = Some((
                target,
                format!(
                    "{spec} on its own would stop working in every other app. Add Ctrl, Alt or \
                     Shift, or use an F-key."
                ),
            ));
            return Effect::None;
        }
        let taken = [
            (Target::Save, &s.hotkey),
            (Target::Screenshot, &s.screenshot_hotkey),
            (Target::Marker, &s.marker_hotkey),
            (Target::Session, &s.session_hotkey),
            (Target::Share, &s.share_hotkey),
        ]
        .into_iter()
        .chain(
            s.quick_saves
                .iter()
                .enumerate()
                .map(|(i, q)| (Target::Quick(i), &q.hotkey)),
        )
        .chain(
            s.game_rules
                .iter()
                .enumerate()
                .map(|(i, r)| (Target::Game(i), &r.hotkey)),
        )
        .any(|(t, k)| t != target && k.eq_ignore_ascii_case(&spec));
        if taken {
            self.hotkey_problem = Some((target, format!("{spec} is already another shortcut.")));
            return Effect::None;
        }
        match hotkey::parse(&spec) {
            Ok(_) => {
                match target {
                    Target::Save => s.hotkey = spec,
                    Target::Screenshot => s.screenshot_hotkey = spec,
                    Target::Marker => s.marker_hotkey = spec,
                    Target::Session => s.session_hotkey = spec,
                    Target::Share => s.share_hotkey = spec,
                    Target::Game(i) => {
                        if let Some(r) = s.game_rules.get_mut(i) {
                            r.hotkey = spec;
                        }
                    }
                    Target::Quick(i) => {
                        if let Some(q) = s.quick_saves.get_mut(i) {
                            q.hotkey = spec;
                        }
                    }
                }
                self.hotkey_problem = None;
                Effect::Changed
            }
            Err(_) => {
                self.hotkey_problem =
                    Some((target, format!("{spec} can't be used as a shortcut.")));
                Effect::None
            }
        }
    }

    /// Which sound plays when a clip is saved, with a button to hear it.
    fn sound_row<'a>(&'a self, s: &'a Settings) -> Element<'a, Msg> {
        let mut options: Vec<(&'static str, String)> = SAVE_SOUNDS
            .iter()
            .map(|(k, name)| (*k, name.to_string()))
            .collect();
        options.push(("custom", "Your own".to_string()));
        let picked = SAVE_SOUNDS
            .iter()
            .map(|(k, _)| *k)
            .chain(std::iter::once("custom"))
            .find(|k| *k == s.save_sound)
            .unwrap_or("chime");
        let mut controls = row![
            segmented(options, picked, Msg::SaveSound),
            icon_button(Icon::Volume, "Play", theme::ink_2())
                .style(theme::secondary)
                .on_press(Msg::PlaySound),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if s.save_sound == "custom" {
            controls = controls.push(
                button(text("Choose...").size(13).font(MEDIUM))
                    .padding(Padding::from([8, 12]))
                    .style(theme::ghost)
                    .on_press(Msg::PickSound),
            );
        }
        let note = if s.save_sound == "custom" {
            std::path::Path::new(&s.save_sound_file)
                .file_name()
                .map(|n| format!("Plays {}.", n.to_string_lossy()))
        } else {
            Some("Built in, or a WAV file of your own.".into())
        };
        setting("Save sound", note, controls)
    }

    /// Games with their own length, quality and shortcut, or marked as a game or private.
    fn games_rows<'a>(
        &'a self,
        s: &'a Settings,
        report: Option<&'a Report>,
        known: &'a [(String, usize)],
    ) -> Vec<Element<'a, Msg>> {
        let _ = report;
        let mut rows: Vec<Element<'a, Msg>> = vec![
            container(
                text(
                    "Give a game its own clip length, quality or save shortcut. Kairo finds it \
                     by the start of its window title, so \"Minecraft\" also covers \
                     \"Minecraft 26.3\". \"Never record\" keeps no replay while that app is in \
                     front.",
                )
                .size(13)
                .color(theme::muted()),
            )
            .padding(Padding::from([14, 18]))
            .into(),
        ];
        for (i, r) in s.game_rules.iter().enumerate() {
            let top = row![
                text_input("Game", &r.game)
                    .on_input(move |t| Msg::GameName(i, t))
                    .padding(Padding::from([8, 10]))
                    .size(14)
                    .width(Length::Fixed(220.0))
                    .style(theme::input),
                Space::new().width(Fill),
                segmented(
                    vec![
                        ("", "As usual".to_string()),
                        ("game", "Always a game".to_string()),
                        ("never", "Never record".to_string()),
                    ],
                    match r.kind.as_str() {
                        "game" => "game",
                        "never" => "never",
                        _ => "",
                    },
                    move |k| Msg::GameKind(i, k),
                ),
                icon_button(Icon::Trash, "Remove", theme::ink_2())
                    .style(theme::danger_ghost)
                    .on_press(Msg::RemoveGame(i)),
            ]
            .spacing(10)
            .align_y(Alignment::Center);
            let lengths = [0u32, 15, 30, 60, 120, 300];
            let length = lengths
                .iter()
                .copied()
                .min_by_key(|l| l.abs_diff(r.clip_seconds))
                .unwrap_or(0);
            let mut key_row = row![self.key_capture(Target::Game(i), &r.hotkey, None)]
                .spacing(6)
                .align_y(Alignment::Start);
            if !r.hotkey.is_empty() {
                key_row = key_row.push(
                    button(text("Usual").size(13).font(MEDIUM))
                        .padding(Padding::from([9, 10]))
                        .style(theme::ghost)
                        .on_press(Msg::GameKeyOff(i)),
                );
            }
            let bottom = row![
                column![
                    text("Length").size(12).color(theme::muted()),
                    segmented(
                        lengths
                            .iter()
                            .map(|l| (
                                *l,
                                if *l == 0 {
                                    "Usual".to_string()
                                } else {
                                    seconds_label(*l)
                                }
                            ))
                            .collect(),
                        length,
                        move |v| Msg::GameSeconds(i, v),
                    ),
                ]
                .spacing(4),
                column![
                    text("Quality").size(12).color(theme::muted()),
                    segmented(
                        vec![
                            (None, "Usual".to_string()),
                            (Some(Quality::Low), "Low".to_string()),
                            (Some(Quality::Medium), "Medium".to_string()),
                            (Some(Quality::High), "High".to_string()),
                        ],
                        r.quality,
                        move |q| Msg::GameQuality(i, q),
                    ),
                ]
                .spacing(4),
                Space::new().width(Fill),
                column![text("Save key").size(12).color(theme::muted()), key_row]
                    .spacing(4)
                    .align_x(Alignment::End),
            ]
            .spacing(14)
            .align_y(Alignment::End);
            rows.push(
                container(column![top, bottom].spacing(12))
                    .padding(Padding::from([14, 18]))
                    .width(Fill)
                    .into(),
            );
        }
        if s.game_rules.len() < MAX_GAME_RULES {
            let suggestions: Vec<String> = known
                .iter()
                .map(|(g, _)| g.clone())
                .filter(|g| !s.game_rules.iter().any(|r| r.game.eq_ignore_ascii_case(g)))
                .collect();
            let mut add = row![
                text_input("A game's name", &self.new_game)
                    .on_input(Msg::NewGameText)
                    .on_submit(Msg::AddGame(None))
                    .padding(Padding::from([8, 10]))
                    .size(14)
                    .width(Length::Fixed(220.0))
                    .style(theme::input),
                button(text("Add").size(13).font(SEMIBOLD))
                    .padding(Padding::from([8, 14]))
                    .style(theme::secondary)
                    .on_press_maybe(
                        (!self.new_game.trim().is_empty()).then_some(Msg::AddGame(None))
                    ),
            ]
            .spacing(8)
            .align_y(Alignment::Center);
            if !suggestions.is_empty() {
                add = add.push(Space::new().width(Fill)).push(
                    pick_list(suggestions, None::<String>, |g| Msg::AddGame(Some(g)))
                        .placeholder("From your clips")
                        .text_size(13)
                        .padding(Padding::from([7, 12]))
                        .style(theme::pick)
                        .menu_style(theme::menu),
                );
            }
            rows.push(
                container(add)
                    .padding(Padding::from([14, 18]))
                    .width(Fill)
                    .into(),
            );
        }
        rows
    }

    fn start_mic_test(&mut self, s: &Settings) {
        self.mic_test = Some(crate::mictest::MicTest::start(
            &s.microphone_device,
            s.mic_volume as f32 / 100.0,
        ));
        self.mic_level = 0.0;
    }

    /// Shows the mic test at `level` without opening a microphone (for previews).
    #[cfg(test)]
    pub fn show_mic_test(&mut self, level: f32) {
        self.mic_test = Some(crate::mictest::MicTest::idle("Microphone (Shure MV7+)"));
        self.mic_level = level;
    }

    /// The mic test is running (the window then ticks its meter).
    pub fn mic_testing(&self) -> bool {
        self.mic_test.is_some()
    }

    pub fn stop_mic_test(&mut self) {
        self.mic_test = None;
        self.mic_level = 0.0;
    }

    /// The mic test's row: the level, what's happening, and record-and-play-back.
    fn mic_test_view<'a>(&'a self, t: &'a crate::mictest::MicTest) -> Element<'a, Msg> {
        use crate::mictest::Phase;
        let level = self.mic_level.clamp(0.0, 1.0);
        let color = if level > 0.92 {
            theme::RED
        } else if level > 0.75 {
            theme::GOLD
        } else {
            theme::GREEN
        };
        let on = (level * 1000.0).round() as u16;
        let mut bar = row![].width(Fill).height(10);
        if on > 0 {
            bar = bar.push(
                container(Space::new().width(Fill).height(10))
                    .width(FillPortion(on))
                    .style(move |_| container::Style {
                        background: Some(color.into()),
                        border: iced::Border {
                            radius: 5.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
            );
        }
        bar = bar.push(Space::new().width(FillPortion(1000 - on.min(999))));
        let meter = container(bar).width(240).style(|_| container::Style {
            background: Some(theme::raised().into()),
            border: iced::Border {
                radius: 5.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
        let phase = t.phase();
        let line = match (t.error(), phase) {
            (Some(e), _) => text(format!("Couldn't open the mic: {e}"))
                .size(12)
                .color(theme::RED),
            (None, Phase::Listening) => text(
                "Talk as you would in a game: the bar should sit in the yellow, and only shouts \
                 should touch the red. Hear yourself records a few seconds and plays them back.",
            )
            .size(12)
            .color(theme::muted()),
            (None, Phase::Recording(until)) => text(format!(
                "Recording... {} s left. Say something.",
                until
                    .saturating_duration_since(std::time::Instant::now())
                    .as_secs()
                    + 1
            ))
            .size(12)
            .color(theme::accent()),
            (None, Phase::Playing(_)) => text("Playing it back.").size(12).color(theme::GREEN),
        };
        let name = t.name.lock().unwrap().clone();
        column![
            row![
                meter,
                button(text("Hear yourself").size(12).font(MEDIUM))
                    .padding(Padding::from([6, 10]))
                    .style(theme::secondary)
                    .on_press_maybe((phase == Phase::Listening).then_some(Msg::MicRecord)),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            line,
            text(if name.is_empty() {
                String::new()
            } else {
                format!("Testing {name}")
            })
            .size(11)
            .color(theme::muted()),
        ]
        .spacing(8)
        .align_x(Alignment::End)
        .into()
    }

    /// "Start with the computer", when this system can.
    pub(super) fn autostart_state(&self) -> Option<bool> {
        self.launcher.is_some().then_some(self.autostart)
    }

    pub fn update(&mut self, s: &mut Settings, msg: Msg) -> (Task<Msg>, Effect) {
        let changed = (Task::none(), Effect::Changed);
        match msg {
            Msg::Capture(t) => {
                self.capturing_key = if self.capturing_key == Some(t) {
                    None
                } else {
                    Some(t)
                };
            }
            Msg::ScreenshotOff => {
                s.screenshot_hotkey.clear();
                self.capturing_key = None;
                return changed;
            }
            Msg::MarkerOff => {
                s.marker_hotkey.clear();
                self.capturing_key = None;
                return changed;
            }
            Msg::ShareOff => {
                s.share_hotkey.clear();
                self.capturing_key = None;
                return changed;
            }
            Msg::OpenNotes(url) => return (Task::none(), Effect::OpenUrl(url)),
            Msg::RecordWhen(v) => {
                s.record_when = v.into();
                return changed;
            }
            Msg::SaveSound(k) => {
                s.save_sound = k.into();
                if k == "custom" && platform::custom_sound(&s.save_sound_file).is_none() {
                    return self.update(s, Msg::PickSound);
                }
                platform::play_save_sound(&s.save_sound, &s.save_sound_file);
                return changed;
            }
            Msg::PickSound => {
                return (
                    Task::perform(
                        background(|| {
                            rfd::FileDialog::new()
                                .add_filter("WAV sound", &["wav"])
                                .pick_file()
                        }),
                        Msg::SoundPicked,
                    ),
                    Effect::None,
                );
            }
            Msg::SoundPicked(Some(p)) => {
                let file = p.display().to_string();
                if platform::custom_sound(&file).is_some() {
                    s.save_sound_file = file;
                    s.save_sound = "custom".into();
                    platform::play_save_sound(&s.save_sound, &s.save_sound_file);
                    return changed;
                }
            }
            Msg::SoundPicked(None) => {
                if s.save_sound == "custom" && platform::custom_sound(&s.save_sound_file).is_none()
                {
                    s.save_sound = "chime".into();
                    return changed;
                }
            }
            Msg::PlaySound => platform::play_save_sound(&s.save_sound, &s.save_sound_file),
            Msg::NewGameText(t) => self.new_game = t,
            Msg::AddGame(name) => {
                let name = name.unwrap_or_else(|| std::mem::take(&mut self.new_game));
                let name = name.trim().to_string();
                if !name.is_empty()
                    && s.game_rules.len() < MAX_GAME_RULES
                    && !s
                        .game_rules
                        .iter()
                        .any(|r| r.game.eq_ignore_ascii_case(&name))
                {
                    s.game_rules.push(GameRule {
                        game: name,
                        ..Default::default()
                    });
                    return changed;
                }
            }
            Msg::RemoveGame(i) => {
                if i < s.game_rules.len() {
                    s.game_rules.remove(i);
                    self.capturing_key = None;
                    return changed;
                }
            }
            Msg::GameName(i, t) => {
                if let Some(r) = s.game_rules.get_mut(i) {
                    r.game = t;
                    return changed;
                }
            }
            Msg::GameSeconds(i, v) => {
                if let Some(r) = s.game_rules.get_mut(i) {
                    r.clip_seconds = v;
                    return changed;
                }
            }
            Msg::GameQuality(i, q) => {
                if let Some(r) = s.game_rules.get_mut(i) {
                    r.quality = q;
                    return changed;
                }
            }
            Msg::GameKind(i, k) => {
                if let Some(r) = s.game_rules.get_mut(i) {
                    r.kind = k.into();
                    return changed;
                }
            }
            Msg::GameKeyOff(i) => {
                if let Some(r) = s.game_rules.get_mut(i) {
                    r.hotkey.clear();
                    self.capturing_key = None;
                    return changed;
                }
            }
            Msg::SessionOff => {
                s.session_hotkey.clear();
                self.capturing_key = None;
                return changed;
            }
            Msg::SessionAuto(on) => {
                s.session_auto = on;
                return changed;
            }
            Msg::ChatTrack(on) => {
                s.chat_track = on;
                return changed;
            }
            Msg::InputOverlay(on) => {
                s.input_overlay = on;
                return changed;
            }
            Msg::ChatApp(t) => {
                s.chat_app = t;
                return changed;
            }
            Msg::Voice(false) => {
                s.voice_clip = false;
                self.voice_problem = None;
                return changed;
            }
            Msg::Voice(true) => {
                self.voice_problem = None;
                if crate::voice::installed() {
                    s.voice_clip = true;
                    return changed;
                }
                if !crate::voice::available() {
                    self.voice_problem =
                        Some("Voice clipping isn't available on this kind of computer yet.".into());
                } else if self.voice_install.is_none() {
                    let progress: Arc<[AtomicU64; 2]> =
                        Arc::new([AtomicU64::new(0), AtomicU64::new(1)]);
                    self.voice_install = Some(progress.clone());
                    return (
                        Task::perform(
                            background(move || {
                                crate::voice::install(&|done, total| {
                                    progress[0].store(done, Ordering::Relaxed);
                                    progress[1].store(total, Ordering::Relaxed);
                                })
                            }),
                            Msg::VoiceInstalled,
                        ),
                        Effect::None,
                    );
                }
            }
            Msg::VoiceInstalled(r) => {
                self.voice_install = None;
                match r {
                    Ok(()) => {
                        s.voice_clip = true;
                        return changed;
                    }
                    Err(e) => self.voice_problem = Some(e),
                }
            }
            Msg::Phrase(t) => {
                let n = normalize_phrase(&t);
                self.phrase_text = t;
                if !n.is_empty() && n != s.voice_phrase {
                    s.voice_phrase = n;
                    return changed;
                }
            }
            Msg::ClipSeconds(v) => {
                s.clip_seconds = v;
                self.clip_text = v.to_string();
                return changed;
            }
            Msg::AddQuickSave => {
                if s.quick_saves.len() < MAX_QUICK_SAVES {
                    let seconds = [15, 60, 300][s.quick_saves.len()];
                    s.quick_saves.push(QuickSave {
                        hotkey: String::new(),
                        seconds,
                    });
                    self.quick_texts.push(seconds.to_string());
                    // Ask for its key straight away.
                    self.capturing_key = Some(Target::Quick(s.quick_saves.len() - 1));
                    return changed;
                }
            }
            Msg::RemoveQuickSave(i) => {
                if i < s.quick_saves.len() {
                    s.quick_saves.remove(i);
                    self.quick_texts.remove(i);
                    self.capturing_key = None;
                    return changed;
                }
            }
            Msg::QuickText(i, t) => {
                let t: String = t.chars().filter(char::is_ascii_digit).take(4).collect();
                if let Some(slot) = self.quick_texts.get_mut(i) {
                    *slot = t.clone();
                }
                if let (Ok(v), Some(q)) = (t.parse::<u32>(), s.quick_saves.get_mut(i))
                    && (MIN_CLIP_SECONDS..=MAX_CLIP_SECONDS).contains(&v)
                {
                    q.seconds = v;
                    return changed;
                }
            }
            Msg::PostRoll(v) => {
                s.post_roll = v;
                return changed;
            }
            Msg::FreshAfterSave(v) => {
                s.fresh_after_save = v;
                return changed;
            }
            Msg::Codec(c) => {
                s.codec = c;
                return changed;
            }
            Msg::ClipText(t) => {
                self.clip_text = t.chars().filter(char::is_ascii_digit).take(4).collect();
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
                if self.mic_test.is_some() {
                    self.start_mic_test(s);
                }
                return changed;
            }
            Msg::MicVolume(v) => {
                s.mic_volume = v.min(kairo::config::MAX_MIC_VOLUME);
                if let Some(t) = &self.mic_test {
                    t.set_gain(s.mic_volume as f32 / 100.0);
                }
                return changed;
            }
            Msg::MicTest(on) => {
                if on {
                    self.start_mic_test(s);
                } else {
                    self.stop_mic_test();
                }
            }
            Msg::MicRecord => {
                if let Some(t) = &self.mic_test {
                    t.record();
                }
            }
            Msg::MicTick => {
                if let Some(t) = &self.mic_test {
                    let now = kairo::pcm::meter(t.take_peak());
                    self.mic_level = now.max(self.mic_level - 0.035);
                }
            }
            Msg::Webcam(v) => {
                s.webcam = v;
                return changed;
            }
            Msg::Cameras(c) => self.cameras = Some(c),
            Msg::Camera(c) => {
                s.webcam_device = c.value;
                return changed;
            }
            Msg::WebcamCorner(c) => {
                s.webcam_corner = c.into();
                return changed;
            }
            Msg::WebcamSize(v) => {
                s.webcam_size = v;
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
            Msg::Accent(name) => {
                s.accent = name.into();
                theme::set_accent(name);
                return changed;
            }
            Msg::Look(look) => {
                s.look = look.into();
                theme::apply_look(look);
                return changed;
            }
            Msg::Motion(m) => {
                s.motion = m.into();
                return changed;
            }
            Msg::Storage(v) => {
                s.replay_storage = v;
                return changed;
            }
            Msg::Discord(mb) => {
                s.discord_mb = mb;
                return changed;
            }
            Msg::WebhookText(t) => {
                self.webhook_text = t;
                self.webhook_problem = None;
            }
            Msg::ConnectWebhook => {
                if !self.webhook_busy && !self.webhook_text.trim().is_empty() {
                    self.webhook_busy = true;
                    self.webhook_problem = None;
                    let url = self.webhook_text.trim().to_string();
                    return (
                        Task::perform(
                            background(move || {
                                let name = crate::share::check_webhook(&url)?;
                                crate::secret::set(crate::secret::WEBHOOK, &url)?;
                                Ok(name)
                            }),
                            Msg::WebhookChecked,
                        ),
                        Effect::None,
                    );
                }
            }
            Msg::WebhookChecked(r) => {
                self.webhook_busy = false;
                match r {
                    Ok(name) => {
                        self.webhook_text.clear();
                        s.discord_channel = name;
                        return changed;
                    }
                    Err(e) => self.webhook_problem = Some(e),
                }
            }
            Msg::DisconnectWebhook => {
                s.discord_channel.clear();
                return (
                    Task::perform(
                        background(|| crate::secret::remove(crate::secret::WEBHOOK)),
                        |_| Msg::WebhookText(String::new()),
                    ),
                    Effect::Changed,
                );
            }
            Msg::KeepDays(d) => {
                s.keep_days = d;
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
                    Ok(Some(u)) => {
                        self.update.available = Some(u);
                        if std::mem::take(&mut self.update.auto_install) {
                            return self.update(s, Msg::Install);
                        }
                    }
                    Ok(None) => {
                        self.update.auto_install = false;
                        self.update.message = Some((
                            format!(
                                "Kairo {} is the latest version.",
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

    /// What the quality means, and where the replay buffer lives and what it takes.
    fn memory_estimate(&self, s: &Settings) -> (String, String) {
        let m = platform::choose_monitor(&s.monitor);
        let (w, h) = m.map_or((1920, 1080), |m| (m.w, m.h));
        let (ow, oh) = args::output_size((w, h), s.resolution.max_height());
        let bps = (s.quality.bitrate(ow as u64 * oh as u64, s.fps) as f64
            * s.codec.bitrate_factor()) as u64;
        let quality = format!(
            "{ow} x {oh} at about {:.0} Mbit/s (less in calm scenes).",
            bps as f64 / 1e6
        );
        let worst = human_size(s.buffer_worst_bytes(bps));
        let mut storage = if s.buffer_on_disk(bps) {
            format!(
                "The last {} goes in a scratch file of up to {worst}. The system keeps it in spare memory \
                 and frees that for games when they need it, so Kairo itself stays small.",
                seconds_label(s.clip_seconds)
            )
        } else {
            format!(
                "The last {} stays in Kairo's memory: up to {worst} in busy scenes.",
                seconds_label(s.clip_seconds)
            )
        };
        if s.replay_storage == Storage::Auto {
            storage.push_str(" Automatic puts anything over 1 GB on disk.");
        }
        (quality, storage)
    }

    /// The quick-save rows: a shortcut and a length each, and a button to add one.
    fn quick_saves_view<'a>(
        &'a self,
        s: &'a Settings,
        report: Option<&'a Report>,
    ) -> Element<'a, Msg> {
        let mut col = column![].spacing(8).align_x(Alignment::End);
        for (i, q) in s.quick_saves.iter().enumerate() {
            let error = report
                .and_then(|r| r.quick_save_errors.get(i).cloned())
                .flatten();
            col = col.push(
                row![
                    self.key_capture(Target::Quick(i), &q.hotkey, error),
                    text("saves").size(13).color(theme::muted()),
                    text_input("15", self.quick_texts.get(i).map_or("", String::as_str))
                        .on_input(move |t| Msg::QuickText(i, t))
                        .width(60)
                        .padding(Padding::from([7, 9]))
                        .size(13)
                        .style(theme::input),
                    text("s").size(13).color(theme::muted()),
                    button(icon(Icon::Close, 14.0, theme::ink_2()))
                        .padding(7)
                        .style(theme::ghost)
                        .on_press(Msg::RemoveQuickSave(i)),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            );
        }
        if s.quick_saves.len() < MAX_QUICK_SAVES {
            col = col.push(
                button(text("Add a quick save").size(13).font(MEDIUM))
                    .padding(Padding::from([8, 12]))
                    .style(theme::secondary)
                    .on_press(Msg::AddQuickSave),
            );
        }
        col.into()
    }

    /// The button that shows a shortcut and, when clicked, waits for a new one.
    pub(super) fn key_capture<'a>(
        &'a self,
        target: Target,
        spec: &'a str,
        error: Option<String>,
    ) -> Element<'a, Msg> {
        let capturing = self.capturing_key == Some(target);
        let label = if capturing {
            row![
                text("Press a key")
                    .font(SEMIBOLD)
                    .size(14)
                    .color(theme::accent())
            ]
        } else if spec.is_empty() {
            // A game's own key falls back to the usual one.
            let off = if matches!(target, Target::Game(_)) {
                "Usual"
            } else {
                "Off"
            };
            row![text(off).font(SEMIBOLD).size(14).color(theme::muted())]
        } else {
            spec.split('+')
                .fold(row![].spacing(4), |r, k| r.push(keycap(k)))
        };
        let key_button = button(label.align_y(Alignment::Center))
            .padding(Padding::from([8, 14]))
            .style(move |t, st| {
                if capturing {
                    theme::segment(true)(t, st)
                } else {
                    theme::secondary(t, st)
                }
            })
            .on_press(Msg::Capture(target));
        let note = if capturing {
            Some("Add Ctrl, Alt or Shift if you like. Esc cancels.".to_string())
        } else {
            self.hotkey_problem
                .as_ref()
                .filter(|(t, _)| *t == target)
                .map(|(_, p)| p.clone())
                .or(error)
        };
        let mut col = column![key_button].spacing(6).align_x(Alignment::End);
        if let Some(n) = note {
            col = col.push(text(n).size(12).color(if capturing {
                theme::muted()
            } else {
                theme::accent()
            }));
        }
        col.into()
    }

    pub fn view<'a>(
        &'a self,
        s: &'a Settings,
        report: Option<&'a Report>,
        used: u64,
        known_games: &'a [(String, usize)],
    ) -> Element<'a, Msg> {
        let mut sections = column![].spacing(26).max_width(780);
        if let Some(u) = &self.update.available {
            let mut points = column![].spacing(6);
            for p in updater::highlights(&u.notes, 5) {
                points = points.push(
                    row![
                        icon(Icon::Check, 14.0, theme::accent()),
                        text(p).size(13).color(theme::ink())
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                );
            }
            let actions: Element<'_, Msg> = if self.update.installing {
                text(format!(
                    "Installing Kairo {}. It restarts on its own in a moment.",
                    u.version
                ))
                .size(13)
                .color(theme::ink_2())
                .into()
            } else {
                row![
                    button(
                        row![
                            icon(Icon::Download, 15.0, theme::on_accent()),
                            text("Update and restart").size(13).font(SEMIBOLD)
                        ]
                        .spacing(7)
                        .align_y(Alignment::Center)
                    )
                    .padding(Padding::from([8, 14]))
                    .style(theme::primary)
                    .on_press(Msg::Install),
                    button(
                        row![
                            icon(Icon::External, 14.0, theme::ink_2()),
                            text("Release notes").size(13).font(MEDIUM)
                        ]
                        .spacing(7)
                        .align_y(Alignment::Center)
                    )
                    .padding(Padding::from([8, 14]))
                    .style(theme::secondary)
                    .on_press(Msg::OpenNotes(updater::release_page(&u.version))),
                ]
                .spacing(8)
                .into()
            };
            sections = sections.push(
                container(
                    column![
                        text(format!("Kairo {} is ready", u.version))
                            .font(BOLD)
                            .size(18),
                        text(format!(
                            "You have {}. Updating takes a few seconds and keeps your clips and settings.",
                            updater::current_version()
                        ))
                        .size(13)
                        .color(theme::ink_2()),
                        points,
                        actions,
                    ]
                    .spacing(12),
                )
                .padding(18)
                .width(Fill)
                .style(theme::well),
            );
        }
        let save_error = report.and_then(|r| r.hotkey_error.clone());
        let shot_error = report.and_then(|r| r.screenshot_hotkey_error.clone());
        let key_col = self.key_capture(Target::Save, &s.hotkey, save_error);
        let mut shot_row =
            row![self.key_capture(Target::Screenshot, &s.screenshot_hotkey, shot_error)]
                .spacing(8)
                .align_y(Alignment::Start);
        if !s.screenshot_hotkey.is_empty() {
            shot_row = shot_row.push(
                button(text("Turn off").size(13).font(MEDIUM))
                    .padding(Padding::from([9, 10]))
                    .style(theme::ghost)
                    .on_press(Msg::ScreenshotOff),
            );
        }
        let mark_error = report.and_then(|r| r.marker_hotkey_error.clone());
        let mut mark_row = row![self.key_capture(Target::Marker, &s.marker_hotkey, mark_error)]
            .spacing(8)
            .align_y(Alignment::Start);
        if !s.marker_hotkey.is_empty() {
            mark_row = mark_row.push(
                button(text("Turn off").size(13).font(MEDIUM))
                    .padding(Padding::from([9, 10]))
                    .style(theme::ghost)
                    .on_press(Msg::MarkerOff),
            );
        }
        let session_error = report.and_then(|r| r.session_hotkey_error.clone());
        let mut session_row =
            row![self.key_capture(Target::Session, &s.session_hotkey, session_error)]
                .spacing(8)
                .align_y(Alignment::Start);
        if !s.session_hotkey.is_empty() {
            session_row = session_row.push(
                button(text("Turn off").size(13).font(MEDIUM))
                    .padding(Padding::from([9, 10]))
                    .style(theme::ghost)
                    .on_press(Msg::SessionOff),
            );
        }
        let gb_hour = {
            let m = platform::choose_monitor(&s.monitor);
            let (w, h) = m.map_or((1920, 1080), |m| (m.w, m.h));
            let (ow, oh) = args::output_size((w, h), s.resolution.max_height());
            let bps =
                s.quality.bitrate(ow as u64 * oh as u64, s.fps) as f64 * s.codec.bitrate_factor();
            bps * 3600.0 / 8.0 / 1e9
        };
        if cfg!(windows) {
            sections = sections.push(section(
                "Overlay",
                vec![setting(
                    "Keys and clicks",
                    Some(
                        "Remembers the gaming keys you press (WASD, QERF, Space, Shift, Ctrl, Tab, \
                         1-4) and your mouse buttons while recording, so copies can show them. \
                         Nothing else you type is kept."
                            .into(),
                    ),
                    switch(s.input_overlay, Msg::InputOverlay),
                )],
            ));
        }
        sections = sections.push(section(
            "Sessions",
            vec![
                setting(
                    "Session shortcut",
                    Some(format!(
                        "Starts and stops recording everything, saved to the Sessions folder when \
                         you stop. Saves and marks become bookmarks on its timeline. Up to about \
                         {gb_hour:.0} GB an hour at these settings."
                    )),
                    session_row,
                ),
                setting(
                    "Record a session when Kairo starts",
                    None,
                    switch(s.session_auto, Msg::SessionAuto),
                ),
            ],
        ));
        sections = sections.push(section(
            "Replay",
            vec![
                setting(
                    "Keep a replay",
                    Some(if cfg!(windows) {
                        "Only in games pauses the replay when no game has been in front for 20 \
                         seconds: a full-screen window, or a game you mark below."
                            .into()
                    } else {
                        "Only in games needs Windows; elsewhere Kairo always keeps a replay.".into()
                    }),
                    segmented(
                        vec![
                            ("always", "Always, desktop too".to_string()),
                            ("games", "Only in games".to_string()),
                        ],
                        if s.record_when == "games" {
                            "games"
                        } else {
                            "always"
                        },
                        Msg::RecordWhen,
                    ),
                ),
                setting(
                    "Save shortcut",
                    Some("Works everywhere, even in full-screen games.".into()),
                    key_col,
                ),
                setting(
                    "Quick saves",
                    Some(
                        "Extra shortcuts that save another length: 15 s for one play, \
                         5 min for the whole round."
                            .into(),
                    ),
                    self.quick_saves_view(s, report),
                ),
                setting(
                    "Keep recording after the press",
                    Some("Catches what happens right after you press save.".into()),
                    segmented(
                        vec![
                            (0u32, "Off".to_string()),
                            (2, "2 s".to_string()),
                            (5, "5 s".to_string()),
                            (10, "10 s".to_string()),
                        ],
                        s.post_roll,
                        Msg::PostRoll,
                    ),
                ),
                setting(
                    "Start fresh after saving",
                    Some("Empties the replay, so the next clip doesn't repeat this one.".into()),
                    switch(s.fresh_after_save, Msg::FreshAfterSave),
                ),
                setting(
                    "Screenshot shortcut",
                    Some("Saves a full-size picture of the moment to Screenshots.".into()),
                    shot_row,
                ),
                setting(
                    "Marker shortcut",
                    Some(
                        "Marks a moment while you play, with a short tick. Clips that hold it \
                         show a flag there on their timeline."
                            .into(),
                    ),
                    mark_row,
                ),
                setting(
                    "Clip length",
                    Some(format!(
                        "Kairo keeps the last {} ready to save.",
                        seconds_label(s.clip_seconds)
                    )),
                    column![
                        segmented(
                            [15u32, 30, 60, 120, 300, 600]
                                .iter()
                                .map(|v| (*v, seconds_label(*v)))
                                .collect(),
                            s.clip_seconds,
                            Msg::ClipSeconds
                        ),
                        row![
                            text("Or exactly").size(13).color(theme::muted()),
                            text_input("30", &self.clip_text)
                                .on_input(Msg::ClipText)
                                .width(64)
                                .padding(Padding::from([7, 9]))
                                .size(13)
                                .style(theme::input),
                            text("seconds").size(13).color(theme::muted()),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    ]
                    .spacing(8)
                    .align_x(Alignment::End),
                ),
            ],
        ));

        // Voice clipping.
        let voice_control: Element<'a, Msg> = match &self.voice_install {
            Some(p) => {
                let (done, total) = (
                    p[0].load(Ordering::Relaxed),
                    p[1].load(Ordering::Relaxed).max(1),
                );
                column![
                    text(format!(
                        "Downloading the speech files: {} of {}",
                        human_size(done),
                        human_size(total)
                    ))
                    .size(12)
                    .color(theme::ink_2()),
                    iced::widget::progress_bar(0.0..=1.0, done as f32 / total as f32)
                        .girth(5)
                        .length(220)
                        .style(theme::progress),
                ]
                .spacing(6)
                .align_x(Alignment::End)
                .into()
            }
            None => switch(s.voice_clip, Msg::Voice),
        };
        let listening = report.map(|r| (&r.status.voice, &r.status.voice_error));
        let status = match (&self.voice_problem, listening) {
            (Some(p), _) => Some((theme::accent(), p.clone())),
            (None, Some((_, Some(e)))) if s.voice_clip => Some((theme::accent(), e.clone())),
            (None, Some((Some(phrase), _))) if s.voice_clip => {
                Some((theme::GREEN, format!("Listening for \"{phrase}\" now.")))
            }
            (None, None) if s.voice_clip => {
                Some((theme::muted(), "Starts listening when Kairo runs.".into()))
            }
            _ => None,
        };
        sections = sections.push(section("Games", self.games_rows(s, report, known_games)));

        let mut voice_rows = vec![setting(
            "Voice clipping",
            Some(
                "Say a phrase and Kairo saves a clip, hands-free. The speech is recognised on this computer: \
                 nothing is sent anywhere. The first time, it downloads the speech files (about 55 MB)."
                    .into(),
            ),
            voice_control,
        )];
        if s.voice_clip {
            let mut phrase_col = column![
                text_input("clip that", &self.phrase_text)
                    .on_input(Msg::Phrase)
                    .width(220)
                    .padding(Padding::from([8, 10]))
                    .size(14)
                    .style(theme::input),
            ]
            .spacing(6)
            .align_x(Alignment::End);
            if let Some((color, line)) = status {
                phrase_col = phrase_col.push(text(line).size(12).color(color));
            }
            voice_rows.push(setting(
                "Phrase",
                Some(
                    "Common English words work best. A phrase you don't say while chatting (\"rewind that\") \
                     avoids saving clips by accident."
                        .into(),
                ),
                phrase_col,
            ));
        } else if let Some((color, line)) = status {
            voice_rows.push(setting("", None, text(line).size(12).color(color)));
        }
        sections = sections.push(section("Voice clipping", voice_rows));

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
            encoder_row = encoder_row.push(text("checking...").size(12).color(theme::muted()));
        }
        let (quality_note, storage_note) = self.memory_estimate(s);
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
                    Some(quality_note),
                    segmented(
                        Quality::ALL
                            .iter()
                            .map(|q| (*q, q.label().to_string()))
                            .collect(),
                        s.quality,
                        Msg::Quality,
                    ),
                ),
                setting(
                    "Replay buffer",
                    Some(storage_note),
                    segmented(
                        Storage::ALL
                            .iter()
                            .map(|v| (*v, v.label().to_string()))
                            .collect(),
                        s.replay_storage,
                        Msg::Storage,
                    ),
                ),
                setting(
                    "Video format",
                    Some(
                        "H.265 makes clips and the replay about a third smaller for the same \
                         picture. It needs a graphics card encoder; some apps can't play it, so \
                         Kairo converts clips when you share them."
                            .into(),
                    ),
                    segmented(
                        VideoCodec::ALL
                            .iter()
                            .map(|c| (*c, c.label().to_string()))
                            .collect(),
                        s.codec,
                        Msg::Codec,
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
        let mut sound_rows: Vec<Element<'a, Msg>> = Vec::new();
        if cfg!(windows) {
            let mut chat_col = row![switch(s.chat_track, Msg::ChatTrack)]
                .spacing(12)
                .align_y(Alignment::Center);
            if s.chat_track {
                chat_col = row![
                    text_input("Discord", &s.chat_app)
                        .on_input(Msg::ChatApp)
                        .width(150)
                        .padding(Padding::from([7, 10]))
                        .size(13)
                        .style(theme::input),
                    switch(s.chat_track, Msg::ChatTrack)
                ]
                .spacing(12)
                .align_y(Alignment::Center);
            }
            let chat_line = report
                .filter(|_| s.chat_track)
                .and_then(|r| r.status.chat.clone());
            let chat_col: Element<'a, Msg> = match chat_line {
                Some(line) => column![chat_col, text(line).size(12).color(theme::GREEN)]
                    .spacing(6)
                    .align_x(Alignment::End)
                    .into(),
                None => chat_col.into(),
            };
            sound_rows.push(setting(
                "Voice chat on its own track",
                Some(
                    "Records an app's sound (Discord by default) on a track of its own and leaves \
                     it out of the game track, so you can turn chat down or off in copies. \
                     Windows 11."
                        .into(),
                ),
                chat_col,
            ));
        }
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
                setting(
                    "Mic volume",
                    Some(
                        "How loud your voice is in clips. Over 100% boosts a quiet mic; loud \
                         peaks are softened rather than crackling."
                            .into(),
                    ),
                    row![
                        slider(
                            0..=kairo::config::MAX_MIC_VOLUME,
                            s.mic_volume,
                            Msg::MicVolume
                        )
                        .step(5u32)
                        .width(170)
                        .style(theme::volume_slider),
                        text(format!("{}%", s.mic_volume))
                            .size(13)
                            .font(MEDIUM)
                            .width(44),
                        button(
                            text(if self.mic_test.is_some() {
                                "Stop test"
                            } else {
                                "Test mic"
                            })
                            .size(13)
                            .font(MEDIUM)
                        )
                        .padding(Padding::from([7, 12]))
                        .style(theme::secondary)
                        .on_press(Msg::MicTest(self.mic_test.is_none())),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                ),
            ]
            .into_iter()
            .chain(self.mic_test.as_ref().map(|t| {
                setting(
                    "Mic test",
                    Some("Only you hear this; nothing is saved.".into()),
                    self.mic_test_view(t),
                )
            }))
            .chain(sound_rows)
            .collect(),
        ));

        // Webcam.
        let cam_line = report.filter(|_| s.webcam).and_then(|r| {
            match (&r.status.webcam, &r.status.webcam_error) {
                (_, Some(e)) => Some((e.clone(), theme::RED)),
                (Some(name), None) => Some((format!("Recording {name}"), theme::GREEN)),
                (None, None) => None,
            }
        });
        let cam_switch: Element<'a, Msg> = match cam_line {
            Some((line, color)) => column![
                switch(s.webcam, Msg::Webcam),
                text(line).size(12).color(color)
            ]
            .spacing(6)
            .align_x(Alignment::End)
            .into(),
            None => switch(s.webcam, Msg::Webcam),
        };
        let mut cam_rows = vec![setting(
            "Webcam in clips",
            Some(
                "Your camera goes in a corner of each clip you save. Its light stays on while \
                 Kairo records, and goes off when you pause. Clips take a few seconds longer \
                 to save."
                    .into(),
            ),
            cam_switch,
        )];
        if s.webcam {
            let mut cams = vec![Choice {
                value: String::new(),
                label: "First camera found".into(),
            }];
            for c in self.cameras.iter().flatten() {
                cams.push(Choice {
                    value: c.id.clone(),
                    label: c.name.clone(),
                });
            }
            let picked = cams.iter().find(|c| c.value == s.webcam_device).cloned();
            let cam_pick: Element<'a, Msg> = if self.cameras.is_none() {
                text("Looking for cameras...")
                    .size(13)
                    .color(theme::muted())
                    .into()
            } else {
                pick_list(cams, picked, Msg::Camera)
                    .text_size(13)
                    .padding(Padding::from([7, 12]))
                    .width(260)
                    .style(theme::pick)
                    .menu_style(theme::menu)
                    .into()
            };
            cam_rows.push(setting("Camera", None, cam_pick));
            cam_rows.push(setting(
                "Corner",
                None,
                segmented(
                    vec![
                        ("top-left", "Top left".to_string()),
                        ("top-right", "Top right".to_string()),
                        ("bottom-left", "Bottom left".to_string()),
                        ("bottom-right", "Bottom right".to_string()),
                    ],
                    kairo::config::WEBCAM_CORNERS
                        .into_iter()
                        .find(|c| *c == s.webcam_corner)
                        .unwrap_or("bottom-right"),
                    Msg::WebcamCorner,
                ),
            ));
            let sizes = [18u32, 25, 32];
            cam_rows.push(setting(
                "Size",
                None,
                segmented(
                    vec![
                        (18, "Small".to_string()),
                        (25, "Medium".to_string()),
                        (32, "Large".to_string()),
                    ],
                    sizes
                        .into_iter()
                        .min_by_key(|v| v.abs_diff(s.webcam_size))
                        .unwrap_or(25),
                    Msg::WebcamSize,
                ),
            ));
        }
        sections = sections.push(section("Webcam", cam_rows));

        // Saving.
        let mut folder = row![
            icon_button(Icon::Folder, "Change", theme::ink_2())
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
                .push(text("Keep under").size(13).color(theme::ink_2()))
                .push(
                    text_input("50", &self.gb_text)
                        .on_input(Msg::GbText)
                        .width(64)
                        .padding(Padding::from([7, 9]))
                        .size(13)
                        .style(theme::input),
                )
                .push(text("GB").size(13).color(theme::muted()));
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
                    "Keep clips",
                    Some(format!(
                        "Older clips move to Recently deleted, where they wait {} days; favourites always stay.",
                        kairo::library::TRASH_DAYS
                    )),
                    segmented(
                        vec![
                            (0u32, "Forever".into()),
                            (7, "1 week".into()),
                            (30, "30 days".into()),
                            (90, "90 days".into()),
                        ],
                        s.keep_days,
                        Msg::KeepDays,
                    ),
                ),
                setting(
                    "Discord size",
                    Some("The limit for Share to Discord and Send to Discord channel.".into()),
                    segmented(
                        vec![(10u32, "10 MB".into()), (50, "50 MB".into()), (500, "500 MB".into())],
                        s.discord_mb,
                        Msg::Discord,
                    ),
                ),
                self.channel_row(s),
                setting("Notification", Some("A small card in the corner when a clip is saved.".into()), switch(s.notify, Msg::Notify)),
                setting("Sound", Some("A short tone when a clip is saved.".into()), switch(s.sound, Msg::Sound)),
                self.sound_row(s),
            ],
        ));

        // Kairo.
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
                    theme::ink_2(),
                )
                .style(theme::secondary)
                .on_press_maybe((!self.update.checking).then_some(Msg::CheckNow)),
            );
        }
        let update_note = match (&self.update.message, &self.update.available) {
            (Some((m, _)), _) => m.clone(),
            (None, Some(u)) => format!(
                "Kairo {} is available. You have {}.",
                u.version,
                updater::current_version()
            ),
            (None, None) => format!("You have Kairo {}.", updater::current_version()),
        };
        let share_error = report.and_then(|r| r.share_hotkey_error.clone());
        let mut share_row = row![self.key_capture(Target::Share, &s.share_hotkey, share_error)]
            .spacing(8)
            .align_y(Alignment::Start);
        if !s.share_hotkey.is_empty() {
            share_row = share_row.push(
                button(text("Turn off").size(13).font(MEDIUM))
                    .padding(Padding::from([9, 10]))
                    .style(theme::ghost)
                    .on_press(Msg::ShareOff),
            );
        }
        sections = sections.push(section(
            "Sharing",
            vec![setting(
                "Save and share",
                Some(
                    "One key saves a clip, uploads it and copies the link, ready to paste in a chat.                      Links work for 2 days; anyone with one can watch, and Discord plays it in the chat."
                        .into(),
                ),
                share_row,
            )],
        ));

        let swatches =
            ACCENTS
                .iter()
                .zip(theme::ACCENT_COLORS)
                .fold(row![].spacing(9), |r, (name, rgb)| {
                    let selected = s.accent == *name;
                    let c = theme::rgb(rgb);
                    r.push(
                        button(Space::new().width(22).height(22))
                            .padding(0)
                            .style(move |_, st| button::Style {
                                background: Some(c.into()),
                                border: iced::Border {
                                    color: if selected {
                                        theme::ink()
                                    } else if matches!(st, button::Status::Hovered) {
                                        theme::alpha(theme::ink(), 0.45)
                                    } else {
                                        iced::Color::TRANSPARENT
                                    },
                                    width: if selected { 3.0 } else { 2.0 },
                                    radius: 999.0.into(),
                                },
                                shadow: iced::Shadow {
                                    color: theme::alpha(c, if selected { 0.6 } else { 0.0 }),
                                    offset: iced::Vector::ZERO,
                                    blur_radius: 12.0,
                                },
                                ..Default::default()
                            })
                            .on_press(Msg::Accent(name)),
                    )
                });
        sections = sections.push(section(
            "Kairo",
            vec![
                setting(
                    "Look",
                    Some("Dark, light, or the same as your computer.".into()),
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
                        Msg::Look,
                    ),
                ),
                setting(
                    "Accent colour",
                    Some("Buttons, highlights and the glow around clips.".into()),
                    swatches,
                ),
                setting(
                    "Animations",
                    Some(if crate::platform::reduce_motion() {
                        "Pages, dialogs and notices ease in. Your computer asks for less motion,                          so Match system leaves them still."
                            .into()
                    } else {
                        "Pages, dialogs and notices ease in.".into()
                    }),
                    segmented(
                        vec![
                            ("system", "Match system".to_string()),
                            ("on", "On".to_string()),
                            ("off", "Off".to_string()),
                        ],
                        match s.motion.as_str() {
                            "on" => "on",
                            "off" => "off",
                            _ => "system",
                        },
                        Msg::Motion,
                    ),
                ),
                setting(
                    "Start with the computer",
                    None,
                    if self.launcher.is_some() {
                        switch(self.autostart, Msg::Autostart)
                    } else {
                        text("Not available here")
                            .size(12)
                            .color(theme::muted())
                            .into()
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
                        icon_button(Icon::Log, "Open the log", theme::ink_2())
                            .style(theme::ghost)
                            .on_press(Msg::OpenLog),
                        icon_button(Icon::Power, "Quit Kairo", theme::ink_2())
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

impl State {
    /// Connecting a Discord channel: paste its webhook link; it's checked, then kept in the
    /// system's password store.
    fn channel_row<'a>(&'a self, s: &'a Settings) -> Element<'a, Msg> {
        let store = if cfg!(windows) {
            "Credential Manager"
        } else if cfg!(target_os = "macos") {
            "your keychain"
        } else {
            "your keyring"
        };
        if !s.discord_channel.is_empty() {
            return setting(
                "Discord channel",
                Some(format!(
                    "Connected. Clips you send post as {}. The link is kept in {store}, not in the settings file.",
                    s.discord_channel
                )),
                button(text("Disconnect").size(13).font(MEDIUM))
                    .padding(Padding::from([7, 12]))
                    .style(theme::ghost)
                    .on_press(Msg::DisconnectWebhook),
            );
        }
        let ready = !self.webhook_busy && !self.webhook_text.trim().is_empty();
        setting(
            "Discord channel",
            Some(self.webhook_problem.clone().unwrap_or_else(|| {
                "Send clips straight to a channel from the editor. In Discord: channel settings, \
                 Integrations, Webhooks, New Webhook, Copy Webhook URL."
                    .into()
            })),
            row![
                text_input("Paste the webhook link", &self.webhook_text)
                    .on_input(Msg::WebhookText)
                    .on_submit(Msg::ConnectWebhook)
                    .secure(true)
                    .width(200)
                    .padding(Padding::from([7, 9]))
                    .size(13)
                    .style(theme::input),
                button(
                    text(if self.webhook_busy {
                        "Checking..."
                    } else {
                        "Connect"
                    })
                    .size(13)
                    .font(MEDIUM)
                )
                .padding(Padding::from([7, 12]))
                .style(theme::secondary)
                .on_press_maybe(ready.then_some(Msg::ConnectWebhook)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
    }
}

pub(super) fn switch<'a>(on: bool, msg: impl Fn(bool) -> Msg + 'a) -> Element<'a, Msg> {
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
        text(title.to_string())
            .font(BOLD)
            .size(16)
            .color(theme::ink()),
        container(body).width(Fill).style(theme::panel),
    ]
    .spacing(10)
    .into()
}
