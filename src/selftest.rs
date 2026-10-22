//! `kairo --self-test`: checks on this machine that ffmpeg runs, an encoder works, the replay
//! buffer fills, and a saved clip has video and sound. Uses a temporary folder for everything.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use kairo::args::{Source, VideoPlan};
use kairo::pcm::PcmRing;

use crate::recorder::{self, Shared, now_us};
use crate::{ffmpeg, platform, save};

pub fn run() -> Result<(), String> {
    let started = Instant::now();
    let home = std::env::temp_dir().join(format!("kairo-selftest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    // Safe: nothing else runs yet.
    unsafe { std::env::set_var("KAIRO_HOME", &home) };

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
    let mut settings = kairo::config::Settings {
        fps: 30,
        ..Default::default()
    };
    settings.save_dir = home.join("clips").display().to_string();
    let shared = Shared::new(settings.clone());
    let tone = start_tone(&shared);
    let plan = VideoPlan {
        source: Source::Test { w: 320, h: 240 },
        encoder,
        codec: kairo::config::VideoCodec::H264,
        fps: 30,
        src_size: (320, 240),
        max_height: None,
        bitrate: 1_000_000,
        cq: None,
        vaapi_device: ffmpeg::vaapi_device(),
    };
    // Through the buffer on disk (the screen test below uses memory).
    shared
        .ring
        .lock()
        .unwrap()
        .set_spill(Some((home.join("buffer").join("replay.bin"), 64 << 20)))?;
    // A session from 1.5 s on, and a moment marked 3 s in: the 2 s clip saved at the end must
    // hold the mark, and the session must hold everything from its start.
    let marker = {
        let s = shared.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            let started = crate::session::start(&s, Some("Self test".into()));
            std::thread::sleep(Duration::from_millis(1500));
            s.mark();
            started
        })
    };
    record_for(&ff, &plan, &shared, Duration::from_secs(4));
    marker
        .join()
        .map_err(|_| "the session thread panicked")?
        .map_err(|e| format!("the session didn't start: {e}"))?;
    let (tx, rx) = std::sync::mpsc::channel();
    crate::session::stop(&shared, move |r| {
        let _ = tx.send(r);
    });
    let (session, secs) = rx
        .recv_timeout(Duration::from_secs(60))
        .map_err(|_| "the session wasn't joined")??;
    let info = probe(&ff, &session)?;
    let marks = kairo::library::Library::load().markers(&session).to_vec();
    println!(
        "session: {} ({secs:.1} s, marks {marks:?}): {info}",
        session.display()
    );
    if !(1.5..=3.5).contains(&secs) || !info.contains("Video: h264") || !info.contains("Audio: aac")
    {
        return Err(format!("the session isn't right ({secs:.1} s): {info}"));
    }
    if marks.len() != 1 {
        return Err(format!("the session's bookmark is missing: {marks:?}"));
    }
    let (held, on_disk, in_memory) = {
        let r = shared.ring.lock().unwrap();
        (
            r.seconds(),
            r.on_disk(),
            r.bytes() - r.memory_bytes().min(r.bytes()),
        )
    };
    println!("buffer: {held:.1} s held, {in_memory} bytes of it on disk");
    if held < 2.0 {
        return Err(format!("the buffer held only {held:.1} s"));
    }
    if !on_disk || in_memory == 0 {
        return Err("the buffer didn't use its file".into());
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
    let marks = kairo::library::Library::load().markers(&clip.path).to_vec();
    println!("marks in the clip: {marks:?}");
    if marks.len() != 1 || !(0.0..=clip.seconds).contains(&marks[0]) {
        return Err(format!(
            "the marked moment didn't land in the clip: {marks:?}"
        ));
    }
    // H.265 too, where this encoder has it (hardware only): saved tagged hvc1.
    if encoder
        .ffmpeg_name_for(kairo::config::VideoCodec::Hevc)
        .is_some()
    {
        let shared = Shared::new(settings.clone());
        let plan = VideoPlan {
            source: Source::Test { w: 320, h: 240 },
            encoder,
            codec: kairo::config::VideoCodec::Hevc,
            fps: 30,
            src_size: (320, 240),
            max_height: None,
            bitrate: 600_000,
            cq: None,
            vaapi_device: ffmpeg::vaapi_device(),
        };
        record_for(&ff, &plan, &shared, Duration::from_secs(3));
        let clip = save::save(&shared, Some(2), Some("Self test H265"))?;
        let info = probe(&ff, &clip.path)?;
        println!("H.265 clip: {info}");
        if !info.contains("Video: hevc") || !info.contains("hvc1") {
            return Err(format!("the H.265 clip isn't tagged H.265: {info}"));
        }
    }
    // A screenshot: the newest frame of the buffer, full size.
    let shot = save::screenshot(&shared, Some("Self test"))?;
    let (w, h) = image::image_dimensions(&shot)
        .map_err(|e| format!("the screenshot isn't a picture: {e}"))?;
    println!("screenshot: {} ({w}x{h})", shot.display());
    if (w, h) != (320, 240) {
        return Err(format!("the screenshot is {w}x{h}, not 320x240"));
    }
    if std::env::var_os("KAIRO_SELFTEST_NO_SCREEN").is_some() {
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
            codec: kairo::config::VideoCodec::H264,
            fps: 30,
            src_size: (monitor.w, monitor.h),
            max_height: Some(720),
            bitrate: 2_000_000,
            cq: None,
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
    let shared = Shared::new(kairo::config::Settings::default());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// NVENC at constant quality through the real recorder: the buffer gets 2-second groups
    /// and the stream decodes. Needs an NVIDIA GPU; a test picture, so nothing is captured.
    #[test]
    #[ignore]
    fn nvenc_constant_quality_records() {
        let ff = ffmpeg::path().expect("ffmpeg");
        let shared = Shared::new(kairo::config::Settings::default());
        let plan = VideoPlan {
            source: Source::Test { w: 2560, h: 1440 },
            encoder: kairo::args::Encoder::Nvenc,
            codec: kairo::config::VideoCodec::H264,
            fps: 120,
            src_size: (2560, 1440),
            max_height: None,
            bitrate: 33_000_000,
            cq: Some(21),
            vaapi_device: None,
        };
        record_for(&ff, &plan, &shared, Duration::from_secs(7));
        let snap = shared.ring.lock().unwrap().snapshot(0).expect("recorded");
        let starts: Vec<i64> = snap.gops.iter().map(|g| g.start_us).collect();
        assert!(starts.len() >= 3, "{starts:?}");
        for w in starts.windows(2) {
            let gap = (w[1] - w[0]) as f64 / 1e6;
            assert!((gap - 2.0).abs() < 0.1, "keyframes {gap:.2} s apart");
        }
        let dir = tempfile::tempdir().unwrap();
        let ts = dir.path().join("v.ts");
        snap.write_to(std::fs::File::create(&ts).unwrap()).unwrap();
        let out = ffmpeg::command(&ff)
            .args(["-v", "error", "-i"])
            .arg(&ts)
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        assert!(out.status.success() && out.stderr.is_empty(), "{out:?}");
    }

    /// A clip saved from a test picture and a test tone has its picture, its sound and a
    /// thumbnail. Nothing is captured. Needs `KAIRO_HOME` set: settings and clips go there.
    #[test]
    #[ignore]
    fn saves_a_clip_with_sound_and_thumbnail() {
        assert!(std::env::var_os("KAIRO_HOME").is_some(), "set KAIRO_HOME");
        let ff = ffmpeg::path().expect("ffmpeg");
        let mut settings = kairo::config::Settings {
            fps: 30,
            ..Default::default()
        };
        settings.save_dir = kairo::config::config_dir()
            .join("clips")
            .display()
            .to_string();
        let shared = Shared::new(settings);
        let tone = start_tone(&shared);
        let plan = VideoPlan {
            source: Source::Test { w: 320, h: 240 },
            encoder: *ffmpeg::working_encoders(&ff).first().expect("an encoder"),
            codec: kairo::config::VideoCodec::H264,
            fps: 30,
            src_size: (320, 240),
            max_height: None,
            bitrate: 1_000_000,
            cq: None,
            vaapi_device: None,
        };
        record_for(&ff, &plan, &shared, Duration::from_secs(5));
        let clip = save::save(&shared, Some(4), Some("Save test")).expect("saved");
        shared.stop.store(true, Ordering::SeqCst);
        let _ = tone.join();
        let info = probe(&ff, &clip.path).unwrap();
        assert!(
            info.contains("Video: h264") && info.contains("Audio: aac"),
            "{info}"
        );
        assert!(clip.thumbnail.is_some(), "no thumbnail");
    }
}
