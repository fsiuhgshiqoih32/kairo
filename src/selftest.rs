//! `rewindr --self-test`: checks on this machine that ffmpeg runs, an encoder works, the replay
//! buffer fills, and a saved clip has video and sound. Uses a temporary folder for everything.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rewindr::args::{Source, VideoPlan};
use rewindr::pcm::PcmRing;

use crate::recorder::{self, Shared, now_us};
use crate::{ffmpeg, platform, save};

pub fn run() -> Result<(), String> {
    let started = Instant::now();
    let home = std::env::temp_dir().join(format!("rewindr-selftest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    // Safe: nothing else runs yet.
    unsafe { std::env::set_var("REWINDR_HOME", &home) };

    let ff = ffmpeg::path()?;
    println!(
        "ffmpeg: {} ({})",
        ffmpeg::version(&ff).unwrap_or_else(|| "unknown version".into()),
        ff.display()
    );
    let encoders = ffmpeg::working_encoders(&ff);
    println!(
        "encoders: {}",
        encoders
            .iter()
            .map(|e| e.label())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let encoder = *encoders.first().ok_or("no encoder works")?;

    // 1. The whole path with a test picture and a test tone: encoder, buffer, cut, mux.
    let mut settings = rewindr::config::Settings {
        fps: 30,
        ..Default::default()
    };
    settings.save_dir = home.join("clips").display().to_string();
    let shared = Shared::new(settings.clone());
    let tone = start_tone(&shared);
    let plan = VideoPlan {
        source: Source::Test { w: 320, h: 240 },
        encoder,
        fps: 30,
        src_size: (320, 240),
        max_height: None,
        bitrate: 1_000_000,
        vaapi_device: ffmpeg::vaapi_device(),
    };
    record_for(&ff, &plan, &shared, Duration::from_secs(4));
    let held = shared.ring.lock().unwrap().seconds();
    println!("buffer: {held:.1} s held");
    if held < 2.0 {
        return Err(format!("the buffer held only {held:.1} s"));
    }
    let clip = save::save(&shared, Some(2), Some("Self test"))?;
    shared.stop.store(true, Ordering::SeqCst);
    let _ = tone.join();
    let info = probe(&ff, &clip.path)?;
    println!(
        "clip: {} ({:.1} s): {info}",
        clip.path.display(),
        clip.seconds
    );
    if !info.contains("Video: h264") || !info.contains("Audio: aac") {
        return Err(format!("the clip is missing video or sound: {info}"));
    }
    if clip.thumbnail.is_none() {
        return Err("no thumbnail".into());
    }
    // A screenshot: the newest frame of the buffer, full size.
    let shot = save::screenshot(&shared, Some("Self test"))?;
    let (w, h) = image::image_dimensions(&shot)
        .map_err(|e| format!("the screenshot isn't a picture: {e}"))?;
    println!("screenshot: {} ({w}x{h})", shot.display());
    if (w, h) != (320, 240) {
        return Err(format!("the screenshot is {w}x{h}, not 320x240"));
    }
    if std::env::var_os("REWINDR_SELFTEST_NO_SCREEN").is_some() {
        let _ = std::fs::remove_dir_all(&home);
        println!(
            "self-test passed without the screen in {:.1} s",
            started.elapsed().as_secs_f64()
        );
        return Ok(());
    }

    // 2. The real screen, briefly.
    let monitors = platform::monitors();
    println!(
        "screens: {}",
        monitors
            .iter()
            .map(|m| m.label())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let monitor = platform::choose_monitor("primary").ok_or("no screen found")?;
    let mut captured = None;
    for source in platform::capture_sources(&monitor) {
        let shared = Shared::new(settings.clone());
        let plan = VideoPlan {
            source: source.clone(),
            encoder,
            fps: 30,
            src_size: (monitor.w, monitor.h),
            max_height: Some(720),
            bitrate: 2_000_000,
            vaapi_device: ffmpeg::vaapi_device(),
        };
        record_for(&ff, &plan, &shared, Duration::from_secs(4));
        let held = shared.ring.lock().unwrap().seconds();
        if held > 1.0 {
            captured = Some((source, held));
            break;
        }
        println!("screen capture via {} didn't work here", source.label());
    }
    match captured {
        Some((s, held)) => println!("screen capture: {} ({held:.1} s held)", s.label()),
        // CI's macOS has no screen recording permission; the rest still counts there.
        None if cfg!(target_os = "macos") => {
            println!("screen capture: no permission here (skipped)")
        }
        None => return Err("no way to capture the screen worked".into()),
    }
    system_audio_check();
    let _ = std::fs::remove_dir_all(&home);
    println!(
        "self-test passed in {:.1} s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// macOS: starts ScreenCaptureKit system audio briefly and reports what arrived. Without the
/// Screen Recording permission (as on CI) it can't start, which is reported, not failed.
#[cfg(target_os = "macos")]
fn system_audio_check() {
    let shared = Shared::new(rewindr::config::Settings::default());
    match crate::platform::macos_audio::start(shared.clone()) {
        Ok(capture) => {
            // Something to hear: ScreenCaptureKit may send nothing while the Mac is silent.
            let sound = std::process::Command::new("afplay")
                .arg("/System/Library/Sounds/Glass.aiff")
                .spawn();
            std::thread::sleep(Duration::from_secs(3));
            drop(capture);
            if let Ok(mut s) = sound {
                let _ = s.wait();
            }
            let held = shared
                .desktop
                .lock()
                .unwrap()
                .as_ref()
                .map_or(0.0, |r| r.seconds());
            let (frames, buffers, decoded) = crate::platform::macos_audio::counts();
            if let Some(e) = crate::platform::macos_audio::first_error() {
                println!("desktop audio: first unreadable buffer: {e}");
            }
            println!(
                "desktop audio: ScreenCaptureKit delivered {held:.1} s ({frames} video frames, {buffers} audio buffers, {decoded} decoded)"
            );
        }
        Err(e) => println!("desktop audio: not available here ({e})"),
    }
}

#[cfg(not(target_os = "macos"))]
fn system_audio_check() {}

/// Records `plan` into `shared` for `d`.
fn record_for(ff: &std::path::Path, plan: &VideoPlan, shared: &Arc<Shared>, d: Duration) {
    let generation = shared.video_generation.load(Ordering::SeqCst);
    let s = shared.clone();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(d);
        s.video_generation.fetch_add(1, Ordering::SeqCst);
    });
    let _ = recorder::run_plan(ff, plan, shared, generation);
    let _ = stopper.join();
}

/// A 440 Hz tone into the desktop ring, in 10 ms blocks, until `shared.stop`.
fn start_tone(shared: &Arc<Shared>) -> std::thread::JoinHandle<()> {
    *shared.desktop.lock().unwrap() = Some(PcmRing::new(48_000, 2, 30_000_000));
    let s = shared.clone();
    std::thread::spawn(move || {
        let mut n = 0u64;
        while !s.stop.load(Ordering::Relaxed) {
            let block: Vec<f32> = (0..480)
                .flat_map(|i| {
                    let v = ((n + i) as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin() * 0.2;
                    [v, v]
                })
                .collect();
            n += 480;
            if let Some(r) = s.desktop.lock().unwrap().as_mut() {
                r.push(&block, now_us());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })
}

/// ffmpeg's description of a file's streams.
fn probe(ff: &std::path::Path, clip: &std::path::Path) -> Result<String, String> {
    let out = ffmpeg::command(ff)
        .arg("-hide_banner")
        .arg("-i")
        .arg(clip)
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stderr);
    let streams: Vec<&str> = text
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            (l.starts_with("Stream #") || l.starts_with("Duration:")).then_some(l)
        })
        .collect();
    Ok(streams.join(" | "))
}
