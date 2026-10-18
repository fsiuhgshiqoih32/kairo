//! Rewindr's platform-independent parts: settings, the replay buffer, recent audio, and the
//! ffmpeg command lines. The binary adds capture, the tray, hotkeys and windows.

pub mod args;
pub mod art;
pub mod config;
pub mod library;
pub mod media;
pub mod mp4;
pub mod pcm;
pub mod ring;
pub mod ts;
