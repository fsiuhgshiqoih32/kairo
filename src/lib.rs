//! Kairo's platform-independent parts: settings, the replay buffer, recent audio, and the
//! ffmpeg command lines. The binary adds capture, the tray, hotkeys and windows.

pub mod args;
pub mod art;
pub mod captions;
pub mod config;
pub mod cuts;
pub mod keys;
pub mod library;
pub mod medal;
pub mod media;
pub mod mp4;
pub mod overlays;
pub mod pcm;
pub mod ring;
pub mod sqlite;
pub mod ts;
