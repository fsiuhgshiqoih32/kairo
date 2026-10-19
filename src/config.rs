//! Settings, stored as TOML in the user's config folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Highest output resolution: the screen is scaled down to this height if it's taller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Resolution {
    Native,
    #[serde(rename = "1440p")]
    P1440,
    #[serde(rename = "1080p")]
    P1080,
    #[serde(rename = "720p")]
    P720,
}

impl Resolution {
    pub const ALL: [Resolution; 4] = [
        Resolution::Native,
        Resolution::P1440,
        Resolution::P1080,
        Resolution::P720,
    ];

    pub fn max_height(self) -> Option<u32> {
        match self {
            Resolution::Native => None,
            Resolution::P1440 => Some(1440),
            Resolution::P1080 => Some(1080),
            Resolution::P720 => Some(720),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Resolution::Native => "Same as screen",
            Resolution::P1440 => "1440p",
            Resolution::P1080 => "1080p",
            Resolution::P720 => "720p",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Low,
    Medium,
    High,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Low, Quality::Medium, Quality::High];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Low => "Low (small files)",
            Quality::Medium => "Medium",
            Quality::High => "High",
        }
    }

    /// Target bits per second for `pixels` per frame at `fps`: about 12 Mbit/s for 1080p60 at
    /// Medium, scaled by pixel rate.
    pub fn bitrate(self, pixels: u64, fps: u32) -> u64 {
        let per_pixel_frame = match self {
            Quality::Low => 0.05,
            Quality::Medium => 0.1,
            Quality::High => 0.2,
        };
        let bps = pixels as f64 * fps.max(1) as f64 * per_pixel_frame;
        (bps as u64).clamp(1_000_000, 150_000_000)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Saves a clip: "F8", "Ctrl+Shift+S" and so on.
    pub hotkey: String,
    /// How much a clip holds, in seconds.
    pub clip_seconds: u32,
    pub fps: u32,
    pub resolution: Resolution,
    pub quality: Quality,
    /// "auto", or an ffmpeg encoder name such as "h264_nvenc".
    pub encoder: String,
    /// Which screen: "primary", or a monitor's position in the list (1 = first).
    pub monitor: String,
    pub desktop_audio: bool,
    pub microphone: bool,
    /// Microphone device name; empty for the system default.
    pub microphone_device: String,
    /// Where clips go; empty for Videos/Rewindr.
    pub save_dir: String,
    /// Play a short sound when a clip is saved.
    pub sound: bool,
    /// Show a notification when a clip is saved.
    pub notify: bool,
    pub check_updates: bool,
    /// Delete the oldest clips (never favourites) when the folder grows past this many GB;
    /// 0 keeps everything.
    pub max_storage_gb: u32,
    /// Size limit for "Share for Discord", in MB (10 for free accounts).
    pub discord_mb: u32,
    /// Saves a screenshot: "F9" and so on; empty turns it off.
    pub screenshot_hotkey: String,
    /// How long share links work: "forever", or "72h", "24h", "12h", "1h".
    pub share_expiry: String,
    /// Save a clip when the microphone hears `voice_phrase`.
    pub voice_clip: bool,
    pub voice_phrase: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            hotkey: "F8".into(),
            clip_seconds: 30,
            fps: 60,
            resolution: Resolution::Native,
            quality: Quality::Medium,
            encoder: "auto".into(),
            monitor: "primary".into(),
            desktop_audio: true,
            microphone: false,
            microphone_device: String::new(),
            save_dir: String::new(),
            sound: true,
            notify: true,
            check_updates: true,
            max_storage_gb: 0,
            discord_mb: 10,
            screenshot_hotkey: "F9".into(),
            share_expiry: "forever".into(),
            voice_clip: false,
            voice_phrase: DEFAULT_VOICE_PHRASE.into(),
        }
    }
}

pub const MIN_CLIP_SECONDS: u32 = 5;
pub const DEFAULT_VOICE_PHRASE: &str = "clip that";
pub const SHARE_EXPIRIES: [&str; 4] = ["forever", "72h", "24h", "1h"];
pub const MAX_CLIP_SECONDS: u32 = 300;

impl Settings {
    /// Brings out-of-range values back into range.
    pub fn sanitize(&mut self) {
        self.clip_seconds = self.clip_seconds.clamp(MIN_CLIP_SECONDS, MAX_CLIP_SECONDS);
        self.fps = self.fps.clamp(10, 240);
        self.max_storage_gb = self.max_storage_gb.min(10_000);
        self.discord_mb = self.discord_mb.clamp(5, 500);
        if self.hotkey.trim().is_empty() {
            self.hotkey = "F8".into();
        }
        if self.encoder.trim().is_empty() {
            self.encoder = "auto".into();
        }
        if self.monitor.trim().is_empty() {
            self.monitor = "primary".into();
        }
        if !SHARE_EXPIRIES.contains(&self.share_expiry.as_str()) {
            self.share_expiry = "forever".into();
        }
        self.voice_phrase = normalize_phrase(&self.voice_phrase);
        if self.voice_phrase.is_empty() {
            self.voice_phrase = DEFAULT_VOICE_PHRASE.into();
        }
        self.screenshot_hotkey = self.screenshot_hotkey.trim().to_string();
    }

