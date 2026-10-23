//! Finding and running the bundled ffmpeg.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kairo::args::{self, Encoder};

use crate::platform;

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

/// The ffmpeg to use: `KAIRO_FFMPEG`, else the one shipped next to Kairo, else one on PATH.
pub fn path() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("KAIRO_FFMPEG").filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!(
                "KAIRO_FFMPEG points at {}, which doesn't exist",
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
    Err("ffmpeg is missing. It ships with Kairo: keep ffmpeg next to the kairo program (re-download the \
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

/// Starts `cmd`, tied to this process: if Kairo ends any way at all (a crash, being killed,
/// signing out), Windows ends the ffmpeg too, instead of leaving it recording or writing half a
/// file.
pub fn spawn(cmd: &mut Command) -> std::io::Result<Child> {
    let child = cmd.spawn()?;
    #[cfg(windows)]
    tie(&child);
    Ok(child)
}

#[cfg(windows)]
fn tie(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    // Never closed: the handle goes when the process does, and every process in the job with it.
    static JOB: OnceLock<usize> = OnceLock::new();
    let job = *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return 0;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of_val(&info) as u32,
        );
        job as usize
    });
    if job != 0 {
        unsafe { AssignProcessToJobObject(job as _, child.as_raw_handle() as _) };
    }
}

/// ffmpegs reading a clip for pictures (hover previews, filmstrips, the banner), by the clip.
/// Windows can't move or delete a file while ffmpeg has it open, so deleting a clip stops them
/// first, and none starts on a clip in `HELD` meanwhile.
type Readers = Vec<(PathBuf, Arc<Mutex<Child>>)>;
static READERS: Mutex<Readers> = Mutex::new(Vec::new());
static HELD: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// An ffmpeg reading a clip, which [`stop_reading`] can end.
pub struct Reading(Arc<Mutex<Child>>);

impl Reading {
    /// Lets [`stop_reading`] end `child`, which reads `clip` (at once, if it's being deleted).
    /// Take its output pipes first.
    pub fn new(clip: &Path, child: Child) -> Reading {
        let child = Arc::new(Mutex::new(child));
        let mut readers = READERS.lock().unwrap();
        if HELD.lock().unwrap().iter().any(|p| p == clip) {
            let _ = child.lock().unwrap().kill();
        }
        readers.push((clip.to_path_buf(), child.clone()));
        Reading(child)
    }

    pub fn try_wait(&self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.0.lock().unwrap().try_wait()
    }

