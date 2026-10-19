//! Operating-system specifics.
//!
//! Every backend provides:
//! - `init_process()`: DPI awareness and console attachment, first thing in `main`.
//! - `show_error(msg)`: an error the user sees even without a terminal.
//! - `monitors()`: the screens, in desktop pixels.
//! - `capture_sources(monitor)`: ways to capture a screen, best first.
//! - `foreground_title()`: the active window's title, for naming clips.
//! - `hide_console(cmd)`: start a child process without a console window.
//! - `play_builtin(kind)`, `play_sound_file(path)`: the save sound (see `play_save_sound`).

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;
#[cfg(windows)]
pub mod windows_app_audio;
#[cfg(windows)]
pub mod windows_input;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub mod macos_audio;
#[cfg(target_os = "macos")]
pub use self::macos::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use self::linux::*;

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
compile_error!("Kairo supports Windows, macOS and Linux.");

/// A screen, in desktop pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub primary: bool,
    /// The system's handle: HMONITOR on Windows, CGDirectDisplayID on macOS, unused on Linux.
    pub handle: u64,
    /// Position in the system's own display list (DXGI output on Windows, AVFoundation screen on
    /// macOS), when known.
    pub system_index: Option<u32>,
}

impl Monitor {
    pub fn label(&self) -> String {
        let main = if self.primary { ", main" } else { "" };
        format!("{} ({}x{}{main})", self.name, self.w, self.h)
    }
}

/// The monitor a setting names: "primary", or a 1-based position in [`monitors`].
pub fn choose_monitor(setting: &str) -> Option<Monitor> {
    let all = monitors();
    let by_index = setting
        .trim()
        .parse::<usize>()
        .ok()
        .and_then(|i| all.get(i.checked_sub(1)?).cloned());
    by_index
        .or_else(|| all.iter().find(|m| m.primary).cloned())
        .or_else(|| all.first().cloned())
}

/// True on a Wayland session, where X11 screen capture sees only X11 windows and apps can't
/// register global shortcuts.
pub fn is_wayland() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t.eq_ignore_ascii_case("wayland")))
}

/// A short rising two-note chime (16-bit mono WAV) for "clip saved".
#[allow(dead_code)]
pub fn saved_sound_wav() -> Vec<u8> {
    chime(&[(880.0, 0.07), (1318.5, 0.11)])
}

/// One of the built-in save sounds (`kairo::config::SAVE_SOUNDS`) as a WAV.
/// The system asks apps for less motion: checked once, when first needed.
pub fn reduce_motion() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(animations_off)
}

pub fn save_sound_wav(kind: &str) -> Vec<u8> {
    match kind {
        "click" => chime(&[(2093.0, 0.012), (1568.0, 0.02)]),
        "pop" => chime(&[(784.0, 0.04), (523.25, 0.06)]),
        "bell" => chime(&[(1046.5, 0.07), (1318.5, 0.07), (1568.0, 0.14)]),
        _ => saved_sound_wav(),
    }
}

/// A WAV file of the user's for the save sound, if it looks like one (and isn't huge).
pub fn custom_sound(file: &str) -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from(file.trim());
    let meta = std::fs::metadata(&p).ok()?;
    if !meta.is_file() || meta.len() > 8 * 1024 * 1024 {
        return None;
    }
    let mut head = [0u8; 12];
    use std::io::Read;
    std::fs::File::open(&p).ok()?.read_exact(&mut head).ok()?;
    (&head[..4] == b"RIFF" && &head[8..12] == b"WAVE").then_some(p)
}

/// Plays the save sound picked in Settings.
pub fn play_save_sound(kind: &str, file: &str) {
    if kind == "custom"
        && let Some(p) = custom_sound(file)
    {
        play_sound_file(&p);
        return;
    }
    let kind = kairo::config::SAVE_SOUNDS
        .iter()
        .map(|(k, _)| *k)
        .find(|k| *k == kind)
        .unwrap_or("chime");
    play_builtin(kind);
}

/// A single short, high blip: a moment was marked.
#[allow(dead_code)]
pub fn marker_sound_wav() -> Vec<u8> {
    chime(&[(1568.0, 0.06)])
}

/// Notes (Hz, seconds) one after another as a mono 16-bit WAV. (macOS plays its own sounds.)
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn chime(notes: &[(f32, f32)]) -> Vec<u8> {
    let rate = 44_100u32;
    let notes = notes.iter().copied();
    let mut samples: Vec<i16> = Vec::new();
    for (freq, secs) in notes {
        let n = (rate as f32 * secs) as usize;
        for i in 0..n {
            let t = i as f32 / rate as f32;
            // Quick attack, smooth decay: a soft "blip" rather than a beep.
            let env = (i as f32 / 200.0).min(1.0) * (1.0 - i as f32 / n as f32).powf(1.6);
            let v = (t * freq * std::f32::consts::TAU).sin() * env * 0.35;
            samples.push((v * i16::MAX as f32) as i16);
        }
    }
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

/// Turns a window title into something usable in a file name.
pub fn clean_title(title: &str) -> Option<String> {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_alphanumeric() || " -_.()".contains(c) {
            out.push(c);
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    let cut: String = out
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(48)
        .collect();
    let cut = cut.trim().trim_end_matches('.').trim().to_string();
    (!cut.is_empty()).then_some(cut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_sounds_are_wavs() {
        for (kind, _) in kairo::config::SAVE_SOUNDS {
            let wav = save_sound_wav(kind);
            assert_eq!(&wav[..4], b"RIFF", "{kind}");
            assert_eq!(&wav[8..12], b"WAVE", "{kind}");
            assert!(wav.len() > 1000, "{kind} is too short");
        }
        assert!(custom_sound("no such file.wav").is_none());
    }

    #[test]
    fn looks_for_a_game_without_crashing() {
        // Whatever is in front while the tests run; it only has to answer.
        let _ = game_in_front();
        let _ = prefers_light();
    }

    #[test]
    fn titles() {
        assert_eq!(
            clean_title("Minecraft* 26.3").as_deref(),
            Some("Minecraft 26.3")
        );
        assert_eq!(clean_title("a/b\\c:d?").as_deref(), Some("a b c d"));
        assert_eq!(clean_title("   ").as_deref(), None);
        assert_eq!(clean_title(&"x".repeat(100)).unwrap().len(), 48);
    }
}
