// No console window for the tray app; the CLI reattaches to the launching terminal.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod autostart;
mod daemon;
mod export;
mod ffmpeg;
mod friends;
mod gui;
mod hotkey;
mod log;
mod mictest;
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
mod webcam;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "kairo",
    version,
    about = "Instant replay: press F8 to save the last 30 seconds of your screen and sound.",
    after_help = "With no command, Kairo starts in the background (tray icon) and opens its window."
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
    Daemon {
        /// Wait for the old background process to finish (it saves a running session first).
        #[arg(long, hide = true)]
        wait: bool,
    },
    /// Open the clips and settings window.
    Window {
        /// Wait for a closing window (after an update).
        #[arg(long, hide = true)]
        wait: bool,
        /// Open on the update and install it.
        #[arg(long, hide = true)]
        update: bool,
        /// Open on Friends.
        #[arg(long, hide = true)]
        friends: bool,
    },
    /// Download and install the latest version.
    Update,
    /// Save a clip now (asks the background process; bind this to a key on Wayland).
    Save,
    /// Save a screenshot now (asks the background process).
    Screenshot,
    /// Upload a clip and print a link anyone can watch it with.
    Share { clip: PathBuf },
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
    /// Copy your clips and screenshots from Medal into Kairo. Medal's own files stay as they are.
    ImportMedal {
        /// The folder Medal keeps its clips in, if Kairo doesn't find it.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Only say what would be imported, and where to.
        #[arg(long)]
        dry_run: bool,
    },
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
        /// Upload the saved clip and copy its link.
        #[arg(long)]
        share: bool,
        /// A newer version is out.
        #[arg(long)]
        update: Option<String>,
        /// One line about the newer version.
        #[arg(long)]
        about: Option<String>,
        /// A friend's message (`--from`, `--text`, a clip's `--link`) or friend request.
        #[arg(long)]
        friend: bool,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        link: Option<String>,
        #[arg(long)]
        request: bool,
    },
    /// Check voice clipping on recordings: `voice-test "clip that" +said.wav -other.wav`.
    #[command(hide = true)]
    VoiceTest {
        phrase: String,
        #[arg(allow_hyphen_values = true)]
        files: Vec<String>,
    },
    /// Turn "start with computer" on or off (the installer uses this).
    #[command(hide = true)]
    Autostart {
        #[arg(value_parser = ["on", "off"])]
        state: String,
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
    // The pop-up after a save draws on the CPU: a new graphics device on a card busy with a game
    // and the recording can stall the game. The graphics card stays the fallback.
    if std::env::args_os().nth(1).is_some_and(|a| a == "notify")
        && std::env::var_os("ICED_BACKEND").is_none()
    {
        // SAFETY: as above, before any other thread exists.
        unsafe { std::env::set_var("ICED_BACKEND", app::POPUP_BACKEND) };
    }
    // A panic on any thread goes in the log, where the user's log shows what went wrong.
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        log::error!(
            "panic on the {} thread: {info}",
            thread.name().unwrap_or("unnamed")
        );
        report(info);
        // The graphics card ran out of memory for the window (a game and a stream holding it
        // all): open it again drawn on the CPU instead of leaving it dead.
        if app::window_lost_gpu(&info.to_string()) {
            match app::reopen_window_on_cpu() {
                Ok(()) => log::warning!("the window lost the graphics card; opening it again"),
                Err(e) => log::error!("{e}"),
            }
            std::process::exit(1);
        }
    }));
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
        None | Some(Cmd::Window { .. }) | Some(Cmd::Daemon { .. }) | Some(Cmd::Edit { .. })
    );
    let result = match cli.command {
        // The window starts the background process itself.
        None => gui::run(None, false, false, false),
        Some(Cmd::Daemon { wait }) => daemon::run(wait),
        Some(Cmd::Window {
            wait,
            update,
            friends,
        }) => gui::run(None, wait, update, friends),
        Some(Cmd::Update) => update(),
        Some(Cmd::Save) => request(daemon::Request::Save),
        Some(Cmd::Screenshot) => request(daemon::Request::Screenshot),
        Some(Cmd::Share { clip }) => share_clip(&clip),
        Some(Cmd::Pause) => request(daemon::Request::TogglePause),
        Some(Cmd::Quit) => request(daemon::Request::Quit),
        Some(Cmd::Devices) => devices(),
        Some(Cmd::ImportMedal { from, dry_run }) => import_medal(from.as_deref(), dry_run),
        Some(Cmd::Captions { clip }) => captions(&clip),
        Some(Cmd::Edit { clip }) => {
            let clip = std::path::absolute(&clip).unwrap_or(clip);
            gui::run(Some(clip), false, false, false)
        }
        Some(Cmd::Notify {
            update: Some(version),
            about,
            ..
        }) => gui::toast::run(gui::toast::Message::Update { version, about }),
        Some(Cmd::Notify {
            friend: true,
            from,
            text,
            link,
            request,
            ..
        }) => gui::toast::run(gui::toast::Message::Friend {
            from: from.unwrap_or_else(|| "A friend".into()),
            text: text.unwrap_or_default(),
            link,
            request,
        }),
        Some(Cmd::Notify {
            clip,
            seconds,
            error,
            screenshot,
            share,
            ..
        }) => match (clip, screenshot, error) {
            (Some(clip), _, _) => gui::toast::run(gui::toast::Message::Saved {
                clip,
                seconds,
                share,
            }),
            (None, Some(path), _) => gui::toast::run(gui::toast::Message::Screenshot(path)),
            (None, None, Some(e)) => gui::toast::run(gui::toast::Message::Failed(e)),
            (None, None, None) => Err("notify needs --clip, --screenshot or --error".into()),
        },
        Some(Cmd::Autostart { state }) => match autostart::launcher() {
            Some(l) => {
                autostart::set_enabled(&l, state == "on");
                Ok(())
            }
            None => Err("start with computer isn't available here".into()),
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
    let srt = kairo::captions::srt(&caps);
    let out = clip.with_extension("srt");
    std::fs::write(&out, &srt).map_err(|e| format!("{}: {e}", out.display()))?;
    print!("{srt}");
    eprintln!("wrote {}", out.display());
    Ok(())
}

fn share_clip(clip: &std::path::Path) -> Result<(), String> {
    let up = share::start(clip, clip, None);
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
        return Err("Kairo isn't running. Start it first (run `kairo`).".into());
    }
    daemon::request(r)
}

