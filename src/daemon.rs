//! The background process: records into the replay buffer, owns the tray icon and the shortcut,
//! and saves clips. The settings window and `kairo save` talk to it through small files in the
//! config folder.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use global_hotkey::GlobalHotKeyEvent;
use serde::{Deserialize, Serialize};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::MenuEvent;

use kairo::config;

use crate::hotkey::Hotkey;
use crate::log::{error, info, warning};
use crate::recorder::{self, Shared, Status};
use crate::tray::{MenuAction, Tray};
use crate::voice::FollowUp;
use crate::{audio, autostart, platform, save, single_instance};

/// Requests other processes leave for the background process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Saves a clip; the file may hold how many seconds (else the settings' length).
    Save,
    Screenshot,
    Mark,
    /// Starts or stops recording a session.
    Session,
    TogglePause,
    /// Opens the sound devices again (Settings' Refresh), so a microphone connected since is
    /// used.
    ReopenAudio,
    /// After an update: start the new program file in our place.
    Restart,
    Quit,
}

impl Request {
    fn file(self) -> PathBuf {
        let name = match self {
            Request::Save => "save.request",
            Request::Screenshot => "screenshot.request",
            Request::Mark => "mark.request",
            Request::Session => "session.request",
            Request::TogglePause => "pause.request",
            Request::ReopenAudio => "audio.request",
            Request::Restart => "restart.request",
            Request::Quit => "quit.request",
        };
        config::config_dir().join(name)
    }
}

pub fn request(r: Request) -> Result<(), String> {
    let _ = std::fs::create_dir_all(config::config_dir());
    std::fs::write(r.file(), b"").map_err(|e| e.to_string())
}

/// Asks for a clip of the last `seconds`.
pub fn request_save(seconds: u32) -> Result<(), String> {
    let _ = std::fs::create_dir_all(config::config_dir());
    std::fs::write(Request::Save.file(), seconds.to_string()).map_err(|e| e.to_string())
}

fn take_request(r: Request) -> bool {
    std::fs::remove_file(r.file()).is_ok()
}

/// Takes a save request, with the seconds it asks for if any.
fn take_save_request() -> Option<Option<u32>> {
    let f = Request::Save.file();
    let text = std::fs::read_to_string(&f).ok();
    std::fs::remove_file(&f)
        .ok()
        .map(|_| text.and_then(|t| t.trim().parse().ok()))
}

/// What the settings window shows about the background process.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Report {
    pub status: Status,
    pub hotkey: Option<String>,
    pub hotkey_error: Option<String>,
    pub screenshot_hotkey: Option<String>,
    pub screenshot_hotkey_error: Option<String>,
    pub marker_hotkey: Option<String>,
    pub marker_hotkey_error: Option<String>,
    /// Problems with the quick-save shortcuts, in the settings' order.
    pub quick_save_errors: Vec<Option<String>>,
    pub session_hotkey: Option<String>,
    pub session_hotkey_error: Option<String>,
    #[serde(default)]
    pub share_hotkey: Option<String>,
    #[serde(default)]
    pub share_hotkey_error: Option<String>,
    /// A newer version the background process found, if any.
    #[serde(default)]
    pub update: Option<String>,
    /// Unix milliseconds of the last update; stale means the process is gone.
    pub updated_ms: i64,
}

fn report_path() -> PathBuf {
    config::config_dir().join("status.json")
}

pub fn read_report() -> Option<Report> {
    let r: Report = serde_json::from_str(&std::fs::read_to_string(report_path()).ok()?).ok()?;
    (recorder::now_us() / 1000 - r.updated_ms < 4000).then_some(r)
}

pub fn is_running() -> bool {
    single_instance::is_held(single_instance::DAEMON)
}

/// Starts the background process unless it's already running.
pub fn ensure_running() -> Result<(), String> {
    if !is_running() {
        crate::app::spawn_self(&["daemon"])?;
    }
    Ok(())
}

enum UserEvent {
    Hotkey(GlobalHotKeyEvent),
    Menu(MenuEvent),
}

