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
    /// Medium. Above 60 fps it grows slower than the frame rate, since neighbouring frames are
    /// more alike: 120 fps gets about 1.5 times the bits of 60, not twice (measured on real
    /// game footage, the picture is the same).
    pub fn bitrate(self, pixels: u64, fps: u32) -> u64 {
        let per_pixel_frame = match self {
            Quality::Low => 0.05,
            Quality::Medium => 0.1,
            Quality::High => 0.2,
        };
        let fps = fps.max(1) as f64;
        let frames = if fps > 60.0 {
            60.0 * (fps / 60.0).powf(0.6)
        } else {
            fps
        };
        let bps = pixels as f64 * frames * per_pixel_frame;
        (bps as u64).clamp(1_000_000, 150_000_000)
    }
}

/// The video format clips are recorded in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    /// Plays everywhere.
    #[default]
    H264,
    /// H.265: the same picture in about two thirds of the bits, where the encoder has it.
    Hevc,
}

impl VideoCodec {
    pub const ALL: [VideoCodec; 2] = [VideoCodec::H264, VideoCodec::Hevc];

    pub fn label(self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264",
            VideoCodec::Hevc => "H.265",
        }
    }

    /// Bits needed for the same picture, relative to H.264.
    pub fn bitrate_factor(self) -> f64 {
        match self {
            VideoCodec::H264 => 1.0,
            VideoCodec::Hevc => 0.65,
        }
    }
}

/// An extra save shortcut with its own length.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuickSave {
    pub hotkey: String,
    pub seconds: u32,
}

/// At most this many quick saves.
pub const MAX_QUICK_SAVES: usize = 3;

/// Where the replay buffer keeps finished video.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Storage {
    /// Memory for small buffers, disk for ones that could pass `AUTO_DISK_BYTES`.
    #[default]
    Auto,
    Memory,
    Disk,
}

impl Storage {
    pub const ALL: [Storage; 3] = [Storage::Auto, Storage::Memory, Storage::Disk];

    pub fn label(self) -> &'static str {
        match self {
            Storage::Auto => "Automatic",
            Storage::Memory => "Memory",
            Storage::Disk => "Disk",
        }
    }
}

/// On Automatic, a buffer that could grow past this lives on disk.
pub const AUTO_DISK_BYTES: u64 = 1 << 30;

/// The window's accent colours, by name.
pub const ACCENTS: [&str; 8] = [
    "red", "orange", "gold", "green", "cyan", "blue", "purple", "pink",
];

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
    /// The name the connected Discord channel's webhook posts as; empty when none is connected.
    /// (The webhook link itself is in the system's password store.)
    pub discord_channel: String,
    /// Clips older than this many days go to Recently deleted (never favourites); 0 keeps them.
    pub keep_days: u32,
    /// Saves a screenshot: "F9" and so on; empty turns it off.
    pub screenshot_hotkey: String,
    /// How long share links work: "forever", or "72h", "24h", "12h", "1h".
    pub share_expiry: String,
    /// Save a clip when the microphone hears `voice_phrase`.
    pub voice_clip: bool,
    pub voice_phrase: String,
    /// Where the replay buffer lives.
    pub replay_storage: Storage,
    /// Marks the moment (shown on the clip's timeline): "F10"; empty turns it off.
    pub marker_hotkey: String,
    /// The window's accent colour, one of `ACCENTS`.
    pub accent: String,
    pub codec: VideoCodec,
    /// Extra shortcuts that save other lengths.
    pub quick_saves: Vec<QuickSave>,
    /// Keep recording this many seconds after a save is asked for, then save.
    pub post_roll: u32,
    /// Empty the buffer after each save, so the next clip doesn't repeat it.
    pub fresh_after_save: bool,
    /// Starts and stops recording a whole session; empty turns it off.
    pub session_hotkey: String,
    /// Record a session from when Rewindr starts.
    pub session_auto: bool,
    /// Put a voice-chat app's sound on its own track, and leave it out of the game track
    /// (Windows 11).
    pub chat_track: bool,
    /// The app: the start of its program's name ("Discord" also finds DiscordPTB, DiscordCanary).
    pub chat_app: String,
    /// Keep the gaming keys and clicks pressed while recording, for an overlay in copies.
    pub input_overlay: bool,
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
            discord_channel: String::new(),
            keep_days: 0,
            screenshot_hotkey: "F9".into(),
            share_expiry: "forever".into(),
            voice_clip: false,
            voice_phrase: DEFAULT_VOICE_PHRASE.into(),
            replay_storage: Storage::Auto,
            marker_hotkey: "F10".into(),
            accent: "red".into(),
            codec: VideoCodec::H264,
            quick_saves: Vec::new(),
            post_roll: 0,
            fresh_after_save: false,
            session_hotkey: String::new(),
            session_auto: false,
            chat_track: false,
            chat_app: "Discord".into(),
            input_overlay: false,
        }
    }
}

