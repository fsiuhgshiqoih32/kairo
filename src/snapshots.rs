//! Renders Rewindr's windows to PNG files without opening them on screen, to check how they
//! look. Needs ffmpeg and a folder of clips:
//!
//! `REWINDR_HOME=<folder with Clips/ and thumbs/> REWINDR_SNAPSHOTS=<out dir> cargo test -- --ignored snapshots`

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::vec2;
use egui_kittest::Harness;

fn out_dir() -> PathBuf {
    let d = PathBuf::from(
        std::env::var("REWINDR_SNAPSHOTS").expect("set REWINDR_SNAPSHOTS to a folder"),
    );
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Steps the app in real time until `ready` or `limit`, so worker threads can finish.
fn settle<S: eframe::App>(h: &mut Harness<'_, S>, limit: Duration, ready: impl Fn(&S) -> bool) {
    let start = Instant::now();
    while start.elapsed() < limit {
        h.step();
        if ready(h.state()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    for _ in 0..3 {
        h.step();
    }
}

fn save<S: eframe::App>(h: &mut Harness<'_, S>, name: &str) {
    let img = h.render().expect("render");
    let path = out_dir().join(format!("{name}.png"));
    img.save(&path).unwrap();
    println!("wrote {}", path.display());
}

fn fake_report() {
    let report = crate::daemon::Report {
        status: crate::recorder::Status {
            recording: true,
            encoder: Some("NVIDIA (NVENC)".into()),
            source: Some("Windows Graphics Capture".into()),
            size: Some((1920, 1440)),
            fps: 60,
            buffer_seconds: 33.0,
            buffer_mb: 68.0,
            desktop_audio: Some("Speakers".into()),
            microphone: Some("Microphone".into()),
            ..Default::default()
        },
        hotkey: Some("F8".into()),
        hotkey_error: None,
        updated_ms: crate::recorder::now_us() / 1000 + 600_000,
    };
    let dir = rewindr::config::config_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("status.json"),
        serde_json::to_string(&report).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "renders windows to PNGs; needs ffmpeg, REWINDR_HOME and REWINDR_SNAPSHOTS"]
fn snapshots() {
    // Safe: set before any thread reads it.
    unsafe { std::env::set_var("REWINDR_NO_UPDATE_CHECK", "1") };
    fake_report();
    let clips = rewindr::library::scan(&rewindr::config::default_save_dir());
    assert!(!clips.is_empty(), "put some clips in REWINDR_HOME/Clips");

    for (tab, name) in [("clips", "window-clips"), ("settings", "window-settings")] {
        // Safe: tests in this module run one at a time.
        unsafe { std::env::set_var("REWINDR_TAB", tab) };
        let mut h = Harness::builder()
            .with_size(vec2(900.0, 640.0))
            .wgpu()
            .build_eframe(|cc| {
                crate::ui::apply_theme(&cc.egui_ctx);
                crate::window::Window::new(cc, None)
            });
        settle(&mut h, Duration::from_secs(4), |_| false);
        save(&mut h, name);
    }

    let ff = crate::ffmpeg::path().unwrap();
    let clip = clips[0].path.clone();
    let info = crate::editor::probe(&ff, &clip).unwrap();
    let mut h = Harness::builder()
        .with_size(vec2(1100.0, 780.0))
        .wgpu()
        .build_eframe(|cc| {
            crate::ui::apply_theme(&cc.egui_ctx);
            crate::editor::Editor::new(cc, ff.clone(), clip.clone(), info.clone())
        });
    settle(&mut h, Duration::from_secs(6), |e| e.loaded());
    save(&mut h, "editor");
    h.state_mut().snapshot_trim(4.0, 19.5, 11.0);
    settle(&mut h, Duration::from_secs(4), |e| e.loaded());
    save(&mut h, "editor-trimmed");

    let mut h = Harness::builder()
        .with_size(vec2(380.0, 104.0))
        .wgpu()
        .build_eframe(|cc| {
            crate::ui::apply_theme(&cc.egui_ctx);
            crate::notify::Card::new(crate::notify::Message::Saved {
                clip: clip.clone(),
                seconds: 30.0,
            })
        });
    settle(&mut h, Duration::from_secs(1), |_| false);
    save(&mut h, "notify");
}

/// Exports a trimmed copy and a Discord copy of a real clip and checks both.
#[test]
#[ignore = "encodes real clips; needs ffmpeg and REWINDR_HOME with clips"]
fn exports() {
    use std::sync::{Arc, Mutex};
    let clips = rewindr::library::scan(&rewindr::config::default_save_dir());
    let clip = clips
        .iter()
        .find(|c| !c.name.contains("(trim") && !c.name.contains("(Discord"))
        .expect("an original clip in REWINDR_HOME/Clips")
        .path
        .clone();
    let ff = crate::ffmpeg::path().unwrap();
    let info = crate::editor::probe(&ff, &clip).unwrap();
    let encoders = Arc::new(Mutex::new(None));
    for kind in [
        crate::export::Kind::Trim,
        crate::export::Kind::Share(10_000_000),
    ] {
        let started = Instant::now();
        let job = crate::export::start(
            kind,
            ff.clone(),
            clip.clone(),
            info.clone(),
            (2.0, 14.5),
            encoders.clone(),
        );
        while !job.finished() {
            std::thread::sleep(Duration::from_millis(50));
        }
        let out = job
            .status
            .lock()
            .unwrap()
            .result
            .take()
            .unwrap()
            .expect("export");
        let secs = rewindr::mp4::duration_secs(&out).unwrap();
        let size = std::fs::metadata(&out).unwrap().len();
        let got = crate::editor::probe(&ff, &out).unwrap();
        println!(
            "{kind:?}: {} in {:.1} s: {secs:.2} s long, {size} bytes, {}x{} {} fps, tracks {:?}",
            out.display(),
            started.elapsed().as_secs_f64(),
            got.width,
            got.height,
            got.fps,
            got.audio_tracks
        );
        assert!((secs - 12.5).abs() < 0.1, "length {secs}");
        if let crate::export::Kind::Share(limit) = kind {
            assert!(size <= limit);
            assert_eq!(got.audio_tracks.len(), info.audio_tracks.len().min(1));
        } else {
            assert_eq!(got.audio_tracks.len(), info.audio_tracks.len());
        }
        assert!(crate::save::thumb_for(&out).exists());
    }
}

/// Plays a real clip silently for three seconds and checks the clock and frame rate.
#[test]
#[ignore = "plays a real clip (muted); needs ffmpeg, a sound device and REWINDR_HOME with clips"]
fn playback() {
    let clips = rewindr::library::scan(&rewindr::config::default_save_dir());
    let clip = clips[0].path.clone();
    let ff = crate::ffmpeg::path().unwrap();
    let info = crate::editor::probe(&ff, &clip).unwrap();
    let mut p = crate::player::Player::new(ff, clip, info, (1280, 720));
    p.set_volume(0.0);
    p.seek(5.0);
    p.play();
    let start = Instant::now();
    let mut shown = 0;
    let mut first: Option<(Instant, f64)> = None;
    while start.elapsed() < Duration::from_secs(3) {
        p.tick(1e9);
        if let Some(f) = p.frame.take() {
            shown += 1;
            first.get_or_insert((Instant::now(), f.time));
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    let (t0, pos0) = first.expect("frames arrived");
    let wall = t0.elapsed().as_secs_f64();
    let advanced = p.position - pos0;
    println!(
        "first frame after {:.0} ms; {shown} frames shown; clock advanced {advanced:.3} s in {wall:.3} s of wall time",
        (t0 - start).as_secs_f64() * 1000.0
    );
    assert!(
        (advanced - wall).abs() < 0.15,
        "clock drift {advanced} vs {wall}"
    );
    assert!(
        shown as f64 > wall * 50.0,
        "only {shown} frames in {wall} s"
    );
}
