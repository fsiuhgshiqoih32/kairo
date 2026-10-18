//! Finding and running the bundled ffmpeg.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rewindr::args::{self, Encoder};

use crate::platform;

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

/// The ffmpeg to use: `REWINDR_FFMPEG`, else the one shipped next to Rewindr, else one on PATH.
pub fn path() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("REWINDR_FFMPEG").filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!(
                "REWINDR_FFMPEG points at {}, which doesn't exist",
                p.display()
            ))
        };
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let mut near = vec![dir.join(exe_name())];
        if cfg!(target_os = "macos") {
            near.push(dir.join("../Resources").join(exe_name()));
        }
        if let Some(p) = near.into_iter().find(|p| p.is_file()) {
            return Ok(p);
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&paths) {
            let p = d.join(exe_name());
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    Err("ffmpeg is missing. It ships with Rewindr: keep ffmpeg next to the rewindr program (re-download the \
         release if it's gone)."
        .into())
}

/// An ffmpeg command with no console window and no stdin.
pub fn command(ffmpeg: &Path) -> Command {
    let mut cmd = Command::new(ffmpeg);
    cmd.stdin(Stdio::null());
    platform::hide_console(&mut cmd);
    cmd
}

/// Runs ffmpeg to completion, giving up after `timeout`. `Err` carries its error output.
pub fn run(ffmpeg: &Path, args: &[String], timeout: Duration) -> Result<(), String> {
    let mut child = command(ffmpeg)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    let mut stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            use std::io::Read;
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break st;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err("ffmpeg took too long".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let err = reader.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        Err(last_lines(&err, 4).unwrap_or_else(|| format!("ffmpeg failed ({status})")))
    }
}

/// The last few non-empty lines of ffmpeg's error output.
pub fn last_lines(text: &str, n: usize) -> Option<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    (!lines.is_empty()).then(|| lines[lines.len().saturating_sub(n)..].join(" / "))
}

pub fn version(ffmpeg: &Path) -> Option<String> {
    let out = command(ffmpeg)
        .arg("-version")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(|l| {
        l.trim_start_matches("ffmpeg version ")
            .split(' ')
            .next()
            .unwrap_or(l)
            .to_string()
    })
}

/// The VA-API render node to use on Linux.
pub fn vaapi_device() -> Option<String> {
    (128..136)
        .map(|n| format!("/dev/dri/renderD{n}"))
        .find(|p| Path::new(p).exists())
}

/// True if `encoder` can encode on this machine (it may be built in yet lack the hardware).
pub fn encoder_works(ffmpeg: &Path, encoder: Encoder) -> Result<(), String> {
    if encoder == Encoder::Vaapi && vaapi_device().is_none() {
        return Err("no VA-API device".into());
    }
    run(
        ffmpeg,
        &args::probe_args(encoder, vaapi_device().as_deref()),
        Duration::from_secs(15),
    )
}

/// The encoders that work here, best first.
pub fn working_encoders(ffmpeg: &Path) -> Vec<Encoder> {
    Encoder::candidates()
        .into_iter()
        .filter(|e| encoder_works(ffmpeg, *e).is_ok())
        .collect()
}
