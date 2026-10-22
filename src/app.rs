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

/// Runs a worker thread's `work`, turning a panic into an error: the window waiting on the
/// result then shows what went wrong instead of waiting forever. The panic itself is in the log.
pub fn unless_panic<T>(work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|p| {
        let why = p
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "no details".into());
        Err(format!(
            "Something went wrong inside Kairo ({why}). Try again."
        ))
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_in_a_worker_becomes_an_error() {
        let r: Result<(), String> = unless_panic(|| panic!("index out of bounds"));
        let e = r.unwrap_err();
        assert!(e.contains("index out of bounds"), "{e}");
        assert!(e.ends_with("Try again."), "{e}");
        let r: Result<u32, String> = unless_panic(|| panic!("{} went wrong", 2));
        assert!(r.unwrap_err().contains("2 went wrong"));
        assert_eq!(unless_panic(|| Ok::<_, String>(7)), Ok(7));
        assert_eq!(
            unless_panic(|| Err::<u8, _>("no".to_string())),
            Err("no".into())
        );
    }
}