fn update() -> Result<(), String> {
    match updater::check()? {
        None => {
            println!("Kairo {} is up to date.", updater::current_version());
            Ok(())
        }
        Some(u) => {
            println!(
                "Installing Kairo {} (this is {})...",
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

fn import_medal(from: Option<&std::path::Path>, dry_run: bool) -> Result<(), String> {
    use kairo::medal::{self, Kind};
    let settings = kairo::config::load(&kairo::config::default_path()).unwrap_or_default();
    let app_data = medal::app_data();
    let found = match from {
        Some(dir) => medal::scan_folder(dir, app_data.as_deref()),
        None => medal::find(app_data.as_deref(), &medal::default_folders()),
    };
    if found.items.is_empty() {
        return Err(match from {
            Some(dir) => format!("There are no Medal clips in {}.", dir.display()),
            None => "Medal's clips weren't found. Say where they are with --from <folder>.".into(),
        });
    }
    let folders: Vec<String> = found
        .folders
        .iter()
        .map(|f| f.display().to_string())
        .collect();
    let games: std::collections::HashSet<&str> = found
        .items
        .iter()
        .filter(|i| i.kind == Kind::Clip)
        .map(|i| i.game.as_str())
        .collect();
    println!(
        "Found {} ({}{}) in {}.",
        medal::describe(
            found.count(Kind::Clip),
            found.count(Kind::Screenshot),
            "Medal"
        ),
        gui::human_size(found.bytes()),
        match games.len() {
            0 => String::new(),
            1 => ", from 1 game".into(),
            n => format!(", from {n} games"),
        },
        folders.join(" and ")
    );
    let to = medal::Target::kairo(&settings);
    let plan = medal::plan(&found, &to);
    match plan.already {
        0 => {}
        1 => println!("1 is in Kairo already."),
        n => println!("{n} are in Kairo already."),
    }
    if plan.copies.is_empty() {
        println!("Nothing new to import.");
        return Ok(());
    }
    let clips = plan.count(Kind::Clip);
    let what = medal::describe(clips, plan.count(Kind::Screenshot), "");
    let free = platform::free_space_for(&to.clips);
    if dry_run {
        for (item, dest) in &plan.copies {
            println!("  {} -> {}", item.path.display(), dest.display());
        }
    }
    println!(
        "{} {what} ({}) to {}{}{}. They're copied; Medal's files stay where they are.",
        if dry_run { "Would import" } else { "Importing" },
        gui::human_size(plan.bytes()),
        to.clips.display(),
        free.map(|f| format!(" ({} free)", gui::human_size(f)))
            .unwrap_or_default(),
        if clips > 0 {
            format!(", clips in the folder {}", medal::FOLDER)
        } else {
            String::new()
        }
    );
    if !medal::fits(plan.bytes(), free) {
        return Err(format!(
            "Not enough room: importing needs {} free, and there's {}.",
            gui::human_size(plan.bytes() + medal::SPARE_BYTES),
            gui::human_size(free.unwrap_or(0))
        ));
    }
    if dry_run {
        return Ok(());
    }
    let progress = std::sync::Arc::new(std::sync::Mutex::new(medal::Progress::default()));
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let job = {
        let (p, c) = (progress.clone(), cancel.clone());
        std::thread::spawn(move || gui::medal::copy(&plan, &to, &p, &c))
    };
    // A line each time a file is done.
    let mut shown = None;
    while !job.is_finished() {
        {
            let p = progress.lock().unwrap();
            if p.total > 0 && shown != Some(p.done) {
                eprintln!(
                    "Importing {} of {} ({} of {})",
                    (p.done + 1).min(p.total),
                    p.total,
                    gui::human_size(p.bytes),
                    gui::human_size(p.total_bytes)
                );
                shown = Some(p.done);
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = job.join();
    let (said, ok) = progress.lock().unwrap().summary();
    if !ok {
        return Err(said);
    }
    println!("{said}");
    Ok(())
}

fn test_record(seconds: u64, save_secs: u32) -> Result<(), String> {
    let (settings, warn) = kairo::config::load_or_create(&kairo::config::default_path());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The background process passes a friend's message as `--text=<message>`, so one starting
    /// with "-" (a "-_-") is still the message, not an option.
    #[test]
    fn a_friends_message_reaches_the_pop_up_whatever_it_starts_with() {
        let cli = Cli::try_parse_from([
            "kairo",
            "notify",
            "--friend",
            "--from=fabio",
            "--text=-_- --help",
            "--link=https://files.catbox.moe/a.mp4",
        ])
        .unwrap();
        let Some(Cmd::Notify {
            friend,
            from,
            text,
            link,
            request,
            ..
        }) = cli.command
        else {
            panic!("not a notify");
        };
        assert!(friend && !request);
        assert_eq!(from.as_deref(), Some("fabio"));
        assert_eq!(text.as_deref(), Some("-_- --help"));
        assert_eq!(link.as_deref(), Some("https://files.catbox.moe/a.mp4"));
    }
}
