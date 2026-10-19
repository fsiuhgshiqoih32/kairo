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
mod selftest;
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
    /// Pause or resume recording.
    Pause,
    /// Stop the background process.
    Quit,
    /// Open a clip in the editor: play, trim, share.
    Edit { clip: PathBuf },
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
        Some(Cmd::Pause) => request(daemon::Request::TogglePause),
        Some(Cmd::Quit) => request(daemon::Request::Quit),
        Some(Cmd::Devices) => devices(),
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
