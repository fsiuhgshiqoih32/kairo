# Changelog

## 0.4.0

- **Cut and undo in the editor.** Press S to split at the playhead and Delete to cut a part out of
  the middle; playback skips it and every copy closes the gap. Ctrl+Z undoes any edit (a whole
  drag or slider move at once), and J, K, L, Q and W work like in other editors.
- **Instant copy.** A plain trim saves in about a second at full quality, without encoding again.
- **Text on your clips.** Type a line, drag it where you want it on the video, pick its colour and
  size, and choose how it comes in and goes out: a pop, a slide or a blur, drawn with motion blur.
- **Webcam in clips.** Turn it on in Settings > Webcam and your camera goes in a corner of each
  clip you save, in the size and corner you pick.
- **Only record games.** Settings can keep a replay only while a game is in front, and give a game
  its own length, quality and shortcut, or never record an app (Windows).
- **Folders and achievements.** Sort clips into folders from the sidebar or the editor, and earn
  15 badges on the stats page as your library grows.
- **A lighter look.** Choose Dark, Light or Match system, with bigger library cards whose previews
  play when you rest the pointer on them, and a short setup the first time Kairo starts.
- **Smoother window.** Pages, dialogs and notices ease in; Settings > Animations turns it on or
  off, and it follows your system's reduce-motion setting by default.
- **Your save sound.** Pick the sound a saved clip makes, or use a WAV of your own.
- **Fixes.** The game, microphone and voice-chat volume controls now find their tracks in clips
  (older clips included), and Settings no longer crashes when it shows the per-game list.

## 0.3.0

- **Update notices.** Kairo now looks for a new version a few minutes after it starts and every
  six hours, and the first time one is out it shows a notice with what's in it and an Update now
  button. The tray menu offers the update too, and Settings shows it at the top with the release
  notes. You can still turn update checks off in Settings.
- **What's new.** After an update, the window opens once with the highlights of the version you
  now have.
- **Save and share.** An optional shortcut (Settings > Sharing) saves a clip, uploads it straight
  away and copies the link, so a highlight is one key away from your friends.
- **Your stats.** A new page in the library counts your clips, the time and space they take, your
  top games, the last two weeks and your streak of days with a clip.

## 0.2.0

**Rewindr is now Kairo**, with a new logo and a Windows installer.

- **Moving from Rewindr.** Rewindr 0.1 updates itself to Kairo as usual (the `rewindr-…` files in
  this release are only for its updater), or run the Kairo installer, which closes and replaces
  it. Settings and "start with computer" move over the first time Kairo starts, and clips stay in
  `Videos/Rewindr` if that's where they were. On macOS a fresh `Kairo.app` needs Screen Recording
  allowed again.
- **Windows installer.** `kairo-…-windows-x64-setup.exe` installs Kairo for you, without
  administrator rights: Start menu and desktop shortcuts, start with Windows, and an entry in
  Settings > Apps to uninstall it. Uninstalling never touches your clips and can also remove
  settings. It runs silently with `--silent`. The portable zip is still there.
- The default accent is Kairo's orange (settings that chose one keep it).

Everything below is free: no account, no subscription, no watermark.

- **Longer clips and quick saves.** Clips can be up to 20 minutes. Add up to three quick-save
  shortcuts with their own lengths (15 s for one play, 5 min for the round). "Keep recording
  after the press" catches the 2 to 10 seconds after you hit save, and "Start fresh after saving"
  stops the next clip repeating this one.
- **Session recording.** A shortcut (or the Session button, or the tray) records everything until
  you stop, up to 5 hours, into a Sessions folder with its own list. Markers you set while it runs
  become bookmarks on its timeline. It can start with Kairo.
- **H.265.** Settings > Video > Video format records clips and the replay about a third smaller
  for the same picture, on graphics cards that encode it, and falls back to H.264 where they
  don't. Share links are converted to H.264 so every browser plays them.
- **Editor effects.** Speed (0.25x to 4x, sound kept in tune), a look (Vivid, Cinematic, Mono,
  Vintage), fade in and out, background music with its own volume, a mix of game, microphone and
  voice chat, microphone clean-up and even loudness. They apply to every copy you make.
- **Captions.** Make captions writes subtitles of what's said, recognised offline on your
  computer. Fix any line, keep them as an .srt beside the clip, or burn them into copies.
  `kairo captions <clip>` does it from a terminal.
