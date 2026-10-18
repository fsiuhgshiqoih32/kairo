use std::process::Command;

use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};

use rewindr::args::Source;

use super::Monitor;

pub fn init_process() {}

pub fn show_error(message: &str) {
    eprintln!("rewindr: {message}");
    // A desktop dialog where one of the usual tools is installed.
    for (prog, args) in [
        (
            "zenity",
            vec!["--error", "--title=Rewindr", "--text", message],
        ),
        ("kdialog", vec!["--title", "Rewindr", "--error", message]),
        ("notify-send", vec!["Rewindr", message]),
    ] {
        if Command::new(prog).args(&args).status().is_ok() {
            return;
        }
    }
}

fn display_name() -> String {
    std::env::var("DISPLAY")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| ":0".into())
}

pub fn monitors() -> Vec<Monitor> {
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return Vec::new();
    };
    let root = conn.setup().roots[screen_num].root;
    let mut out = Vec::new();
    if let Some(reply) = conn
        .randr_get_monitors(root, true)
        .ok()
        .and_then(|c| c.reply().ok())
    {
        for (i, m) in reply.monitors.iter().enumerate() {
            let name = conn
                .get_atom_name(m.name)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| String::from_utf8_lossy(&r.name).to_string())
                .unwrap_or_else(|| format!("Display {}", i + 1));
            out.push(Monitor {
                name,
                x: m.x as i32,
                y: m.y as i32,
                w: m.width as u32,
                h: m.height as u32,
                primary: m.primary,
                handle: 0,
                system_index: Some(i as u32),
            });
        }
    }
    if out.is_empty() {
        let s = &conn.setup().roots[screen_num];
        out.push(Monitor {
            name: "Screen".into(),
            x: 0,
            y: 0,
            w: s.width_in_pixels as u32,
            h: s.height_in_pixels as u32,
            primary: true,
            handle: 0,
            system_index: Some(0),
        });
    }
    out
}

pub fn capture_sources(m: &Monitor) -> Vec<Source> {
    vec![Source::X11 {
        display: display_name(),
        x: m.x,
        y: m.y,
        w: m.w,
        h: m.h,
    }]
}

pub fn foreground_title() -> Option<String> {
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen_num].root;
    let atom = |name: &[u8]| {
        conn.intern_atom(false, name)
            .ok()?
            .reply()
            .ok()
            .map(|r| r.atom)
    };
    let active = atom(b"_NET_ACTIVE_WINDOW")?;
    let wm_name = atom(b"_NET_WM_NAME")?;
    let utf8 = atom(b"UTF8_STRING")?;
    let reply = conn
        .get_property(false, root, active, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    let win = reply.value32()?.next()?;
    let title = conn
        .get_property(false, win, wm_name, utf8, 0, 256)
        .ok()?
        .reply()
        .ok()?;
    let s = String::from_utf8_lossy(&title.value).trim().to_string();
    (!s.is_empty()).then_some(s)
}

pub fn hide_console(_cmd: &mut Command) {}

pub fn play_saved_sound() {
    let path = std::env::temp_dir().join("rewindr-saved.wav");
    if !path.exists() {
        let _ = std::fs::write(&path, super::saved_sound_wav());
    }
    if Command::new("paplay").arg(&path).spawn().is_err() {
        let _ = Command::new("aplay").arg("-q").arg(&path).spawn();
    }
}

pub fn copy_files(paths: &[std::path::PathBuf]) -> Result<(), String> {
    // text/uri-list through xclip or wl-copy, whichever is installed.
    use std::io::Write;
    let uris: String = paths
        .iter()
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .map(|p| format!("file://{}\r\n", p.display()))
        .collect();
    for (prog, args) in [
        ("wl-copy", vec!["--type", "text/uri-list"]),
        (
            "xclip",
            vec!["-selection", "clipboard", "-t", "text/uri-list"],
        ),
    ] {
        if let Ok(mut child) = Command::new(prog)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(uris.as_bytes());
            }
            if child.wait().is_ok_and(|s| s.success()) {
                return Ok(());
            }
        }
    }
    Err("install xclip (or wl-clipboard) to copy clips".into())
}

/// The primary monitor's usable area, when the system says (else the caller estimates).
pub fn work_area() -> Option<(i32, i32, u32, u32)> {
    None
}
