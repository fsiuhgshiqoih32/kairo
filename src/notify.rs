//! The "Clip saved" card in the corner of the screen: a thumbnail, the file name, and buttons to
//! open, show or copy the clip. It runs as its own short-lived process (`rewindr notify`), so
//! the recorder never waits on a window.

use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui::{self, Color32, RichText, Sense, Stroke, TextureOptions, vec2};

use crate::{platform, ui};

const W: f32 = 380.0;
const H: f32 = 104.0;
const SHOW_FOR: f64 = 5.0;

pub enum Message {
    Saved { clip: PathBuf, seconds: f64 },
    Failed(String),
}

pub fn run(msg: Message) -> Result<(), String> {
    if platform::is_wayland() {
        return system_notification(&msg);
    }
    let viewport = egui::ViewportBuilder::default()
        .with_title("Rewindr")
        .with_app_id("rewindr")
        .with_decorations(false)
        .with_resizable(false)
        .with_always_on_top()
        .with_taskbar(false)
        .with_active(false)
        .with_inner_size(vec2(W, H));
    let mut msg = Some(msg);
    crate::app::run_window("Rewindr", viewport, crate::app::Kind::Accessory, |_cc| {
        Card {
            msg: msg.take().expect("built once"),
            texture: None,
            frame_no: 0,
            shown_at: None,
            status: None,
        }
    })
}

struct Card {
    msg: Message,
    texture: Option<egui::TextureHandle>,
    frame_no: u64,
    shown_at: Option<f64>,
    status: Option<String>,
}

impl Card {
    fn place(&self, frame: &eframe::Frame) {
        let Some(w) = frame.winit_window() else {
            return;
        };
        let scale = w.scale_factor();
        let (cw, ch) = ((W as f64 * scale) as i32, (H as f64 * scale) as i32);
        let margin = (14.0 * scale) as i32;
        let area = platform::work_area().or_else(|| {
            let m = w.primary_monitor().or_else(|| w.current_monitor())?;
            let (p, s) = (m.position(), m.size());
            // Leave room for a taskbar or dock we can't measure.
            Some((
                p.x,
                p.y,
                s.width,
                s.height.saturating_sub((48.0 * scale) as u32),
            ))
        });
        if let Some((x, y, aw, ah)) = area {
            w.set_outer_position(winit::dpi::PhysicalPosition::new(
                x + aw as i32 - cw - margin,
                y + ah as i32 - ch - margin,
            ));
        }
    }
}

fn load_thumb(ctx: &egui::Context, path: &Path) -> Option<egui::TextureHandle> {
    let img = image::open(path).ok()?.to_rgba8();
    let ci = egui::ColorImage::from_rgba_unmultiplied(
        [img.width() as usize, img.height() as usize],
        img.as_raw(),
    );
    Some(ctx.load_texture("thumb", ci, TextureOptions::LINEAR))
}

impl eframe::App for Card {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        if self.frame_no == 0 || self.frame_no == 3 {
            self.place(frame);
        }
        self.frame_no += 1;
        crate::app::shot_hook(&ctx, self.frame_no, "notify");
        let shown = *self.shown_at.get_or_insert(now);

        let screen = ctx.viewport_rect();
        let hovered = ctx
            .input(|i| i.pointer.hover_pos())
            .is_some_and(|p| screen.contains(p));
        let limit = if matches!(self.msg, Message::Failed(_)) {
            15.0
        } else {
            SHOW_FOR
        };
        if hovered {
            self.shown_at = Some(now);
        } else if now - shown > limit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        let painter = ui.painter();
        painter.rect_filled(screen, 8.0, ui::PANEL);
        painter.rect_stroke(
            screen.shrink(0.5),
            8.0,
            Stroke::new(1.0, Color32::from_white_alpha(30)),
            egui::StrokeKind::Inside,
        );
        let inner = screen.shrink(10.0);

        match &self.msg {
            Message::Saved { clip, seconds } => {
                let clip = clip.clone();
                let body = ui.interact(screen, ui.scope_id().with("card-body"), Sense::click());
                if body.clicked() {
                    crate::app::open(&clip);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                let thumb_rect = egui::Rect::from_min_size(
                    inner.min,
                    vec2(inner.height() * 16.0 / 9.0, inner.height()),
                );
                if self.texture.is_none() {
                    self.texture = load_thumb(&ctx, &crate::save::thumb_for(&clip));
                }
                ui.painter()
                    .rect_filled(thumb_rect, 4.0, Color32::from_black_alpha(90));
                if let Some(t) = &self.texture {
                    let size = t.size_vec2();
                    let k = (thumb_rect.width() / size.x).min(thumb_rect.height() / size.y);
                    let r = egui::Rect::from_center_size(thumb_rect.center(), size * k);
                    ui.painter().image(
                        t.id(),
                        r,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                let text = egui::Rect::from_min_max(
                    egui::pos2(thumb_rect.max.x + 12.0, inner.min.y),
                    inner.max,
                );
                let mut c = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(text)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                c.spacing_mut().item_spacing.y = 3.0;
                c.horizontal(|ui| {
                    ui.label(
                        RichText::new("Clip saved")
                            .strong()
                            .size(14.0)
                            .color(ui::OK),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").on_hover_text("Close").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
                let name = clip
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                c.add(egui::Label::new(RichText::new(name).size(12.0)).truncate());
                let size = std::fs::metadata(&clip)
                    .map(|m| crate::app::human_size(m.len()))
                    .unwrap_or_default();
                let info = self
                    .status
                    .clone()
                    .unwrap_or_else(|| format!("{seconds:.0} s  ·  {size}"));
                c.label(RichText::new(info).size(11.5).color(ui::MUTED));
                c.horizontal(|ui| {
                    if ui.small_button("Show in folder").clicked() {
                        crate::app::reveal(&clip);
                    }
                    if ui
                        .small_button("Copy")
                        .on_hover_text("Copy the file, to paste into Discord or a folder")
                        .clicked()
                    {
                        self.status =
                            Some(match platform::copy_files(std::slice::from_ref(&clip)) {
                                Ok(()) => "Copied: paste it anywhere".into(),
                                Err(e) => e,
                            });
                        self.shown_at = Some(now);
                    }
                });
            }
            Message::Failed(e) => {
                let mut c = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(inner)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                c.horizontal(|ui| {
                    ui.label(
                        RichText::new("Couldn't save the clip")
                            .strong()
                            .size(14.0)
                            .color(ui::DANGER),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
                c.add(egui::Label::new(RichText::new(e).size(12.0)).wrap());
            }
        }
        ctx.request_repaint_after(Duration::from_millis(200));
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c = ui::PANEL.to_normalized_gamma_f32();
        [c[0], c[1], c[2], 1.0]
    }
}

/// Wayland won't let us place a window in a corner; use the desktop's notifications.
fn system_notification(msg: &Message) -> Result<(), String> {
    let (title, body, icon) = match msg {
        Message::Saved { clip, seconds } => (
            "Clip saved".to_string(),
            format!(
                "{} ({seconds:.0} s)",
                clip.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            ),
            Some(crate::save::thumb_for(clip)),
        ),
        Message::Failed(e) => ("Couldn't save the clip".to_string(), e.clone(), None),
    };
    let mut cmd = std::process::Command::new("notify-send");
    cmd.args(["--app-name=Rewindr", &title, &body]);
    if let Some(i) = icon.filter(|i| i.exists()) {
        cmd.arg(format!("--icon={}", i.display()));
    }
    cmd.status()
        .map(|_| ())
        .map_err(|e| format!("notify-send: {e}"))
}
