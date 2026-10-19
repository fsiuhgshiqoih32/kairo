// No console window for the tray app; the CLI reattaches to the launching terminal.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod autostart;
mod daemon;
mod export;
mod ffmpeg;
mod gui;
mod hotkey;
mod log;
mod platform;
mod player;
mod recorder;
mod save;
mod secret;
mod selftest;
mod session;
mod share;
mod single_instance;
mod tray;
mod updater;
mod voice;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "rewindr",
    version,
    about = "Instant replay: press F8 to save the last 30 seconds of your screen and sound.",
    after_help = "With no command, Rewindr starts in the background (tray icon) and opens its window."
)]
struct Cli {
    /// Check that recording and saving work on this machine, then exit.
    #[arg(long)]
    self_test: bool,
    /// Check that the window plays and exports a clip (hidden), then exit.
    #[arg(long, hide = true)]
    ui_self_test: bool,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run in the background: record, show the tray icon, save clips on the shortcut.
    Daemon,
    /// Open the clips and settings window.
    Window {
        /// Wait for a closing window (after an update).
        #[arg(long, hide = true)]
        wait: bool,
    },
    /// Download and install the latest version.
    Update,
    /// Save a clip now (asks the background process; bind this to a key on Wayland).
    Save,
    /// Save a screenshot now (asks the background process).
    Screenshot,
    /// Upload a clip and print a link anyone can watch it with.
    Share {
        clip: PathBuf,
        /// How long the link lasts: forever, 72h, 24h or 1h (default: as in Settings).
        #[arg(long)]
        lasts: Option<String>,
    },
    /// Pause or resume recording.
    Pause,
    /// Stop the background process.
    Quit,
    /// Open a clip in the editor: play, trim, share.
    Edit { clip: PathBuf },
    /// Write captions of what's said in a clip (an .srt beside it), offline.
    Captions { clip: PathBuf },
    /// List screens, microphones and the encoders that work.
    Devices,
    /// Show the "clip saved" notification.
    #[command(hide = true)]
    Notify {
        #[arg(long)]
        clip: Option<PathBuf>,
        #[arg(long, default_value_t = 0.0)]
        seconds: f64,
        #[arg(long)]
        error: Option<String>,
        #[arg(long)]
        screenshot: Option<PathBuf>,
    },
    /// Check voice clipping on recordings: `voice-test "clip that" +said.wav -other.wav`.
    #[command(hide = true)]
    VoiceTest {
        phrase: String,
        #[arg(allow_hyphen_values = true)]
        files: Vec<String>,
    },
    /// Record for a while without the tray, then save a clip (for testing).
    #[command(hide = true)]
    TestRecord {
        #[arg(long, default_value_t = 8)]
        seconds: u64,
        #[arg(long, default_value_t = 5)]
        save: u32,
    },
}

fn main() {
    // Voice clipping's math library (OpenBLAS, inside Vosk) otherwise starts a thread per core
    // and reserves 128 MB for each; listening for one phrase needs one. Set before any thread
    // starts, so nothing reads the environment meanwhile.
    if std::env::var_os("OPENBLAS_NUM_THREADS").is_none() {
        // SAFETY: the first thing main does; no other threads exist yet.
        unsafe { std::env::set_var("OPENBLAS_NUM_THREADS", "1") };
    }
    platform::init_process();
    let cli = Cli::parse();
    if cli.self_test || cli.ui_self_test {
        let result = if cli.ui_self_test {
            gui::self_test()
        } else {
            selftest::run()
        };
        match result {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("self-test failed: {e}");
                std::process::exit(1);
            }
        }
    }
    let windowed = matches!(
        cli.command,
        None | Some(Cmd::Window { .. }) | Some(Cmd::Daemon) | Some(Cmd::Edit { .. })
    );
    let result = match cli.command {
        // The window starts the background process itself.
        None => gui::run(None, false),
        Some(Cmd::Daemon) => daemon::run(),
        Some(Cmd::Window { wait }) => gui::run(None, wait),
        Some(Cmd::Update) => update(),
        Some(Cmd::Save) => request(daemon::Request::Save),
        Some(Cmd::Screenshot) => request(daemon::Request::Screenshot),
        Some(Cmd::Share { clip, lasts }) => share_clip(&clip, lasts),
        Some(Cmd::Pause) => request(daemon::Request::TogglePause),
        Some(Cmd::Quit) => request(daemon::Request::Quit),
        Some(Cmd::Devices) => devices(),
        Some(Cmd::Captions { clip }) => captions(&clip),
        Some(Cmd::Edit { clip }) => {
            let clip = std::path::absolute(&clip).unwrap_or(clip);
            gui::run(Some(clip), false)
        }
        Some(Cmd::Notify {
            clip,
            seconds,
            error,
            screenshot,
        }) => match (clip, screenshot, error) {
            (Some(clip), _, _) => gui::toast::run(gui::toast::Message::Saved { clip, seconds }),
            (None, Some(path), _) => gui::toast::run(gui::toast::Message::Screenshot(path)),
            (None, None, Some(e)) => gui::toast::run(gui::toast::Message::Failed(e)),
            (None, None, None) => Err("notify needs --clip, --screenshot or --error".into()),
        },
        Some(Cmd::TestRecord { seconds, save }) => test_record(seconds, save),
        Some(Cmd::VoiceTest { phrase, files }) => voice::test_files(&phrase, &files),
    };
    if let Err(e) = result {
        log::error!("{e}");
        if windowed {
            platform::show_error(&e);
        }
        std::process::exit(1);
    }
}

