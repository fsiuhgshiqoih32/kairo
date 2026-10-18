//! The clip editor (`rewindr edit <clip>`): play the clip with sound, scrub a filmstrip
//! timeline, trim it, and save a trimmed copy or a copy small enough for Discord.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, CornerRadius, Key, Layout, Margin, Rect, RichText, Sense, Stroke,
    TextureOptions, pos2, vec2,
};

use rewindr::args::Encoder;
use rewindr::config::{self, Settings};
use rewindr::library::{self, Library};
use rewindr::media::{self, Info, format_time};

use crate::export::{self, Job, Kind};
use crate::player::Player;
use crate::{ffmpeg, platform, ui};

/// Frames are decoded for playback at most this big.
const PLAYBACK_MAX: (u32, u32) = (1280, 720);
const STRIP_H: u32 = 54;
const TIMELINE_H: f32 = 64.0;
const HANDLE_W: f32 = 10.0;

pub fn probe(ff: &Path, clip: &Path) -> Result<Info, String> {
    let out = ffmpeg::command(ff)
        .arg("-hide_banner")
        .arg("-i")
        .arg(clip)
        .output()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    media::parse_info(&String::from_utf8_lossy(&out.stderr))
        .ok_or_else(|| format!("{} isn't a video Rewindr can open.", clip.display()))
}

pub fn run(clip: PathBuf) -> Result<(), String> {
    let ff = ffmpeg::path()?;
    if !clip.is_file() {
        return Err(format!("{} doesn't exist.", clip.display()));
    }
    let info = probe(&ff, &clip)?;
    let viewport = egui::ViewportBuilder::default()
        .with_title(title_for(&clip))
        .with_app_id("rewindr")
        .with_inner_size(vec2(1100.0, 780.0))
        .with_min_inner_size(vec2(720.0, 520.0));
    let mut parts = Some((ff, clip, info));
    crate::app::run_window("Rewindr", viewport, crate::app::Kind::Normal, |cc| {
        let (ff, clip, info) = parts.take().expect("editor built once");
        Editor::new(cc, ff, clip, info)
    })
}

