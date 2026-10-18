# Changelog

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