/// `wait`: started by the old process restarting after an update, which saves a running
/// session before it goes; that can take minutes.
pub fn run(wait: bool) -> Result<(), String> {
    let lock = if wait {
        single_instance::acquire_within(single_instance::DAEMON, Duration::from_secs(3600))?
    } else {
        single_instance::acquire_patiently(single_instance::DAEMON)?
    };
    let Some(lock) = lock else {
        println!("Kairo is already running.");
        return Ok(());
    };
    let path = config::default_path();
    let (settings, warn) = config::load_or_create(&path);
    if let Some(w) = warn {
        warning!("{w}");
    }
    info!(
        "background process started (version {})",
        env!("CARGO_PKG_VERSION")
    );
    for r in [
        Request::Save,
        Request::Screenshot,
        Request::Mark,
        Request::Session,
        Request::TogglePause,
        Request::ReopenAudio,
        Request::Restart,
        Request::Quit,
    ] {
        let _ = take_request(r);
    }
    let shared = Shared::new(settings.clone());
    // Sessions a stopped Kairo didn't get to save (shut down, crashed, killed): listed before
    // anything records, then saved in the background.
    let leftovers = crate::session::leftovers();
    {
        let s = settings.clone();
        std::thread::spawn(move || {
            crate::session::sweep_parts(&s);
            for work in leftovers {
                info!("saving a session that was cut short: {}", work.display());
                match crate::session::recover(&work, &s) {
                    Ok(Some(r)) => tell(s.notify, Ok(r)),
                    Ok(None) => {}
                    Err(e) => tell(s.notify, Err(e)),
                }
            }
        });
    }
    let threads = vec![
        {
            let s = shared.clone();
            std::thread::Builder::new()
                .name("recorder".into())
                .spawn(move || recorder::run(s))
                .map_err(|e| e.to_string())?
        },
        {
            let s = shared.clone();
            std::thread::Builder::new()
                .name("audio".into())
                .spawn(move || audio::run(s))
                .map_err(|e| e.to_string())?
        },
        {
            let s = shared.clone();
            std::thread::Builder::new()
                .name("webcam".into())
                .spawn(move || crate::webcam::run(s))
                .map_err(|e| e.to_string())?
        },
        {
            let s = shared.clone();
            std::thread::Builder::new()
                .name("friends".into())
                .spawn(move || crate::friends::run(s))
                .map_err(|e| e.to_string())?
        },
        {
            let (s, s2, s3) = (shared.clone(), shared.clone(), shared.clone());
            let spoken = Arc::new(Mutex::new(Spoken::default()));
            let spoken2 = spoken.clone();
            std::thread::Builder::new()
                .name("voice".into())
                .spawn(move || {
                    crate::voice::run(
                        s,
                        move || {
                            let before = s2.status.lock().unwrap().last_clip.clone();
                            *spoken.lock().unwrap() = Spoken { before, clip: None };
                            start_save(&s2, None, false)
                        },
                        move |f| do_spoken(&s3, &spoken2, f),
                    )
                })
                .map_err(|e| e.to_string())?
        },
    ];

    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let proxy = event_loop.create_proxy();
    GlobalHotKeyEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::Hotkey(e));
    }));
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::Menu(e));
    }));

    let mut hotkey = Hotkey::new();
    hotkey.set(&settings.hotkey);
    let mut shot_key = hotkey.sibling();
    shot_key.set(&settings.screenshot_hotkey);
    let mut mark_key = hotkey.sibling();
    mark_key.set(&settings.marker_hotkey);
    let mut session_key = hotkey.sibling();
    session_key.set(&settings.session_hotkey);
    let mut share_key = hotkey.sibling();
    share_key.set(&settings.share_hotkey);
    shared
        .session_wanted
        .store(settings.session_auto, Ordering::SeqCst);
    let mut quick: Vec<Hotkey> = (0..kairo::config::MAX_QUICK_SAVES)
        .map(|_| hotkey.sibling())
        .collect();
    set_quick_keys(&mut quick, &settings);
    #[cfg(windows)]
    let mut input_hooks = settings
        .input_overlay
        .then(|| crate::platform::windows_input::start(shared.clone()))
        .and_then(|r| r.map_err(|e| warning!("{e}")).ok());
    autostart::move_old_entry();
    let launcher = autostart::launcher();
    let mut tray: Option<Tray> = None;
    let mut mtime = config::mtime(&path);
    let mut last_tick = Instant::now() - Duration::from_secs(1);
    // When the clips folder was last tidied (old clips out, Recently deleted emptied).
    let mut last_tidy: Option<Instant> = None;
    // Updates: a first look a few minutes after starting, then every few hours.
    let mut next_update_check = Instant::now() + FIRST_UPDATE_CHECK;
    // Games-only recording and per-game settings: the main settings as saved, the window in
    // front, and whether Kairo paused itself (as opposed to the user pausing).
    let mut base = settings.clone();
    let mut front: Option<String> = None;
    // The game whose own settings are in use (None: the main settings).
    let mut applied: Option<String> = None;
    let mut last_look = Instant::now();
    let mut no_game_since: Option<Instant> = None;
    let mut auto_paused = false;
    // The user resumed while Kairo waited: leave it be until a game shows up.
    let mut hold = false;
    let update_found: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mut threads = Some(threads);
    let started = Instant::now();
    let mut told_stopped = false;
    let _lock = lock;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(250));
        let mut quit = false;
        match event {
            Event::NewEvents(StartCause::Init) => {
                let s = shared.settings();
                let auto = launcher.as_ref().map(autostart::is_enabled);
                match Tray::new(s.clip_seconds, hotkey.label(), auto) {
                    Ok(t) => tray = Some(t),
                    Err(e) => error!("tray icon unavailable: {e}"),
                }
            }
            Event::UserEvent(UserEvent::Hotkey(e)) => {
                if hotkey.is_press(&e) {
                    start_save(&shared, None, false);
                } else if share_key.is_press(&e) {
                    start_save(&shared, None, true);
                } else if shot_key.is_press(&e) {
                    start_screenshot(&shared);
                } else if mark_key.is_press(&e) {
                    mark(&shared);
                } else if session_key.is_press(&e) {
                    toggle_session(&shared);
                } else if let Some(i) = quick.iter().position(|k| k.is_press(&e)) {
                    let secs = shared.settings().quick_saves.get(i).map(|q| q.seconds);
                    start_save(&shared, secs, false);
                }
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                match tray.as_ref().and_then(|t| t.action_for(e.id())) {
                    Some(MenuAction::Save) => start_save(&shared, None, false),
                    Some(MenuAction::Update) => {
                        if let Err(e) = crate::app::spawn_self(&["window", "--update"]) {
                            warning!("{e}");
                        }
                    }
                    Some(MenuAction::TogglePause) => toggle_pause(&shared),
                    Some(MenuAction::ToggleSession) => toggle_session(&shared),
                    Some(MenuAction::OpenFolder) => {
                        let dir = shared.settings().save_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        crate::app::open(&dir);
                    }
                    Some(MenuAction::Settings) => {
                        if let Err(e) = crate::app::spawn_self(&["window"]) {
                            warning!("{e}");
                        }
                    }
                    Some(MenuAction::TogglePopups) => {
                        // Through the settings file, which the window and the loop below follow.
                        match config::load(&path) {
                            Ok(mut s) => {
                                s.notify = !s.notify;
                                if let Err(e) = config::save(&path, &s) {
                                    warning!("{e}");
                                }
                            }
                            Err(e) => warning!("{e}"),
                        }
                    }
                    Some(MenuAction::ToggleAutostart) => {
                        if let Some(l) = &launcher {
                            autostart::set_enabled(l, !autostart::is_enabled(l));
                        }
                    }
                    Some(MenuAction::Quit) => {
                        info!("quit from the tray");
                        quit = true;
                    }
                    None => {}
                }
                last_tick = Instant::now() - Duration::from_secs(1);
            }
            _ => {}
        }
        if last_tick.elapsed() >= Duration::from_millis(250) {
            last_tick = Instant::now();
            // Settings edited in the window (or by hand).
            let m = config::mtime(&path);
            if m != mtime {
                mtime = m;
                match config::load(&path) {
                    Ok(s) => {
                        base = s.clone();
                        let s = base.for_game(applied.as_deref());
                        hotkey.set(&s.hotkey);
                        shot_key.set(&s.screenshot_hotkey);
                        mark_key.set(&s.marker_hotkey);
                        session_key.set(&s.session_hotkey);
                        share_key.set(&s.share_hotkey);
                        set_quick_keys(&mut quick, &s);
                        #[cfg(windows)]
                        if s.input_overlay != input_hooks.is_some() {
                            input_hooks = s
                                .input_overlay
                                .then(|| crate::platform::windows_input::start(shared.clone()))
                                .and_then(|r| r.map_err(|e| warning!("{e}")).ok());
                        }
                        if s.keep_days != shared.settings().keep_days
                            || s.save_dir() != shared.settings().save_dir()
                        {
                            last_tidy = None;
                        }
                        shared.apply(s);
                    }
                    Err(e) => warning!("{e}"),
                }
            }
            if let Some(seconds) = take_save_request() {
                start_save(&shared, seconds, false);
            }
            if last_look.elapsed() >= Duration::from_secs(2) {
                last_look = Instant::now();
                let title = game_title();
                if title != front {
                    // A game's own settings come in with it and stay while another app is in
                    // front for a moment: switching back would restart the recorder and lose
                    // the replay. Another game (or one with its own settings) takes over.
                    let next = if base
                        .rule_for(title.as_deref())
                        .is_some_and(|r| r.kind != "never")
                    {
                        title.clone()
                    } else if platform::game_in_front() == Some(true) {
                        None
                    } else {
                        applied.clone()
                    };
                    if next != applied {
                        let before = base.for_game(applied.as_deref());
                        let now = base.for_game(next.as_deref());
                        applied = next;
                        if now != before {
                            hotkey.set(&now.hotkey);
                            shared.apply(now);
                        }
                    }
                    front = title;
                }
                let rule = base.rule_for(front.as_deref()).cloned();
                let private = rule.as_ref().is_some_and(|r| r.kind == "never");
                let want = !private
                    && (base.record_when != "games"
                        || rule.as_ref().is_some_and(|r| r.kind == "game")
                        || platform::game_in_front().unwrap_or(true));
                let in_session = shared.session.lock().unwrap().is_some();
                let paused = shared.paused.load(Ordering::SeqCst);
                if auto_paused && !paused {
                    // Resumed from the tray or a shortcut while waiting.
                    auto_paused = false;
                    hold = true;
                }
                if want {
                    no_game_since = None;
                    hold = false;
                    if auto_paused {
                        shared.paused.store(false, Ordering::SeqCst);
                        auto_paused = false;
                        info!("a game is in front: recording again");
                    }
                } else if !in_session && !hold && !paused {
                    let since = *no_game_since.get_or_insert_with(Instant::now);
                    // A private app stops the replay at once; no game, after a while.
                    if private || since.elapsed() >= NO_GAME_GRACE {
                        shared.paused.store(true, Ordering::SeqCst);
                        auto_paused = true;
                        info!(
                            "{}",
                            if private {
                                "a private app is in front: paused"
                            } else {
                                "no game in front: paused"
                            }
                        );
                    }
                }
                shared.status.lock().unwrap().waiting = auto_paused;
            }
            if Instant::now() >= next_update_check {
                next_update_check = Instant::now() + UPDATE_EVERY;
                let s = shared.settings();
                if s.check_updates && std::env::var_os("KAIRO_NO_UPDATE_CHECK").is_none() {
                    let found = update_found.clone();
                    std::thread::spawn(move || look_for_update(&found, s.notify));
                }
            }
            if take_request(Request::Screenshot) {
                start_screenshot(&shared);
            }
            if take_request(Request::Mark) {
                mark(&shared);
            }
            if take_request(Request::Session) {
                toggle_session(&shared);
            }
            if crate::session::tend(&shared, game_title) {
                finish_session(&shared);
            }
            if last_tidy.is_none_or(|t| t.elapsed() >= Duration::from_secs(3600)) {
                last_tidy = Some(Instant::now());
                let s = shared.settings();
                std::thread::spawn(move || crate::save::tidy_folder(&s));
            }
            if take_request(Request::TogglePause) {
                toggle_pause(&shared);
            }
            if take_request(Request::ReopenAudio) {
                info!("opening the sound devices again on request");
                shared.audio_generation.fetch_add(1, Ordering::SeqCst);
            }
            if take_request(Request::Quit) {
                info!("quit on request");
                quit = true;
            }
            if take_request(Request::Restart) {
                info!("restarting after an update");
                if let Err(e) = crate::app::spawn_self(&["daemon", "--wait"]) {
                    error!("{e}");
                } else {
                    quit = true;
                }
            }
            // The workers run until Kairo quits; one that stopped panicked (the log says
            // where). Start over rather than carry on without recording, unless it happened
            // straight away, where starting over would only repeat it.
            let stopped = threads
                .as_ref()
                .and_then(|ts| ts.iter().find(|t| t.is_finished()))
                .map(|t| t.thread().name().unwrap_or("worker").to_string());
            if let Some(name) = stopped
                && !quit
                && !told_stopped
            {
                told_stopped = true;
                if started.elapsed() < Duration::from_secs(60) {
                    error!("the {name} thread stopped right after starting");
                } else {
                    error!("the {name} thread stopped; starting Kairo again");
                    match crate::app::spawn_self(&["daemon", "--wait"]) {
                        Ok(_) => quit = true,
                        Err(e) => error!("{e}"),
                    }
                }
            }
            let status = shared.status();
            let s = shared.settings();
            let update = update_found.lock().unwrap().clone();
            if let Some(t) = &mut tray {
                t.sync(
                    &status,
                    s.clip_seconds,
                    hotkey.label(),
                    launcher.as_ref().map(autostart::is_enabled),
                    s.notify,
                    update.as_deref(),
                );
            }
            let report = Report {
                status,
                hotkey: hotkey.label().map(String::from),
                hotkey_error: hotkey.error.clone(),
                screenshot_hotkey: shot_key.label().map(String::from),
                screenshot_hotkey_error: shot_key.error.clone(),
                marker_hotkey: mark_key.label().map(String::from),
                marker_hotkey_error: mark_key.error.clone(),
                quick_save_errors: quick.iter().map(|k| k.error.clone()).collect(),
                session_hotkey: session_key.label().map(String::from),
                session_hotkey_error: session_key.error.clone(),
                share_hotkey: share_key.label().map(String::from),
                share_hotkey_error: share_key.error.clone(),
                update,
                updated_ms: recorder::now_us() / 1000,
            };
            if let Ok(text) = serde_json::to_string(&report) {
                let tmp = report_path().with_extension("json.tmp");
                if std::fs::write(&tmp, text).is_ok() {
                    let _ = std::fs::rename(&tmp, report_path());
                }
            }
        }
        if quit {
            // Save a running session, and any being saved, before going: however long that
            // takes (ffmpeg's own time limit bounds it). A restart's new process waits for us.
            finish_session(&shared);
            crate::session::wait_for_joins();
            shared.stop.store(true, Ordering::SeqCst);
            if let Some(ts) = threads.take() {
                for t in ts {
                    let _ = t.join();
                }
            }
            let _ = std::fs::remove_file(report_path());
            tray = None;
            *control_flow = ControlFlow::Exit;
        }
    })
}

