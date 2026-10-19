//! The tray (menu bar) icon and its menu.

use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::recorder::Status;
use rewindr::art;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    Save,
    TogglePause,
    ToggleSession,
    OpenFolder,
    Settings,
    ToggleAutostart,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    save: MenuItem,
    pause: MenuItem,
    session: MenuItem,
    folder: MenuItem,
    settings: MenuItem,
    autostart: CheckMenuItem,
    quit: MenuItem,
    shown: Option<(bool, String)>,
}

fn save_label(seconds: u32, hotkey: Option<&str>) -> String {
    let what = if seconds.is_multiple_of(60) && seconds >= 60 {
        format!("{} min", seconds / 60)
    } else {
        format!("{seconds} s")
    };
    match hotkey {
        Some(k) => format!("Save last {what}\t{k}"),
        None => format!("Save last {what}"),
    }
}

impl Tray {
    pub fn new(
        seconds: u32,
        hotkey: Option<&str>,
        autostart: Option<bool>,
    ) -> Result<Self, String> {
        let save = MenuItem::new(save_label(seconds, hotkey), true, None);
        let pause = MenuItem::new("Pause recording", true, None);
        let session = MenuItem::new("Record a session", true, None);
        let folder = MenuItem::new("Open clips folder", true, None);
        let settings = MenuItem::new("Clips and settings...", true, None);
        let autostart_item = CheckMenuItem::new(
            "Start with computer",
            autostart.is_some(),
            autostart.unwrap_or(false),
            None,
        );
        let quit = MenuItem::new("Quit Rewindr", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &save,
            &pause,
            &session,
            &PredefinedMenuItem::separator(),
            &folder,
            &settings,
            &PredefinedMenuItem::separator(),
            &autostart_item,
            &quit,
        ])
        .map_err(|e| e.to_string())?;
        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Rewindr");
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icon(true)?);
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(icon(true)?);
        let icon = builder.build().map_err(|e| e.to_string())?;
        Ok(Tray {
            icon,
            save,
            pause,
            session,
            folder,
            settings,
            autostart: autostart_item,
            quit,
            shown: None,
        })
    }

    pub fn action_for(&self, id: &MenuId) -> Option<MenuAction> {
        [
            (self.save.id(), MenuAction::Save),
            (self.pause.id(), MenuAction::TogglePause),
            (self.session.id(), MenuAction::ToggleSession),
            (self.folder.id(), MenuAction::OpenFolder),
            (self.settings.id(), MenuAction::Settings),
            (self.autostart.id(), MenuAction::ToggleAutostart),
            (self.quit.id(), MenuAction::Quit),
        ]
        .into_iter()
        .find(|(i, _)| *i == id)
        .map(|(_, a)| a)
    }

    pub fn sync(
        &mut self,
        status: &Status,
        seconds: u32,
        hotkey: Option<&str>,
        autostart: Option<bool>,
    ) {
        self.save.set_text(save_label(seconds, hotkey));
        self.save.set_enabled(status.recording);
        self.pause.set_text(if status.paused {
            "Resume recording"
        } else {
            "Pause recording"
        });
        self.autostart.set_checked(autostart.unwrap_or(false));
        self.session.set_text(match &status.session {
            Some(s) => format!("Stop the session ({})", clock(s.seconds)),
            None => "Record a session".to_string(),
        });
        self.session
            .set_enabled(status.recording || status.session.is_some());
        let active = status.recording && !status.paused;
        let tip = if status.paused {
            "Rewindr - paused".to_string()
        } else if status.recording {
            match hotkey {
                Some(k) => format!("Rewindr - press {k} to save the last {seconds} s"),
                None => "Rewindr - recording".to_string(),
            }
        } else if let Some(e) = &status.error {
            format!("Rewindr - {}", e.chars().take(100).collect::<String>())
        } else {
            "Rewindr - starting".to_string()
        };
        let state = (active, tip.clone());
        if self.shown.as_ref() != Some(&state) {
            if let Ok(i) = icon(active) {
                #[cfg(target_os = "macos")]
                let _ = self.icon.set_icon_templated(Some(i));
                #[cfg(not(target_os = "macos"))]
                let _ = self.icon.set_icon(Some(i));
            }
            let _ = self.icon.set_tooltip(Some(tip));
            self.shown = Some(state);
        }
    }
}

/// "1:02:03" or "12:34".
fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn icon(active: bool) -> Result<Icon, String> {
    let size = if cfg!(target_os = "macos") {
        36
    } else if cfg!(target_os = "linux") {
        48
    } else {
        32
    };
    Icon::from_rgba(art::tray_icon(size, active), size as u32, size as u32)
        .map_err(|e| e.to_string())
}
