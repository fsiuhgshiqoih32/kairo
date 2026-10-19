//! "Your stats": the library counted. Clips, the time and space they take, top games, the last
//! two weeks and the streak of days with a clip. Sessions are left out: they're hours long.

use std::collections::HashMap;
use std::time::SystemTime;

use chrono::{DateTime, Local, NaiveDate};
use iced::widget::{Space, column, container, row, text};
use iced::{Alignment, Element, Fill, FillPortion, Padding};

use super::icons::{Icon, icon};
use super::library::{ClipItem, ClipKind, nothing, page};
use super::theme::{self, BOLD, MEDIUM, SEMIBOLD};
use super::{App, Message, human_size};

/// How many days the chart shows, today included.
const DAYS: usize = 14;

#[derive(Debug, Default, PartialEq)]
pub struct Stats {
    pub clips: usize,
    pub seconds: f64,
    pub bytes: u64,
    pub favorites: usize,
    pub shared: usize,
    pub marks: usize,
    /// Each game with its clips and seconds, most clips first.
    pub games: Vec<(String, usize, f64)>,
    /// Clips saved on each of the last `days` days, oldest first, today last.
    pub days: Vec<usize>,
    /// Days in a row with a clip, up to today (or yesterday, before today's first clip).
    pub streak: usize,
    /// The day with the most clips.
    pub best_day: Option<(NaiveDate, usize)>,
    /// The most days in a row with a clip, ever.
    pub longest_streak: usize,
    /// Recorded sessions (counted only here).
    pub sessions: usize,
    /// Trims and other exported edits.
    pub edits: usize,
    /// Clips with a tag or in a folder.
    pub sorted: usize,
}

pub fn summarize(clips: &[ClipItem], now: SystemTime, days: usize) -> Stats {
    let date = |t: SystemTime| DateTime::<Local>::from(t).date_naive();
    let today = date(now);
    let mut st = Stats {
        days: vec![0; days],
        ..Default::default()
    };
    let mut games: HashMap<&str, (usize, f64)> = HashMap::new();
    let mut per_day: HashMap<NaiveDate, usize> = HashMap::new();
    for c in clips {
        if c.kind == ClipKind::Session {
            st.sessions += 1;
            continue;
        }
        st.edits += usize::from(c.kind != ClipKind::Clip);
        st.sorted += usize::from(!c.tags.is_empty() || c.folder.is_some());
        let secs = c.duration.unwrap_or(0.0).max(0.0);
        st.clips += 1;
        st.seconds += secs;
        st.bytes += c.size;
        st.favorites += usize::from(c.favorite);
        st.shared += usize::from(c.link.is_some());
        st.marks += c.marks;
        let g = games.entry(c.game.as_str()).or_default();
        g.0 += 1;
        g.1 += secs;
        let d = date(c.modified);
        *per_day.entry(d).or_default() += 1;
        let ago = (today - d).num_days();
        if (0..days as i64).contains(&ago) {
            st.days[days - 1 - ago as usize] += 1;
        }
    }
    st.games = games
        .into_iter()
        .map(|(g, (n, s))| (g.to_string(), n, s))
        .collect();
    st.games
        .sort_by(|a, b| b.1.cmp(&a.1).then(b.2.total_cmp(&a.2)).then(a.0.cmp(&b.0)));
    let mut day = if per_day.contains_key(&today) {
        today
    } else {
        today.pred_opt().unwrap_or(today)
    };
    while per_day.contains_key(&day) {
        st.streak += 1;
        match day.pred_opt() {
            Some(d) => day = d,
            None => break,
        }
    }
    let mut dates: Vec<NaiveDate> = per_day.keys().copied().collect();
    dates.sort_unstable();
    let mut run = 0;
    for (i, d) in dates.iter().enumerate() {
        run = match i.checked_sub(1).map(|j| dates[j]) {
            Some(prev) if prev.succ_opt() == Some(*d) => run + 1,
            _ => 1,
        };
        st.longest_streak = st.longest_streak.max(run);
    }
    st.best_day = per_day
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)));
    st
}

/// "3 h 05 min", "12 min", "40 s".
pub fn play_time(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{} h {:02} min", s / 3600, s % 3600 / 60)
    } else if s >= 60 {
        format!("{} min", s / 60)
    } else {
        format!("{s} s")
    }
}

