//! The background process: records into the replay buffer, owns the tray icon and the shortcut,
//! and saves clips. The settings window and `rewindr save` talk to it through small files in the
//! config folder.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use global_hotkey::GlobalHotKeyEvent;
use serde::{Deserialize, Serialize};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::MenuEvent;

use rewindr::config;

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
        println!("Rewindr is already running.");
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
                .spawn(move || crate::voice::run(s, move || start_save(&s2, None)))
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
    shared
        .session_wanted
        .store(settings.session_auto, Ordering::SeqCst);
    let mut quick: Vec<Hotkey> = (0..rewindr::config::MAX_QUICK_SAVES)
        .map(|_| hotkey.sibling())
        .collect();
    set_quick_keys(&mut quick, &settings);
    let launcher = autostart::launcher();
    let mut tray: Option<Tray> = None;
    let mut mtime = config::mtime(&path);
    let mut last_tick = Instant::now() - Duration::from_secs(1);
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
                    start_save(&shared, None);
                } else if shot_key.is_press(&e) {
                    start_screenshot(&shared);
                } else if mark_key.is_press(&e) {
                    mark(&shared);
                } else if session_key.is_press(&e) {
                    toggle_session(&shared);
                } else if let Some(i) = quick.iter().position(|k| k.is_press(&e)) {
                    let secs = shared.settings().quick_saves.get(i).map(|q| q.seconds);
                    start_save(&shared, secs);
                }
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                match tray.as_ref().and_then(|t| t.action_for(e.id())) {
                    Some(MenuAction::Save) => start_save(&shared, None),
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
                        hotkey.set(&s.hotkey);
                        shot_key.set(&s.screenshot_hotkey);
                        mark_key.set(&s.marker_hotkey);
                        session_key.set(&s.session_hotkey);
                        set_quick_keys(&mut quick, &s);
                        shared.apply(s);
                    }
                    Err(e) => warning!("{e}"),
                }
            }
            if let Some(seconds) = take_save_request() {
                start_save(&shared, seconds);
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
            if let Some(t) = &mut tray {
                t.sync(
                    &status,
                    s.clip_seconds,
                    hotkey.label(),
                    launcher.as_ref().map(autostart::is_enabled),
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
        .filter(|t| t != "Rewindr");
    if settings.sound && shared.status.lock().unwrap().recording {
        platform::play_saved_sound();
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
        .filter(|t| t != "Rewindr")
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
fn start_save(shared: &Arc<Shared>, seconds: Option<u32>) {
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
        .filter(|t| t != "Rewindr");
    if settings.sound && shared.status.lock().unwrap().recording {
        platform::play_saved_sound();
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
        if !settings.notify {
            if let Err(e) = result {
                warning!("{e}");
            }
            return;
        }
        let args: Vec<String> = match &result {
            Ok(clip) => vec![
                "notify".into(),
                "--clip".into(),
                clip.path.display().to_string(),
                "--seconds".into(),
                format!("{:.1}", clip.seconds),
            ],
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
