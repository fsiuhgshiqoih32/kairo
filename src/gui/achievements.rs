//! Achievements: badges for milestones in the library. Progress is counted from the clips, and
//! an earned badge is kept with its date in achievements.json, so deleting clips never takes it
//! back.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local};
use iced::widget::{Space, column, container, grid, row, text};
use iced::{Alignment, Element, Fill, FillPortion, Length, Padding};

use super::Message;
use super::icons::{Icon, icon};
use super::stats::Stats;
use super::theme::{self, SEMIBOLD};

pub struct Badge {
    pub id: &'static str,
    pub name: &'static str,
    pub about: &'static str,
    icon: Icon,
    pub goal: u64,
    /// What the progress counts, after the numbers ("min").
    unit: &'static str,
    progress: fn(&Stats) -> u64,
}

impl Badge {
    pub fn progress(&self, st: &Stats) -> u64 {
        (self.progress)(st).min(self.goal)
    }
}

pub const BADGES: [Badge; 15] = [
    Badge {
        id: "first-clip",
        name: "First clip",
        about: "Save your first clip.",
        icon: Icon::Record,
        goal: 1,
        unit: "",
        progress: |s| s.clips as u64,
    },
    Badge {
        id: "clips-10",
        name: "Warming up",
        about: "Keep 10 clips.",
        icon: Icon::Clips,
        goal: 10,
        unit: "",
        progress: |s| s.clips as u64,
    },
    Badge {
        id: "clips-100",
        name: "Collector",
        about: "Keep 100 clips.",
        icon: Icon::Layers,
        goal: 100,
        unit: "",
        progress: |s| s.clips as u64,
    },
    Badge {
        id: "clips-500",
        name: "Archivist",
        about: "Keep 500 clips.",
        icon: Icon::Trophy,
        goal: 500,
        unit: "",
        progress: |s| s.clips as u64,
    },
    Badge {
        id: "streak-3",
        name: "Three in a row",
        about: "Save clips on 3 days in a row.",
        icon: Icon::Flame,
        goal: 3,
        unit: "days",
        progress: |s| s.longest_streak as u64,
    },
    Badge {
        id: "streak-7",
        name: "Week streak",
        about: "Save clips on 7 days in a row.",
        icon: Icon::Flame,
        goal: 7,
        unit: "days",
        progress: |s| s.longest_streak as u64,
    },
    Badge {
        id: "streak-30",
        name: "Month streak",
        about: "Save clips on 30 days in a row.",
        icon: Icon::Flame,
        goal: 30,
        unit: "days",
        progress: |s| s.longest_streak as u64,
    },
    Badge {
        id: "big-day",
        name: "Big day",
        about: "Save 10 clips in one day.",
        icon: Icon::Sparkles,
        goal: 10,
        unit: "",
        progress: |s| s.best_day.map_or(0, |(_, n)| n as u64),
    },
    Badge {
        id: "hour",
        name: "An hour of highlights",
        about: "Keep an hour of clips.",
        icon: Icon::Clock,
        goal: 60,
        unit: "min",
        progress: |s| (s.seconds / 60.0) as u64,
    },
    Badge {
        id: "games-5",
        name: "Variety",
        about: "Keep clips from 5 different games.",
        icon: Icon::Gamepad,
        goal: 5,
        unit: "",
        progress: |s| s.games.len() as u64,
    },
    Badge {
        id: "favorites-10",
        name: "Favourites",
        about: "Star 10 clips.",
        icon: Icon::Star,
        goal: 10,
        unit: "",
        progress: |s| s.favorites as u64,
    },
    Badge {
        id: "shared",
        name: "Shared",
        about: "Share a clip with a link.",
        icon: Icon::Share,
        goal: 1,
        unit: "",
        progress: |s| s.shared as u64,
    },
    Badge {
        id: "edited",
        name: "Editor",
        about: "Export a trim or an edit.",
        icon: Icon::Scissors,
        goal: 1,
        unit: "",
        progress: |s| s.edits as u64,
    },
    Badge {
        id: "sorted",
        name: "Tidy",
        about: "Tag 5 clips or put them in folders.",
        icon: Icon::Tag,
        goal: 5,
        unit: "",
        progress: |s| s.sorted as u64,
    },
    Badge {
        id: "session",
        name: "The long game",
        about: "Record a whole session.",
        icon: Icon::Film,
        goal: 1,
        unit: "",
        progress: |s| s.sessions as u64,
    },
];

/// Each earned badge's id with when it was earned (seconds since 1970).
pub type Earned = BTreeMap<String, u64>;

fn path() -> PathBuf {
    kairo::config::config_dir().join("achievements.json")
}