/// How long no game may be in front before games-only recording pauses.
const NO_GAME_GRACE: Duration = Duration::from_secs(20);

/// When the first look for updates happens, and how often after that.
const FIRST_UPDATE_CHECK: Duration = Duration::from_secs(3 * 60);
const UPDATE_EVERY: Duration = Duration::from_secs(6 * 3600);

/// The version the last update notice was about, so each version is announced once.
fn announced_path() -> PathBuf {
    config::config_dir().join("update-announced.txt")
}

/// Asks GitHub for a newer version; the first time one turns up, shows a notice about it.
fn look_for_update(found: &Mutex<Option<String>>, popup: bool) {
    match crate::updater::check() {
        Ok(Some(u)) => {
            *found.lock().unwrap() = Some(u.version.clone());
            let told = std::fs::read_to_string(announced_path()).unwrap_or_default();
            if told.trim() == u.version {
                return;
            }
            let _ = std::fs::write(announced_path(), &u.version);
            info!("Kairo {} is available", u.version);
            // With pop-ups off, the tray menu and the window still offer it.
            if !popup {
                return;
            }
            let mut args = vec!["notify".to_string(), "--update".into(), u.version.clone()];
            if let Some(point) = crate::updater::highlights(&u.notes, 1).into_iter().next() {
                args.push("--about".into());
                args.push(point);
            }
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            if let Err(e) = crate::app::spawn_self(&refs) {
                warning!("{e}");
            }
        }
        Ok(None) => *found.lock().unwrap() = None,
        Err(e) => info!("update check: {e}"),
    }
}