- **Square and 4:5 copies** for feeds, beside the 9:16 one.
- **Send to a Discord channel.** Connect a channel once with its webhook link (Settings > Saving)
  and the editor posts a Discord-size copy there in one click. The link is kept in the system's
  password store (Credential Manager, the keychain, the Secret Service), not in the settings
  file. Posts never ping anyone.
- **Recently deleted.** Deleting moves clips and screenshots to Recently deleted, with Undo right
  on the notice. Put them back or delete them for good from its page; it empties itself after 30
  days. The size limit counts it, and empties it before touching your clips.
- **Keep clips** for a week, 30 or 90 days: older ones (never favourites) move to Recently
  deleted.
- **Tags.** Tag clips in the editor (tags from other clips are a click away), filter by tag in
  the sidebar, and find them in search.
- **Voice chat on its own track (Windows 11).** Kairo records Discord (or any app you name) on a
  track of its own and keeps it out of the game track, so you can turn chat down or off in
  copies.
- **Keys and clicks overlay (Windows).** Turn it on and Kairo remembers the gaming keys you
  press (WASD, QERF, Space, Shift, Ctrl, Tab, 1 to 4) and your mouse buttons while it records;
  nothing else you type is kept. The editor shows them over the clip and can put them in copies.
- The sidebar's game and tag counts are worked out when the clip list changes, not on every frame
  of playback.

- **Markers.** Press F10 while you play to mark a moment (a short tick confirms it). Clips that
  hold it show a gold flag there on their timeline; the editor's Moments panel jumps to each one
  or keeps the 10 seconds around it. M marks a moment in the editor, N jumps to the next.
- **Montages.** Select clips in the library, in the order they should play, and join them into
  one video that dips to black between clips. Selecting also deletes several clips at once.
- **Lighter on your PC.**
  - Long replay buffers can live on disk (Settings > Video > Replay buffer). Automatic puts any
    buffer that could pass 1 GB in a scratch file the system keeps in spare memory and hands
    back to games, so a 200-second 1440p buffer no longer sits in Kairo's memory.
  - 120 fps recordings use about 25% fewer bits for the same picture, so clips and buffers are
    that much smaller.
  - NVIDIA's encoder runs a faster preset with the same picture.
  - The buffer wastes no memory on spare room, and sound takes half the memory it did.
  - Voice clipping runs one thread instead of one per CPU core.
  - The window wakes up half as often and shows screenshots from small thumbnails.
- **A cooler window.** Eight accent colours (Settings > Kairo). Clips glow in the accent when
  you point at them. All clips opens with your latest clip as a big banner.
- **Save a shorter clip** from the menu next to the save button.
- **Share links, like Medal.** One click uploads a clip (or just the trimmed part) and copies a
  link anyone can watch in their browser; Discord plays it right in the chat. A QR code opens it
  on a phone. Links last forever (removed after two years without views) or 1 hour to 3 days, as
  you choose. Shared clips get a badge and their own "Shared" list. The "Clip saved" card has a
  Get link button too.
  Big or high-frame-rate clips are shared as a 1080p60 copy made for browsers (so they fit the
  host and play smoothly); if the host for lasting links is down, you get a 3-day link instead.
  `kairo share <clip>` does the same from a terminal.
- **Voice clipping.** Say "clip that" (or a phrase of your own) and Kairo saves a clip,
  hands-free. The speech is recognised offline on your computer with Vosk; nothing is sent
  anywhere. It's off until you turn it on, and downloads its speech files (about 55 MB) once.
- **Screenshots.** Press F9 (or the camera button) to save a full-size picture of the moment, with
  their own Screenshots page.
- **More formats** from the editor: a GIF, a vertical 9:16 copy for TikTok, Shorts and Reels (the
  whole picture over a blurred background), and slow motion at half or quarter speed with the
  sound slowed to match.
- **Hover previews.** Moving the pointer across a clip's card plays through the clip.
- **A new window.** Kairo's window is rebuilt from scratch with iced: a sidebar with your games
  and what's being recorded, clip cards grouped by day with hover actions (copy, show, rename,
  delete), and a one-click "Save last 30 s" button.
- **The editor lives in the window now**: a larger player, a filmstrip timeline with dimmed
  cut-off parts and grab handles, start/end readouts, loop playback, and Share to Discord or a
  trimmed copy from a side panel. All the keyboard shortcuts still work.
- **Redesigned settings**, grouped into Replay, Video, Sound, Saving and Kairo, with toggles
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
