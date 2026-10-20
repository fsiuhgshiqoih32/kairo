//! The webcam. While it's on (and recording isn't paused), a second, small ffmpeg keeps the
//! camera in a replay buffer of its own, stamped with the same wall clock as the screen; saving a
//! clip lays the matching stretch over a corner of it.

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kairo::args;

use crate::ffmpeg;
use crate::log::{info, warning};
use crate::recorder::{Shared, now_us};

/// A camera: what to show, and what ffmpeg opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Camera {
    pub name: String,
    pub id: String,
}

/// The cameras on this computer, the system's first one first.
pub fn cameras(ffmpeg: &Path) -> Vec<Camera> {
    if cfg!(target_os = "linux") {
        return linux_cameras();
    }
    let Ok(out) = ffmpeg::command(ffmpeg)
        .args(args::camera_list_args())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stderr);
    let names = if cfg!(target_os = "macos") {
        args::avfoundation_cameras(&text)
    } else {
        args::dshow_cameras(&text)
    };
    names
        .into_iter()
        .map(|n| Camera {
            name: n.clone(),
            id: n,
        })
        .collect()
}

/// Video4Linux capture devices. A camera often has a second node for metadata; only the first
/// (index 0) gives pictures.
fn linux_cameras() -> Vec<Camera> {
    let Ok(dir) = std::fs::read_dir("/sys/class/video4linux") else {
        return Vec::new();
    };
    let mut found: Vec<Camera> = dir
        .flatten()
        .filter(|e| {
            std::fs::read_to_string(e.path().join("index")).map_or(true, |i| i.trim() == "0")
        })
        .map(|e| {
            let node = e.file_name().to_string_lossy().to_string();
            let name = std::fs::read_to_string(e.path().join("name"))
                .map(|n| n.trim().to_string())
                .unwrap_or_else(|_| node.clone());
            Camera {
                name,
                id: format!("/dev/{node}"),
            }
        })
        .collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

/// Whether the camera should be recording now.
fn wanted(shared: &Shared) -> bool {
    shared.settings().webcam && !shared.paused.load(Ordering::SeqCst)
}

/// Sleeps up to `d`, returning early when the camera's settings change or Kairo stops.
fn wait(shared: &Shared, d: Duration, generation: u64) {
    let until = Instant::now() + d;
    while Instant::now() < until
        && !shared.stop.load(Ordering::Relaxed)
        && shared.cam_generation.load(Ordering::SeqCst) == generation
    {
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn set_status(shared: &Shared, camera: Option<String>, error: Option<String>) {
    let mut st = shared.status.lock().unwrap();
    st.webcam = camera;
    st.webcam_error = error;
}

/// How long to wait before trying the camera again after `failures` failures in a row: longer
/// each time (a camera another app holds would otherwise blink its light every few seconds).
fn retry_after(failures: u32) -> Duration {
    Duration::from_secs((10u64 << failures.min(5)).min(300))
}

/// Runs until `shared.stop` is set. Call on its own thread.
pub fn run(shared: Arc<Shared>) {
    let mut failures = 0u32;
    while !shared.stop.load(Ordering::Relaxed) {
        let generation = shared.cam_generation.load(Ordering::SeqCst);
        if !wanted(&shared) {
            failures = 0;
            shared.cam.lock().unwrap().clear();
            set_status(&shared, None, None);
            wait(&shared, Duration::from_secs(1), generation);
            continue;
        }
        let Ok(ff) = ffmpeg::path() else {
            wait(&shared, Duration::from_secs(10), generation);
            continue;
        };
        let device = shared.settings().webcam_device;
        let found = cameras(&ff);
        let Some(cam) = found
            .iter()
            .find(|c| c.id == device)
            .or(found.first())
            .cloned()
        else {
            set_status(&shared, None, Some("No webcam found.".into()));
            wait(&shared, retry_after(failures), generation);
            failures += 1;
            continue;
        };
        match record(&ff, &cam, &shared, generation) {
            Ok(()) => {
                failures = 0;
                set_status(&shared, None, None);
            }
            Err(e) => {
                warning!("webcam {}: {e}", cam.name);
                set_status(&shared, None, Some(e));
                wait(&shared, retry_after(failures), generation);
                failures += 1;
            }
        }
    }
}

/// Records `cam` into the webcam buffer until it's no longer wanted (`Ok`) or ffmpeg stops.
fn record(ff: &Path, cam: &Camera, shared: &Arc<Shared>, generation: u64) -> Result<(), String> {
    let mut child = ffmpeg::command(ff)
        .args(args::webcam_args(&cam.id))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    shared.cam.lock().unwrap().new_run();
    let got = Arc::new(AtomicBool::new(false));
    let reader = {
        let mut out = child.stdout.take().expect("piped stdout");
        let (shared, got) = (shared.clone(), got.clone());
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n) = out.read(&mut buf) {
                if n == 0 {
                    break;
                }
                shared.cam.lock().unwrap().push(&buf[..n], now_us());
                got.store(true, Ordering::Relaxed);
            }
        })
    };
    let errors = {
        let mut err = child.stderr.take().expect("piped stderr");
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            s
        })
    };
    let started = Instant::now();
    let mut announced = false;
    let result = loop {
        if let Ok(Some(status)) = child.try_wait() {
            let _ = reader.join();
            let text = errors.join().unwrap_or_default();
            break Err(ffmpeg::last_lines(&text, 2)
                .unwrap_or_else(|| format!("the camera stopped ({status})")));
        }
        let unwanted = shared.stop.load(Ordering::Relaxed)
            || !wanted(shared)
            || shared.cam_generation.load(Ordering::SeqCst) != generation;
        let stalled = !got.load(Ordering::Relaxed) && started.elapsed() > Duration::from_secs(10);
        if unwanted || stalled {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            let _ = errors.join();
            break if stalled {
                Err("The camera sent no pictures. Another app may be using it.".into())
            } else {
                Ok(())
            };
        }
        if !announced && got.load(Ordering::Relaxed) {
            announced = true;
            info!("webcam: recording {}", cam.name);
            set_status(shared, Some(cam.name.clone()), None);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    shared.cam.lock().unwrap().clear();
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn retries_slow_down() {
        assert_eq!(super::retry_after(0).as_secs(), 10);
        assert_eq!(super::retry_after(1).as_secs(), 20);
        assert_eq!(super::retry_after(4).as_secs(), 160);
        assert_eq!(super::retry_after(9).as_secs(), 300);
    }
}