fn captions(clip: &std::path::Path) -> Result<(), String> {
    let ff = ffmpeg::path()?;
    let info = ffmpeg::probe_clip(&ff, clip)?;
    let track = info
        .audio_tracks
        .iter()
        .position(|t| t == "Microphone")
        .unwrap_or(0);
    let job = export::start_captions(ff, clip.to_path_buf(), track);
    let mut last = String::new();
    while !job.finished() {
        let line = {
            let s = job.status.lock().unwrap();
            format!("{} {:.0}%", s.stage, s.progress * 100.0)
        };
        if line != last {
            eprintln!("{line}");
            last = line;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let caps = job
        .status
        .lock()
        .unwrap()
        .result
        .take()
        .unwrap_or(Ok(Vec::new()))?;
    let srt = rewindr::captions::srt(&caps);
    let out = clip.with_extension("srt");
    std::fs::write(&out, &srt).map_err(|e| format!("{}: {e}", out.display()))?;
    print!("{srt}");
    eprintln!("wrote {}", out.display());
    Ok(())
}

fn share_clip(clip: &std::path::Path, lasts: Option<String>) -> Result<(), String> {
    let (settings, _) = rewindr::config::load_or_create(&rewindr::config::default_path());
    let lasts = lasts.unwrap_or(settings.share_expiry);
    let up = share::start(clip, clip, &lasts, None);
    let mut shown = String::new();
    loop {
        if let Some(r) = up.take() {
            let link = r?;
            println!("{}", link.url);
            return Ok(());
        }
        let label = up.label();
        if label != shown {
            eprintln!("{label}");
            shown = label;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn request(r: daemon::Request) -> Result<(), String> {
    if !daemon::is_running() {
        return Err("Rewindr isn't running. Start it first (run `rewindr`).".into());
    }
    daemon::request(r)
}

fn update() -> Result<(), String> {
    match updater::check()? {
        None => {
            println!("Rewindr {} is up to date.", updater::current_version());
            Ok(())
        }
        Some(u) => {
            println!(
                "Installing Rewindr {} (this is {})...",
                u.version,
                updater::current_version()
            );
            updater::install(&u)?;
            if daemon::is_running() {
                daemon::request(daemon::Request::Restart)?;
            }
            println!("Updated to {}.", u.version);
            Ok(())
        }
    }
}

fn devices() -> Result<(), String> {
    println!("Screens:");
    for (i, m) in platform::monitors().iter().enumerate() {
        println!("  {}. {} at {},{}", i + 1, m.label(), m.x, m.y);
    }
    println!("Microphones:");
    for m in audio::microphones() {
        println!("  {m}");
    }
    let ff = ffmpeg::path()?;
    println!(
        "ffmpeg {} at {}",
        ffmpeg::version(&ff).unwrap_or_default(),
        ff.display()
    );
    println!("Encoders that work:");
    for e in ffmpeg::working_encoders(&ff) {
        println!("  {}", e.label());
    }
    Ok(())
}

fn test_record(seconds: u64, save_secs: u32) -> Result<(), String> {
    let (settings, warn) = rewindr::config::load_or_create(&rewindr::config::default_path());
    if let Some(w) = warn {
        eprintln!("{w}");
    }
    let shared = recorder::Shared::new(settings);
    let rec = {
        let s = shared.clone();
        std::thread::spawn(move || recorder::run(s))
    };
    let aud = {
        let s = shared.clone();
        std::thread::spawn(move || audio::run(s))
    };
    std::thread::sleep(Duration::from_secs(seconds));
    println!("status: {:?}", shared.status());
    let title = platform::foreground_title().and_then(|t| platform::clean_title(&t));
    let result = save::save(&shared, Some(save_secs), title.as_deref());
    shared.stop.store(true, Ordering::SeqCst);
    let _ = rec.join();
    let _ = aud.join();
    let clip = result?;
    println!("saved {} ({:.2} s)", clip.path.display(), clip.seconds);
    Ok(())
}
