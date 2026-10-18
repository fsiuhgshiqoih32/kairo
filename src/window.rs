//! The window: a gallery of saved clips, the settings, and what the background process is doing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, RichText, Sense, Stroke,
    TextureOptions, pos2, vec2,
};

use rewindr::args::{self, Encoder};
use rewindr::config::{self, MAX_CLIP_SECONDS, MIN_CLIP_SECONDS, Quality, Resolution, Settings};
use rewindr::library::{self, Clip, Library};
use rewindr::mp4;

use crate::daemon::{self, Report, Request};
use crate::platform::{self, Monitor};
use crate::single_instance::{self, InstanceLock};
use crate::{autostart, hotkey, ui, updater};

/// Narrowest a clip card gets before the gallery drops a column.
const CARD_MIN_W: f32 = 230.0;
const GAP: f32 = 14.0;

/// `wait`: after an update, the old window may still be closing; wait for it.
pub fn run(wait: bool) -> Result<(), String> {
    let lock = if wait {
        single_instance::acquire_patiently(single_instance::WINDOW)?
    } else {
        single_instance::acquire(single_instance::WINDOW)?
    };
    let Some(lock) = lock else {
        println!("The Rewindr window is already open.");
        return Ok(());
    };
    if let Err(e) = daemon::ensure_running() {
        crate::log::warning!("{e}");
    }
    let viewport = egui::ViewportBuilder::default()
        .with_title("Rewindr")
        .with_app_id("rewindr")
        .with_inner_size(vec2(900.0, 640.0))
        .with_min_inner_size(vec2(620.0, 460.0));
    let mut lock = Some(lock);
    crate::app::run_window("Rewindr", viewport, crate::app::Kind::Normal, |cc| {
        Window::new(cc, lock.take())
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Clips,
    Settings,
}

enum ClipAction {
    Play,
    Show,
    Copy,
    Delete,
    Favorite,
    Rename,
}

/// Which clips the gallery shows.
#[derive(Clone, PartialEq, Eq)]
enum Filter {
    All,
    Favorites,
    Game(String),
}

struct Rename {
    clip: PathBuf,
    name: String,
    error: Option<String>,
    focused: bool,
}

pub struct Window {
    _lock: Option<InstanceLock>,
    path: PathBuf,
    settings: Settings,
    saved: Settings,
    changed_at: Option<Instant>,
    tab: Tab,
    report: Option<Report>,
    report_read: Instant,
    clips: Vec<Clip>,
    library: Library,
    filter: Filter,
    search: String,
    rename: Option<Rename>,
    clips_read: Option<Instant>,
    thumbs: HashMap<PathBuf, Option<egui::TextureHandle>>,
    durations: HashMap<PathBuf, Option<f64>>,
    icon: Option<egui::TextureHandle>,
    monitors: Vec<Monitor>,
    mics: Vec<String>,
    encoders: Arc<Mutex<Option<Vec<Encoder>>>>,
    capturing_key: bool,
    hotkey_problem: Option<String>,
    launcher: Option<auto_launch::AutoLaunch>,
    autostart: bool,
    confirm_delete: Option<PathBuf>,
    note: Option<(String, Color32, Instant)>,
    frame_no: u64,
    updates: Updates,
}

#[derive(Default)]
struct Updates {
    checking: Option<Receiver<Result<Option<updater::Update>, String>>>,
    installing: Option<Receiver<Result<(), String>>>,
    available: Option<updater::Update>,
    message: Option<(String, Color32)>,
}

impl Window {
    pub fn new(cc: &eframe::CreationContext<'_>, lock: Option<InstanceLock>) -> Self {
        let path = config::default_path();
        let (settings, _) = config::load_or_create(&path);
        let encoders: Arc<Mutex<Option<Vec<Encoder>>>> = Arc::default();
        {
            let out = encoders.clone();
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                let found = crate::ffmpeg::path()
                    .map(|f| crate::ffmpeg::working_encoders(&f))
                    .unwrap_or_default();
                *out.lock().unwrap() = Some(found);
                ctx.request_repaint();
            });
        }
        let launcher = autostart::launcher();
        let autostart = launcher.as_ref().is_some_and(autostart::is_enabled);
        let tab = if std::env::var("REWINDR_TAB").is_ok_and(|t| t == "settings") {
            Tab::Settings
        } else {
            Tab::Clips
        };
        let size = 64;
        let icon =
            egui::ColorImage::from_rgba_unmultiplied([size, size], &rewindr::art::app_icon(size));
        let mut w = Window {
            _lock: lock,
            path,
            saved: settings.clone(),
            settings,
            changed_at: None,
            tab,
            report: daemon::read_report(),
            report_read: Instant::now(),
            clips: Vec::new(),
            library: Library::load(),
            filter: Filter::All,
            search: String::new(),
            rename: None,
            clips_read: None,
            thumbs: HashMap::new(),
            durations: HashMap::new(),
            icon: Some(
                cc.egui_ctx
                    .load_texture("icon", icon, TextureOptions::LINEAR),
            ),
            monitors: platform::monitors(),
            mics: crate::audio::microphones(),
            encoders,
            capturing_key: false,
            hotkey_problem: None,
            launcher,
            autostart,
            confirm_delete: None,
            note: None,
            frame_no: 0,
            updates: Updates::default(),
        };
        if w.settings.check_updates && std::env::var_os("REWINDR_NO_UPDATE_CHECK").is_none() {
            w.start_update_check(&cc.egui_ctx);
        }
        w
    }

    fn note(&mut self, text: impl Into<String>, color: Color32) {
        self.note = Some((text.into(), color, Instant::now()));
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        if self.report_read.elapsed() > Duration::from_millis(500) {
            self.report = daemon::read_report();
            self.report_read = Instant::now();
        }
        if self
            .clips_read
            .is_none_or(|t| t.elapsed() > Duration::from_secs(2))
        {
            self.clips = library::scan(&self.settings.save_dir());
            self.library = Library::load();
            self.clips_read = Some(Instant::now());
        }
        for clip in self.clips.iter().take(300) {
            self.thumbs
                .entry(clip.path.clone())
                .or_insert_with(|| load_thumb(ctx, &clip.path));
            self.durations
                .entry(clip.path.clone())
                .or_insert_with(|| mp4::duration_secs(&clip.path));
        }
        // Write settings shortly after the last change, so dragging a slider doesn't restart
        // the recording on every step.
        if let Some(t) = self.changed_at
            && t.elapsed() > Duration::from_millis(600)
        {
            self.changed_at = None;
            self.settings.sanitize();
            if self.settings != self.saved {
                match config::save(&self.path, &self.settings) {
                    Ok(()) => self.saved = self.settings.clone(),
                    Err(e) => self.note(format!("Couldn't save settings: {e}"), ui::DANGER),
                }
            }
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(icon) = &self.icon {
                let (r, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                ui.painter().image(
                    icon.id(),
                    r,
                    egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            ui.label(RichText::new("Rewindr").size(20.0).strong());
            ui.add_space(6.0);
            self.status_pill(ui);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let running = self.report.is_some();
                let paused = self.report.as_ref().is_some_and(|r| r.status.paused);
                let pause = egui::Button::new(if paused { "Resume" } else { "Pause" })
                    .min_size(vec2(0.0, 30.0));
                if ui.add_enabled(running, pause).clicked() {
                    let _ = daemon::request(Request::TogglePause);
                }
                let can_save = self.report.as_ref().is_some_and(|r| r.status.recording);
                let text = format!("Save last {}", seconds_label(self.settings.clip_seconds));
                let save = egui::Button::new(RichText::new(text).strong().color(Color32::WHITE))
                    .fill(if can_save { ui::ACCENT } else { ui::PANEL })
                    .min_size(vec2(0.0, 30.0));
                if ui.add_enabled(can_save, save).clicked() {
                    let _ = daemon::request(Request::Save);
                }
            });
        });
    }

    fn status_pill(&mut self, ui: &mut egui::Ui) {
        let (color, text) = match &self.report {
            None => (ui::MUTED, "Not running".to_string()),
            Some(r) if r.status.paused => (ui::MUTED, "Paused".to_string()),
            Some(r) if r.status.recording => {
                let s = &r.status;
                let size = s.size.map(|(w, h)| format!("{w}x{h}")).unwrap_or_default();
                let enc = s.encoder.clone().unwrap_or_default();
                (
                    ui::OK,
                    format!("Recording  ·  {size}  ·  {} fps  ·  {enc}", s.fps),
                )
            }
            Some(r) if r.status.error.is_some() => (ui::DANGER, "Can't record".to_string()),
            Some(_) => (ui::MUTED, "Starting...".to_string()),
        };
        egui::Frame::new()
            .fill(color.gamma_multiply(0.14))
            .stroke(Stroke::new(1.0, color.gamma_multiply(0.35)))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::symmetric(10, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.0, color);
                    ui.label(RichText::new(text).size(12.5).color(ui::TEXT));
                });
            });
        if self.report.is_none()
            && ui.button("Start").clicked()
            && let Err(e) = daemon::ensure_running()
        {
            self.note(e, ui::DANGER);
        }
    }

    /// The line under the header: the shortcut, the buffer, the sound, and any problem.
    fn summary(&self, ui: &mut egui::Ui) {
        let Some(r) = &self.report else {
            ui::hint(
                ui,
                "Start Rewindr to keep the last moments of your screen ready to save.",
            );
            return;
        };
        let s = &r.status;
        if let Some(e) = s.error.as_ref().filter(|_| !s.recording) {
            ui.label(RichText::new(e).size(12.0).color(ui::DANGER));
        }
        let mut parts = Vec::new();
        match (&r.hotkey, &r.hotkey_error) {
            (_, Some(_)) => {}
            (Some(k), None) => parts.push(format!(
                "Press {k} to save the last {}",
                seconds_label(self.settings.clip_seconds)
            )),
            _ => {}
        }
        if s.recording {
            parts.push(format!(
                "{:.0} s ready, {:.0} MB of memory",
                s.buffer_seconds, s.buffer_mb
            ));
        }
        parts.push(
            match (&s.desktop_audio, &s.microphone) {
                (Some(_), Some(_)) => "desktop audio and microphone",
                (Some(_), None) => "desktop audio",
                (None, Some(_)) => "microphone",
                (None, None) => "no sound",
            }
            .to_string(),
        );
        ui::hint(ui, &parts.join("  ·  "));
        for e in [&r.hotkey_error, &s.audio_error].into_iter().flatten() {
            ui.label(RichText::new(e).size(12.0).color(ui::DANGER));
        }
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 20.0;
            for (t, name) in [(Tab::Clips, "Clips"), (Tab::Settings, "Settings")] {
                let selected = self.tab == t;
                let text = RichText::new(name).size(15.0).color(if selected {
                    ui::TEXT
                } else {
                    ui::MUTED
                });
                let r = ui.add(egui::Label::new(text).sense(Sense::click()));
                if r.clicked() {
                    self.tab = t;
                }
                if selected {
                    let y = r.rect.bottom() + 5.0;
                    ui.painter().line_segment(
                        [pos2(r.rect.left(), y), pos2(r.rect.right(), y)],
                        Stroke::new(2.0, ui::ACCENT),
                    );
                }
                r.on_hover_cursor(egui::CursorIcon::PointingHand);
            }
            if let Some((text, color, at)) = &self.note {
                if at.elapsed() < Duration::from_secs(5) {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(text).color(*color).size(12.5));
                    });
                } else {
                    self.note = None;
                }
            }
        });
    }

    fn clips_tab(&mut self, ui: &mut egui::Ui) {
        let dir = self.settings.save_dir();
        ui.horizontal(|ui| {
            let total: u64 = self.clips.iter().map(|c| c.size).sum();
            let count = match self.clips.len() {
                1 => "1 clip".to_string(),
                n => format!("{n} clips"),
            };
            ui.label(RichText::new(count).strong());
            if !self.clips.is_empty() {
                ui.label(RichText::new(crate::app::human_size(total)).color(ui::MUTED));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .button("Open folder")
                    .on_hover_text(dir.display().to_string())
                    .clicked()
                {
                    let _ = std::fs::create_dir_all(&dir);
                    crate::app::open(&dir);
                }
                if !self.clips.is_empty() {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("Search clips")
                            .desired_width(200.0),
                    );
                }
            });
        });
        if self.clips.is_empty() {
            ui.add_space(8.0);
            self.empty_gallery(ui);
            return;
        }
        ui.add_space(6.0);
        self.filter_chips(ui);
        ui.add_space(10.0);
        let search = self.search.trim().to_lowercase();
        let visible: Vec<usize> = (0..self.clips.len())
            .filter(|&i| {
                let c = &self.clips[i];
                let by_filter = match &self.filter {
                    Filter::All => true,
                    Filter::Favorites => self.library.is_favorite(&c.path),
                    Filter::Game(g) => &c.game() == g,
                };
                by_filter && (search.is_empty() || c.name.to_lowercase().contains(&search))
            })
            .take(300)
            .collect();
        if visible.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui::hint(ui, "No clips match."));
            return;
        }
        let mut action: Option<(usize, ClipAction)> = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                let avail = ui.available_width() - 2.0;
                let cols = ((avail + GAP) / (CARD_MIN_W + GAP)).floor().max(1.0) as usize;
                let card_w = (avail - GAP * (cols - 1) as f32) / cols as f32;
                for row in visible.chunks(cols) {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = GAP;
                        for &i in row {
                            let clip = &self.clips[i];
                            ui.allocate_ui_with_layout(
                                vec2(card_w, 0.0),
                                Layout::top_down(Align::Min),
                                |ui| {
                                    ui.set_width(card_w);
                                    if let Some(a) = self.card(ui, clip, card_w) {
                                        action = Some((i, a));
                                    }
                                },
                            );
                        }
                    });
                    ui.add_space(GAP);
                }
            });
        if let Some((i, what)) = action {
            let path = self.clips[i].path.clone();
            match what {
                ClipAction::Play => crate::app::open_clip(&path),
                ClipAction::Show => crate::app::reveal(&path),
                ClipAction::Copy => match platform::copy_files(std::slice::from_ref(&path)) {
                    Ok(()) => {
                        self.note("Copied. Paste it into Discord, a chat or a folder.", ui::OK)
                    }
                    Err(e) => self.note(e, ui::DANGER),
                },
                ClipAction::Delete => self.confirm_delete = Some(path),
                ClipAction::Favorite => {
                    let on = !self.library.is_favorite(&path);
                    self.library.set_favorite(&path, on);
                    if let Err(e) = self.library.save() {
                        self.note(format!("Couldn't save favourites: {e}"), ui::DANGER);
                    }
                }
                ClipAction::Rename => {
                    self.rename = Some(Rename {
                        name: self.clips[i].name.clone(),
                        clip: path,
                        error: None,
                        focused: false,
                    })
                }
            }
        }
    }

    /// "All", "Favourites", and one chip per game, most clipped first.
    fn filter_chips(&mut self, ui: &mut egui::Ui) {
        let mut games: Vec<(String, usize)> = Vec::new();
        for c in &self.clips {
            let g = c.game();
            match games.iter_mut().find(|(n, _)| *n == g) {
                Some((_, k)) => *k += 1,
                None => games.push((g, 1)),
            }
        }
        games.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let favs = self
            .clips
            .iter()
            .filter(|c| self.library.is_favorite(&c.path))
            .count();
        if let Filter::Game(g) = &self.filter
            && !games.iter().any(|(n, _)| n == g)
        {
            self.filter = Filter::All;
        }
        if self.filter == Filter::Favorites && favs == 0 {
            self.filter = Filter::All;
        }
        let mut chips = vec![(Filter::All, format!("All  {}", self.clips.len()))];
        if favs > 0 {
            chips.push((Filter::Favorites, format!("Favourites  {favs}")));
        }
        if games.len() > 1 || favs > 0 {
            for (g, n) in games.iter().take(10) {
                let short: String = if g.chars().count() > 30 {
                    format!("{}...", g.chars().take(28).collect::<String>().trim_end())
                } else {
                    g.clone()
                };
                chips.push((Filter::Game(g.clone()), format!("{short}  {n}")));
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (value, text) in chips {
                let selected = self.filter == value;
                let b = egui::Button::new(RichText::new(text).size(12.5))
                    .selected(selected)
                    .corner_radius(CornerRadius::same(12))
                    .min_size(vec2(0.0, 24.0));
                if ui.add(b).clicked() {
                    self.filter = value;
                }
            }
        });
    }

    fn rename_dialog(&mut self, ctx: &egui::Context) {
        let Some(r) = &mut self.rename else { return };
        let mut done: Option<bool> = None;
        egui::Window::new("Rename clip")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let edit = ui.add(egui::TextEdit::singleline(&mut r.name).desired_width(360.0));
                if !r.focused {
                    edit.request_focus();
                    r.focused = true;
                }
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = Some(true);
                }
                if let Some(e) = &r.error {
                    ui.label(RichText::new(e).color(ui::DANGER).size(12.0));
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Rename").clicked() {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        done = Some(false);
                    }
                });
            });
        match done {
            Some(false) => self.rename = None,
            Some(true) => {
                let r = self.rename.as_mut().expect("open");
                match library::rename_target(&r.clip, &r.name) {
                    Ok(to) if to == r.clip => self.rename = None,
                    Ok(to) => match std::fs::rename(&r.clip, &to) {
                        Ok(()) => {
                            let _ = std::fs::rename(
                                crate::save::thumb_for(&r.clip),
                                crate::save::thumb_for(&to),
                            );
                            self.library.renamed(&r.clip, &to);
                            let _ = self.library.save();
                            self.rename = None;
                            self.clips_read = None;
                        }
                        Err(e) => r.error = Some(format!("Couldn't rename: {e}")),
                    },
                    Err(e) => r.error = Some(e),
                }
            }
            None => {}
        }
    }

    fn empty_gallery(&self, ui: &mut egui::Ui) {
        ui.add_space(70.0);
        ui.vertical_centered(|ui| {
            if let Some(icon) = &self.icon {
                let (r, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
                ui.painter().image(
                    icon.id(),
                    r,
                    egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE.gamma_multiply(0.85),
                );
            }
            ui.add_space(10.0);
            ui.label(RichText::new("No clips yet").size(18.0).strong());
            let key = self
                .report
                .as_ref()
                .and_then(|r| r.hotkey.clone())
                .unwrap_or_else(|| self.settings.hotkey.clone());
            ui::hint(
                ui,
                &format!(
                    "Play as usual. When something great happens, press {key} and Rewindr saves the last {}.",
                    seconds_label(self.settings.clip_seconds)
                ),
            );
        });
    }

    /// One clip in the gallery: thumbnail (click to play), name, details, buttons.
    fn card(&self, ui: &mut egui::Ui, clip: &Clip, w: f32) -> Option<ClipAction> {
        let mut action = None;
        let h = w * 9.0 / 16.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
        let painter = ui.painter_at(rect.expand(1.0));
        let radius = CornerRadius::same(8);
        painter.rect_filled(rect, radius, Color32::from_rgb(0x12, 0x13, 0x16));
        if let Some(Some(t)) = self.thumbs.get(&clip.path) {
            // Fill the card, cropping the picture's edges rather than letterboxing.
            let size = t.size_vec2();
            let (ia, ra) = (size.x / size.y, w / h);
            let uv = if ia > ra {
                let u = ra / ia;
                egui::Rect::from_min_max(pos2((1.0 - u) / 2.0, 0.0), pos2((1.0 + u) / 2.0, 1.0))
            } else {
                let v = ia / ra;
                egui::Rect::from_min_max(pos2(0.0, (1.0 - v) / 2.0), pos2(1.0, (1.0 + v) / 2.0))
            };
            painter.add(
                egui::epaint::RectShape::filled(rect, radius, Color32::WHITE)
                    .with_texture(t.id(), uv),
            );
        }
        if resp.hovered() {
            painter.rect_filled(rect, radius, Color32::from_black_alpha(80));
            let c = rect.center();
            painter.circle_filled(c, 22.0, Color32::from_black_alpha(160));
            painter.add(egui::Shape::convex_polygon(
                vec![
                    c + vec2(-6.0, -10.0),
                    c + vec2(10.0, 0.0),
                    c + vec2(-6.0, 10.0),
                ],
                Color32::WHITE,
                Stroke::NONE,
            ));
        }
        if let Some(tag) = copy_tag(&clip.name) {
            let galley =
                painter.layout_no_wrap(tag.to_string(), FontId::proportional(11.0), Color32::WHITE);
            let pad = vec2(6.0, 2.0);
            let tr = rect.right_top() + vec2(-8.0, 8.0);
            let bg = egui::Rect::from_min_max(
                pos2(tr.x - galley.size().x - pad.x * 2.0, tr.y),
                pos2(tr.x, tr.y + galley.size().y + pad.y * 2.0),
            );
            painter.rect_filled(bg, CornerRadius::same(4), ui::ACCENT.gamma_multiply(0.9));
            painter.galley(bg.min + pad, galley, Color32::WHITE);
        }
        if let Some(Some(d)) = self.durations.get(&clip.path) {
            let galley = painter.layout_no_wrap(
                mp4::format_duration(*d),
                FontId::proportional(12.0),
                Color32::WHITE,
            );
            let pad = vec2(6.0, 2.0);
            let br = rect.right_bottom() - vec2(8.0, 8.0);
            let bg = egui::Rect::from_min_max(br - galley.size() - pad * 2.0, br);
            painter.rect_filled(bg, CornerRadius::same(4), Color32::from_black_alpha(190));
            painter.galley(bg.min + pad, galley, Color32::WHITE);
        }
        let fav = self.library.is_favorite(&clip.path);
        let star_rect = egui::Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(26.0, 26.0));
        let star = ui.interact(
            star_rect,
            ui.scope_id().with(("star", &clip.path)),
            Sense::click(),
        );
        if fav || resp.hovered() || star.hovered() {
            painter.circle_filled(star_rect.center(), 13.0, Color32::from_black_alpha(150));
            let color = if fav {
                Color32::from_rgb(0xFF, 0xC4, 0x3D)
            } else {
                Color32::from_white_alpha(if star.hovered() { 255 } else { 170 })
            };
            ui::paint_star(&painter, star_rect.center(), 8.0, fav, color);
        }
        star.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Favourite"));
        if star
            .on_hover_text(if fav {
                "Remove from favourites"
            } else {
                "Favourite: kept when old clips are cleaned up"
            })
            .clicked()
        {
            action = Some(ClipAction::Favorite);
        }
        resp.context_menu(|ui| {
            let fav_label = if fav {
                "Remove from favourites"
            } else {
                "Add to favourites"
            };
            for (label, what) in [
                ("Open in editor", ClipAction::Play),
                (fav_label, ClipAction::Favorite),
                ("Rename...", ClipAction::Rename),
                ("Show in folder", ClipAction::Show),
                ("Copy", ClipAction::Copy),
                ("Delete...", ClipAction::Delete),
            ] {
                if ui.button(label).clicked() {
                    action = Some(what);
                    ui.close();
                }
            }
        });
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &clip.name));
        if resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Open in the editor (right-click for more)")
            .clicked()
        {
            action = Some(ClipAction::Play);
        }
        ui.add_space(4.0);
        // Rewindr's own names are "<game> <date>"; show the game, and the full name on hover.
        let title =
            if library::game_of(&clip.name) == "Desktop" && !clip.name.starts_with("Rewindr") {
                clip.name.clone()
            } else {
                clip.game()
            };
        ui.add(egui::Label::new(RichText::new(title).strong()).truncate())
            .on_hover_text(&clip.name);
        let when: chrono::DateTime<chrono::Local> = clip.modified.into();
        ui.label(
            RichText::new(format!(
                "{}  ·  {}",
                when.format("%b %-d, %H:%M"),
                crate::app::human_size(clip.size)
            ))
            .color(ui::MUTED)
            .size(12.0),
        );
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui.small_button("Show in folder").clicked() {
                action = Some(ClipAction::Show);
            }
            if ui
                .small_button("Copy")
                .on_hover_text("Copy the file, to paste into Discord, a chat or a folder")
                .clicked()
            {
                action = Some(ClipAction::Copy);
            }
            if ui.small_button("Delete").clicked() {
                action = Some(ClipAction::Delete);
            }
        });
        action
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui) {
        let before = self.settings.clone();
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.set_max_width(760.0);
                card(ui, "Replay", |ui| self.replay_settings(ui));
                card(ui, "Video", |ui| self.video_settings(ui));
                card(ui, "Sound", |ui| self.sound_settings(ui));
                card(ui, "Saving", |ui| self.saving_settings(ui));
                card(ui, "General", |ui| self.general_settings(ui));
                ui.add_space(12.0);
            });
        if self.settings != before {
            self.changed_at = Some(Instant::now());
        }
    }

    fn replay_settings(&mut self, ui: &mut egui::Ui) {
        ui.label("Save shortcut");
        ui.horizontal(|ui| {
            let text = if self.capturing_key {
                RichText::new("Press a key...").color(ui::ACCENT).strong()
            } else {
                RichText::new(&self.settings.hotkey).strong()
            };
            let button = egui::Button::new(text)
                .min_size(vec2(130.0, 28.0))
                .selected(self.capturing_key);
            if ui.add(button).clicked() {
                self.capturing_key = !self.capturing_key;
            }
            if self.capturing_key {
                ui::hint(ui, "Add Ctrl, Alt or Shift if you like. Esc cancels.");
                self.capture_key(ui);
            } else {
                ui::hint(ui, "Click to change");
            }
        });
        ui.end_row();
        let report_error = self.report.as_ref().and_then(|r| r.hotkey_error.clone());
        if let Some(p) = self.hotkey_problem.clone().or(report_error) {
            ui.label("");
            ui.label(RichText::new(p).color(ui::DANGER).size(12.0));
            ui.end_row();
        }

        ui.label("Clip length");
        ui.horizontal(|ui| {
            for s in [15, 30, 60, 120] {
                if ui
                    .selectable_label(self.settings.clip_seconds == s, seconds_label(s))
                    .clicked()
                {
                    self.settings.clip_seconds = s;
                }
            }
            ui.add(
                egui::DragValue::new(&mut self.settings.clip_seconds)
                    .range(MIN_CLIP_SECONDS..=MAX_CLIP_SECONDS)
                    .suffix(" s"),
            );
        });
        ui.end_row();
    }

    /// Turns the next key press into the shortcut.
    fn capture_key(&mut self, ui: &mut egui::Ui) {
        let events = ui.input(|i| i.events.clone());
        for e in events {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = e
            else {
                continue;
            };
            self.capturing_key = false;
            if key == egui::Key::Escape {
                break;
            }
            let mut parts: Vec<&str> = Vec::new();
            if modifiers.ctrl {
                parts.push("Ctrl");
            }
            if modifiers.mac_cmd {
                parts.push("Cmd");
            }
            if modifiers.alt {
                parts.push("Alt");
            }
            if modifiers.shift {
                parts.push("Shift");
            }
            parts.push(key.name());
            let spec = parts.join("+");
            match hotkey::parse(&spec) {
                Ok(_) => {
                    self.settings.hotkey = spec;
                    self.hotkey_problem = None;
                }
                Err(_) => {
                    self.hotkey_problem = Some(format!("{spec} can't be used as a shortcut."))
                }
            }
            break;
        }
    }

    fn video_settings(&mut self, ui: &mut egui::Ui) {
        ui.label("Screen");
        let current = self.monitor_label();
        egui::ComboBox::from_id_salt("monitor")
            .selected_text(current)
            .width(280.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut self.settings.monitor,
                    "primary".to_string(),
                    "Main screen",
                );
                for (i, m) in self.monitors.iter().enumerate() {
                    ui.selectable_value(&mut self.settings.monitor, (i + 1).to_string(), m.label());
                }
            });
        ui.end_row();

        ui.label("Frame rate");
        ui.horizontal(|ui| {
            for f in [30, 60, 120] {
                ui.selectable_value(&mut self.settings.fps, f, format!("{f} fps"));
            }
        });
        ui.end_row();

        ui.label("Resolution");
        egui::ComboBox::from_id_salt("resolution")
            .selected_text(self.settings.resolution.label())
            .width(180.0)
            .show_ui(ui, |ui| {
                for r in Resolution::ALL {
                    ui.selectable_value(&mut self.settings.resolution, r, r.label());
                }
            });
        ui.end_row();

        ui.label("Quality");
        egui::ComboBox::from_id_salt("quality")
            .selected_text(self.settings.quality.label())
            .width(180.0)
            .show_ui(ui, |ui| {
                for q in Quality::ALL {
                    ui.selectable_value(&mut self.settings.quality, q, q.label());
                }
            });
        ui.end_row();

        ui.label("Encoder");
        let found = self.encoders.lock().unwrap().clone();
        let selected = Encoder::from_id(&self.settings.encoder).map_or("Automatic", |e| e.label());
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("encoder")
                .selected_text(selected)
                .width(180.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.settings.encoder,
                        "auto".to_string(),
                        "Automatic",
                    );
                    for e in found.clone().unwrap_or_default() {
                        ui.selectable_value(
                            &mut self.settings.encoder,
                            e.id().to_string(),
                            e.label(),
                        );
                    }
                });
            if found.is_none() {
                ui.add(egui::Spinner::new().size(12.0));
                ui::hint(ui, "checking which encoders work...");
            }
        });
        ui.end_row();

        ui.label("");
        ui::hint(ui, &self.memory_estimate());
        ui.end_row();
    }

    fn sound_settings(&mut self, ui: &mut egui::Ui) {
        ui.label("Desktop audio");
        ui.horizontal(|ui| {
            let mac = cfg!(target_os = "macos");
            ui.add_enabled(
                !mac,
                egui::Checkbox::new(
                    &mut self.settings.desktop_audio,
                    "Record what the computer plays",
                ),
            );
            if mac {
                ui::hint(ui, "not available on macOS yet");
            }
        });
        ui.end_row();

        ui.label("Microphone");
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.settings.microphone, "Record");
            let name = if self.settings.microphone_device.is_empty() {
                "Default microphone".to_string()
            } else {
                self.settings.microphone_device.clone()
            };
            ui.add_enabled_ui(self.settings.microphone, |ui| {
                egui::ComboBox::from_id_salt("mic")
                    .selected_text(name)
                    .width(280.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.settings.microphone_device,
                            String::new(),
                            "Default microphone",
                        );
                        for m in &self.mics {
                            ui.selectable_value(&mut self.settings.microphone_device, m.clone(), m);
                        }
                    });
            });
        });
        ui.end_row();
    }

    fn saving_settings(&mut self, ui: &mut egui::Ui) {
        ui.label("Folder");
        ui.horizontal(|ui| {
            if ui.button("Change...").clicked()
                && let Some(d) = rfd::FileDialog::new()
                    .set_directory(self.settings.save_dir())
                    .pick_folder()
            {
                self.settings.save_dir = d.display().to_string();
            }
            if !self.settings.save_dir.is_empty() && ui.button("Default").clicked() {
                self.settings.save_dir.clear();
            }
            ui.add(egui::Label::new(self.settings.save_dir().display().to_string()).truncate());
        });
        ui.end_row();

        ui.label("When saved");
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.settings.notify, "Show a notification");
            ui.checkbox(&mut self.settings.sound, "Play a sound");
        });
        ui.end_row();

        ui.label("Clean up");
        ui.horizontal(|ui| {
            let mut on = self.settings.max_storage_gb > 0;
            let check = ui.checkbox(&mut on, "Keep the clips folder under");
            if check.changed() {
                self.settings.max_storage_gb = if on { 50 } else { 0 };
            }
            let mut gb = self.settings.max_storage_gb.max(1);
            let drag = ui.add_enabled(
                on,
                egui::DragValue::new(&mut gb)
                    .range(1..=10_000)
                    .suffix(" GB"),
            );
            if drag.labelled_by(check.id).changed() && on {
                self.settings.max_storage_gb = gb;
            }
        });
        ui.end_row();
        ui.label("");
        let used: u64 = self.clips.iter().map(|c| c.size).sum();
        ui::hint(
            ui,
            &format!(
                "Clips use {} now. Over the limit, the oldest are deleted after each save; favourites are always kept.",
                crate::app::human_size(used)
            ),
        );
        ui.end_row();

        ui.label("Discord size");
        ui.horizontal(|ui| {
            for (mb, label) in [
                (10, "10 MB (free)"),
                (50, "50 MB (Nitro Basic)"),
                (500, "500 MB (Nitro)"),
            ] {
                ui.selectable_value(&mut self.settings.discord_mb, mb, label);
            }
        });
        ui.end_row();
    }

    fn start_update_check(&mut self, ctx: &egui::Context) {
        if self.updates.checking.is_some() || self.updates.installing.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(updater::check());
            ctx.request_repaint();
        });
        self.updates.checking = Some(rx);
        self.updates.message = None;
    }

    fn poll_updates(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.updates.checking
            && let Ok(result) = rx.try_recv()
        {
            self.updates.checking = None;
            match result {
                Ok(Some(u)) => self.updates.available = Some(u),
                Ok(None) => {
                    self.updates.message = Some((
                        format!(
                            "Rewindr {} is the latest version.",
                            updater::current_version()
                        ),
                        ui::OK,
                    ))
                }
                Err(e) => {
                    self.updates.message =
                        Some((format!("Couldn't check for updates: {e}"), ui::DANGER))
                }
            }
        }
        if let Some(rx) = &self.updates.installing
            && let Ok(result) = rx.try_recv()
        {
            self.updates.installing = None;
            match result {
                Ok(()) => {
                    crate::log::info!("updated; restarting");
                    let _ = daemon::request(Request::Restart);
                    let _ = crate::app::spawn_self(&["window", "--wait"]);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Err(e) => self.updates.message = Some((format!("Update failed: {e}"), ui::DANGER)),
            }
        }
    }

    fn install_update(&mut self, ctx: &egui::Context) {
        let Some(u) = self.updates.available.clone() else {
            return;
        };
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(updater::install(&u));
            ctx.request_repaint();
        });
        self.updates.installing = Some(rx);
    }

    /// "Rewindr 0.2.0 is available" above the tabs.
    fn update_banner(&mut self, ui: &mut egui::Ui) {
        let Some(u) = &self.updates.available else {
            return;
        };
        let version = u.version.clone();
        let installing = self.updates.installing.is_some();
        let mut install = false;
        ui.add_space(6.0);
        egui::Frame::new()
            .fill(ui::ACCENT.gamma_multiply(0.16))
            .stroke(Stroke::new(1.0, ui::ACCENT.gamma_multiply(0.45)))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(12, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Rewindr {version} is available")).strong());
                    ui.label(
                        RichText::new(format!("(you have {})", updater::current_version()))
                            .color(ui::MUTED),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if installing {
                            ui.label("Updating...");
                            ui.add(egui::Spinner::new());
                        } else if ui
                            .button(RichText::new("Update and restart").strong())
                            .clicked()
                        {
                            install = true;
                        }
                    });
                });
            });
        if install {
            let ctx = ui.ctx().clone();
            self.install_update(&ctx);
        }
    }

    fn general_settings(&mut self, ui: &mut egui::Ui) {
        ui.label("Start");
        ui.add_enabled_ui(self.launcher.is_some(), |ui| {
            if ui
                .checkbox(&mut self.autostart, "Start Rewindr with the computer")
                .changed()
                && let Some(l) = &self.launcher
            {
                autostart::set_enabled(l, self.autostart);
                self.autostart = autostart::is_enabled(l);
            }
        });
        ui.end_row();

        ui.label("Version");
        ui.horizontal(|ui| {
            ui.label(env!("CARGO_PKG_VERSION"));
            if ui.link("Log file").clicked() {
                crate::app::open(&crate::log::path());
            }
            if ui.link("Quit Rewindr").clicked() {
                let _ = daemon::request(Request::Quit);
            }
        });
        ui.end_row();

        ui.label("Updates");
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut self.settings.check_updates,
                "Check when the window opens",
            );
            let busy = self.updates.checking.is_some();
            if ui
                .add_enabled(!busy, egui::Button::new("Check now"))
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.start_update_check(&ctx);
            }
            if busy {
                ui.add(egui::Spinner::new().size(12.0));
            }
        });
        ui.end_row();
        if let Some((text, color)) = self.updates.message.clone() {
            ui.label("");
            ui.label(RichText::new(text).color(color).size(12.0));
            ui.end_row();
        }
    }

    fn monitor_label(&self) -> String {
        match self
            .settings
            .monitor
            .parse::<usize>()
            .ok()
            .and_then(|i| self.monitors.get(i.checked_sub(1)?))
        {
            Some(m) => m.label(),
            None => "Main screen".into(),
        }
    }

    fn memory_estimate(&self) -> String {
        let m = platform::choose_monitor(&self.settings.monitor);
        let (w, h) = m.map_or((1920, 1080), |m| (m.w, m.h));
        let (ow, oh) = args::output_size((w, h), self.settings.resolution.max_height());
        let bps = self
            .settings
            .quality
            .bitrate(ow as u64 * oh as u64, self.settings.fps);
        let mb = bps as f64 / 8.0 * self.settings.buffer_seconds() as f64 / 1e6;
        format!(
            "{ow}x{oh} at about {:.0} Mbit/s. The replay is kept in memory (about {mb:.0} MB), never on disk.",
            bps as f64 / 1e6
        )
    }

    fn delete_dialog(&mut self, ctx: &egui::Context) {
        let Some(p) = self.confirm_delete.clone() else {
            return;
        };
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        egui::Window::new("Delete clip?")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!("{name} will be deleted."));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let delete = egui::Button::new(RichText::new("Delete").color(Color32::WHITE))
                        .fill(ui::DANGER);
                    if ui.add(delete).clicked() {
                        match std::fs::remove_file(&p) {
                            Ok(()) => {
                                let _ = std::fs::remove_file(crate::save::thumb_for(&p));
                                self.clips.retain(|c| c.path != p);
                                self.thumbs.remove(&p);
                            }
                            Err(e) => self.note(format!("Couldn't delete: {e}"), ui::DANGER),
                        }
                        self.confirm_delete = None;
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_delete = None;
                    }
                });
            });
    }
}

