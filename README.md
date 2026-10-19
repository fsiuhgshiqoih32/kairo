# Rewindr

Instant replay for Windows, macOS and Linux. Rewindr keeps the last moments of your screen and
sound in memory. Press **F8** and the last 30 seconds are saved as an MP4, like Medal or
ShadowPlay. It's free and open source (MIT), with no account and no ads. Nothing is uploaded
unless you ask for a share link.

- **Always ready, light on games.** The replay is captured and encoded on the GPU (NVIDIA
  NVENC, AMD AMF, Intel and others through Media Foundation, Apple VideoToolbox), with x264 as a
  fallback. It lives in memory, or for long buffers in a scratch file the system caches in spare
  memory, so even minutes of 1440p cost Rewindr little.
- **Instant saves.** A clip is cut from the buffer without re-encoding. It's saved in about a
  second and named after the game in front.
- **Editor.** Play a clip with sound, scrub its filmstrip, drag the handles to trim, and save an
  exact trimmed copy. Change its speed and look, add fades and music, balance game, mic and voice
  chat, and make square, 4:5 and 9:16 copies.
- **Captions** of what's said, recognised offline, to edit, keep as .srt or burn in.
- **Share links.** One click uploads a clip and copies a link friends can watch in their browser
  (Discord plays it inline), with a QR code for phones. Links last forever or 1 hour to 3 days.
- **Share for Discord.** One click makes a copy under 10 MB (or 50/500 MB for Nitro) and puts it
  on the clipboard, or posts it straight to a channel you connected with a webhook.
- **Voice clipping.** Say "clip that" and it saves a clip. Speech is recognised offline (Vosk).
- **Markers.** F10 marks a moment while you play; the clip shows a flag there.
- **Longer clips and sessions.** Clips up to 20 minutes, quick-save shortcuts with their own
  lengths, and whole-session recording with bookmarks.
- **H.265** recording on graphics cards that support it, for smaller files.
- **Montages.** Pick clips and join them into one video that dips to black between them.
- **Screenshots** with F9, and **more formats**: GIF, vertical 9:16, slow motion.
- **Gallery.** Filter by game or tag, search, star favourites, rename. Deleted clips wait 30 days
  in Recently deleted, with Undo. Optionally keep the folder under a size limit or drop clips
  older than a week, 30 or 90 days (favourites always stay).
- **Sound.** Desktop audio and your microphone are mixed on the first track, and each is also
  kept on its own track for editing. On Windows 11, voice chat (Discord by default) can go on a
  track of its own.
- **Keys and clicks overlay** (Windows): show the gaming keys and mouse buttons you pressed over
  a clip.

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
| **F10** | Mark this moment; the clip that holds it shows a flag there |
| Session shortcut | Start or stop recording a whole session (set one in Settings > Sessions) |
| "clip that" | Save a clip by voice (turn it on in Settings > Voice clipping) |
| Click a clip | Open it in the editor: play, trim, share |
| Space, Left/Right, `,` `.`, I/O | In the editor: play, jump a second, step a frame, set start/end |
| M, N / Shift+N | In the editor: mark a moment, go to the next / previous mark |
| Select (library) | Pick clips to join into a montage or delete together |
| Tray > Pause recording | Stop keeping a replay until you resume |
| `rewindr save` | Save a clip from a script or desktop shortcut (needed on Wayland) |
| `rewindr screenshot` | Save a screenshot the same way |
| `rewindr share <clip>` | Upload a clip and print its share link |
| `rewindr captions <clip>` | Write captions of what's said (an .srt beside the clip), offline |
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
keeps the last N seconds in a ring buffer, in memory or (for long buffers) in a fixed-size
scratch file that's deleted when Rewindr closes. Sound is captured separately (WASAPI loopback on
Windows, ScreenCaptureKit on macOS, the PulseAudio/PipeWire monitor on Linux, plus the
microphone) and stamped with the
same clock. On save, the video is written from the keyframe just before the cut, the matching
sound beside it, and ffmpeg joins them into an MP4.

## Platform notes

- **Windows:** everything works. Voice chat on its own track needs Windows 11. ARM64 builds
  aren't published yet.
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
