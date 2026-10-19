//! The clip library: cards grouped by day, filters, search, hover previews, share links, and
//! the screenshots.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use iced::alignment::{Horizontal, Vertical};
use iced::mouse;
use iced::widget::canvas::{self, Action, Canvas, Frame, Geometry};
use iced::widget::{
    Space, button, column, container, grid, hover, image, pick_list, row, scrollable, space, stack,
    text, tooltip,
};
use iced::{
    Alignment, Color, ContentFit, Element, Fill, Padding, Point, Rectangle, Renderer, Size, Theme,
};

use rewindr::library::{self, Library, Link};

use super::icons::{Icon, icon, icon_filled};
use super::theme::{self, BOLD, MEDIUM, MUTED, SEMIBOLD, TEXT, TEXT_2};
use super::widgets::{icon_button, icon_only};
use super::{App, Filter, Message, Sort, day_label, duration, human_size, seconds_label, when};

/// A clip as the library shows it.
#[derive(Debug, Clone)]
pub struct ClipItem {
    pub path: PathBuf,
    pub name: String,
    pub game: String,
    pub size: u64,
    pub modified: SystemTime,
    pub duration: Option<f64>,
    pub favorite: bool,
    pub kind: ClipKind,
    pub thumb: Option<image::Handle>,
    pub link: Option<Link>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipKind {
    Clip,
    Trim,
    Discord,
    Vertical,
    Slow,
}

impl ClipItem {
    /// The card's title: the game for Rewindr's own names, or the name the user gave it.
    pub fn title(&self) -> &str {
        if self.name.starts_with(&self.game) {
            &self.game
        } else {
            &self.name
        }
    }
}

/// A screenshot as the library shows it.
#[derive(Debug, Clone)]
pub struct ShotItem {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub modified: SystemTime,
    pub picture: image::Handle,
    pub link: Option<Link>,
}

/// Reads the clips folder: names, games, lengths, favourites, links, thumbnails.
pub fn load_clips(dir: &Path) -> Vec<ClipItem> {
    let lib = Library::load();
    let now = crate::share::now_secs();
    library::scan(dir)
        .into_iter()
        .map(|c| {
            let thumb = crate::save::thumb_for(&c.path);
            ClipItem {
                duration: rewindr::mp4::duration_secs(&c.path),
                favorite: lib.is_favorite(&c.path),
                link: lib.link(&c.path, now).cloned(),
                kind: if c.name.contains("(Discord") {
                    ClipKind::Discord
                } else if c.name.contains("(trim") {
                    ClipKind::Trim
                } else if c.name.contains("(vertical") {
                    ClipKind::Vertical
                } else if c.name.contains("(slow") {
                    ClipKind::Slow
                } else {
                    ClipKind::Clip
                },
                thumb: thumb.exists().then(|| image::Handle::from_path(thumb)),
                game: c.game(),
                size: c.size,
                modified: c.modified,
                name: c.name,
                path: c.path,
            }
        })
        .collect()
}

/// A small copy of a screenshot for its card, made once in the thumbnails folder: showing the
/// full picture (2560x1440) takes 15 MB of memory a card.
fn shot_thumb(path: &Path, modified: Option<SystemTime>) -> PathBuf {
    let Some(stem) = path.file_stem() else {
        return path.to_path_buf();
    };
    let thumb = crate::save::thumbs_dir().join(format!("shot-{}.jpg", stem.to_string_lossy()));
    let made = std::fs::metadata(&thumb).and_then(|m| m.modified()).ok();
    if made.is_some() && made >= modified {
        return thumb;
    }
    let small = ::image::open(path).map(|p| p.thumbnail(480, 480).to_rgb8());
    let saved = small.ok().is_some_and(|p| {
        let _ = std::fs::create_dir_all(crate::save::thumbs_dir());
        std::fs::File::create(&thumb).is_ok_and(|f| {
            let mut enc = ::image::codecs::jpeg::JpegEncoder::new_with_quality(
                std::io::BufWriter::new(f),
                85,
            );
            enc.encode_image(&p).is_ok()
        })
    });
    if saved { thumb } else { path.to_path_buf() }
}

/// A number that changes when a file in `dirs` is added, removed, renamed or rewritten, or when
/// `file` changes. Cheap enough to check every few seconds instead of reloading everything.
pub fn folder_signature(dirs: &[&Path], file: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for d in dirs {
        let mut v: Vec<(std::ffi::OsString, u64, Option<SystemTime>)> = std::fs::read_dir(d)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .filter_map(|e| {
                        let m = e.metadata().ok()?;
                        Some((e.file_name(), m.len(), m.modified().ok()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v.hash(&mut h);
    }
    std::fs::metadata(file)
        .ok()
        .map(|m| (m.len(), m.modified().ok()))
        .hash(&mut h);
    h.finish()
}

/// Reads the screenshots folder, newest first.
pub fn load_shots(dir: &Path) -> Vec<ShotItem> {
    let lib = Library::load();
    let now = crate::share::now_secs();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<ShotItem> = rd
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("jpg") || x.eq_ignore_ascii_case("png"))
        })
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let path = e.path();
            Some(ShotItem {
                name: path.file_stem()?.to_string_lossy().to_string(),
                size: meta.len(),
                modified: meta.modified().ok()?,
                picture: image::Handle::from_path(shot_thumb(&path, meta.modified().ok())),
                link: lib.link(&path, now).cloned(),
                path,
            })
        })
        .collect();
    v.sort_by_key(|s| std::cmp::Reverse(s.modified));
    v
}

