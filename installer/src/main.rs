//! Kairo Setup: installs Kairo for the current user (no administrator rights), updates an
//! installed copy, and uninstalls it. The release zip rides at the end of this program's file
//! (see `payload`).
//!
//! ```text
//! kairo-setup                          the installer window
//! kairo-setup --silent [--dir D] [--no-desktop] [--no-start-menu] [--autostart on|off]
//!               [--no-register] [--no-stop] [--launch]
//! uninstall --uninstall [--silent] [--remove-settings]
//! kairo-setup --pack <release.zip> <out.exe>    make an installer from a release zip
//! ```

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod install;
mod payload;
mod ui;
mod win;

use std::path::{Path, PathBuf};
use std::sync::Arc;

struct Args {
    list: Vec<String>,
}

impl Args {
    fn has(&self, flag: &str) -> bool {
        self.list.iter().any(|a| a == flag)
    }

    fn value(&self, flag: &str) -> Option<String> {
        let i = self.list.iter().position(|a| a == flag)?;
        self.list.get(i + 1).cloned()
    }
}

fn main() {
    let args = Args {
        list: std::env::args().skip(1).collect(),
    };
    let silent = args.has("--silent");
    if let Err(e) = run(&args) {
        eprintln!("kairo-setup: {e}");
        if !silent {
            let _ = rfd::MessageDialog::new()
                .set_title("Kairo Setup")
                .set_description(&e)
                .set_level(rfd::MessageLevel::Error)
                .show();
        }
        std::process::exit(1);
    }
}

fn run(args: &Args) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    if args.has("--pack") {
        let (Some(zip), Some(out)) = (args.value("--pack"), args.list.last()) else {
            return Err("usage: --pack <release.zip> <out.exe>".into());
        };
        return payload::pack(&me, Path::new(&zip), Path::new(out)).map_err(|e| e.to_string());
    }
    let zip = payload::read(&me).map_err(|e| e.to_string())?;
    // The uninstaller is this program without a zip.
    if args.has("--uninstall") || zip.is_none() {
        return uninstall(args, &me);
    }
    let zip = zip.expect("checked above");
    if !args.has("--silent") {
        return ui::run(ui::Mode::Install(Arc::new(zip)));
    }
    let dir = install::install_dir(
        &args
            .value("--dir")
            .map(PathBuf::from)
            .unwrap_or_else(install::default_dir),
    );
    let o = install::Options {
        dir: dir.clone(),
        start_menu: !args.has("--no-start-menu"),
        desktop: !args.has("--no-desktop"),
        autostart: args.value("--autostart").map(|v| v == "on"),
        register: !args.has("--no-register"),
        stop_running: !args.has("--no-stop"),
        replace: if args.has("--no-stop") {
            None
        } else {
            install::old_rewindr()
        },
    };
    install::install(&zip, &me, &o, &install::Shared::default())?;
    if args.has("--launch") {
        win::launch(&dir.join(install::PROGRAM), &[])?;
    }
    Ok(())
}

fn uninstall(args: &Args, me: &Path) -> Result<(), String> {
    let dir = args
        .value("--dir")
        .map(PathBuf::from)
        .or_else(|| me.parent().map(Path::to_path_buf))
        .ok_or("no install folder")?;
    if !dir.join(install::PROGRAM).exists() && !dir.join(install::FILE_LIST).exists() {
        return Err(format!("Kairo isn't installed in {}.", dir.display()));
    }
    // Run from a copy outside the folder, so the folder can go too.
    if !args.has("--relaunched") && me.parent() == Some(dir.as_path()) {
        let copy = std::env::temp_dir().join(format!("kairo-uninstall-{}.exe", std::process::id()));
        payload::write_program(me, &copy).map_err(|e| e.to_string())?;
        let mut rest: Vec<String> = vec![
            "--uninstall".into(),
            "--relaunched".into(),
            "--dir".into(),
            dir.display().to_string(),
        ];
        for flag in ["--silent", "--remove-settings"] {
            if args.has(flag) {
                rest.push(flag.into());
            }
        }
        let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
        return win::launch(&copy, &rest);
    }
    let result = if args.has("--silent") {
        let r = install::Removal {
            dir: dir.clone(),
            settings: args.has("--remove-settings"),
            system: true,
        };
        let result = install::uninstall(&r, &install::Shared::default());
        install::remove_uninstaller(&dir);
        result
    } else {
        ui::run(ui::Mode::Uninstall(dir))
    };
    if args.has("--relaunched") {
        win::delete_later(me);
    }
    result
}