fn title_for(clip: &Path) -> String {
    let name = clip
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    format!("{name} - Rewindr")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Drag {
    Seek,
    In,
    Out,
}

pub struct Editor {
    ff: PathBuf,
    clip: PathBuf,
    name: String,
    player: Player,
    video_tex: Option<egui::TextureHandle>,
    strip: Arc<Mutex<Option<Vec<egui::ColorImage>>>>,
    strip_tex: Vec<egui::TextureHandle>,
    trim: (f64, f64),
    drag: Option<Drag>,
    resume_after_drag: bool,
    last_scrub: f64,
    job: Option<Job>,
    encoders: Arc<Mutex<Option<Vec<Encoder>>>>,
    settings: Settings,
    library: Library,
    note: Option<(String, Color32, Instant)>,
    last_export: Option<PathBuf>,
    confirm_delete: bool,
    frame_no: u64,
}

impl Editor {
    pub fn new(cc: &eframe::CreationContext<'_>, ff: PathBuf, clip: PathBuf, info: Info) -> Self {
        let (settings, _) = config::load_or_create(&config::default_path());
        let duration = info.duration;
        let strip: Arc<Mutex<Option<Vec<egui::ColorImage>>>> = Arc::default();
        {
            let strip = strip.clone();
            let ctx = cc.egui_ctx.clone();
            let (ff, clip) = (ff.clone(), clip.clone());
            let w = ((STRIP_H as f64 * info.width as f64 / info.height.max(1) as f64).round()
                as u32)
                .max(2)
                & !1;
            std::thread::spawn(move || {
                let images = filmstrip(&ff, &clip, (w, STRIP_H));
                *strip.lock().unwrap() = Some(images);
                ctx.request_repaint();
            });
        }
        let encoders: Arc<Mutex<Option<Vec<Encoder>>>> = Arc::default();
        {
            let (enc, ff) = (encoders.clone(), ff.clone());
            std::thread::spawn(move || {
                export::best_encoder(&ff, &enc);
            });
        }
        let name = clip
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        Editor {
            player: Player::new(ff.clone(), clip.clone(), info, PLAYBACK_MAX),
            ff,
            clip,
            name,
            video_tex: None,
            strip,
            strip_tex: Vec::new(),
            trim: (0.0, duration),
            drag: None,
            resume_after_drag: false,
            last_scrub: -1.0,
            job: None,
            encoders,
            settings,
            library: Library::load(),
            note: None,
            last_export: None,
            confirm_delete: false,
            frame_no: 0,
        }
    }

    /// The first frame and the filmstrip are on screen (for snapshots).
    #[cfg(test)]
    pub fn loaded(&self) -> bool {
        self.video_tex.is_some() && !self.strip_tex.is_empty() && !self.player.has_pending_still()
    }

    /// Sets a trim and shows the frame at `at` (for snapshots).
    #[cfg(test)]
    pub fn snapshot_trim(&mut self, start: f64, end: f64, at: f64) {
        self.trim = (start, end);
        self.video_tex = None;
        self.player.seek(at);
    }

    fn note(&mut self, text: impl Into<String>, color: Color32) {
        self.note = Some((text.into(), color, Instant::now()));
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

    fn keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.confirm_delete {
            return;
        }
        let (space, left, right, comma, period, i, o, home, end, shift) = ctx.input(|inp| {
            (
                inp.key_pressed(Key::Space),
                inp.key_pressed(Key::ArrowLeft),
                inp.key_pressed(Key::ArrowRight),
                inp.key_pressed(Key::Comma),
                inp.key_pressed(Key::Period),
                inp.key_pressed(Key::I),
                inp.key_pressed(Key::O),
                inp.key_pressed(Key::Home),
                inp.key_pressed(Key::End),
                inp.modifiers.shift,
            )
        });
        let jump = if shift { 5.0 } else { 1.0 };
        let pos = self.player.position;
        if space {
            self.toggle_play();
        }
        if left {
            self.player.seek(pos - jump);
        }
        if right {
            self.player.seek(pos + jump);
        }
        if comma || period {
            self.player.pause();
            let step = self.player.frame_step();
            self.player
                .seek(if comma { pos - step } else { pos + step });
        }
        if i {
            self.set_in(pos);
        }
        if o {
            self.set_out(pos);
        }
        if home {
            self.player.seek(self.trim.0);
        }
        if end {
            self.player.seek(self.trim.1);
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

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let fav = self.library.is_favorite(&self.clip);
            let (r, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
            let color = if fav {
                Color32::from_rgb(0xFF, 0xC4, 0x3D)
            } else {
                ui::MUTED
            };
            ui::paint_star(ui.painter(), r.center(), 10.0, fav, color);
            if resp
                .on_hover_text(if fav {
                    "Remove from favourites"
                } else {
                    "Add to favourites (kept when old clips are cleaned up)"
                })
                .clicked()
            {
                self.library.set_favorite(&self.clip, !fav);
                let _ = self.library.save();
            }
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.name)
                    .font(egui::FontId::proportional(17.0))
                    .desired_width((ui.available_width() - 260.0).max(200.0))
                    .frame(egui::Frame::NONE)
                    .hint_text("Clip name"),
            );
            if edit.lost_focus() {
                self.rename(ui.ctx());
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Delete").clicked() {
                    self.confirm_delete = true;
                }
                if ui.button("Show in folder").clicked() {
                    crate::app::reveal(&self.clip);
                }
                if ui
                    .button("Copy")
                    .on_hover_text("Copy the file, to paste into Discord, a chat or a folder")
                    .clicked()
                {
                    match platform::copy_files(std::slice::from_ref(&self.clip)) {
                        Ok(()) => self.note("Copied. Paste it anywhere.", ui::OK),
                        Err(e) => self.note(e, ui::DANGER),
                    }
                }
            });
        });
    }

    fn rename(&mut self, ctx: &egui::Context) {
        let current = self
            .clip
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if self.name.trim() == current {
            self.name = current;
            return;
        }
        let to = match library::rename_target(&self.clip, &self.name) {
            Ok(t) => t,
            Err(e) => {
                self.note(e, ui::DANGER);
                self.name = current;
                return;
            }
        };
        let was_playing = self.player.is_playing();
        self.player.release_file();
        match std::fs::rename(&self.clip, &to) {
            Ok(()) => {
                let _ = std::fs::rename(
                    crate::save::thumb_for(&self.clip),
                    crate::save::thumb_for(&to),
                );
                self.library.renamed(&self.clip, &to);
                let _ = self.library.save();
                self.clip = to.clone();
                self.player.clip = to;
                self.name = self.name.trim().trim_end_matches(".mp4").to_string();
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(title_for(&self.clip)));
                self.note("Renamed", ui::OK);
            }
            Err(e) => {
                self.note(format!("Couldn't rename: {e}"), ui::DANGER);
                self.name = current;
            }
        }
        let pos = self.player.position;
        self.player.seek(pos);
        if was_playing {
            self.player.play();
        }
    }

    fn video(&mut self, ui: &mut egui::Ui) {
        let area = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(area, Sense::click());
        let painter = ui.painter_at(area);
        painter.rect_filled(area, CornerRadius::same(8), Color32::BLACK);
        if let Some(t) = &self.video_tex {
            let size = t.size_vec2();
            let k = (area.width() / size.x).min(area.height() / size.y);
            let r = Rect::from_center_size(area.center(), size * k);
            painter.image(
                t.id(),
                r,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            painter.text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                "Loading...",
                egui::FontId::proportional(14.0),
                ui::MUTED,
            );
        }
        if !self.player.is_playing() {
            let alpha = if resp.hovered() { 200 } else { 140 };
            painter.circle_filled(area.center(), 34.0, Color32::from_black_alpha(alpha));
            ui::paint_play(
                &painter,
                area.center() + vec2(3.0, 0.0),
                26.0,
                false,
                Color32::WHITE,
            );
        }
        if resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            self.toggle_play();
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (r, resp) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::click());
            let hover = resp.hovered();
            ui.painter().circle_filled(
                r.center(),
                18.0,
                if hover {
                    ui::ACCENT
                } else {
                    ui::ACCENT.gamma_multiply(0.85)
                },
            );
            ui::paint_play(
                ui.painter(),
                r.center() + vec2(if self.player.is_playing() { 0.0 } else { 2.0 }, 0.0),
                14.0,
                self.player.is_playing(),
                Color32::WHITE,
            );
            if resp.on_hover_text("Play or pause (Space)").clicked() {
                self.toggle_play();
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "{}  /  {}",
                    format_time(self.player.position),
                    format_time(self.player.duration())
                ))
                .size(14.0)
                .monospace(),
            );
            ui.add_space(8.0);
            ui::hint(
                ui,
                "Space play · Left/Right 1 s · , . one frame · I/O start/end",
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mut v = self.player.volume;
                let slider = ui.add(
                    egui::Slider::new(&mut v, 0.0..=1.5)
                        .show_value(false)
                        .trailing_fill(true),
                );
                let label = ui.label("Volume");
                if slider.labelled_by(label.id).changed() {
                    self.player.set_volume(v);
                }
                let tracks = self.player.info.audio_tracks.clone();
                if tracks.len() > 1 {
                    let mut t = self.player.track;
                    let name = |i: usize| {
                        let n = &tracks[i];
                        if n.is_empty() {
                            format!("Track {}", i + 1)
                        } else {
                            n.clone()
                        }
                    };
                    egui::ComboBox::from_id_salt("track")
                        .selected_text(name(t))
                        .show_ui(ui, |ui| {
                            for i in 0..tracks.len() {
                                ui.selectable_value(&mut t, i, name(i));
                            }
                        });
                    ui.label("Sound");
                    self.player.set_track(t);
                }
            });
        });
    }

    fn time_at(&self, rect: Rect, x: f32) -> f64 {
        let f = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
        f * self.player.duration()
    }

    fn x_at(&self, rect: Rect, t: f64) -> f32 {
        let d = self.player.duration().max(0.001);
        rect.left() + (t / d).clamp(0.0, 1.0) as f32 * rect.width()
    }

    fn timeline(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let (outer, resp) =
            ui.allocate_exact_size(vec2(width, TIMELINE_H), Sense::click_and_drag());
        // Leave room for the handles at both ends.
        let rect = outer.shrink2(vec2(HANDLE_W, 4.0));
        let painter = ui.painter_at(outer);
        painter.rect_filled(
            rect,
            CornerRadius::same(6),
            Color32::from_rgb(0x12, 0x13, 0x16),
        );
        if !self.strip_tex.is_empty() {
            let n = self.strip_tex.len();
            let tile_w = rect.width() / n as f32;
            for (i, t) in self.strip_tex.iter().enumerate() {
                let r = Rect::from_min_size(
                    pos2(rect.left() + i as f32 * tile_w, rect.top()),
                    vec2(tile_w + 0.5, rect.height()),
                );
                let size = t.size_vec2();
                let (ia, ra) = (size.x / size.y, r.width() / r.height());
                let uv = if ia > ra {
                    let u = ra / ia;
                    Rect::from_min_max(pos2((1.0 - u) / 2.0, 0.0), pos2((1.0 + u) / 2.0, 1.0))
                } else {
                    let v = ia / ra;
                    Rect::from_min_max(pos2(0.0, (1.0 - v) / 2.0), pos2(1.0, (1.0 + v) / 2.0))
                };
                painter.image(t.id(), r, uv, Color32::WHITE);
            }
        }
        let (xi, xo) = (self.x_at(rect, self.trim.0), self.x_at(rect, self.trim.1));
        let shade = Color32::from_black_alpha(170);
        painter.rect_filled(
            Rect::from_min_max(rect.left_top(), pos2(xi, rect.bottom())),
            CornerRadius::ZERO,
            shade,
        );
        painter.rect_filled(
            Rect::from_min_max(pos2(xo, rect.top()), rect.right_bottom()),
            CornerRadius::ZERO,
            shade,
        );
        let sel = Rect::from_min_max(pos2(xi, rect.top()), pos2(xo, rect.bottom()));
        painter.rect_stroke(
            sel,
            CornerRadius::same(3),
            Stroke::new(2.0, ui::ACCENT),
            egui::StrokeKind::Outside,
        );
        for (x, left) in [(xi, true), (xo, false)] {
            let h = if left {
                Rect::from_min_max(
                    pos2(x - HANDLE_W, rect.top() - 2.0),
                    pos2(x, rect.bottom() + 2.0),
                )
            } else {
                Rect::from_min_max(
                    pos2(x, rect.top() - 2.0),
                    pos2(x + HANDLE_W, rect.bottom() + 2.0),
                )
            };
            let corners = if left {
                CornerRadius {
                    nw: 4,
                    sw: 4,
                    ne: 0,
                    se: 0,
                }
            } else {
                CornerRadius {
                    nw: 0,
                    sw: 0,
                    ne: 4,
                    se: 4,
                }
            };
            painter.rect_filled(h, corners, ui::ACCENT);
            for dy in [-5.0, 0.0, 5.0] {
                let c = h.center() + vec2(0.0, dy);
                painter.line_segment(
                    [c - vec2(2.0, 0.0), c + vec2(2.0, 0.0)],
                    Stroke::new(1.0, Color32::WHITE),
                );
            }
        }
        let xp = self.x_at(rect, self.player.position);
        painter.line_segment(
            [pos2(xp, outer.top()), pos2(xp, outer.bottom())],
            Stroke::new(2.0, Color32::WHITE),
        );
        painter.circle_filled(pos2(xp, outer.top() + 3.0), 4.0, Color32::WHITE);

        if let Some(p) = resp.hover_pos() {
            let t = self.time_at(rect, p.x);
            let near_in = (p.x - (xi - HANDLE_W / 2.0)).abs() <= HANDLE_W;
            let near_out = (p.x - (xo + HANDLE_W / 2.0)).abs() <= HANDLE_W;
            if near_in || near_out || self.drag.is_some_and(|d| d != Drag::Seek) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if self.drag.is_none() {
                resp.clone().on_hover_text_at_pointer(format_time(t));
            }
        }
        if (resp.drag_started() || (resp.is_pointer_button_down_on() && self.drag.is_none()))
            && let Some(p) = resp.interact_pointer_pos()
        {
            let near_in = (p.x - (xi - HANDLE_W / 2.0)).abs() <= HANDLE_W;
            let near_out = (p.x - (xo + HANDLE_W / 2.0)).abs() <= HANDLE_W;
            self.drag = Some(if near_in {
                Drag::In
            } else if near_out {
                Drag::Out
            } else {
                Drag::Seek
            });
            self.resume_after_drag = self.player.is_playing();
            self.player.pause();
        }
        if let (Some(d), Some(p)) = (self.drag, resp.interact_pointer_pos()) {
            let t = self.time_at(rect, p.x);
            match d {
                Drag::Seek => {}
                Drag::In => self.set_in(t),
                Drag::Out => self.set_out(t),
            }
            let show = match d {
                Drag::Seek => t,
                Drag::In => self.trim.0,
                Drag::Out => self.trim.1,
            };
            // Fetching a frame per pixel would pile up; only follow real moves.
            if (show - self.last_scrub).abs() >= self.player.frame_step() {
                self.last_scrub = show;
                self.player.seek(show);
            }
        }
        if self.drag.is_some() && !resp.is_pointer_button_down_on() && !resp.dragged() {
            self.drag = None;
            if self.resume_after_drag {
                self.resume_after_drag = false;
                self.toggle_play();
            }
        }
    }

    fn trim_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Trim").strong());
            ui.label(
                RichText::new(format!(
                    "{}  –  {}   ({:.1} s)",
                    format_time(self.trim.0),
                    format_time(self.trim.1),
                    self.trim.1 - self.trim.0
                ))
                .monospace(),
            );
            if ui.small_button("Set start (I)").clicked() {
                let p = self.player.position;
                self.set_in(p);
            }
            if ui.small_button("Set end (O)").clicked() {
                let p = self.player.position;
                self.set_out(p);
            }
            if ui.add_enabled(self.trimmed(), egui::Button::new("Reset").small()).clicked() {
                self.trim = (0.0, self.player.duration());
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(job) = &self.job {
                    let st = job.status.lock().unwrap();
                    let (p, stage) = (st.progress, st.stage.clone());
                    drop(st);
                    if ui.button("Cancel").clicked() {
                        job.cancel();
                    }
                    ui.add(
                        egui::ProgressBar::new(p)
                            .desired_width(240.0)
                            .text(format!("{stage}  {:.0}%", p * 100.0)),
                    );
                    return;
                }
                let mb = self.settings.discord_mb;
                let share = egui::Button::new(
                    RichText::new(format!("Share for Discord ({mb} MB)")).strong().color(Color32::WHITE),
                )
                .fill(ui::ACCENT)
                .min_size(vec2(0.0, 30.0));
                if ui
                    .add(share)
                    .on_hover_text("Makes a copy under the size limit and copies it, ready to paste into Discord")
                    .clicked()
                {
                    self.start_job(Kind::Share(mb as u64 * 1_000_000));
                }
                let save = egui::Button::new("Save trimmed copy").min_size(vec2(0.0, 30.0));
                if ui
                    .add_enabled(self.trimmed(), save)
                    .on_disabled_hover_text("Drag the orange handles (or press I and O) to choose the part to keep")
                    .clicked()
                {
                    self.start_job(Kind::Trim);
                }
            });
        });
    }

    fn start_job(&mut self, kind: Kind) {
        if self.job.is_some() {
            return;
        }
        // An exact copy of the whole clip is just the clip.
        let range = if kind == Kind::Trim || self.trimmed() {
            self.trim
        } else {
            (0.0, self.player.duration())
        };
        if let Kind::Share(limit) = kind
            && let Err(e) = media::share_plan(limit, range.1 - range.0, &self.player.info, 1.0)
        {
            self.note(e, ui::DANGER);
            return;
        }
        self.job = Some(export::start(
            kind,
            self.ff.clone(),
            self.clip.clone(),
            self.player.info.clone(),
            range,
            self.encoders.clone(),
        ));
    }

    fn poll_job(&mut self) {
        let Some(job) = &self.job else { return };
        if !job.finished() {
            return;
        }
        let kind = job.kind;
        let result = job.status.lock().unwrap().result.take();
        self.job = None;
        match result {
            Some(Ok(path)) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let size = std::fs::metadata(&path)
                    .map(|m| crate::app::human_size(m.len()))
                    .unwrap_or_default();
                match kind {
                    Kind::Share(_) => match platform::copy_files(std::slice::from_ref(&path)) {
                        Ok(()) => self.note(
                            format!("Ready ({size}) and copied. Paste it into Discord."),
                            ui::OK,
                        ),
                        Err(_) => self.note(format!("Saved {name} ({size})"), ui::OK),
                    },
                    Kind::Trim => self.note(format!("Saved {name} ({size})"), ui::OK),
                }
                self.last_export = Some(path);
            }
            Some(Err(e)) if e == "Cancelled" => self.note("Cancelled", ui::MUTED),
            Some(Err(e)) => self.note(e, ui::DANGER),
            None => {}
        }
    }

    fn delete_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_delete {
            return;
        }
        egui::Window::new("Delete clip?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!("{} will be deleted.", self.name));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let delete = egui::Button::new(RichText::new("Delete").color(Color32::WHITE))
                        .fill(ui::DANGER);
                    if ui.add(delete).clicked() {
                        self.player.release_file();
                        match std::fs::remove_file(&self.clip) {
                            Ok(()) => {
                                let _ = std::fs::remove_file(crate::save::thumb_for(&self.clip));
                                self.library.set_favorite(&self.clip, false);
                                let _ = self.library.save();
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            Err(e) => self.note(format!("Couldn't delete: {e}"), ui::DANGER),
                        }
                        self.confirm_delete = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_delete = false;
                    }
                });
            });
    }

    fn upload(&mut self, ctx: &egui::Context) {
        if let Some(f) = self.player.frame.take() {
            let (w, h) = self.player.size;
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &f.rgba);
            match &mut self.video_tex {
                Some(t) => t.set(img, TextureOptions::LINEAR),
                None => {
                    self.video_tex = Some(ctx.load_texture("video", img, TextureOptions::LINEAR))
                }
            }
        }
        if self.strip_tex.is_empty()
            && let Some(images) = self.strip.lock().unwrap().take()
        {
            self.strip_tex = images
                .into_iter()
                .enumerate()
                .map(|(i, img)| ctx.load_texture(format!("strip{i}"), img, TextureOptions::LINEAR))
                .collect();
        }
    }
}