fn toggle_pause(shared: &Shared) {
    let now = !shared.paused.load(Ordering::SeqCst);
    shared.paused.store(now, Ordering::SeqCst);
    info!("{}", if now { "paused" } else { "resumed" });
}

/// Takes a screenshot on a worker thread, then plays the sound and shows the notification.
fn start_screenshot(shared: &Arc<Shared>) {
    let settings = shared.settings();
    let title = game_title();
    if settings.sound && shared.status.lock().unwrap().recording {
        platform::play_save_sound(&settings.save_sound, &settings.save_sound_file);
    }
    let shared = shared.clone();
    std::thread::spawn(move || {
        let args: Vec<String> = match save::screenshot(&shared, title.as_deref()) {
            Ok(p) if settings.notify => vec![
                "notify".into(),
                "--screenshot".into(),
                p.display().to_string(),
            ],
            Ok(_) => return,
            Err(e) => {
                warning!("{e}");
                if !settings.notify {
                    return;
                }
                vec!["notify".into(), "--error".into(), e]
            }
        };
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        if let Err(e) = crate::app::spawn_self(&refs) {
            warning!("{e}");
        }
    });
}

/// The game or app in front, for naming clips. Kairo's own windows ("Kairo", or "<clip> -
/// Kairo" in the editor) don't count.
fn game_title() -> Option<String> {
    platform::foreground_title()
        .and_then(|t| platform::clean_title(&t))
        .filter(|t| t != "Kairo" && !t.ends_with(" - Kairo"))
}