impl App {
    pub(super) fn shown(&self) -> Vec<&ClipItem> {
        let q = self.search.trim().to_lowercase();
        let mut v: Vec<&ClipItem> = self
            .clips
            .iter()
            .filter(|c| match &self.filter {
                Filter::All => true,
                Filter::Favorites => c.favorite,
                Filter::Shared => c.link.is_some(),
                Filter::Game(g) => &c.game == g,
                Filter::Screenshots => false,
            })
            .filter(|c| {
                q.is_empty()
                    || c.name.to_lowercase().contains(&q)
                    || c.game.to_lowercase().contains(&q)
            })
            .collect();
        match self.sort {
            Sort::Newest => v.sort_by_key(|c| std::cmp::Reverse(c.modified)),
            Sort::Oldest => v.sort_by_key(|c| c.modified),
            Sort::Longest => v.sort_by(|a, b| {
                b.duration
                    .unwrap_or(0.0)
                    .total_cmp(&a.duration.unwrap_or(0.0))
            }),
            Sort::Largest => v.sort_by_key(|c| std::cmp::Reverse(c.size)),
        }
        v
    }

    pub(super) fn library_view(&self) -> Element<'_, Message> {
        if self.filter == Filter::Screenshots {
            return self.shots_view();
        }
        if self.loaded && self.clips.is_empty() {
            return self.empty();
        }
        let shown = self.shown();
        let total: u64 = shown.iter().map(|c| c.size).sum();
        let toolbar = row![
            text(match shown.len() {
                1 => "1 clip".to_string(),
                n => format!("{n} clips"),
            })
            .font(MEDIUM)
            .size(14)
            .color(TEXT_2),
            text(human_size(total)).size(14).color(MUTED),
            space::horizontal(),
            pick_list(Sort::ALL, Some(self.sort), Message::Sort)
                .text_size(13)
                .padding(Padding::from([7, 12]))
                .style(theme::pick)
                .menu_style(theme::menu),
            icon_button(Icon::Folder, "Folder", TEXT_2)
                .style(theme::secondary)
                .on_press(Message::OpenFolder),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let now = self.now.unwrap_or_else(SystemTime::now);
        let by_time = matches!(self.sort, Sort::Newest | Sort::Oldest);
        let mut groups: Vec<(String, Vec<&ClipItem>)> = Vec::new();
        for c in shown {
            let label = if by_time {
                day_label(c.modified, now)
            } else {
                String::new()
            };
            match groups.last_mut() {
                Some((l, v)) if *l == label => v.push(c),
                _ => groups.push((label, vec![c])),
            }
        }
        let mut body = column![].spacing(28);
        if groups.is_empty() && self.loaded {
            let what = match (&self.filter, self.search.trim()) {
                (_, q) if !q.is_empty() => format!("No clips match \"{q}\"."),
                (Filter::Shared, _) => "Clips you share as a link show up here.".to_string(),
                (Filter::Favorites, _) => {
                    "Star a clip to keep it here (and safe from clean-up).".to_string()
                }
                _ => "No clips here yet.".to_string(),
            };
            body = body.push(nothing(Icon::Search, what));
        }
        for (label, clips) in groups {
            let cards = clips.into_iter().map(|c| self.card(c, now));
            let g = grid(cards)
                .fluid(340)
                .spacing(18)
                .height(grid::Sizing::AspectRatio(1.3));
            body = body.push(if label.is_empty() {
                column![g]
            } else {
                column![text(label).font(SEMIBOLD).size(15).color(TEXT_2), g].spacing(12)
            });
        }
        page(toolbar.into(), body.into())
    }

