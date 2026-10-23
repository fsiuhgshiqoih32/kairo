//! Operating-system specifics.
//!
//! Every backend provides:
//! - `init_process()`: DPI awareness and console attachment, first thing in `main`.
//! - `show_error(msg)`: an error the user sees even without a terminal.
//! - `monitors()`: the screens, in desktop pixels.
//! - `capture_sources(monitor)`: ways to capture a screen, best first.
//! - `foreground_title()`: the active window's title, for naming clips.
//! - `hide_console(cmd)`: start a child process without a console window.
//! - `hide_console_low_priority(cmd)`: the same, below normal priority (Windows only).
//! - `play_builtin(kind)`, `play_sound_file(path)`: the save sound (see `play_save_sound`).
//! - `free_space(dir)`: bytes free on a folder's drive (macOS and Linux share the one here).

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

/// The system asks apps for less motion: checked once, when first needed.
pub fn reduce_motion() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(animations_off)
}

/// One of the built-in save sounds (`kairo::config::SAVE_SOUNDS`) as a WAV.
pub fn save_sound_wav(kind: &str) -> Vec<u8> {
    use Wave::*;
    let n = |wave, from, to, secs, vol| Tone {
        wave,
        from,
        to,
        secs,
        vol,
    };
    match kind {
        "click" => chime(&[(2093.0, 0.012), (1568.0, 0.02)]),
        "pop" => chime(&[(784.0, 0.04), (523.25, 0.06)]),
        "bell" => chime(&[(1046.5, 0.07), (1318.5, 0.07), (1568.0, 0.14)]),
        // Two quick square notes, B5 then E6, the second held.
        "coin" => synth(&[
            n(Square, 987.8, 987.8, 0.07, 0.16),
            n(Square, 1318.5, 1318.5, 0.26, 0.16),
        ]),
        // C, E, G and the octave, rising.
        "level" => synth(&[
            n(Triangle, 523.25, 523.25, 0.07, 0.4),
            n(Triangle, 659.25, 659.25, 0.07, 0.4),
            n(Triangle, 783.99, 783.99, 0.07, 0.4),
            n(Triangle, 1046.5, 1046.5, 0.22, 0.4),
        ]),
        // Two short bursts of noise, like a shutter opening and closing.
        "shutter" => synth(&[
            n(Noise, 0.0, 0.0, 0.035, 0.3),
            n(Silence, 0.0, 0.0, 0.04, 0.0),
            n(Noise, 0.0, 0.0, 0.05, 0.22),
        ]),
        "whoosh" => synth(&[n(Swish, 0.0, 0.0, 0.28, 0.32)]),
        "laser" => synth(&[n(Square, 1800.0, 220.0, 0.18, 0.13)]),
        "bubble" => synth(&[n(Sine, 300.0, 1200.0, 0.12, 0.4)]),
        "retro" => synth(&[
            n(Square, 1046.5, 1046.5, 0.05, 0.13),
            n(Silence, 0.0, 0.0, 0.025, 0.0),
            n(Square, 1046.5, 1046.5, 0.05, 0.13),
            n(Silence, 0.0, 0.0, 0.025, 0.0),
            n(Square, 1568.0, 1568.0, 0.12, 0.13),
        ]),
        // A major third, soft.
        "success" => synth(&[
            n(Sine, 659.25, 659.25, 0.1, 0.35),
            n(Sine, 830.6, 830.6, 0.24, 0.35),
        ]),
        "drop" => synth(&[n(Sine, 1400.0, 380.0, 0.16, 0.38)]),
        "tick" => synth(&[n(Sine, 1760.0, 1760.0, 0.025, 0.25)]),
        _ => saved_sound_wav(),
    }
}

/// The kinds of tone `synth` makes.
#[derive(Clone, Copy, PartialEq)]
enum Wave {
    Sine,
    Square,
    Triangle,
    /// White noise.
    Noise,
    /// Noise swelling and fading, brighter in the middle: air moving past.
    Swish,
    /// Nothing, for a pause between tones.
    Silence,
}

/// One tone: its pitch slides from `from` to `to` Hz over `secs`; `vol` is 0 to 1.
struct Tone {
    wave: Wave,
    from: f32,
    to: f32,
    secs: f32,
    vol: f32,
}

/// Tones one after another as a mono 16-bit WAV, each with a quick attack and a smooth decay.
fn synth(tones: &[Tone]) -> Vec<u8> {
    let rate = 44_100.0f32;
    let mut samples: Vec<i16> = Vec::new();
    // A fixed seed: the same sound every time.
    let mut seed = 0x2545_f491u32;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32 * 2.0 - 1.0
    };
    let mut low = 0.0f32;
    for t in tones {
        let n = (rate * t.secs) as usize;
        let mut phase = 0.0f32;
        for i in 0..n {
            let k = i as f32 / n.max(1) as f32;
            let hz = t.from + (t.to - t.from) * k;
            phase = (phase + hz / rate).fract();
            let raw = match t.wave {
                Wave::Sine => (phase * std::f32::consts::TAU).sin(),
                Wave::Square => {
                    if phase < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                Wave::Triangle => 1.0 - 4.0 * (phase - 0.5).abs(),
                Wave::Noise => noise(),
                Wave::Swish => {
                    // Low-passed noise whose cut-off rises then falls.
                    let a = 0.05 + 0.5 * (k * std::f32::consts::PI).sin();
                    low += (noise() - low) * a;
                    low * 2.0
                }
                Wave::Silence => 0.0,
            };
            let env = match t.wave {
                Wave::Swish => (k * std::f32::consts::PI).sin().powf(1.5),
                _ => (i as f32 / 200.0).min(1.0) * (1.0 - k).powf(1.6),
            };
            let v = (raw * env * t.vol).clamp(-1.0, 1.0);
            samples.push((v * i16::MAX as f32) as i16);
        }
    }
    wav16(44_100, &samples)
}

/// A mono 16-bit WAV file.
fn wav16(rate: u32, samples: &[i16]) -> Vec<u8> {
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
    wav16(rate, &samples)
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

/// Bytes free for this user on the drive `dir` is on (`dir` must exist).
#[cfg(unix)]
// The fields' types differ between macOS and Linux.
#[allow(clippy::unnecessary_cast)]
pub fn free_space(dir: &std::path::Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: plain numbers, for statvfs to fill in.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path and a struct for the answer.
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

/// Bytes free where `path` would go: on the drive of its nearest folder that exists.
pub fn free_space_for(path: &std::path::Path) -> Option<u64> {
    path.ancestors().find(|d| d.is_dir()).and_then(free_space)
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
    fn free_space_where_a_folder_will_go() {
        let dir = tempfile::tempdir().unwrap();
        let free = free_space_for(&dir.path().join("not/made/yet"));
        assert!(free.is_some_and(|b| b > 0));
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