/// A titled group of settings: a heading, then a panel holding a two-column grid.
fn card(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).strong().size(13.0).color(ui::MUTED));
    ui.add_space(4.0);
    egui::Frame::new()
        .fill(ui::PANEL)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new(title)
                .num_columns(2)
                .min_col_width(130.0)
                .spacing(vec2(16.0, 12.0))
                .show(ui, add);
        });
}

impl eframe::App for Window {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.frame_no += 1;
        let shot = if self.tab == Tab::Clips {
            "window-clips"
        } else {
            "window-settings"
        };
        crate::app::shot_hook(&ctx, self.frame_no, shot);
        self.refresh(&ctx);
        self.poll_updates(&ctx);
        egui::Panel::top("header")
            .frame(egui::Frame::new().fill(ui::BG).inner_margin(Margin {
                left: 16,
                right: 16,
                top: 12,
                bottom: 10,
            }))
            .show(ui, |ui| {
                self.header(ui);
                ui.add_space(4.0);
                self.summary(ui);
                self.update_banner(ui);
                ui.add_space(10.0);
                self.tabs(ui);
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui::BG)
                    .inner_margin(Margin::symmetric(16, 8)),
            )
            .show(ui, |ui| match self.tab {
                Tab::Clips => self.clips_tab(ui),
                Tab::Settings => self.settings_tab(ui),
            });
        self.delete_dialog(&ctx);
        self.rename_dialog(&ctx);
        if self.capturing_key {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.settings.sanitize();
        if self.settings != self.saved {
            let _ = config::save(&self.path, &self.settings);
        }
    }
}

/// "TRIMMED" or "DISCORD" for copies the editor made.
fn copy_tag(name: &str) -> Option<&'static str> {
    if name.contains("(Discord") {
        Some("DISCORD")
    } else if name.contains("(trim") {
        Some("TRIMMED")
    } else {
        None
    }
}

fn seconds_label(s: u32) -> String {
    if s >= 60 && s.is_multiple_of(60) {
        format!("{} min", s / 60)
    } else {
        format!("{s} s")
    }
}

fn load_thumb(ctx: &egui::Context, clip: &Path) -> Option<egui::TextureHandle> {
    let img = image::open(crate::save::thumb_for(clip)).ok()?.to_rgba8();
    let ci = egui::ColorImage::from_rgba_unmultiplied(
        [img.width() as usize, img.height() as usize],
        img.as_raw(),
    );
    Some(ctx.load_texture(clip.display().to_string(), ci, TextureOptions::LINEAR))
}
