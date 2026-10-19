//! The background process: records into the replay buffer, owns the tray icon and the shortcut,
//! and saves clips. The settings window and `kairo save` talk to it through small files in the
//! config folder.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

pub fn run() -> Result<(), String> {
    let Some(lock) = single_instance::acquire_patiently(single_instance::DAEMON)? else {
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
        Request::Restart,
        Request::Quit,
    ] {
        let _ = take_request(r);
    }
    let shared = Shared::new(settings.clone());
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
            let (s, s2) = (shared.clone(), shared.clone());
            std::thread::Builder::new()
                .name("voice".into())
                .spawn(move || crate::voice::run(s, move || start_save(&s2, None, false)))
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
    let mut last_look = Instant::now();
    let mut no_game_since: Option<Instant> = None;
    let mut auto_paused = false;
    // The user resumed while Kairo waited: leave it be until a game shows up.
    let mut hold = false;
    let update_found: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mut threads = Some(threads);
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
                        let s = base.for_game(front.as_deref());
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
                    let before = base.for_game(front.as_deref());
                    let now = base.for_game(title.as_deref());
                    front = title;
                    if now != before {
                        hotkey.set(&now.hotkey);
                        shared.apply(now);
                    }
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
                    std::thread::spawn(move || look_for_update(&found));
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
            if take_request(Request::Quit) {
                info!("quit on request");
                quit = true;
            }
            if take_request(Request::Restart) {
                info!("restarting after an update");
                if let Err(e) = crate::app::spawn_self(&["daemon"]) {
                    error!("{e}");
                } else {
                    quit = true;
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
            // Join a running session before going (its files would be lost otherwise).
            if shared.session.lock().unwrap().is_some() {
                let (tx, rx) = std::sync::mpsc::channel();
                crate::session::stop(&shared, move |r| {
                    let _ = tx.send(r);
                });
                let _ = rx.recv_timeout(Duration::from_secs(600));
            }
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
fn look_for_update(found: &Mutex<Option<String>>) {
    match crate::updater::check() {
        Ok(Some(u)) => {
            *found.lock().unwrap() = Some(u.version.clone());
            let told = std::fs::read_to_string(announced_path()).unwrap_or_default();
            if told.trim() == u.version {
                return;
            }
            let _ = std::fs::write(announced_path(), &u.version);
            info!("Kairo {} is available", u.version);
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
    let title = platform::foreground_title()
        .and_then(|t| platform::clean_title(&t))
        .filter(|t| t != "Kairo");
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

/// The game or app in front, for naming clips.
fn game_title() -> Option<String> {
    platform::foreground_title()
        .and_then(|t| platform::clean_title(&t))
        .filter(|t| t != "Kairo")
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
    crate::session::stop(shared, move |r| {
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
                vec!["notify".into(), "--error".into(), e]
            }
        };
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        if let Err(e) = crate::app::spawn_self(&refs) {
            warning!("{e}");
        }
    });
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
    let title = platform::foreground_title()
        .and_then(|t| platform::clean_title(&t))
        .filter(|t| t != "Kairo");
    if settings.sound && shared.status.lock().unwrap().recording {
        platform::play_save_sound(&settings.save_sound, &settings.save_sound_file);
    }
    let shared = shared.clone();
    std::thread::spawn(move || {
        // Keep recording a moment after the press, so the clip doesn't end right at it.
        let post = settings.post_roll;
        if post > 0 {
            std::thread::sleep(std::time::Duration::from_secs(post as u64));
        }
        let secs = seconds.unwrap_or(settings.clip_seconds) + post;
        let result = save::save_inner(&shared, Some(secs), title.as_deref());
        if result.is_ok() && settings.fresh_after_save {
            shared.ring.lock().unwrap().clear();
            shared.marks.lock().unwrap().clear();
        }
        shared.saving.store(false, Ordering::SeqCst);
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
