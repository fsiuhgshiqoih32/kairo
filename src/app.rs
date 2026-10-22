//! Small things every window and command needs.

use std::path::Path;
use std::process::{Command, Stdio};

/// The renderers a pop-up uses, in order (`ICED_BACKEND`, set in `main`): the CPU, then the
/// graphics card.
pub const POPUP_BACKEND: &str = "tiny-skia,wgpu";

/// Starts another copy of this program, detached, e.g. `kairo notify <clip>`.
pub fn spawn_self(args: &[&str]) -> Result<std::process::Child, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // A pop-up's renderer isn't for the windows it opens: the editor plays video on the card.
    if std::env::var_os("ICED_BACKEND").is_some_and(|b| b == POPUP_BACKEND) {
        cmd.env_remove("ICED_BACKEND");
    }
    detach(&mut cmd);
    cmd.spawn()
        .map_err(|e| format!("could not start kairo {}: {e}", args.join(" ")))
}

/// Keeps a child alive when the terminal that started us closes.
pub fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
}

/// Opens a file, folder or link with the system's default program.
pub fn open(target: &Path) {
    if let Err(e) = opener::open(target) {
        crate::log::warning!("could not open {}: {e}", target.display());
    }
}

/// Opens a clip in Kairo's editor, or the system's player if the editor can't start.
pub fn open_clip(clip: &Path) {
    let arg = clip.display().to_string();
    if spawn_self(&["edit", &arg]).is_err() {
        open(clip);
    }
}

/// Shows a file selected in its folder.
pub fn reveal(path: &Path) {
    if let Err(e) = opener::reveal(path) {
        crate::log::warning!("could not show {}: {e}", path.display());
        if let Some(dir) = path.parent() {
            open(dir);
        }
    }
}