pub fn load() -> Earned {
    std::fs::read(path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(earned: &Earned) {
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(earned) {
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// Marks the badges `st` reaches as earned at `now`, and returns the names of the new ones.
pub fn award(earned: &mut Earned, st: &Stats, now: SystemTime) -> Vec<&'static str> {
    let at = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut new = Vec::new();
    for b in &BADGES {
        if !earned.contains_key(b.id) && b.progress(st) >= b.goal {
            earned.insert(b.id.to_string(), at);
            new.push(b.name);
        }
    }
    new
}

fn tile<'a>(b: &'a Badge, st: &Stats, earned_at: Option<u64>) -> Element<'a, Message> {
    let got = earned_at.is_some();
    let badge = container(icon(
        if got { b.icon } else { Icon::Lock },
        18.0,
        if got {
            theme::on_accent()
        } else {
            theme::muted()
        },
    ))
    .center(Length::Fixed(40.0))
    .style(move |_| container::Style {
        background: Some(
            if got {
                theme::accent()
            } else {
                theme::raised()
            }
            .into(),
        ),
        border: iced::Border {
            radius: 20.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });
    let status: Element<'a, Message> = match earned_at {
        Some(at) => {
            let day = DateTime::<Local>::from(UNIX_EPOCH + Duration::from_secs(at));
            text(format!("Earned {}", day.format("%b %-d, %Y")))
                .size(12)
                .color(theme::accent())
                .into()
        }
        None => {
            let done = b.progress(st);
            let (on, off) = (
                (done * 100 / b.goal.max(1)).clamp(1, 100) as u16,
                (100 - (done * 100 / b.goal.max(1)).min(100)) as u16,
            );
            let mut track = row![].width(Fill).height(4);
            if done > 0 {
                track = track.push(
                    container(Space::new().width(Fill).height(4))
                        .width(FillPortion(on))
                        .style(|_| container::Style {
                            background: Some(theme::accent().into()),
                            border: iced::Border {
                                radius: 2.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                );
            }
            track = track.push(Space::new().width(FillPortion(off.max(1))));
            let count = if b.unit.is_empty() {
                format!("{done} / {}", b.goal)
            } else {
                format!("{done} / {} {}", b.goal, b.unit)
            };
            row![
                container(track).width(Fill).style(|_| container::Style {
                    background: Some(theme::raised().into()),
                    border: iced::Border {
                        radius: 2.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                text(count).size(12).color(theme::muted()),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .into()
        }
    };
    container(
        row![
            badge,
            column![
                text(b.name).font(SEMIBOLD).size(14).color(if got {
                    theme::ink()
                } else {
                    theme::ink_2()
                }),
                text(b.about).size(12).color(theme::muted()),
                status,
            ]
            .spacing(3)
            .width(Fill),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([12, 14]))
    .width(Fill)
    .height(96)
    .align_y(Alignment::Center)
    .style(move |_| container::Style {
        background: Some(theme::surface().into()),
        border: iced::Border {
            color: if got {
                theme::alpha(theme::accent(), 0.45)
            } else {
                theme::edge()
            },
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    })
    .into()
}

/// The stats page's achievements: earned ones first, then the closest to being earned.
pub fn panel<'a>(st: &Stats, earned: &Earned) -> Element<'a, Message> {
    let mut order: Vec<&Badge> = BADGES.iter().collect();
    order.sort_by_key(|b| match earned.get(b.id) {
        Some(at) => (0, u64::MAX - at, 0),
        None => (1, 0, 1000 - b.progress(st) * 1000 / b.goal.max(1)),
    });
    let got = BADGES.iter().filter(|b| earned.contains_key(b.id)).count();
    let tiles = grid(
        order
            .into_iter()
            .map(|b| tile(b, st, earned.get(b.id).copied())),
    )
    .fluid(340)
    .spacing(10)
    .height(grid::Sizing::EvenlyDistribute(Length::Shrink));
    column![
        row![
            text("Achievements")
                .font(SEMIBOLD)
                .size(15)
                .color(theme::ink())
                .width(Fill),
            text(format!("{got} of {}", BADGES.len()))
                .size(13)
                .color(theme::muted()),
        ]
        .align_y(Alignment::Center),
        tiles,
    ]
    .spacing(12)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badges_are_earned_once_and_kept() {
        let now = UNIX_EPOCH + Duration::from_secs(1_760_000_000);
        let mut st = Stats {
            clips: 12,
            shared: 1,
            longest_streak: 3,
            ..Default::default()
        };
        let mut earned = Earned::new();
        let new = award(&mut earned, &st, now);
        assert_eq!(
            new,
            vec!["First clip", "Warming up", "Three in a row", "Shared"]
        );
        assert_eq!(earned.get("clips-10"), Some(&1_760_000_000));

        // Deleting clips doesn't take a badge back, and earned ones aren't announced again.
        st.clips = 0;
        assert!(award(&mut earned, &st, now + Duration::from_secs(60)).is_empty());
        assert_eq!(earned.len(), 4);
    }

    #[test]
    fn badge_ids_are_unique_and_goals_reachable() {
        let mut ids: Vec<_> = BADGES.iter().map(|b| b.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), BADGES.len());
        assert!(BADGES.iter().all(|b| b.goal > 0));
        let st = Stats {
            seconds: 90.0 * 60.0,
            ..Default::default()
        };
        let hour = BADGES.iter().find(|b| b.id == "hour").expect("hour badge");
        assert_eq!(hour.progress(&st), 60, "progress stops at the goal");
    }
}
