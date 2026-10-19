//! The clip library: cards grouped by day, filters, search, and the actions on a clip.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{
    Space, button, column, container, grid, hover, image, pick_list, row, scrollable, space, stack,
    text, tooltip,
};
use iced::{Alignment, ContentFit, Element, Fill, Padding};

use rewindr::library::{self, Library};

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipKind {
    Clip,
    Trim,
    Discord,
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

/// Reads the clips folder: names, games, lengths, favourites, thumbnails.
pub fn load_clips(dir: &Path) -> Vec<ClipItem> {
    let lib = Library::load();
    library::scan(dir)
        .into_iter()
        .map(|c| {
            let thumb = crate::save::thumb_for(&c.path);
            ClipItem {
                duration: rewindr::mp4::duration_secs(&c.path),
                favorite: lib.is_favorite(&c.path),
                kind: if c.name.contains("(Discord") {
                    ClipKind::Discord
                } else if c.name.contains("(trim") {
                    ClipKind::Trim
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

impl App {
    pub(super) fn shown(&self) -> Vec<&ClipItem> {
        let q = self.search.trim().to_lowercase();
        let mut v: Vec<&ClipItem> = self
            .clips
            .iter()
            .filter(|c| match &self.filter {
                Filter::All => true,
                Filter::Favorites => c.favorite,
                Filter::Game(g) => &c.game == g,
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
            body = body.push(
                container(
                    column![
                        icon(Icon::Search, 26.0, MUTED),
                        text(if self.search.trim().is_empty() {
                            "No clips here yet.".to_string()
                        } else {
                            format!("No clips match \"{}\".", self.search.trim())
                        })
                        .color(TEXT_2),
                    ]
                    .spacing(10)
                    .align_x(Alignment::Center),
                )
                .width(Fill)
                .padding(60)
                .align_x(Horizontal::Center),
            );
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

    fn card<'a>(&'a self, c: &'a ClipItem, now: SystemTime) -> Element<'a, Message> {
        let picture: Element<'a, Message> = match &c.thumb {
            Some(h) => image(h.clone())
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
        let mut layers = stack![
            picture,
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
        ];
        if c.kind != ClipKind::Clip {
            let (label, i) = match c.kind {
                ClipKind::Trim => ("Trimmed", Icon::Scissors),
                _ => ("Discord", Icon::Send),
            };
            layers = layers.push(
                container(
                    container(
                        row![icon(i, 12.0, TEXT), text(label).size(11).font(SEMIBOLD)]
                            .spacing(5)
                            .align_y(Alignment::Center),
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

        let action = |i: Icon, label: &'static str, m: Message| {
            tip(
                icon_only(i, TEXT).style(theme::overlay_icon).on_press(m),
                label,
            )
        };
        let actions = container(
            row![
                action(Icon::Copy, "Copy file", Message::Copy(c.path.clone())),
                action(
                    Icon::Folder,
                    "Show in folder",
                    Message::Reveal(c.path.clone())
                ),
                action(Icon::Pencil, "Rename", Message::AskRename(c.path.clone())),
                action(Icon::Trash, "Delete", Message::AskDelete(c.path.clone())),
            ]
            .spacing(4),
        )
        .width(Fill)
        .align_x(Horizontal::Right)
        .padding(14);
        hover(base, actions)
    }

    fn empty(&self) -> Element<'_, Message> {
        let key = self.hotkey();
        container(
            column![
                container(text(key.clone()).font(BOLD).size(30))
                    .padding(Padding::from([16, 26]))
                    .style(|_| container::Style {
                        background: Some(theme::SURFACE.into()),
                        border: iced::Border { color: theme::RAISED, width: 1.0, radius: 14.0.into() },
                        shadow: iced::Shadow {
                            color: theme::alpha(theme::ACCENT, 0.25),
                            offset: iced::Vector::new(0.0, 6.0),
                            blur_radius: 30.0,
                        },
                        ..Default::default()
                    }),
                Space::new().height(6),
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
            .align_x(Alignment::Center)
            .max_width(440),
        )
        .center(Fill)
        .into()
    }
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