/// Starts recording a session, or stops the one running.
fn toggle_session(shared: &Arc<Shared>) {
    let on = !shared.session_wanted.load(Ordering::SeqCst);
    shared.session_wanted.store(on, Ordering::SeqCst);
    if on {
        if let Err(e) = crate::session::start(shared, game_title()) {
            warning!("couldn't start a session yet: {e}");
        }
    } else {
        finish_session(shared);
    }
}

/// Stops the session and joins it, then shows it like a saved clip.
fn finish_session(shared: &Arc<Shared>) {
    let notify = shared.settings().notify;
    crate::session::stop(shared, move |r| tell(notify, r));
}

/// Shows a saved session like a saved clip, or why it couldn't be saved.
fn tell(notify: bool, r: Result<(PathBuf, f64), String>) {
    let args: Vec<String> = match r {
        Ok((path, seconds)) if notify => vec![
            "notify".into(),
            "--clip".into(),
            path.display().to_string(),
            "--seconds".into(),
            format!("{seconds:.1}"),
        ],
        Ok(_) => return,
        Err(e) => {
            warning!("{e}");
            if !notify {
                return;
            }
            vec!["notify".into(), "--error".into(), e]
        }
    };
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Err(e) = crate::app::spawn_self(&refs) {
        warning!("{e}");
    }
}

