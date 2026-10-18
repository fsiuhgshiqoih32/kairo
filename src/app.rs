//! Small things every window and command needs.

use std::path::Path;
use std::process::{Command, Stdio};

use eframe::egui;

/// Whether a window gets a macOS Dock icon.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A regular window (settings).
    Normal,
    /// No Dock icon (the notification).
    Accessory,
}

fn native_options(viewport: egui::ViewportBuilder, kind: Kind) -> eframe::NativeOptions {
    let accessory = kind == Kind::Accessory;
    eframe::NativeOptions {
        viewport: viewport.with_icon(crate::ui::app_icon()),
        centered: false,
        event_loop_builder: Some(Box::new(move |builder| {
            #[cfg(target_os = "macos")]
            {
                use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
                builder.with_activation_policy(if accessory {
                    ActivationPolicy::Accessory
                } else {
                    ActivationPolicy::Regular
                });
            }
            let _ = (builder, accessory);
        })),
        ..Default::default()
    }
}

/// Renderers to try: OpenGL starts fastest on Windows and Linux; wgpu covers machines without a
/// usable OpenGL (VMs, remote desktops). macOS uses Metal through wgpu. Rewindr's windows are
/// small, so OpenGL's full-screen quirks don't apply. `REWINDR_RENDERER=glow|wgpu` overrides.
fn renderers() -> Vec<eframe::Renderer> {
    use eframe::Renderer::{Glow, Wgpu};
    match std::env::var("REWINDR_RENDERER")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "wgpu" => vec![Wgpu],
        "glow" => vec![Glow, Wgpu],
        _ if cfg!(target_os = "macos") => vec![Wgpu, Glow],
        _ => vec![Glow, Wgpu],
    }
}

/// Opens a window and runs it until it closes, trying each renderer in turn if one fails to
/// start. `make` is called again only if the first renderer failed before the app was built.
pub fn run_window<A: eframe::App + 'static>(
    title: &str,
    viewport: egui::ViewportBuilder,
    kind: Kind,
    mut make: impl FnMut(&eframe::CreationContext<'_>) -> A,
) -> Result<(), String> {
    let mut last = String::from("no renderer available");
    for renderer in renderers() {
        let mut options = native_options(viewport.clone(), kind);
        options.renderer = renderer;
        let created = std::cell::Cell::new(false);
        let result = eframe::run_native(
            title,
            options,
            Box::new(|cc| {
                created.set(true);
                crate::ui::apply_theme(&cc.egui_ctx);
                Ok(Box::new(make(cc)) as Box<dyn eframe::App>)
            }),
        );
        match result {
            Ok(()) => return Ok(()),
            Err(e) if !created.get() => {
                crate::log::warning!("{renderer} renderer unavailable ({e}); trying the next one");
                last = e.to_string();
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Err(last)
}

/// For checking windows without a user: with `REWINDR_SHOT=<folder>` set, a window saves a
/// picture of itself as `<folder>/<name>.png` after a few frames and closes.
pub fn shot_hook(ctx: &egui::Context, frame_no: u64, name: &str) {
    if frame_no != 20 {
        if frame_no < 20 && std::env::var_os("REWINDR_SHOT").is_some() {
            ctx.request_repaint();
        }
        return;
    }
    let Some(path) = std::env::var_os("REWINDR_SHOT")
        .map(|d| std::path::PathBuf::from(d).join(format!("{name}.png")))
    else {
        return;
    };
    let c = ctx.clone();
    ctx.request_screenshot(move |img| {
        let [w, h] = img.size;
        let mut rgba = Vec::with_capacity(w * h * 4);
        for px in &img.pixels {
            rgba.extend_from_slice(&px.to_srgba_unmultiplied());
        }
        if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
            && let Err(e) = buf.save(&path)
        {
            eprintln!("could not save screenshot: {e}");
        }
        c.send_viewport_cmd(egui::ViewportCommand::Close);
        c.request_repaint();
    });
}

/// Starts another copy of this program, detached, e.g. `rewindr notify <clip>`.
pub fn spawn_self(args: &[&str]) -> Result<std::process::Child, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut cmd);
    cmd.spawn()
        .map_err(|e| format!("could not start rewindr {}: {e}", args.join(" ")))
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

/// Shows a file selected in its folder.
pub fn reveal(path: &Path) {
    if let Err(e) = opener::reveal(path) {
        crate::log::warning!("could not show {}: {e}", path.display());
        if let Some(dir) = path.parent() {
            open(dir);
        }
    }
}

/// "1.2 GB", "340 MB", "12 KB".
pub fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).max(1.0))
    }
}