impl eframe::App for Editor {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.frame_no += 1;
        crate::app::shot_hook(&ctx, self.frame_no, "editor");
        self.keys(&ctx);
        self.player.tick(self.trim.1);
        self.upload(&ctx);
        self.poll_job();
        if let Some(e) = self.player.error.take() {
            self.note(e, ui::DANGER);
        }
        egui::Panel::top("bar")
            .frame(egui::Frame::new().fill(ui::BG).inner_margin(Margin {
                left: 12,
                right: 14,
                top: 8,
                bottom: 6,
            }))
            .show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("controls")
            .frame(egui::Frame::new().fill(ui::BG).inner_margin(Margin {
                left: 14,
                right: 14,
                top: 8,
                bottom: 12,
            }))
            .show(ui, |ui| {
                self.controls(ui);
                ui.add_space(6.0);
                self.timeline(ui);
                ui.add_space(8.0);
                self.trim_row(ui);
                let note = self.note.clone();
                if let Some((text, color, at)) = note {
                    if at.elapsed() < Duration::from_secs(8) {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(text).color(color));
                            if let Some(p) = self.last_export.clone()
                                && color == ui::OK
                                && ui.link("Show in folder").clicked()
                            {
                                crate::app::reveal(&p);
                            }
                        });
                    } else {
                        self.note = None;
                    }
                }
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui::BG)
                    .inner_margin(Margin::symmetric(14, 4)),
            )
            .show(ui, |ui| self.video(ui));
        self.delete_dialog(&ctx);
        if self.player.is_playing() || self.job.is_some() || self.drag.is_some() {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.player.release_file();
        if let Some(j) = &self.job {
            j.cancel();
        }
    }
}

/// The keyframe pictures for the timeline.
fn filmstrip(ff: &Path, clip: &Path, size: (u32, u32)) -> Vec<egui::ColorImage> {
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
        .map(|c| egui::ColorImage::from_rgba_unmultiplied([size.0 as usize, size.1 as usize], c))
        .collect()
}