/// Points the quick-save shortcuts at the settings' (unused slots are off).
fn set_quick_keys(keys: &mut [Hotkey], s: &config::Settings) {
    for (i, k) in keys.iter_mut().enumerate() {
        k.set(s.quick_saves.get(i).map_or("", |q| q.hotkey.as_str()));
    }
}

/// Marks this moment for the clip that will hold it, with a short tick.
fn mark(shared: &Shared) {
    if !shared.status.lock().unwrap().recording {
        return;
    }
    let n = shared.mark();
    if shared.settings().sound {
        platform::play_marker_sound();
    }
    info!("marked a moment ({n} in the buffer)");
}

/// Saves a clip of the last `seconds` (the settings' length if `None`) on a worker thread, then
/// plays the sound and shows the notification.
/// Saves the last `seconds` (the usual length when `None`). With `share`, the notice that
/// follows uploads the clip and copies its link.
/// The clip that what's said after the voice phrase ("rename it to ...", "trim it to ...") is
/// about: the one the phrase saved, under its latest name.
#[derive(Default)]
struct Spoken {
    /// The newest clip before the phrase was heard, to tell the new one from it.
    before: Option<String>,
    clip: Option<PathBuf>,
}

/// Does what was said about the voice-saved clip, on its own thread (its save may still be
/// running). One at a time, in order: "rename it", then "trim it", trims the renamed clip.
fn do_spoken(shared: &Arc<Shared>, spoken: &Arc<Mutex<Spoken>>, said: FollowUp) {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let (shared, spoken) = (shared.clone(), spoken.clone());
    std::thread::spawn(move || {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let (before, known) = {
            let s = spoken.lock().unwrap();
            (s.before.clone(), s.clip.clone())
        };
        let Some(clip) = known.or_else(|| voice_saved(&shared, before.as_deref())) else {
            warning!("{said:?}: no clip was saved to do it to");
            return;
        };
        let done = match &said {
            FollowUp::Rename(name) => rename_spoken(&clip, name),
            FollowUp::Trim(seconds) => trim_spoken(&clip, *seconds),
        };
        let settings = shared.settings();
        match done {
            Ok((path, seconds)) => {
                info!("{said:?}: {} ({seconds:.1} s)", path.display());
                {
                    let mut s = spoken.lock().unwrap();
                    // Unless the phrase was said again meanwhile, for a new clip.
                    if s.before == before {
                        s.clip = Some(path.clone());
                    }
                }
                shared.status.lock().unwrap().last_clip = Some(path.display().to_string());
                if settings.sound {
                    platform::play_save_sound(&settings.save_sound, &settings.save_sound_file);
                }
                tell(settings.notify, Ok((path, seconds)));
            }
            Err(e) => {
                let what = match said {
                    FollowUp::Rename(_) => "rename",
                    FollowUp::Trim(_) => "trim",
                };
                tell(
                    settings.notify,
                    Err(format!("Couldn't {what} the clip: {e}")),
                );
            }
        }
    });
}

