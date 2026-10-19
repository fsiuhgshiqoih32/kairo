//! Renders the windows to PNG files without opening them, to check how they look:
//!
//! `REWINDR_HOME=<folder with Clips/ and thumbs/> REWINDR_SNAPSHOTS=<out dir> cargo test -- --ignored gui::preview`

use std::borrow::Cow;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use iced::{Element, Theme};
use iced_test::Simulator;

use super::{App, ClipItem, Filter, Page, library::ClipKind, theme, toast};
use crate::daemon::Report;
use crate::recorder::Status;

fn out(name: &str) -> PathBuf {
    let dir = PathBuf::from(std::env::var("REWINDR_SNAPSHOTS").expect("set REWINDR_SNAPSHOTS"));
    std::fs::create_dir_all(&dir).unwrap();
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        if e.file_name()
            .to_string_lossy()
            .starts_with(&format!("{name}-"))
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
    dir.join(format!("{name}.png"))
}

fn settings() -> iced::Settings {
    iced::Settings {
        fonts: theme::FONTS.iter().map(|f| Cow::Borrowed(*f)).collect(),
        default_font: theme::FONT,
        ..Default::default()
    }
}

/// Sample cards: the fixture's thumbnails under several game names, so the layout shows its range.
fn sample(now: SystemTime) -> Vec<ClipItem> {
    let real = super::load_clips(&rewindr::config::default_save_dir());
    assert!(!real.is_empty(), "put clips in REWINDR_HOME/Clips");
    let games = [
        ("Minecraft", ClipKind::Clip, 30.3, 3, false),
        ("VALORANT", ClipKind::Clip, 30.1, 21, true),
        ("Minecraft", ClipKind::Trim, 12.5, 48, false),
        ("Rocket League", ClipKind::Clip, 30.0, 95, true),
        ("Minecraft", ClipKind::Discord, 12.5, 180, false),
        ("Apex Legends", ClipKind::Clip, 30.0, 60 * 26, false),
        ("VALORANT", ClipKind::Clip, 17.0, 60 * 27, false),
    ];
    games
        .iter()
        .enumerate()
        .map(|(i, (game, kind, secs, age_min, fav))| {
            let src = &real[i % real.len()];
            ClipItem {
                path: if i == 0 {
                    src.path.clone()
                } else {
                    PathBuf::from(format!("sample-{i}.mp4"))
                },
                name: if i == 3 {
                    "Ceiling shot double tap".into()
                } else {
                    format!("{game} 2026-10-18 1{i}-00-00")
                },
                game: game.to_string(),
                size: (*secs * 2.1e6) as u64,
                modified: now - Duration::from_secs(*age_min * 60),
                duration: Some(*secs),
                favorite: *fav,
                kind: *kind,
                thumb: src.thumb.clone(),
                marks: [2, 0, 1, 0, 0, 0, 0][i],
                link: (i == 3).then(|| rewindr::library::Link {
                    url: "https://files.catbox.moe/k3x9qa.mp4".into(),
                    created: 0,
                    expires: None,
                }),
            }
        })
        .collect()
}

fn report() -> Report {
    Report {
        status: Status {
            recording: true,
            encoder: Some("NVIDIA (NVENC)".into()),
            source: Some("Windows Graphics Capture".into()),
            size: Some((2560, 1440)),
            fps: 60,
            buffer_seconds: 24.0,
            buffer_mb: 52.0,
            desktop_audio: Some("Speakers".into()),
            microphone: Some("Shure MV7+".into()),
            ..Default::default()
        },
        hotkey: Some("F8".into()),
        updated_ms: 0,
        ..Default::default()
    }
}

fn shoot<'a, M: 'a>(view: impl Into<Element<'a, M>>, name: &str, size: (f32, f32)) {
    shoot_at(view, name, size, None);
}

/// With the pointer resting at `at`, to show hover states.
fn shoot_at<'a, M: 'a>(
    view: impl Into<Element<'a, M>>,
    name: &str,
    size: (f32, f32),
    at: Option<(f32, f32)>,
) {
    let mut ui = Simulator::with_size(settings(), size, view);
    if let Some(p) = at {
        ui.point_at(iced::Point::new(p.0, p.1));
    }
    // Pictures load on a worker thread: draw once to start them, then again once they're in.
    let _ = ui.snapshot(&theme::theme());
    std::thread::sleep(Duration::from_millis(700));
    let snap = ui.snapshot(&theme::theme()).expect("snapshot");
    snap.matches_image(out(name)).expect("write png");
}

