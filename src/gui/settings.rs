//! The settings page.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use iced::keyboard::{self, Key, key};
use iced::widget::{
    Space, button, column, container, pick_list, row, scrollable, text, text_input, toggler,
};
use iced::{Alignment, Element, Fill, Length, Padding, Task};

use kairo::args::{self, Encoder};
use kairo::config::{
    ACCENTS, MAX_CLIP_SECONDS, MAX_QUICK_SAVES, MIN_CLIP_SECONDS, Quality, QuickSave, Resolution,
    SHARE_EXPIRIES, Settings, Storage, VideoCodec, normalize_phrase,
};

use super::icons::{Icon, icon};
use super::theme::{self, BOLD, MEDIUM, MUTED, SEMIBOLD, TEXT, TEXT_2};
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
    SessionAuto(bool),
    ChatTrack(bool),
    ChatApp(String),
    InputOverlay(bool),
    Voice(bool),
    VoiceInstalled(Result<(), String>),
    Phrase(String),
    ShareExpiry(&'static str),
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
        let mut tasks = vec![encoders];
        let mut update = Updates::default();
        if settings.check_updates && std::env::var_os("KAIRO_NO_UPDATE_CHECK").is_none() {
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
                capturing_key: None,
                hotkey_problem: None,
                clip_text: settings.clip_seconds.to_string(),
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
        parts.push(name);
        let spec = parts.join("+");
        self.capturing_key = None;
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
            Msg::ShareExpiry(e) => {
                s.share_expiry = e.to_string();
                return changed;
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
                    text("saves").size(13).color(MUTED),
                    text_input("15", self.quick_texts.get(i).map_or("", String::as_str))
                        .on_input(move |t| Msg::QuickText(i, t))
                        .width(60)
                        .padding(Padding::from([7, 9]))
                        .size(13)
                        .style(theme::input),
                    text("s").size(13).color(MUTED),
                    button(icon(Icon::Close, 14.0, TEXT_2))
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
    fn key_capture<'a>(
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
            row![text("Off").font(SEMIBOLD).size(14).color(MUTED)]
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
            col = col.push(
                text(n)
                    .size(12)
                    .color(if capturing { MUTED } else { theme::accent() }),
            );
        }
        col.into()
    }

    pub fn view<'a>(
        &'a self,
        s: &'a Settings,
        report: Option<&'a Report>,
        used: u64,
    ) -> Element<'a, Msg> {
        let mut sections = column![].spacing(26).max_width(780);
        if let Some(u) = &self.update.available {
            let mut points = column![].spacing(6);
            for p in updater::highlights(&u.notes, 5) {
                points = points.push(
                    row![
                        icon(Icon::Check, 14.0, theme::accent()),
                        text(p).size(13).color(TEXT)
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
                .color(TEXT_2)
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
                            icon(Icon::External, 14.0, TEXT_2),
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
                        .color(TEXT_2),
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
                            text("Or exactly").size(13).color(MUTED),
                            text_input("30", &self.clip_text)
                                .on_input(Msg::ClipText)
                                .width(64)
                                .padding(Padding::from([7, 9]))
                                .size(13)
                                .style(theme::input),
                            text("seconds").size(13).color(MUTED),
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
                    .color(TEXT_2),
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
                Some((MUTED, "Starts listening when Kairo runs.".into()))
            }
            _ => None,
        };
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
            encoder_row = encoder_row.push(text("checking...").size(12).color(MUTED));
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
            ]
            .into_iter()
            .chain(sound_rows)
            .collect(),
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
                    TEXT_2,
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
        let expiry = SHARE_EXPIRIES
            .iter()
            .copied()
            .find(|e| *e == s.share_expiry)
            .unwrap_or("forever");
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
                    "One key saves a clip, uploads it and copies the link, ready to paste in a chat."
                        .into(),
                ),
                share_row,
            ),
            setting(
                "Links last",
                Some(
                    "Share links upload the clip to catbox.moe (forever) or litterbox.catbox.moe (for a while). \
                     Anyone with a link can watch. Forever links are removed after two years without views."
                        .into(),
                ),
                segmented(
                    vec![
                        ("forever", "Forever".to_string()),
                        ("72h", "3 days".to_string()),
                        ("24h", "1 day".to_string()),
                        ("1h", "1 hour".to_string()),
                    ],
                    expiry,
                    Msg::ShareExpiry,
                ),
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
                                        TEXT
                                    } else if matches!(st, button::Status::Hovered) {
                                        theme::alpha(TEXT, 0.45)
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
                    "Accent colour",
                    Some("Buttons, highlights and the glow around clips.".into()),
                    swatches,
                ),
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
                        icon_button(Icon::Power, "Quit Kairo", TEXT_2)
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