    fn card<'a>(&'a self, c: &'a ClipItem, now: SystemTime) -> Element<'a, Message> {
        // While the pointer moves over the picture, it shows the clip at that point.
        let scrubbed = self
            .scrub
            .as_ref()
            .filter(|(p, _)| *p == c.path)
            .and_then(|(_, f)| {
                let frames = self.strips.get(&c.path)?;
                (!frames.is_empty()).then(|| {
                    frames[((*f * frames.len() as f32) as usize).min(frames.len() - 1)].clone()
                })
            });
        let picture: Element<'a, Message> = match scrubbed.or_else(|| c.thumb.clone()) {
            Some(h) => image(h)
                .width(Fill)
                .height(Fill)
                .content_fit(ContentFit::Cover)
                .border_radius(10.0)
                .into(),
            None => container(icon(Icon::Film, 28.0, MUTED))
                .center(Fill)
                .style(theme::well)
                .into(),
        };
        let mut badges = row![].spacing(5);
        let tag = |i: Icon, label: &'static str| {
            container(
                row![icon(i, 12.0, TEXT), text(label).size(11).font(SEMIBOLD)]
                    .spacing(5)
                    .align_y(Alignment::Center),
            )
            .padding(Padding::from([3, 7]))
            .style(theme::badge)
        };
        match c.kind {
            ClipKind::Clip => {}
            ClipKind::Trim => badges = badges.push(tag(Icon::Scissors, "Trimmed")),
            ClipKind::Discord => badges = badges.push(tag(Icon::Send, "Discord")),
            ClipKind::Vertical => badges = badges.push(tag(Icon::Phone, "Vertical")),
            ClipKind::Slow => badges = badges.push(tag(Icon::Snail, "Slow-mo")),
        }
        if c.link.is_some() {
            badges = badges.push(tag(Icon::Link, "Shared"));
        }
        let scrub_pos = self
            .scrub
            .as_ref()
            .filter(|(p, _)| *p == c.path)
            .map(|(_, f)| *f);
        let mut layers = stack![
            picture,
            Canvas::new(Scrubber {
                clip: c.path.clone(),
                at: scrub_pos
            })
            .width(Fill)
            .height(Fill),
            container(
                container(text(duration(c.duration)).size(12).font(SEMIBOLD))
                    .padding(Padding::from([2, 7]))
                    .style(theme::badge)
            )
            .width(Fill)
            .height(Fill)
            .align_x(Horizontal::Right)
            .align_y(Vertical::Bottom)
            .padding(8),
            container(badges)
                .width(Fill)
                .height(Fill)
                .align_y(Vertical::Bottom)
                .padding(8),
        ];
        if let Some(p) = self.upload_progress(&c.path) {
            layers = layers.push(uploading(p.0, p.1));
        }
        let star = if c.favorite {
            icon_filled(Icon::Star, 15.0, theme::GOLD)
        } else {
            icon(Icon::Star, 15.0, TEXT)
        };
        layers = layers.push(
            container(tip(
                button(star)
                    .padding(7)
                    .style(theme::overlay_icon)
                    .on_press(Message::Favorite(c.path.clone())),
                if c.favorite {
                    "Remove from favourites"
                } else {
                    "Favourite: never cleaned up"
                },
            ))
            .padding(8),
        );

        let meta = row![
            text(when(c.modified, now)).size(12).color(MUTED),
            text(human_size(c.size)).size(12).color(MUTED)
        ]
        .spacing(10);
        let content = column![
            container(layers).height(Fill).padding(Padding {
                top: 6.0,
                right: 6.0,
                bottom: 0.0,
                left: 6.0
            }),
            container(
                column![
                    container(
                        text(c.title())
                            .font(SEMIBOLD)
                            .size(14)
                            .wrapping(text::Wrapping::None)
                    )
                    .clip(true),
                    meta
                ]
                .spacing(3),
            )
            .padding(Padding {
                top: 10.0,
                right: 12.0,
                bottom: 12.0,
                left: 12.0
            }),
        ];
        let base = button(content)
            .padding(0)
            .width(Fill)
            .height(Fill)
            .style(theme::card)
            .on_press(Message::Open(c.path.clone()));
        hover(base, self.actions(&c.path, c.link.is_some(), true))
    }

    /// The buttons that appear over a card's corner when the pointer is on it.
    fn actions<'a>(&self, path: &Path, has_link: bool, renamable: bool) -> Element<'a, Message> {
        let action = |i: Icon, label: &'static str, m: Message| {
            tip(
                icon_only(i, TEXT).style(theme::overlay_icon).on_press(m),
                label,
            )
        };
        let p = path.to_path_buf();
        let mut r = row![if has_link {
            action(
                Icon::Link,
                "Copy the share link",
                Message::ShowLink(p.clone()),
            )
        } else {
            action(Icon::Share, "Get a share link", Message::Share(p.clone()))
        }]
        .spacing(4);
        r = r.push(action(Icon::Copy, "Copy file", Message::Copy(p.clone())));
        r = r.push(action(
            Icon::Folder,
            "Show in folder",
            Message::Reveal(p.clone()),
        ));
        if renamable {
            r = r.push(action(
                Icon::Pencil,
                "Rename",
                Message::AskRename(p.clone()),
            ));
        }
        r = r.push(action(Icon::Trash, "Delete", Message::AskDelete(p)));
        container(r)
            .width(Fill)
            .align_x(Horizontal::Right)
            .padding(14)
            .into()
    }

    fn shots_view(&self) -> Element<'_, Message> {
        let total: u64 = self.shots.iter().map(|s| s.size).sum();
        let key = self
            .report
            .as_ref()
            .and_then(|r| r.screenshot_hotkey.clone())
            .unwrap_or_else(|| self.settings.screenshot_hotkey.clone());
        let toolbar = row![
            text(match self.shots.len() {
                1 => "1 screenshot".to_string(),
                n => format!("{n} screenshots"),
            })
            .font(MEDIUM)
            .size(14)
            .color(TEXT_2),
            text(human_size(total)).size(14).color(MUTED),
            space::horizontal(),
            icon_button(Icon::Folder, "Folder", TEXT_2)
                .style(theme::secondary)
                .on_press(Message::OpenShotsFolder),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        let now = self.now.unwrap_or_else(SystemTime::now);
        let body: Element<'_, Message> = if self.shots.is_empty() {
            nothing(
                Icon::Camera,
                if key.is_empty() {
                    "Take screenshots with the camera button at the top.".to_string()
                } else {
                    format!("Press {key} in a game to save a screenshot of that moment.")
                },
            )
        } else {
            let cards = self.shots.iter().map(|s| {
                let mut layers = stack![
                    image(s.picture.clone())
                        .width(Fill)
                        .height(Fill)
                        .content_fit(ContentFit::Cover)
                        .border_radius(10.0)
                ];
                if s.link.is_some() {
                    layers = layers.push(
                        container(
                            container(
                                row![
                                    icon(Icon::Link, 12.0, TEXT),
                                    text("Shared").size(11).font(SEMIBOLD)
                                ]
                                .spacing(5),
                            )
                            .padding(Padding::from([3, 7]))
                            .style(theme::badge),
                        )
                        .width(Fill)
                        .height(Fill)
                        .align_y(Vertical::Bottom)
                        .padding(8),
                    );
                }
                if let Some(p) = self.upload_progress(&s.path) {
                    layers = layers.push(uploading(p.0, p.1));
                }
                let content = column![
                    container(layers).height(Fill).padding(Padding {
                        top: 6.0,
                        right: 6.0,
                        bottom: 0.0,
                        left: 6.0
                    }),
                    container(
                        column![
                            container(
                                text(s.name.clone())
                                    .font(SEMIBOLD)
                                    .size(14)
                                    .wrapping(text::Wrapping::None)
                            )
                            .clip(true),
                            row![
                                text(when(s.modified, now)).size(12).color(MUTED),
                                text(human_size(s.size)).size(12).color(MUTED)
                            ]
                            .spacing(10)
                        ]
                        .spacing(3)
                    )
                    .padding(Padding {
                        top: 10.0,
                        right: 12.0,
                        bottom: 12.0,
                        left: 12.0
                    }),
                ];
                let base = button(content)
                    .padding(0)
                    .width(Fill)
                    .height(Fill)
                    .style(theme::card)
                    .on_press(Message::OpenExternal(s.path.clone()));
                hover(base, self.actions(&s.path, s.link.is_some(), false))
            });
            grid(cards)
                .fluid(340)
                .spacing(18)
                .height(grid::Sizing::AspectRatio(1.3))
                .into()
        };
        page(toolbar.into(), body)
    }

    fn empty(&self) -> Element<'_, Message> {
        let key = self.hotkey();
        let mut tips = column![
            text("Your clips will show up here").font(BOLD).size(22),
            text(format!(
                "Play as usual. When something worth keeping happens, press {key} and Rewindr saves the last {}, sound included.",
                seconds_label(self.settings.clip_seconds)
            ))
            .size(14)
            .color(TEXT_2)
            .align_x(Horizontal::Center),
        ]
        .spacing(12)
        .align_x(Alignment::Center);
        if self.settings.voice_clip {
            tips = tips.push(
                row![
                    icon(Icon::Mic, 15.0, theme::ACCENT),
                    text(format!("Or just say \"{}\".", self.settings.voice_phrase))
                        .size(14)
                        .color(TEXT_2),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            );
        }
        container(
            column![
                container(text(key.clone()).font(BOLD).size(30))
                    .padding(Padding::from([16, 26]))
                    .style(|_| container::Style {
                        background: Some(theme::SURFACE.into()),
                        border: iced::Border {
                            color: theme::RAISED,
                            width: 1.0,
                            radius: 14.0.into()
                        },
                        shadow: iced::Shadow {
                            color: theme::alpha(theme::ACCENT, 0.25),
                            offset: iced::Vector::new(0.0, 6.0),
                            blur_radius: 30.0,
                        },
                        ..Default::default()
                    }),
                Space::new().height(6),
                tips,
            ]
            .spacing(12)
            .align_x(Alignment::Center)
            .max_width(440),
        )
        .center(Fill)
        .into()
    }
}

