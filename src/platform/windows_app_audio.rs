//! Voice chat on its own track (Windows 11): WASAPI process loopback captures one app's sound
//! (and its child processes) and, separately, everything except that app. The first fills the
//! chat ring; the second takes the place of plain desktop audio, so the game track leaves chat
//! out. When the app isn't running, the "everything except" capture leaves out only Kairo.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ::windows::Win32::Foundation::{CloseHandle, HANDLE};
use ::windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, PROCESS_LOOPBACK_MODE,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVE_FORMAT_PCM, WAVEFORMATEX,
};
use ::windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use ::windows::Win32::System::Com::{BLOB, COINIT_MULTITHREADED, CoInitializeEx};
use ::windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use ::windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use ::windows::Win32::System::Variant::VT_BLOB;
use ::windows::core::{HRESULT, IUnknown, Interface, Ref, implement};

use kairo::pcm::PcmRing;

use crate::log::{info, warning};
use crate::recorder::{Shared, now_us};

const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const BUFFER_100NS: i64 = 2_000_000;

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated(mpsc::Sender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(
        &self,
        _op: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> ::windows::core::Result<()> {
        let _ = self.0.send(());
        Ok(())
    }
}

/// One process-loopback capture: a process tree's sound, or everything but it.
struct Capture {
    client: IAudioClient,
    capture: IAudioCaptureClient,
}

impl Capture {
    fn open(pid: u32, mode: PROCESS_LOOPBACK_MODE) -> Result<Capture, String> {
        let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: mode,
                },
            },
        };
        // ManuallyDrop: PROPVARIANT's Drop would free the blob, which points at the stack.
        let prop = std::mem::ManuallyDrop::new(PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 {
                        blob: BLOB {
                            cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                            pBlobData: &mut params as *mut _ as *mut u8,
                        },
                    },
                }),
            },
        });
        let (tx, rx) = mpsc::channel();
        let handler: IActivateAudioInterfaceCompletionHandler = Activated(tx).into();
        // SAFETY: the parameters outlive the call (we wait for completion below).
        let op = unsafe {
            ActivateAudioInterfaceAsync(
                VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
                &IAudioClient::IID,
                Some(&*prop),
                &handler,
            )
        }
        .map_err(|e| format!("process loopback isn't available here ({e})"))?;
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| "process loopback didn't start in time".to_string())?;
        let mut hr = HRESULT(0);
        let mut unknown: Option<IUnknown> = None;
        // SAFETY: the operation has completed.
        unsafe { op.GetActivateResult(&mut hr, &mut unknown) }.map_err(|e| e.to_string())?;
        hr.ok()
            .map_err(|e| format!("process loopback isn't available here ({e})"))?;
        let client: IAudioClient = unknown
            .ok_or("no audio client")?
            .cast()
            .map_err(|e| e.to_string())?;
        let fmt = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: CHANNELS,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * CHANNELS as u32 * 2,
            nBlockAlign: CHANNELS * 2,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        // SAFETY: an activated client, initialised once with a format that lives through the call.
        unsafe {
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
                    BUFFER_100NS,
                    0,
                    &fmt,
                    None,
                )
                .map_err(|e| e.to_string())?;
            let capture: IAudioCaptureClient = client.GetService().map_err(|e| e.to_string())?;
            client.Start().map_err(|e| e.to_string())?;
            Ok(Capture { client, capture })
        }
    }

    /// Hands every waiting packet to `f`: its start on the wall clock (µs), and its samples.
    fn read(&mut self, clock: &Clock, f: &mut dyn FnMut(i64, &[f32])) -> Result<(), String> {
        // SAFETY: GetBuffer/ReleaseBuffer pairs on a started capture client; the slice is read
        // before the buffer is released.
        unsafe {
            loop {
                let n = self
                    .capture
                    .GetNextPacketSize()
                    .map_err(|e| e.to_string())?;
                if n == 0 {
                    return Ok(());
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                let mut qpc = 0u64;
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))
                    .map_err(|e| e.to_string())?;
                let len = frames as usize * CHANNELS as usize;
                let samples: Vec<f32> =
                    if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                        vec![0.0; len]
                    } else {
                        std::slice::from_raw_parts(data as *const i16, len)
                            .iter()
                            .map(|s| *s as f32 / 32768.0)
                            .collect()
                    };
                self.capture
                    .ReleaseBuffer(frames)
                    .map_err(|e| e.to_string())?;
                f(clock.wall_us(qpc as i64), &samples);
            }
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: stopping a started client.
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

/// Turns the performance counter (as audio packets are stamped, in 100 ns) into wall-clock µs.
struct Clock {
    freq: i64,
}

impl Clock {
    fn new() -> Clock {
        let mut freq = 0i64;
        // SAFETY: writes the frequency into a local.
        let _ = unsafe { QueryPerformanceFrequency(&mut freq) };
        Clock { freq: freq.max(1) }
    }

    fn wall_us(&self, qpc_100ns: i64) -> i64 {
        let mut now = 0i64;
        // SAFETY: writes the counter into a local.
        let _ = unsafe { QueryPerformanceCounter(&mut now) };
        let now_100ns = (now as i128 * 10_000_000 / self.freq as i128) as i64;
        now_us() - (now_100ns - qpc_100ns).max(0) / 10
    }
}

