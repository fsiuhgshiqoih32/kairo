# Rewindr

Instant replay for Windows, macOS and Linux. Rewindr keeps the last moments of your screen and
sound in memory. Press **F8** and the last 30 seconds are saved as an MP4, like Medal or
ShadowPlay. It's free and open source (MIT), with no account and no ads. Nothing is uploaded
unless you ask for a share link.

- **Always ready, light on games.** The replay lives in RAM and is captured and encoded on the
  GPU (NVIDIA NVENC, AMD AMF, Intel and others through Media Foundation, Apple VideoToolbox),
  with x264 as a fallback. Nothing touches the disk until you save.
- **Instant saves.** A clip is cut from the buffer without re-encoding. It's saved in about a
  second and named after the game in front.
- **Editor.** Play a clip with sound, scrub its filmstrip, drag the handles to trim, and save an
  exact trimmed copy.
- **Share links.** One click uploads a clip and copies a link friends can watch in their browser
  (Discord plays it inline), with a QR code for phones. Links last forever or 1 hour to 3 days.
- **Share for Discord.** One click makes a copy under 10 MB (or 50/500 MB for Nitro) and puts it
  on the clipboard: paste it into Discord.
- **Voice clipping.** Say "clip that" and it saves a clip. Speech is recognised offline (Vosk).
- **Screenshots** with F9, and **more formats**: GIF, vertical 9:16, slow motion.
- **Gallery.** Filter by game, search, star favourites, rename, and optionally keep the clips
  folder under a size limit (oldest go first, favourites stay).
- **Sound.** Desktop audio and your microphone are mixed on the first track, and each is also
  kept on its own track for editing.

## Download

Get the package for your system from the
[latest release](https://github.com/fsiuhgshiqoih32/rewindr/releases/latest). ffmpeg is
included, so there is nothing else to install.

- **Windows 10/11 (x64):** unzip `rewindr-…-windows-x64.zip` anywhere and run `rewindr.exe`.
- **macOS 11+ (Apple silicon and Intel):** unzip `rewindr-…-macos.zip` and move `Rewindr.app`
  to Applications. It isn't notarized: the first time, right-click it and choose Open. Then
  allow Screen Recording in System Settings > Privacy & Security.
- **Linux (x86_64, X11):** unpack `rewindr-…-linux-x86_64.tar.gz` and run `./rewindr`. Desktop
  audio needs `parec` (PulseAudio or PipeWire); copying clips needs `xclip` or `wl-copy`.

Rewindr checks for updates when its window opens and updates itself in place.

## Use

Run Rewindr. A tray icon appears and the window opens. Then:

| Key or command | What it does |
| --- | --- |
| **F8** | Save the last 30 seconds (change the key and length in Settings) |
| **F9** | Save a screenshot |
| "clip that" | Save a clip by voice (turn it on in Settings > Voice clipping) |
| Click a clip | Open it in the editor: play, trim, share |
| Space, Left/Right, `,` `.`, I/O | In the editor: play, jump a second, step a frame, set start/end |
| Tray > Pause recording | Stop keeping a replay until you resume |
| `rewindr save` | Save a clip from a script or desktop shortcut (needed on Wayland) |
| `rewindr screenshot` | Save a screenshot the same way |
| `rewindr edit <clip>` | Open a clip in the editor |
| `rewindr pause`, `rewindr quit` | Pause or resume; stop the background process |
| `rewindr devices` | List screens, microphones and the encoders that work |
| `rewindr update` | Install the latest version |
| `rewindr --self-test` | Check recording and saving on this machine |

Clips go to `Videos/Rewindr`. Settings are stored in `config.toml` in your config folder
(`%APPDATA%\rewindr` on Windows, `~/Library/Application Support/rewindr` on macOS,
`~/.config/rewindr` on Linux).

## How it works

ffmpeg captures the screen (Windows Graphics Capture, Desktop Duplication or GDI on Windows,
AVFoundation on macOS, X11 on Linux), encodes it with a keyframe every second, and streams
MPEG-TS stamped with wall-clock time into Rewindr. Rewindr cuts the stream at keyframes and
keeps the last N seconds in a ring buffer. Sound is captured separately (WASAPI loopback on
Windows, ScreenCaptureKit on macOS, the PulseAudio/PipeWire monitor on Linux, plus the
microphone) and stamped with the
same clock. On save, the video is written from the keyframe just before the cut, the matching
sound beside it, and ffmpeg joins them into an MP4.

## Platform notes

- **Windows:** everything works. ARM64 builds aren't published yet.
- **macOS:** desktop audio is captured through ScreenCaptureKit on macOS 13 and later (it uses
  the Screen Recording permission); on macOS 11 and 12, only the microphone.
- **Linux:** X11 sessions work. On Wayland, apps can't capture other windows or register global
  shortcuts: run Rewindr under XWayland, and bind `rewindr save` to a key in your desktop's
  settings. The bundled ffmpeg encodes with NVENC or x264; for VA-API, point `REWINDR_FFMPEG` at
  a system ffmpeg built with it.

## Build

```
cargo build --release
```

Rewindr needs an `ffmpeg` next to the program or on `PATH` (or set `REWINDR_FFMPEG`).
`packaging/ffmpeg/build.sh` builds the small one the releases ship.

The window is built with [iced](https://iced.rs). Its screens can be rendered to PNG files
without opening a window, for checking the UI:
`REWINDR_HOME=<folder with Clips/ and thumbs/> REWINDR_SNAPSHOTS=<out> cargo test -- --ignored gui::preview`.
`rewindr --ui-self-test` runs the real window, hidden, on a generated clip: it must show the
first frame and the filmstrip, play, and save a trimmed copy (CI runs it on all three systems).

## License

Rewindr is MIT licensed. The bundled ffmpeg is FFmpeg with x264, under the GNU GPL (see
`FFMPEG-LICENSE.txt` and `FFMPEG-SOURCE.txt` in each package). The window uses the Inter
typeface (SIL Open Font License) and Lucide icons (ISC); see `THIRD-PARTY-NOTICES.txt`.
Voice clipping downloads Vosk (Apache 2.0) and its small English model (Apache 2.0) from their
authors when you turn it on.