#[test]
#[ignore = "renders the windows to PNGs; needs REWINDR_HOME with clips and REWINDR_SNAPSHOTS"]
fn preview() {
    let size = (1280.0, 800.0);
    let now = SystemTime::now();
    let (mut app, _) = App::new(rewindr::config::Settings::default());
    app.now = Some(now);
    app.clips = sample(now);
    app.loaded = true;
    app.report = Some(report());
    // The banner's big picture, made the way the window makes it.
    let first = app.clips[0].path.clone();
    app.hero = super::library::hero_picture(&first, 6.0).map(|h| (first, h));
    shoot(app.view(), "iced-library", size);
    app.save_menu = true;
    shoot(app.view(), "iced-save-menu", size);
    app.save_menu = false;
    app.selecting = true;
    app.selected = vec![app.clips[2].path.clone(), app.clips[0].path.clone()];
    shoot(app.view(), "iced-select", size);
    app.selecting = false;
    app.selected.clear();
    theme::set_accent("purple");
    shoot_at(
        app.view(),
        "iced-library-purple",
        size,
        Some((560.0, 560.0)),
    );
    theme::set_accent("gold");
    shoot(app.view(), "iced-library-gold", size);
    theme::set_accent("red");
    shoot_at(app.view(), "iced-library-hover", size, Some((560.0, 230.0)));
    app.dialog = Some(super::Dialog::Delete(vec![app.clips[3].path.clone()]));
    shoot(app.view(), "iced-delete", size);
    app.dialog = None;

    app.filter = Filter::Game("VALORANT".into());
    shoot(app.view(), "iced-filtered", size);
    app.filter = Filter::All;

    // Shared links, the link dialog, screenshots.
    app.filter = Filter::Shared;
    shoot(app.view(), "iced-shared", size);
    app.filter = Filter::All;
    let link = app.clips[3].link.clone().expect("sample link");
    let _ = app.show_link(&link);
    shoot(app.view(), "iced-link", size);
    app.dialog = None;
    app.shots = app
        .clips
        .iter()
        .take(5)
        .enumerate()
        .map(|(i, c)| super::ShotItem {
            path: PathBuf::from(format!("shot-{i}.jpg")),
            name: format!("{} 2026-10-18 2{i}-14-0{i}", c.game),
            size: 900_000 + i as u64 * 40_000,
            modified: c.modified,
            picture: c.thumb.clone().expect("thumb"),
            link: None,
        })
        .collect();
    app.filter = Filter::Screenshots;
    shoot(app.view(), "iced-screenshots", size);
    app.filter = Filter::All;

    app.page = Page::Settings;
    shoot(app.view(), "iced-settings", size);
    app.settings.voice_clip = true;
    shoot(app.view(), "iced-settings-tall", (1280.0, 2400.0));
    app.settings.voice_clip = false;

    // The editor, on the first real clip, with a trim.
    let clip = app.clips[0].path.clone();
    let _ = app.open_editor(clip);
    let ed = app.editor.as_mut().expect("editor opened");
    ed.settle((2.5, 9.0), 5.5);
    ed.show_marks(vec![3.4, 7.1, 11.0]);
    shoot(app.view(), "iced-editor", size);

    app.editor = None;
    app.page = Page::Library;
    app.clips.clear();
    shoot(app.view(), "iced-empty", size);

    let t = toast::Toast::new(toast::Message::Saved {
        clip: sample(now)[0].path.clone(),
        seconds: 30.3,
    });
    shoot(t.view(), "iced-toast", (380.0, 112.0));
    let t = toast::Toast::new(toast::Message::Screenshot(crate::save::thumb_for(
        &sample(now)[0].path,
    )));
    shoot(t.view(), "iced-toast-shot", (380.0, 112.0));
    let t = toast::Toast::new(toast::Message::Failed(
        "No screen capture is running.".into(),
    ));
    shoot(t.view(), "iced-toast-failed", (380.0, 112.0));
    let _: Option<Theme> = None;
}