/// A toolbar over a scrolling body, with the page's margins.
fn page<'a>(toolbar: Element<'a, Message>, body: Element<'a, Message>) -> Element<'a, Message> {
    column![
        container(toolbar).padding(Padding {
            top: 0.0,
            right: 28.0,
            bottom: 16.0,
            left: 28.0
        }),
        scrollable(container(body).padding(Padding {
            top: 0.0,
            right: 28.0,
            bottom: 32.0,
            left: 28.0
        }))
        .height(Fill)
        .style(theme::scroll),
    ]
    .into()
}

fn nothing<'a>(i: Icon, what: String) -> Element<'a, Message> {
    container(
        column![icon(i, 26.0, MUTED), text(what).color(TEXT_2)]
            .spacing(10)
            .align_x(Alignment::Center),
    )
    .width(Fill)
    .padding(60)
    .align_x(Horizontal::Center)
    .into()
}

/// "Uploading 42%" over a card's picture.
fn uploading<'a>(label: String, p: f32) -> Element<'a, Message> {
    container(
        container(
            column![
                row![
                    icon(Icon::Share, 13.0, TEXT),
                    text(label).size(12).font(SEMIBOLD)
                ]
                .spacing(6)
                .align_y(Alignment::Center),
                iced::widget::progress_bar(0.0..=1.0, p)
                    .girth(4)
                    .style(theme::progress),
            ]
            .spacing(6)
            .width(150),
        )
        .padding(10)
        .style(theme::badge),
    )
    .center(Fill)
    .into()
}

