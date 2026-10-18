// No console window for the tray app; the CLI reattaches to the launching terminal.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod autostart;
mod daemon;
mod ffmpeg;
mod hotkey;
mod log;
mod notify;
mod platform;
mod recorder;
mod save;
mod selftest;
mod single_instance;
mod tray;
mod ui;
mod window;

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
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run in the background: record, show the tray icon, save clips on the shortcut.
    Daemon,
    /// Open the clips and settings window.
    Window,
    /// Save a clip now (asks the background process; bind this to a key on Wayland).
    Save,
    /// Pause or resume recording.
    Pause,
    /// Stop the background process.
    Quit,
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
    if cli.self_test {
        match selftest::run() {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("self-test failed: {e}");
                std::process::exit(1);
            }
        }
    }
    let windowed = matches!(cli.command, None | Some(Cmd::Window) | Some(Cmd::Daemon));
    let result = match cli.command {
        None => daemon::ensure_running().and_then(|_| window::run()),
        Some(Cmd::Daemon) => daemon::run(),
        Some(Cmd::Window) => window::run(),
        Some(Cmd::Save) => request(daemon::Request::Save),
        Some(Cmd::Pause) => request(daemon::Request::TogglePause),
        Some(Cmd::Quit) => request(daemon::Request::Quit),
        Some(Cmd::Devices) => devices(),
        Some(Cmd::Notify {
            clip,
            seconds,
            error,
        }) => match (clip, error) {
            (Some(clip), _) => notify::run(notify::Message::Saved { clip, seconds }),
            (None, Some(e)) => notify::run(notify::Message::Failed(e)),
            (None, None) => Err("notify needs --clip or --error".into()),
        },
        Some(Cmd::TestRecord { seconds, save }) => test_record(seconds, save),
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
