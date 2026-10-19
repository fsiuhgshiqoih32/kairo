# Changelog

## Unreleased

- **Share links, like Medal.** One click uploads a clip (or just the trimmed part) and copies a
  link anyone can watch in their browser; Discord plays it right in the chat. A QR code opens it
  on a phone. Links last forever (removed after two years without views) or 1 hour to 3 days, as
  you choose. Shared clips get a badge and their own "Shared" list. The "Clip saved" card has a
  Get link button too.
  Big or high-frame-rate clips are shared as a 1080p60 copy made for browsers (so they fit the
  host and play smoothly); if the host for lasting links is down, you get a 3-day link instead.
  `rewindr share <clip>` does the same from a terminal.
- **Voice clipping.** Say "clip that" (or a phrase of your own) and Rewindr saves a clip,
  hands-free. The speech is recognised offline on your computer with Vosk; nothing is sent
  anywhere. It's off until you turn it on, and downloads its speech files (about 55 MB) once.
- **Screenshots.** Press F9 (or the camera button) to save a full-size picture of the moment, with
  their own Screenshots page.
- **More formats** from the editor: a GIF, a vertical 9:16 copy for TikTok, Shorts and Reels (the
  whole picture over a blurred background), and slow motion at half or quarter speed with the
  sound slowed to match.
- **Hover previews.** Moving the pointer across a clip's card plays through the clip.
- **A new window.** Rewindr's window is rebuilt from scratch with iced: a sidebar with your games
  and what's being recorded, clip cards grouped by day with hover actions (copy, show, rename,
  delete), and a one-click "Save last 30 s" button.
- **The editor lives in the window now**: a larger player, a filmstrip timeline with dimmed
  cut-off parts and grab handles, start/end readouts, loop playback, and Share to Discord or a
  trimmed copy from a side panel. All the keyboard shortcuts still work.
- **Redesigned settings**, grouped into Replay, Video, Sound, Saving and Rewindr, with toggles
  and choice buttons instead of drop-downs where it's quicker.
- **A new "Clip saved" card** with the thumbnail and Open, Get link and Copy buttons. On Windows it
  appears without taking focus from your game.
- **macOS: desktop audio.** Clips now include game and app sound on macOS 13 and later, captured
  with ScreenCaptureKit (it uses the Screen Recording permission), like on Windows and Linux.
- Opening a clip while the window is already open shows it there instead of doing nothing.

## 0.1.1

- Pressing F8 while recording is paused now says so, instead of saving a split second of old
  footage. Pausing clears the replay, so nothing from before a pause ends up in a clip.
- Saving right after recording (re)starts (for example after changing settings) reports that
  there's nothing to save yet, instead of writing a clip under 1.5 seconds.

## 0.1.0

The first release of Rewindr: instant replay for Windows, macOS and Linux.

- **Press F8 to save the last 30 seconds** of your screen and sound. The replay is kept in
  memory, captured and encoded on the GPU (NVIDIA, AMD, Intel, Apple), and saved in about a
  second, named after the game in front.
- **Clip editor**: play clips with sound, scrub a filmstrip timeline, trim exactly, and save a
  trimmed copy.
- **Share for Discord**: one click makes a copy under 10 MB (or 50/500 MB for Nitro) and copies
  it, ready to paste.
- **Gallery**: filter by game, search, favourites, rename, and automatic clean-up of old clips
  under a size limit (favourites are kept).
- **Sound**: desktop audio and microphone, mixed on the first track, each also on its own track.
- Tray icon, pause and resume, start with the computer, and check for updates.
- ffmpeg is included; nothing else to install.