/// A short label that appears when the pointer rests on `content`.
pub fn tip<'a, M: 'a>(content: impl Into<Element<'a, M>>, label: &'a str) -> Element<'a, M> {
    tooltip(
        content,
        container(text(label).size(12).font(MEDIUM))
            .padding(Padding::from([4, 8]))
            .style(theme::badge),
        tooltip::Position::Bottom,
    )
    .gap(6)
    .into()
}

/// Watches the pointer over a card's picture and reports where it is (0 to 1 across), so the
/// card can show the clip at that point. Draws a thin line for the position.
struct Scrubber {
    clip: PathBuf,
    at: Option<f32>,
}

impl canvas::Program<Message> for Scrubber {
    type State = bool;

    fn update(
        &self,
        inside: &mut bool,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let iced::Event::Mouse(mouse::Event::CursorMoved { .. } | mouse::Event::CursorLeft) = event
        else {
            return None;
        };
        match cursor.position_in(bounds) {
            Some(p) => {
                *inside = true;
                let f = (p.x / bounds.width.max(1.0)).clamp(0.0, 0.999);
                Some(Action::publish(Message::Scrub(self.clip.clone(), f)))
            }
            None if *inside => {
                *inside = false;
                Some(Action::publish(Message::ScrubEnd(self.clip.clone())))
            }
            None => None,
        }
    }

    fn draw(
        &self,
        _: &bool,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let Some(f) = self.at else { return Vec::new() };
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(
            Point::new(10.0, bounds.height - 3.0),
            Size::new(bounds.width - 20.0, 3.0),
            Color {
                a: 0.35,
                ..Color::BLACK
            },
        );
        frame.fill_rectangle(
            Point::new(10.0, bounds.height - 3.0),
            Size::new((bounds.width - 20.0) * f, 3.0),
            theme::ACCENT,
        );
        vec![frame.into_geometry()]
    }
}