/// The clip the voice phrase saved, once saved (the save records a moment past the phrase
/// first). `None` if saving failed or took over a minute.
fn voice_saved(shared: &Shared, before: Option<&str>) -> Option<PathBuf> {
    let start = Instant::now();
    while shared.saving.load(Ordering::SeqCst) {
        if start.elapsed() > Duration::from_secs(60) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let last = shared.status.lock().unwrap().last_clip.clone()?;
    (Some(last.as_str()) != before).then(|| PathBuf::from(last))
}

/// `f` again for a few seconds while it fails: the window's preview may have the file open.
fn retry<T>(mut f: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut tries = 0;
    loop {
        match f() {
            Err(_) if tries < 15 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
            done => return done,
        }
    }
}

/// A clip's length in seconds (0 if ffmpeg can't tell).
fn length(clip: &Path) -> f64 {
    crate::ffmpeg::path()
        .and_then(|ff| crate::ffmpeg::probe_clip(&ff, clip))
        .map_or(0.0, |i| i.duration)
}

/// Renames a clip as said, with a number added if that name is taken. Returns the new path and
/// the clip's length.
fn rename_spoken(clip: &Path, name: &str) -> Result<(PathBuf, f64), String> {
    let to = (1..10)
        .map(|n| match n {
            1 => name.to_string(),
            n => format!("{name} {n}"),
        })
        .find_map(|n| kairo::library::rename_target(clip, &n).ok())
        .ok_or("that name is taken")?;
    if to != clip {
        retry(|| std::fs::rename(clip, &to)).map_err(|e| kairo::library::why(&e))?;
        save::move_sidecars(clip, &to);
        kairo::library::Library::edit(|l| l.renamed(clip, &to))?;
    }
    Ok((to.clone(), length(&to)))
}

/// Keeps the last `seconds` of a clip, copied rather than encoded again (so from the keyframe
/// at or before). The whole clip waits in Recently deleted, so the trim can be undone.
fn trim_spoken(clip: &Path, seconds: u32) -> Result<(PathBuf, f64), String> {
    let ff = crate::ffmpeg::path()?;
    let info = crate::ffmpeg::probe_clip(&ff, clip)?;
    if info.duration <= seconds as f64 + 0.5 {
        return Ok((clip.to_path_buf(), info.duration));
    }
    let start = info.duration - seconds as f64;
    let part = clip.with_extension("mp4.part");
    let args = kairo::media::copy_args(clip, start, info.duration, &part);
    if let Err(e) = crate::ffmpeg::run(&ff, &args, Duration::from_secs(120)) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    let dir = clip.parent().unwrap_or(Path::new("."));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if let Err(e) = retry(|| kairo::library::trash(dir, clip, now)) {
        let _ = std::fs::remove_file(&part);
        return Err(kairo::library::why(&e));
    }
    std::fs::rename(&part, clip).map_err(|e| e.to_string())?;
    // Marks move with the picture.
    let left = length(clip);
    let cut = info.duration - left;
    let _ = kairo::library::Library::edit(|l| {
        let marks = l
            .markers(clip)
            .iter()
            .map(|m| m - cut)
            .filter(|m| *m >= 0.0)
            .collect();
        l.set_markers(clip, marks);
    });
    Ok((clip.to_path_buf(), left))
}

fn start_save(shared: &Arc<Shared>, seconds: Option<u32>, share: bool) {
    // During a session, a save is a bookmark too.
    if shared.session.lock().unwrap().is_some() {
        shared.mark();
    }
    // Held from the press: a second press while this one waits out its post-roll is ignored.
    if shared.saving.swap(true, Ordering::SeqCst) {
        return;
    }
    let settings = shared.settings();
    // The window in front when the key was pressed names the clip (usually the game).
    let title = game_title();
    if settings.sound && shared.status.lock().unwrap().recording {
        platform::play_save_sound(&settings.save_sound, &settings.save_sound_file);
    }
    let shared = shared.clone();
    std::thread::spawn(move || {
        let done = save::SavingFlag(&shared.saving);
        // Keep recording a moment after the press, so the clip doesn't end right at it.
        let post = settings.post_roll;
        if post > 0 {
            std::thread::sleep(std::time::Duration::from_secs(post as u64));
        }
        let secs = seconds.unwrap_or(settings.clip_seconds) + post;
        let result = save::save_inner(&shared, Some(secs), title.as_deref());
        if result.is_ok() && settings.fresh_after_save {
            // Not `clear`: that ends the stream's tape, cutting a running session in two.
            shared.ring.lock().unwrap().forget_held();
            // A session's bookmarks stay; the next clip starts after this one anyway.
            if shared.session.lock().unwrap().is_none() {
                shared.marks.lock().unwrap().clear();
            }
        }
        drop(done);
        if !settings.notify && !(share && result.is_ok()) {
            if let Err(e) = result {
                warning!("{e}");
            }
            return;
        }
        let args: Vec<String> = match &result {
            Ok(clip) => {
                let mut a = vec![
                    "notify".into(),
                    "--clip".into(),
                    clip.path.display().to_string(),
                    "--seconds".into(),
                    format!("{:.1}", clip.seconds),
                ];
                if share {
                    a.push("--share".into());
                }
                a
            }
            Err(e) => {
                warning!("{e}");
                vec!["notify".into(), "--error".into(), e.clone()]
            }
        };
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        if let Err(e) = crate::app::spawn_self(&refs) {
            warning!("{e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "rename it to ..." then "trim it to ...", on a real clip: the name, the length, the
    /// marks moving with the picture and the whole clip kept in Recently deleted. Needs ffmpeg
    /// with libx264 and `KAIRO_HOME` set to a scratch folder (clips.json goes there).
    #[test]
    #[ignore]
    fn spoken_rename_then_trim() {
        assert!(std::env::var_os("KAIRO_HOME").is_some(), "set KAIRO_HOME");
        let tmp = tempfile::tempdir().unwrap();
        let clip = tmp.path().join("Minecraft 2026-10-22 23-00-00.mp4");
        let ff = crate::ffmpeg::path().unwrap();
        let made = crate::ffmpeg::command(&ff)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=30",
            ])
            .args(["-f", "lavfi", "-i", "sine=frequency=440", "-t", "35"])
            .args(["-c:v", "libx264", "-g", "60", "-c:a", "aac"])
            .arg(&clip)
            .status()
            .unwrap();
        assert!(made.success());
        kairo::library::Library::edit(|l| l.set_markers(&clip, vec![5.0, 30.0])).unwrap();

        let (named, secs) = rename_spoken(&clip, "Insane Clutch").unwrap();
        assert_eq!(named, tmp.path().join("Insane Clutch.mp4"));
        assert!(!clip.exists() && named.exists());
        assert!((secs - 35.0).abs() < 0.5, "{secs}");
        // Taken: the next one gets a number.
        std::fs::copy(&named, &clip).unwrap();
        let (second, _) = rename_spoken(&clip, "Insane Clutch").unwrap();
        assert_eq!(second, tmp.path().join("Insane Clutch 2.mp4"));

        let (trimmed, left) = trim_spoken(&named, 10).unwrap();
        assert_eq!(trimmed, named);
        // From the keyframe at or before 25 s: up to the 2 s keyframe interval more.
        assert!((10.0..=12.5).contains(&left), "{left}");
        let lib = kairo::library::Library::load();
        let marks = lib.markers(&named);
        assert_eq!(marks.len(), 1, "{marks:?}");
        assert!((marks[0] - (30.0 - (35.0 - left))).abs() < 0.2, "{marks:?}");
        let bin = kairo::library::trashed(tmp.path());
        assert!(
            bin.iter().any(|t| t.path.file_name() == named.file_name()),
            "the whole clip is in Recently deleted"
        );
        // Already short enough: nothing to do.
        let (same, again) = trim_spoken(&named, 30).unwrap();
        assert_eq!(same, named);
        assert!((again - left).abs() < 0.1);
    }
}