pub const MIN_CLIP_SECONDS: u32 = 5;
pub const DEFAULT_VOICE_PHRASE: &str = "clip that";
pub const SHARE_EXPIRIES: [&str; 4] = ["forever", "72h", "24h", "1h"];
pub const MAX_CLIP_SECONDS: u32 = 1200;
pub const MAX_POST_ROLL: u32 = 15;

impl Settings {
    /// Brings out-of-range values back into range.
    pub fn sanitize(&mut self) {
        self.clip_seconds = self.clip_seconds.clamp(MIN_CLIP_SECONDS, MAX_CLIP_SECONDS);
        self.fps = self.fps.clamp(10, 240);
        self.max_storage_gb = self.max_storage_gb.min(10_000);
        self.discord_mb = self.discord_mb.clamp(5, 500);
        self.keep_days = self.keep_days.min(3650);
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
        self.marker_hotkey = self.marker_hotkey.trim().to_string();
        self.quick_saves.truncate(MAX_QUICK_SAVES);
        for q in &mut self.quick_saves {
            q.hotkey = q.hotkey.trim().to_string();
            q.seconds = q.seconds.clamp(MIN_CLIP_SECONDS, MAX_CLIP_SECONDS);
        }
        self.post_roll = self.post_roll.min(MAX_POST_ROLL);
        self.session_hotkey = self.session_hotkey.trim().to_string();
        self.chat_app = self.chat_app.trim().trim_end_matches(".exe").to_string();
        if self.chat_app.is_empty() {
            self.chat_app = "Discord".into();
        }
        if !ACCENTS.contains(&self.accent.as_str()) {
            self.accent = "red".into();
        }
    }

    /// The most the replay buffer can hold at `bitrate`, in bytes: encoders run up to 1.5 times
    /// over their target in busy scenes.
    pub fn buffer_worst_bytes(&self, bitrate: u64) -> u64 {
        bitrate * 3 / 2 / 8 * self.buffer_seconds() as u64
    }

    /// Whether the replay buffer at `bitrate` goes to disk.
    pub fn buffer_on_disk(&self, bitrate: u64) -> bool {
        match self.replay_storage {
            Storage::Memory => false,
            Storage::Disk => true,
            Storage::Auto => self.buffer_worst_bytes(bitrate) > AUTO_DISK_BYTES,
        }
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

    /// Seconds of video and sound to keep: the longest clip any shortcut saves, plus a little
    /// for the GOP a clip starts inside.
    pub fn buffer_seconds(&self) -> u32 {
        let longest = self
            .quick_saves
            .iter()
            .map(|q| q.seconds)
            .chain([self.clip_seconds])
            .max()
            .unwrap_or(self.clip_seconds);
        longest + self.post_roll + 3
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
        // Past 60 fps, bits grow slower than frames.
        let p1440 = 2560 * 1440;
        let (b60, b120) = (
            Quality::High.bitrate(p1440, 60),
            Quality::High.bitrate(p1440, 120),
        );
        let r = b120 as f64 / b60 as f64;
        assert!((1.45..=1.6).contains(&r), "{r}");
        assert_eq!(Quality::Medium.bitrate(p1080, 30) * 2, m);
    }

    #[test]
    fn big_buffers_go_to_disk_on_automatic() {
        let p1440 = 2560 * 1440;
        let mut s = Settings::default();
        // 33 s of 1080p60 Medium: about 75 MB.
        assert!(!s.buffer_on_disk(Quality::Medium.bitrate(1920 * 1080, 60)));
        // 200 s of 1440p120 High: several GB.
        s.clip_seconds = 200;
        let b = Quality::High.bitrate(p1440, 120);
        assert!(s.buffer_worst_bytes(b) > 2 * AUTO_DISK_BYTES);
        assert!(s.buffer_on_disk(b));
        s.replay_storage = Storage::Memory;
        assert!(!s.buffer_on_disk(b));
        s.replay_storage = Storage::Disk;
        s.clip_seconds = 10;
        assert!(s.buffer_on_disk(1_000_000));
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
        assert_eq!(s.replay_storage, Storage::Auto);
        assert_eq!(s.marker_hotkey, "F10");
        assert_eq!(s.accent, "red");
        assert_eq!(s.codec, VideoCodec::H264);
        assert!(s.quick_saves.is_empty() && s.post_roll == 0 && !s.fresh_after_save);
    }

    #[test]
    fn the_buffer_holds_the_longest_save_and_the_post_roll() {
        let mut s = Settings {
            clip_seconds: 30,
            post_roll: 5,
            quick_saves: vec![
                QuickSave {
                    hotkey: "Shift+F8".into(),
                    seconds: 120,
                },
                QuickSave {
                    hotkey: " F7 ".into(),
                    seconds: 99_999,
                },
            ],
            ..Default::default()
        };
        s.sanitize();
        assert_eq!(s.quick_saves[1].hotkey, "F7");
        assert_eq!(s.quick_saves[1].seconds, MAX_CLIP_SECONDS);
        assert_eq!(s.buffer_seconds(), MAX_CLIP_SECONDS + 5 + 3);
        let text = toml::to_string(&s).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back, s);
    }
}