    /// Waits for it to end (without holding up `stop_reading` meanwhile).
    pub fn wait(&self) -> std::io::Result<std::process::ExitStatus> {
        loop {
            if let Some(st) = self.try_wait()? {
                return Ok(st);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn kill(&self) {
        let mut c = self.0.lock().unwrap();
        let _ = c.kill();
        let _ = c.wait();
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        READERS
            .lock()
            .unwrap()
            .retain(|(_, c)| !Arc::ptr_eq(c, &self.0));
    }
}

/// Ends the ffmpegs reading `clip`, and keeps new ones off it until the hold is dropped: for
/// moving or deleting it.
pub fn stop_reading(clip: &Path) -> Hold {
    HELD.lock().unwrap().push(clip.to_path_buf());
    let found: Vec<Arc<Mutex<Child>>> = READERS
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _)| p == clip)
        .map(|(_, c)| c.clone())
        .collect();
    for c in found {
        let mut c = c.lock().unwrap();
        let _ = c.kill();
        // Once it has ended, Windows has closed its files.
        let _ = c.wait();
    }
    Hold(clip.to_path_buf())
}

/// Keeps ffmpegs from reading a clip while it's moved; see [`stop_reading`].
pub struct Hold(PathBuf);

impl Drop for Hold {
    fn drop(&mut self) {
        let mut held = HELD.lock().unwrap();
        if let Some(i) = held.iter().position(|p| *p == self.0) {
            held.remove(i);
        }
    }
}

/// What [`run`] says when ffmpeg ran past its time limit (and was stopped).
pub const TOO_LONG: &str = "ffmpeg took too long";

/// Runs ffmpeg to completion, giving up after `timeout`. `Err` carries its error output.
/// Below normal priority: these are saves, exports and pictures, made while a game may be
/// running.
pub fn run(ffmpeg: &Path, args: &[String], timeout: Duration) -> Result<(), String> {
    run_on(ffmpeg, args, timeout, None, None).map(drop)
}

/// Like [`run`], giving back what ffmpeg printed (its log and progress lines) when it succeeds.
pub fn run_logged(ffmpeg: &Path, args: &[String], timeout: Duration) -> Result<String, String> {
    run_on(ffmpeg, args, timeout, None, None)
}

/// Like [`run`], for an ffmpeg that only reads `clip`: deleting the clip stops it.
pub fn run_reading(
    ffmpeg: &Path,
    args: &[String],
    timeout: Duration,
    clip: &Path,
) -> Result<(), String> {
    run_on(ffmpeg, args, timeout, Some(clip), None).map(drop)
}

/// What an ffmpeg reading `pipe:0` gets, written on its own thread.
pub type Feed = Box<dyn FnOnce(&mut dyn std::io::Write) -> std::io::Result<()> + Send>;

/// Like [`run`], with `feed` writing ffmpeg's input (`pipe:0`), so it needn't go through a file
/// first. Fails if `feed` does, even though ffmpeg made a file of what it got.
pub fn run_fed(
    ffmpeg: &Path,
    args: &[String],
    timeout: Duration,
    feed: Feed,
) -> Result<(), String> {
    run_on(ffmpeg, args, timeout, None, Some(feed)).map(drop)
}

fn run_on(
    ffmpeg: &Path,
    args: &[String],
    timeout: Duration,
    reads: Option<&Path>,
    feed: Option<Feed>,
) -> Result<String, String> {
    let mut cmd = command(ffmpeg);
    platform::hide_console_low_priority(&mut cmd);
    if feed.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = spawn(cmd.args(args).stdout(Stdio::null()).stderr(Stdio::piped()))
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    // Dropping the pipe when done is ffmpeg's end of input.
    let writer = feed
        .zip(child.stdin.take())
        .map(|(feed, mut stdin)| std::thread::spawn(move || feed(&mut stdin)));
    let mut stderr = child.stderr.take();
    let child = match reads {
        Some(clip) => Reading::new(clip, child),
        None => Reading(Arc::new(Mutex::new(child))),
    };
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
            child.kill();
            return Err(TOO_LONG.into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let err = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(last_lines(&err, 4).unwrap_or_else(|| format!("ffmpeg failed ({status})")));
    }
    // ffmpeg ended well, so it read all it was given; a feed that failed gave it too little.
    match writer.map(|w| w.join()) {
        Some(Ok(Err(e))) => Err(e.to_string()),
        Some(Err(_)) => Err("the input stopped part way".into()),
        _ => Ok(err),
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

/// Whether this ffmpeg's gfxcapture has `sdr_white` (Kairo's own build, from bundle r7), so a
/// screen in HDR mode can be recorded at the right brightness. Asked once per program file.
pub fn has_sdr_white(ffmpeg: &Path) -> bool {
    use std::collections::HashMap;
    static KNOWN: std::sync::Mutex<Option<HashMap<PathBuf, bool>>> = std::sync::Mutex::new(None);
    let mut known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(v) = known.get_or_insert_with(HashMap::new).get(ffmpeg) {
        return *v;
    }
    let has = command(ffmpeg)
        .args(["-hide_banner", "-h", "filter=gfxcapture"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("sdr_white"));
    known
        .get_or_insert_with(HashMap::new)
        .insert(ffmpeg.to_path_buf(), has);
    has
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

/// What ffmpeg says about a clip: length, picture size, frame rate, sound tracks.
pub fn probe_clip(ff: &Path, clip: &Path) -> Result<kairo::media::Info, String> {
    let out = command(ff)
        .arg("-hide_banner")
        .arg("-i")
        .arg(clip)
        .output()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    kairo::media::parse_info(&String::from_utf8_lossy(&out.stderr))
        .ok_or_else(|| format!("{} isn't a video Kairo can open.", clip.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process that runs for half a minute and reads nothing.
    fn sleeper() -> Child {
        let mut c = if cfg!(windows) {
            let mut c = Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        };
        platform::hide_console(&mut c);
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    /// Deleting a clip ends what reads it (and only that), and nothing starts reading it until
    /// the delete is done.
    #[test]
    fn deleting_stops_readers_of_that_clip() {
        let (a, b) = (Path::new("readers-a.mp4"), Path::new("readers-b.mp4"));
        let ra = Reading::new(a, sleeper());
        let rb = Reading::new(b, sleeper());
        let held = stop_reading(a);
        assert!(ra.try_wait().unwrap().is_some());
        assert!(rb.try_wait().unwrap().is_none());
        // One that starts on it meanwhile is ended at once.
        let start = Instant::now();
        let late = Reading::new(a, sleeper());
        late.wait().unwrap();
        assert!(start.elapsed() < Duration::from_secs(10));
        drop(held);
        let after = Reading::new(a, sleeper());
        assert!(after.try_wait().unwrap().is_none());
        rb.kill();
        after.kill();
        drop((ra, rb, late, after));
        assert!(
            READERS
                .lock()
                .unwrap()
                .iter()
                .all(|(p, _)| p != a && p != b)
        );
    }

    /// What users hit: on Windows a clip can't go to Recently deleted while ffmpeg reads it for a
    /// hover preview, until the preview is stopped. Needs ffmpeg.
    #[test]
    #[ignore]
    fn a_clip_being_previewed_can_be_deleted() {
        let ff = path().expect("ffmpeg");
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let clip = d.join("Game 2026-10-19 10-00-00.mp4");
        let made = command(&ff)
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=640x360:rate=30")
            .args(["-t", "20", "-c:v", "libx264", "-preset", "ultrafast"])
            .arg(&clip)
            .status()
            .unwrap();
        assert!(made.success());
        // Reads it at playing speed: a preview of a big clip that takes a while.
        let child = command(&ff)
            .args(["-loglevel", "error", "-re", "-i"])
            .arg(&clip)
            .args(["-f", "null", "-"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let preview = Reading::new(&clip, child);
        std::thread::sleep(Duration::from_millis(500));
        if cfg!(windows) {
            let e = kairo::library::trash(d, &clip, 1_000).unwrap_err();
            assert!(kairo::library::in_use(&e), "{e}");
            assert!(clip.exists());
        }
        let held = stop_reading(&clip);
        let to = kairo::library::trash(d, &clip, 1_000).unwrap();
        drop(held);
        assert!(to.exists() && !clip.exists());
        assert!(preview.try_wait().unwrap().is_some());
    }
}
