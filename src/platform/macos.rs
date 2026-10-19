use std::process::Command;

use kairo::args::Source;

use super::Monitor;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGDisplayCopyDisplayMode(display: u32) -> *mut std::ffi::c_void;
    fn CGDisplayModeGetPixelWidth(mode: *mut std::ffi::c_void) -> usize;
    fn CGDisplayModeGetPixelHeight(mode: *mut std::ffi::c_void) -> usize;
    fn CGDisplayModeRelease(mode: *mut std::ffi::c_void);
}

/// Light unless the system appearance is Dark; `None` if it can't be read.
pub fn prefers_light() -> Option<bool> {
    let out = Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .ok()?;
    // The key only exists in Dark mode.
    Some(
        !String::from_utf8_lossy(&out.stdout)
            .trim()
            .eq_ignore_ascii_case("dark"),
    )
}

pub fn init_process() {}

pub fn show_error(message: &str) {
    // A dialog from a background app: osascript is always there and needs no main thread.
    let script = format!(
        "display dialog \"{}\" with title \"Kairo\" buttons {{\"OK\"}} with icon stop",
        message.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let _ = Command::new("osascript").args(["-e", &script]).status();
}

pub fn monitors() -> Vec<Monitor> {
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    if unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) } != 0 {
        return Vec::new();
    }
    let main = unsafe { CGMainDisplayID() };
    ids[..count as usize]
        .iter()
        .enumerate()
        .map(|(i, &id)| {
            let b = unsafe { CGDisplayBounds(id) };
            // Capture delivers pixels (twice the points on Retina screens).
            let (pw, ph) = unsafe {
                let mode = CGDisplayCopyDisplayMode(id);
                if mode.is_null() {
                    (b.size.width as usize, b.size.height as usize)
                } else {
                    let s = (
                        CGDisplayModeGetPixelWidth(mode),
                        CGDisplayModeGetPixelHeight(mode),
                    );
                    CGDisplayModeRelease(mode);
                    s
                }
            };
            Monitor {
                name: format!("Display {}", i + 1),
                x: b.origin.x as i32,
                y: b.origin.y as i32,
                w: pw as u32,
                h: ph as u32,
                primary: id == main,
                handle: id as u64,
                system_index: Some(i as u32),
            }
        })
        .collect()
}

pub fn capture_sources(m: &Monitor) -> Vec<Source> {
    vec![Source::Avfoundation {
        screen: m.system_index.unwrap_or(0),
    }]
}

pub fn foreground_title() -> Option<String> {
    // The frontmost app's name, through System Events (no extra permission for the name).
    let out = Command::new("osascript")
        .args(["-e", "tell application \"System Events\" to get name of first process whose frontmost is true"])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !name.is_empty()).then_some(name)
}

pub fn hide_console(_cmd: &mut Command) {}

pub fn play_saved_sound() {
    let _ = Command::new("afplay")
        .arg("/System/Library/Sounds/Pop.aiff")
        .spawn();
}

pub fn play_marker_sound() {
    let _ = Command::new("afplay")
        .arg("/System/Library/Sounds/Tink.aiff")
        .spawn();
}

pub fn copy_files(paths: &[std::path::PathBuf]) -> Result<(), String> {
    // Finder-style file copy, so the clip pastes into chats and folders.
    let list: Vec<String> = paths
        .iter()
        .map(|p| {
            format!(
                "POSIX file \"{}\"",
                p.display().to_string().replace('"', "\\\"")
            )
        })
        .collect();
    let script = format!("set the clipboard to {{{}}}", list.join(", "));
    let ok = Command::new("osascript")
        .args(["-e", &script])
        .status()
        .map_err(|e| e.to_string())?
        .success();
    if ok {
        Ok(())
    } else {
        Err("could not set the clipboard".into())
    }
}

/// The primary monitor's usable area, when the system says (else the caller estimates).
pub fn work_area() -> Option<(i32, i32, u32, u32)> {
    None
}

/// Showing a window without focus isn't done here; the caller shows it normally.
pub fn show_without_focus(_raw: u64) -> bool {
    false
}
