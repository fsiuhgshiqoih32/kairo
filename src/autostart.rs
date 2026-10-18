//! "Start with computer": the login item starts only the background process.

use crate::log::warning;

pub fn launcher() -> Option<auto_launch::AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.to_str()?;
    auto_launch::AutoLaunchBuilder::new()
        .set_app_name("rewindr")
        .set_app_path(path)
        .set_args(&["daemon"])
        .build()
        .map_err(|e| warning!("start with computer unavailable: {e}"))
        .ok()
}

pub fn is_enabled(launcher: &auto_launch::AutoLaunch) -> bool {
    launcher.is_enabled().unwrap_or(false)
}

pub fn set_enabled(launcher: &auto_launch::AutoLaunch, on: bool) {
    let result = if on {
        launcher.enable()
    } else {
        launcher.disable()
    };
    if let Err(e) = result {
        warning!("could not change start with computer: {e}");
    }
}
