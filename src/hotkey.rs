//! The global "save clip" shortcut.

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::log::warning;

pub struct Hotkey {
    manager: Option<GlobalHotKeyManager>,
    current: Option<(HotKey, String)>,
    /// Why the shortcut isn't working, for the settings window.
    pub error: Option<String>,
}

/// Parses "F8", "Ctrl+Shift+S" and the like.
pub fn parse(spec: &str) -> Result<HotKey, String> {
    spec.trim()
        .parse::<HotKey>()
        .map_err(|e| format!("\"{spec}\" isn't a shortcut Rewindr understands ({e})"))
}

impl Hotkey {
    pub fn new() -> Self {
        match GlobalHotKeyManager::new() {
            Ok(m) => Hotkey {
                manager: Some(m),
                current: None,
                error: None,
            },
            Err(e) => {
                let error = if crate::platform::is_wayland() {
                    "Wayland doesn't let apps register global shortcuts. Bind the command `rewindr save` to a key \
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

    /// Switches to `spec`, if it differs from the current shortcut.
    pub fn set(&mut self, spec: &str) {
        let Some(manager) = &self.manager else { return };
        if self.current.as_ref().is_some_and(|(_, s)| s == spec) {
            return;
        }
        if let Some((hk, _)) = self.current.take() {
            let _ = manager.unregister(hk);
        }
        self.error = None;
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