/// The root process of the app named `app` (an executable whose name starts with it, such as
/// Discord, DiscordPTB, DiscordCanary), if it runs.
pub fn find_app(app: &str) -> Option<u32> {
    let want = app.trim().to_lowercase();
    if want.is_empty() {
        return None;
    }
    // SAFETY: a process snapshot walked with a correctly sized entry, then closed.
    unsafe {
        let snap: HANDLE = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut all: Vec<(u32, u32, String)> = Vec::new();
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let end = e
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(e.szExeFile.len());
                let name = String::from_utf16_lossy(&e.szExeFile[..end]).to_lowercase();
                all.push((e.th32ProcessID, e.th32ParentProcessID, name));
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        let matches: Vec<&(u32, u32, String)> = all
            .iter()
            .filter(|(_, _, n)| n.starts_with(&want))
            .collect();
        let is_match = |pid: u32| matches.iter().any(|(p, _, _)| *p == pid);
        matches
            .iter()
            .find(|(_, parent, _)| !is_match(*parent))
            .or(matches.first())
            .map(|(pid, _, _)| *pid)
    }
}

/// The running split capture; dropping it stops it.
pub struct Split {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// The app's process, while it runs (for the status line).
    pub app_running: Arc<Mutex<bool>>,
}

impl Drop for Split {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Puts a ring at `track` for this capture's sound, keeping one that's already there; a new one
/// carries on whatever a session is writing.
fn ensure_ring(shared: &Shared, track: crate::recorder::Track, keep_us: i64) {
    let fits = shared
        .track(track)
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|r| r.rate == RATE && r.channels == CHANNELS);
    if !fits {
        shared.set_ring(track, Some(PcmRing::new(RATE, CHANNELS, keep_us)));
    }
}

/// Starts capturing `app`'s sound into the chat ring and everything else into the desktop ring.
/// Fails at once where process loopback doesn't exist (before Windows 11).
pub fn start(shared: Arc<Shared>, app: String) -> Result<Split, String> {
    // SAFETY: COM on this thread, for the first activation.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    // Probe: everything but Kairo itself.
    drop(Capture::open(
        std::process::id(),
        PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    )?);
    let stop = Arc::new(AtomicBool::new(false));
    let app_running = Arc::new(Mutex::new(false));
    let thread = {
        let (stop, running) = (stop.clone(), app_running.clone());
        std::thread::Builder::new()
            .name("app-audio".into())
            .spawn(move || run(shared, app, stop, running))
            .map_err(|e| e.to_string())?
    };
    Ok(Split {
        stop,
        thread: Some(thread),
        app_running,
    })
}

fn run(shared: Arc<Shared>, app: String, stop: Arc<AtomicBool>, running: Arc<Mutex<bool>>) {
    // SAFETY: COM for this thread's captures.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let clock = Clock::new();
    let keep = shared.settings().buffer_seconds() as i64 * 1_000_000;
    ensure_ring(&shared, crate::recorder::Track::Game, keep);
    let mut target: Option<Option<u32>> = None;
    let mut rest: Option<Capture> = None;
    let mut chat: Option<Capture> = None;
    let mut last_look = Instant::now() - Duration::from_secs(60);
    while !stop.load(Ordering::Relaxed) {
        // Notice the app starting or quitting.
        if last_look.elapsed() > Duration::from_secs(3) {
            last_look = Instant::now();
            let now = find_app(&app);
            if target != Some(now) {
                target = Some(now);
                *running.lock().unwrap() = now.is_some();
                let leave_out = now.unwrap_or(std::process::id());
                rest = Capture::open(leave_out, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE)
                    .map_err(|e| warning!("desktop audio without {app}: {e}"))
                    .ok();
                chat = now.and_then(|pid| {
                    ensure_ring(&shared, crate::recorder::Track::Chat, keep);
                    Capture::open(pid, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE)
                        .map_err(|e| warning!("{app} audio: {e}"))
                        .ok()
                });
                info!(
                    "app audio: {}",
                    match now {
                        Some(pid) => format!("{app} (process {pid}) on its own track"),
                        None => format!("{app} isn't running"),
                    }
                );
            }
        }
        for (cap, slot) in [(&mut rest, &shared.desktop), (&mut chat, &shared.chat)] {
            if let Some(c) = cap {
                let mut push = |start: i64, s: &[f32]| {
                    let frames = (s.len() / CHANNELS as usize) as i64;
                    let end = start + frames * 1_000_000 / RATE as i64;
                    if let Some(r) = slot.lock().unwrap().as_mut() {
                        r.push(s, end);
                    }
                };
                if let Err(e) = c.read(&clock, &mut push) {
                    warning!("app audio stopped: {e}; reopening");
                    // Look again (and reopen) right away.
                    target = None;
                    last_look = Instant::now() - Duration::from_secs(60);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens a capture of everything but this test, reads a moment of it, and closes it.
    #[test]
    #[ignore = "captures this PC's sound for a moment; needs Windows 11"]
    fn process_loopback_starts() {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let mut c = Capture::open(
            std::process::id(),
            PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
        )
        .expect("process loopback");
        let clock = Clock::new();
        let mut frames = 0usize;
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(300) {
            c.read(&clock, &mut |at, s| {
                assert!(
                    (at - now_us()).abs() < 2_000_000,
                    "stamped on the wall clock"
                );
                frames += s.len() / 2;
            })
            .unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }
        println!(
            "{frames} frames in 300 ms; Discord process: {:?}",
            find_app("Discord")
        );
    }
}