    /// Where screenshots go: a folder inside the clips folder.
    pub fn screenshot_dir(&self) -> PathBuf {
        self.save_dir().join("Screenshots")
    }

    pub fn save_dir(&self) -> PathBuf {
        if self.save_dir.trim().is_empty() {
            default_save_dir()
        } else {
            PathBuf::from(self.save_dir.trim())
        }
    }

    /// Seconds of video and sound to keep in memory: a clip, plus a little for the GOP a clip
    /// starts inside.
    pub fn buffer_seconds(&self) -> u32 {
        self.clip_seconds + 3
    }
}

/// `REWINDR_HOME` keeps settings and clips in one folder (portable copies, tests).
fn home_override() -> Option<PathBuf> {
    std::env::var_os("REWINDR_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub fn config_dir() -> PathBuf {
    if let Some(h) = home_override() {
        return h;
    }
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("rewindr")
}

pub fn default_save_dir() -> PathBuf {
    if let Some(h) = home_override() {
        return h.join("Clips");
    }
    dirs::video_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
        .unwrap_or_else(std::env::temp_dir)
        .join("Rewindr")
}

pub fn default_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn load(path: &Path) -> Result<Settings, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let mut s: Settings =
        toml::from_str(&text).map_err(|e| format!("{} is not valid: {e}", path.display()))?;
    s.sanitize();
    Ok(s)
}

/// The settings, or defaults (written out) if there are none yet. A broken file is left alone
/// and reported; defaults are used meanwhile.
pub fn load_or_create(path: &Path) -> (Settings, Option<String>) {
    if !path.exists() {
        let s = Settings::default();
        let _ = save(path, &s);
        return (s, None);
    }
    match load(path) {
        Ok(s) => (s, None),
        Err(e) => (Settings::default(), Some(e)),
    }
}

pub fn save(path: &Path, s: &Settings) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = toml::to_string_pretty(s).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

pub fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// A voice phrase as it's matched: lowercase words, no punctuation, single spaces.
pub fn normalize_phrase(p: &str) -> String {
    p.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (s, warn) = load_or_create(&path);
        assert!(warn.is_none());
        assert_eq!(s, Settings::default());
        assert!(path.exists());
        let mut changed = s.clone();
        changed.clip_seconds = 60;
        changed.resolution = Resolution::P1080;
        save(&path, &changed).unwrap();
        assert_eq!(load(&path).unwrap(), changed);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("resolution = \"1080p\""), "{text}");
    }

    #[test]
    fn partial_and_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "clip_seconds = 99999\nfps = 1\nhotkey = \"\"\n").unwrap();
        let s = load(&path).unwrap();
        assert_eq!(s.clip_seconds, MAX_CLIP_SECONDS);
        assert_eq!(s.fps, 10);
        assert_eq!(s.hotkey, "F8");
        assert!(s.desktop_audio);
        std::fs::write(&path, "clip_seconds = \"lots\"").unwrap();
        let (s, warn) = load_or_create(&path);
        assert!(warn.is_some());
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn bitrates() {
        let p1080 = 1920 * 1080;
        let m = Quality::Medium.bitrate(p1080, 60);
        assert!((11_000_000..=13_000_000).contains(&m), "{m}");
        assert!(Quality::High.bitrate(p1080, 60) > m);
        assert!(Quality::Low.bitrate(p1080, 60) < m);
        assert_eq!(Quality::Low.bitrate(100, 10), 1_000_000);
    }

    #[test]
    fn voice_phrases_are_normalized() {
        assert_eq!(normalize_phrase("  Clip   THAT! "), "clip that");
        assert_eq!(normalize_phrase("Rewindr, clip it."), "rewindr clip it");
        assert_eq!(normalize_phrase("that's it"), "that's it");
        let mut s = Settings {
            voice_phrase: "!!!".into(),
            share_expiry: "week".into(),
            ..Default::default()
        };
        s.sanitize();
        assert_eq!(s.voice_phrase, DEFAULT_VOICE_PHRASE);
        assert_eq!(s.share_expiry, "forever");
    }

    #[test]
    fn old_settings_files_get_the_new_defaults() {
        let s: Settings = toml::from_str("hotkey = \"F10\"\nclip_seconds = 45\n").unwrap();
        assert_eq!(s.hotkey, "F10");
        assert_eq!(s.screenshot_hotkey, "F9");
        assert!(!s.voice_clip);
        assert_eq!(s.share_expiry, "forever");
    }
}