fn tile<'a>(value: String, label: &'a str) -> Element<'a, Message> {
    container(
        column![
            text(value).font(BOLD).size(28).color(theme::ink()),
            text(label).size(13).color(theme::muted()),
        ]
        .spacing(2),
    )
    .padding(Padding::from([14, 16]))
    .width(Fill)
    .style(theme::well)
    .into()
}

fn bar<'a>(height: f32) -> Element<'a, Message> {
    container(Space::new().width(Fill).height(height))
        .style(|_| container::Style {
            background: Some(theme::accent().into()),
            border: iced::Border {
                radius: 3.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

/// Shares of a row for a bar `n` out of `max` long (FillPortion takes small whole numbers).
fn portions(n: usize, max: usize) -> (u16, u16) {
    let filled = ((n as f64 / max.max(1) as f64) * 1000.0)
        .round()
        .clamp(1.0, 1000.0) as u16;
    (filled, 1000 - filled.min(1000))
}

fn heading<'a>(s: &'a str) -> Element<'a, Message> {
    text(s).font(SEMIBOLD).size(15).color(theme::ink()).into()
}

impl App {
    pub(super) fn stats_view(&self) -> Element<'_, Message> {
        let now = self.now.unwrap_or_else(SystemTime::now);
        let st = summarize(&self.clips, now, DAYS);
        let toolbar = row![
            text("What you've kept so far")
                .font(MEDIUM)
                .size(14)
                .color(theme::ink_2())
        ]
        .align_y(Alignment::Center);
        if st.clips == 0 {
            return page(
                toolbar.into(),
                nothing(
                    Icon::Sparkles,
                    "Your stats start with your first clip.".into(),
                ),
            );
        }

        let tiles = row![
            tile(
                st.clips.to_string(),
                if st.clips == 1 { "clip" } else { "clips" }
            ),
            tile(play_time(st.seconds), "of play kept"),
            tile(
                st.games.len().to_string(),
                if st.games.len() == 1 { "game" } else { "games" }
            ),
            tile(human_size(st.bytes), "on disk"),
        ]
        .spacing(12);

        let streak_text = match st.streak {
            0 => "No streak yet. Save a clip today to start one.".to_string(),
            1 => "1 day with a clip. Come back tomorrow to keep it going.".to_string(),
            n => format!("{n} days in a row with a clip."),
        };
        let streak = container(
            row![
                icon(Icon::Sparkles, 22.0, theme::accent()),
                column![
                    text(match st.streak {
                        1 => "1-day streak".to_string(),
                        n => format!("{n}-day streak"),
                    })
                    .font(BOLD)
                    .size(18),
                    text(streak_text).size(13).color(theme::ink_2()),
                ]
                .spacing(2)
                .width(Fill),
                column![
                    text(match &st.best_day {
                        Some((d, n)) => format!("Best day: {n} on {}", d.format("%b %-d")),
                        None => String::new(),
                    })
                    .size(13)
                    .color(theme::ink_2()),
                    text(format!(
                        "{} favourites · {} shared · {} moments marked",
                        st.favorites, st.shared, st.marks
                    ))
                    .size(13)
                    .color(theme::muted()),
                ]
                .spacing(2)
                .align_x(Alignment::End),
            ]
            .spacing(14)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([14, 16]))
        .width(Fill)
        .style(theme::well);

        let top = st.games.first().map_or(1, |g| g.1);
        let mut games = column![heading("Top games")].spacing(10);
        for (name, n, secs) in st.games.iter().take(6) {
            let (on, off) = portions(*n, top);
            games = games.push(
                row![
                    container(text(name.clone()).size(13).font(MEDIUM))
                        .width(150)
                        .clip(true),
                    row![
                        container(bar(10.0)).width(FillPortion(on)),
                        Space::new().width(FillPortion(off.max(1))),
                    ]
                    .width(Fill),
                    container(
                        text(format!(
                            "{n} {} · {}",
                            if *n == 1 { "clip" } else { "clips" },
                            play_time(*secs)
                        ))
                        .size(12)
                        .color(theme::muted())
                    )
                    .width(130)
                    .align_x(Alignment::End),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            );
        }

        let most = st.days.iter().copied().max().unwrap_or(0).max(1);
        let today = DateTime::<Local>::from(now).date_naive();
        let mut chart = row![].spacing(6).align_y(Alignment::End).height(130);
        for (i, n) in st.days.iter().enumerate() {
            let day = today
                .checked_sub_days(chrono::Days::new((DAYS - 1 - i) as u64))
                .unwrap_or(today);
            let h = if *n == 0 {
                3.0
            } else {
                8.0 + 92.0 * (*n as f32 / most as f32)
            };
            let column = column![
                text(if *n > 0 { n.to_string() } else { String::new() })
                    .size(11)
                    .color(theme::ink_2()),
                if *n == 0 {
                    container(Space::new().width(Fill).height(h))
                        .style(theme::well)
                        .into()
                } else {
                    bar(h)
                },
                text(day.format("%-d").to_string())
                    .size(11)
                    .color(theme::muted()),
            ]
            .spacing(4)
            .align_x(Alignment::Center)
            .width(Fill);
            chart = chart.push(column);
        }
        let days = column![heading("The last two weeks"), chart].spacing(12);

        let body = column![
            tiles,
            streak,
            container(games).padding(16).width(Fill).style(theme::well),
            container(days).padding(16).width(Fill).style(theme::well),
            container(super::achievements::panel(&st, &self.earned))
                .padding(16)
                .width(Fill)
                .style(theme::well),
        ]
        .spacing(16)
        .max_width(980);
        page(toolbar.into(), body.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn clip(game: &str, days_ago: u64, secs: f64, kind: ClipKind, now: SystemTime) -> ClipItem {
        ClipItem {
            path: PathBuf::from(format!("{game}-{days_ago}.mp4")),
            name: game.to_string(),
            game: game.to_string(),
            size: 1_000_000,
            modified: now - Duration::from_secs(days_ago * 86_400),
            duration: Some(secs),
            favorite: days_ago == 0,
            kind,
            thumb: None,
            link: None,
            marks: 1,
            tags: Vec::new(),
            folder: None,
        }
    }

    #[test]
    fn counts_the_library() {
        // Midday, so "days ago" never crosses midnight in any time zone's arithmetic.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_760_000_000);
        let clips = vec![
            clip("Red Eclipse", 0, 30.0, ClipKind::Clip, now),
            clip("Red Eclipse", 1, 30.0, ClipKind::Clip, now),
            clip("SuperTuxKart", 1, 15.0, ClipKind::Trim, now),
            clip("Red Eclipse", 2, 30.0, ClipKind::Clip, now),
            clip("SuperTuxKart", 5, 20.0, ClipKind::Clip, now),
            clip("Desktop", 0, 3600.0, ClipKind::Session, now),
        ];
        let st = summarize(&clips, now, 7);
        assert_eq!(st.clips, 5, "sessions are left out");
        assert_eq!(st.seconds, 125.0);
        assert_eq!(st.bytes, 5_000_000);
        assert_eq!(st.favorites, 1);
        assert_eq!(st.marks, 5);
        assert_eq!(st.games[0].0, "Red Eclipse");
        assert_eq!(st.games[0].1, 3);
        assert_eq!(st.games[1], ("SuperTuxKart".to_string(), 2, 35.0));
        assert_eq!(st.days, vec![0, 1, 0, 0, 1, 2, 1]);
        assert_eq!(st.streak, 3);
        assert_eq!(st.longest_streak, 3);
        assert_eq!(st.best_day.map(|(_, n)| n), Some(2));
        assert_eq!((st.sessions, st.edits, st.sorted), (1, 1, 0));
    }

    #[test]
    fn a_streak_survives_until_today_has_a_clip() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_760_000_000);
        let clips = vec![
            clip("A", 1, 10.0, ClipKind::Clip, now),
            clip("A", 2, 10.0, ClipKind::Clip, now),
            clip("A", 4, 10.0, ClipKind::Clip, now),
        ];
        assert_eq!(summarize(&clips, now, 14).streak, 2);
        let mut older = clips.clone();
        for d in 6..10 {
            older.push(clip("A", d, 10.0, ClipKind::Clip, now));
        }
        let st = summarize(&older, now, 14);
        assert_eq!((st.streak, st.longest_streak), (2, 4));
        assert_eq!(
            summarize(&[], now, 14),
            Stats {
                days: vec![0; 14],
                ..Default::default()
            }
        );
    }

    #[test]
    fn writes_play_time() {
        assert_eq!(play_time(40.2), "40 s");
        assert_eq!(play_time(754.0), "12 min");
        assert_eq!(play_time(3600.0 * 3.0 + 300.0), "3 h 05 min");
        assert_eq!(portions(5, 10), (500, 500));
        assert_eq!(portions(0, 10), (1, 999));
    }
}
