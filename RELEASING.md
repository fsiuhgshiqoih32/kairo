# Publishing an update

Everyone who has Kairo gets told about a new version, so a release is three steps.

1. **Write the release notes.** Add a section at the top of `CHANGELOG.md` named after the new
   version, with one bullet per change:

   ```markdown
   ## 0.3.1

   - **Faster saves.** Clips save in half the time on NVIDIA cards.
   - **Fix.** The tray menu no longer shows the wrong length after you change it.
   ```

   The bold titles are what the app's update notice and "What's new" show, with the first sentence
   after each one, so keep them short. Commit and push the notes with your changes as usual.

2. **Publish.** From the repository folder, on an up-to-date `main` with nothing uncommitted:

   ```powershell
   ./scripts/release.ps1 0.3.1
   ```

   It checks everything, sets the version, commits "Kairo 0.3.1" and pushes. Add `-DryRun` to only
   check. (Without PowerShell: change `version` in `Cargo.toml`, run `cargo check` so
   `Cargo.lock` follows, commit both and push.)

3. **Wait about half an hour.** GitHub runs the tests, builds Windows, macOS and Linux with the
   bundled ffmpeg, checks each package runs, and publishes the release with checksums
   ([Actions](https://github.com/fsiuhgshiqoih32/kairo/actions)). The website picks up the new
   version by itself.

## What people see

- Kairo checks GitHub a few minutes after it starts and every six hours. The first time it finds
  your version it shows a notice in the corner, with the first highlight and an **Update now**
  button; the tray menu gets an "Update to Kairo …" item too.
- The window shows the update at the top of Settings with the highlights, **Update and restart**
  and a link to the release notes.
- Updating downloads the package for that system, checks it against `SHA256SUMS.txt` and swaps the
  program in place. Clips and settings stay.
- The first time the new version opens, it shows **What's new** with your release notes.
- Anyone who turned off "Check for updates automatically" can still check in Settings.

## If something goes wrong

- **The release workflow says the bundled ffmpeg isn't published:** run the "ffmpeg" workflow
  first (Actions › ffmpeg › Run workflow), then re-run "Release".
- **A build failed:** fix it on `main` and push. The version hasn't been released yet, so the
  next successful run publishes it.
- **A bad release went out:** publish a fixed version with a higher number. Don't delete the
  release; copies that already updated need the next one to move on.
