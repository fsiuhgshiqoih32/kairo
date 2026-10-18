# Rewindr

Instant replay for Windows, macOS and Linux. Rewindr keeps the last moments of your screen and
sound in memory. Press **F8** and the last 30 seconds are saved as an MP4, like Medal or
ShadowPlay. It's free and open source (MIT), with no account and no uploads.

- **Always ready.** The replay lives in RAM, so nothing is written to disk until you save.
- **Light on games.** Frames are captured and encoded on the GPU (NVIDIA NVENC, AMD AMF,
  Intel through Media Foundation, Apple VideoToolbox, VA-API), with x264 as a fallback.
- **Instant saves.** A clip is cut from the buffer and remuxed without re-encoding. It takes
  about a second, and the file is named after the game or app in front.
- **Sound included.** Desktop audio and your microphone are mixed into the clip.
- **A clips gallery.** Play, show, copy (paste straight into Discord) or delete clips.

## Use

Run `rewindr`. A tray icon appears and the window opens. Then:

| Key or command | What it does |
| --- | --- |
| **F8** | Save the last 30 seconds (change the key and length in Settings) |
| Tray > Pause recording | Stop keeping a replay until you resume |
| `rewindr save` | Save a clip from a script or desktop shortcut (needed on Wayland) |
| `rewindr pause` | Pause or resume |
| `rewindr quit` | Stop the background process |
| `rewindr devices` | List screens, microphones and the encoders that work |
| `rewindr --self-test` | Check recording and saving on this machine |

Clips go to `Videos/Rewindr`. Settings are stored in `config.toml` in your config folder
(`%APPDATA%\rewindr` on Windows, `~/Library/Application Support/rewindr` on macOS,
`~/.config/rewindr` on Linux).

## How it works

ffmpeg captures the screen (Windows Graphics Capture, Desktop Duplication or GDI on Windows,
AVFoundation on macOS, X11 on Linux), encodes it with a keyframe every second, and streams
MPEG-TS stamped with wall-clock time into Rewindr. Rewindr cuts the stream at keyframes and
keeps the last N seconds in a ring buffer. Audio is captured separately (WASAPI loopback on
Windows, PulseAudio/PipeWire monitor on Linux, plus the microphone) and stamped with the same
clock. On save, the video is written from the keyframe just before the cut, the matching
audio is written beside it, and ffmpeg joins them into an MP4.

## Platform notes

- **Windows 10/11:** everything works.
- **macOS:** grant Screen Recording permission. Desktop audio isn't captured yet; the
  microphone works.
- **Linux:** X11 sessions work fully. On Wayland, apps can't register global shortcuts, so
  bind `rewindr save` to a key in your desktop's settings.

## Build

```
cargo build --release
```

Rewindr needs an `ffmpeg` next to the program or on `PATH`. Set `REWINDR_FFMPEG` to use a
specific one.
