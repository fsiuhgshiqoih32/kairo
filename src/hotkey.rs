//! The global shortcuts: save a clip, take a screenshot.

use std::rc::Rc;

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::log::warning;

pub struct Hotkey {
    /// Shared between the shortcuts (`sibling`): one manager per process.
    manager: Option<Rc<GlobalHotKeyManager>>,
    current: Option<(HotKey, String)>,
    /// Why the shortcut isn't working, for the settings window.
    pub error: Option<String>,
}

/// Whether `key` (a key's name, as Settings writes it) can be a shortcut on its own. A global
/// shortcut takes its key from every app, so keys people type with (letters, Space, Enter,
/// arrows) need Ctrl, Alt, Shift or Cmd; keys nobody types with (F1 to F24, Pause, Scroll Lock,
/// Print Screen, Insert, media keys) don't.
pub fn ok_alone(key: &str) -> bool {
    let k = key.trim();
    let f_key = k
        .strip_prefix('F')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=24).contains(&n));
    f_key
        || matches!(
            k,
            "Pause"
                | "ScrollLock"
                | "PrintScreen"
                | "Insert"
                | "MediaPlayPause"
                | "MediaStop"
                | "MediaTrackNext"
                | "MediaTrackPrevious"
        )
}

/// Parses "F8", "Ctrl+Shift+S" and the like.
pub fn parse(spec: &str) -> Result<HotKey, String> {
    spec.trim()
        .parse::<HotKey>()
        .map_err(|e| format!("\"{spec}\" isn't a shortcut Kairo understands ({e})"))
}

impl Hotkey {
    pub fn new() -> Self {
        match GlobalHotKeyManager::new() {
            Ok(m) => Hotkey {
                manager: Some(Rc::new(m)),
                current: None,
                error: None,
            },
            Err(e) => {
                let error = if crate::platform::is_wayland() {
                    "Wayland doesn't let apps register global shortcuts. Bind the command `kairo save` to a key \
                     in your desktop's keyboard settings instead."
                        .to_string()
                } else {
                    format!("Global shortcuts are unavailable: {e}")
                };
                warning!("{error}");
                Hotkey {
                    manager: None,
                    current: None,
                    error: Some(error),
                }
            }
        }
    }

    /// Another shortcut on the same manager.
    pub fn sibling(&self) -> Hotkey {
        Hotkey {
            manager: self.manager.clone(),
            current: None,
            error: None,
        }
    }

    /// Switches to `spec`, if it differs from the current shortcut. An empty `spec` turns it off.
    pub fn set(&mut self, spec: &str) {
        let Some(manager) = self.manager.clone() else {
            return;
        };
        if self.current.as_ref().is_some_and(|(_, s)| s == spec) {
            return;
        }
        if let Some((hk, _)) = self.current.take() {
            let _ = manager.unregister(hk);
        }
        self.error = None;
        if spec.trim().is_empty() {
            return;
        }
        match parse(spec) {
            Ok(hk) => match manager.register(hk) {
                Ok(()) => self.current = Some((hk, spec.to_string())),
                Err(e) => {
                    let msg = e.to_string();
                    self.error = Some(if msg.to_ascii_lowercase().contains("already") {
                        format!("Another app already uses {spec}. Pick another shortcut.")
                    } else {
                        format!("Couldn't register {spec}: {msg}")
                    });
                    warning!("{}", self.error.as_deref().unwrap_or_default());
                }
            },
            Err(e) => self.error = Some(e),
        }
    }

    pub fn label(&self) -> Option<&str> {
        self.current.as_ref().map(|(_, s)| s.as_str())
    }

    /// True for a press of our shortcut.
    pub fn is_press(&self, e: &GlobalHotKeyEvent) -> bool {
        e.state == HotKeyState::Pressed
            && self.current.as_ref().is_some_and(|(hk, _)| hk.id() == e.id)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_keys_nobody_types_work_alone() {
        for k in ["F8", "F12", "F24", "Pause", "ScrollLock", "Insert"] {
            assert!(super::ok_alone(k), "{k}");
        }
        for k in [
            "Space",
            "A",
            "1",
            "Enter",
            "ArrowUp",
            "F25",
            "Tab",
            "Backquote",
        ] {
            assert!(!super::ok_alone(k), "{k}");
        }
    }
}
