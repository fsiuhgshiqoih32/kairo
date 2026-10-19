//! Voice clipping: say a phrase ("clip that") and Rewindr saves a clip. The microphone's sound
//! goes to Vosk, an offline speech recognizer, restricted to the phrase, so it's accurate and
//! light enough to run during games. Nothing leaves the computer.
//!
//! Vosk (its library and a 40 MB English model) is downloaded once, when voice clipping is
//! turned on, and checked against pinned SHA-256 hashes. It's loaded at run time, so Rewindr
//! runs without it.

use std::ffi::{CStr, CString, c_char, c_float, c_int, c_void};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rewindr::config::normalize_phrase;
use sha2::{Digest, Sha256};

use crate::log::{info, warning};
use crate::recorder::Shared;

/// Sound for the recognizer: samples, rate, channels.
pub type Chunk = (Vec<f32>, u32, u16);

/// Vosk listens at this rate.
const RATE: u32 = 16_000;
/// After hearing the phrase, ignore it for this long (one clip per shout).
const COOLDOWN: Duration = Duration::from_secs(4);
const MARKER: &str = "vosk 0.3.45 / model small-en-us-0.15";

struct Download {
    url: &'static str,
    sha256: &'static str,
    size: u64,
    /// Files to take: path in the archive (a prefix ending in '/' takes a whole folder), and
    /// where it goes under the voice folder.
    take: &'static [(&'static str, &'static str)],
}

#[cfg(all(windows, target_arch = "x86_64"))]
const LIBRARY: Option<Download> = Some(Download {
    url: "https://github.com/alphacep/vosk-api/releases/download/v0.3.45/vosk-0.3.45-py3-none-win_amd64.whl",
    sha256: "6994ddc68556c7e5730c3b6f6bad13320e3519b13ce3ed2aa25a86724e7c10ac",
    size: 13_997_596,
    take: &[
        ("vosk/libvosk.dll", "lib/libvosk.dll"),
        ("vosk/libgcc_s_seh-1.dll", "lib/libgcc_s_seh-1.dll"),
        ("vosk/libstdc++-6.dll", "lib/libstdc++-6.dll"),
        ("vosk/libwinpthread-1.dll", "lib/libwinpthread-1.dll"),
    ],
});
#[cfg(target_os = "macos")]
const LIBRARY: Option<Download> = Some(Download {
    url: "https://github.com/alphacep/vosk-api/releases/download/v0.3.42/vosk-0.3.42-py3-none-macosx_10_6_universal2.whl",
    sha256: "631b9ff6515945f92078c546a60deab2eeb4f63489a24526cc90e3164e415ff7",
    size: 4_709_760,
    take: &[("vosk/libvosk.dyld", "lib/libvosk.dylib")],
});
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const LIBRARY: Option<Download> = Some(Download {
    url: "https://github.com/alphacep/vosk-api/releases/download/v0.3.45/vosk-linux-x86_64-0.3.45.zip",
    sha256: "bbdc8ed85c43979f6443142889770ea95cbfbc56cffb5c5dcd73afa875c5fbb2",
    size: 7_185_065,
    take: &[("vosk-linux-x86_64-0.3.45/libvosk.so", "lib/libvosk.so")],
});
#[cfg(not(any(
    all(windows, target_arch = "x86_64"),
    target_os = "macos",
    all(target_os = "linux", target_arch = "x86_64")
)))]
const LIBRARY: Option<Download> = None;

const MODEL: Download = Download {
    url: "https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip",
    sha256: "30f26242c4eb449f948e42cb302dd7a686cb29a3423a8367f99ff41780942498",
    size: 41_205_931,
    take: &[("vosk-model-small-en-us-0.15/", "model/")],
};

#[cfg(windows)]
const LIB_FILE: &str = "lib\\libvosk.dll";
#[cfg(target_os = "macos")]
const LIB_FILE: &str = "lib/libvosk.dylib";
#[cfg(target_os = "linux")]
const LIB_FILE: &str = "lib/libvosk.so";

pub fn dir() -> PathBuf {
    rewindr::config::config_dir().join("voice")
}

pub fn available() -> bool {
    LIBRARY.is_some()
}

/// The recognizer and its model are downloaded.
pub fn installed() -> bool {
    std::fs::read_to_string(dir().join("installed.txt")).is_ok_and(|t| t.trim() == MARKER)
}

/// Downloads and unpacks Vosk and the model; `progress(done, total)` in bytes.
pub fn install(progress: &dyn Fn(u64, u64)) -> Result<(), String> {
    let lib = LIBRARY.ok_or("Voice clipping isn't available on this kind of computer yet.")?;
    let total = lib.size + MODEL.size;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_global(Some(Duration::from_secs(900)))
        .user_agent(concat!("Rewindr/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let root = dir();
    let _ = std::fs::remove_file(root.join("installed.txt"));
    let mut done = 0;
    for d in [&lib, &MODEL] {
        let bytes = fetch(&agent, d, &|n| progress(done + n, total))?;
        done += d.size;
        unpack(&bytes, d.take, &root)?;
    }
    std::fs::write(root.join("installed.txt"), MARKER).map_err(|e| e.to_string())?;
    progress(total, total);
    info!("voice clipping installed in {}", root.display());
    Ok(())
}

fn fetch(agent: &ureq::Agent, d: &Download, progress: &dyn Fn(u64)) -> Result<Vec<u8>, String> {
    let mut response = agent
        .get(d.url)
        .call()
        .map_err(|e| format!("Couldn't download the speech files: {e}"))?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(d.size + 1_000_000)
        .reader();
    let mut bytes = Vec::with_capacity(d.size as usize);
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("Download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        progress(bytes.len() as u64);
    }
    let hash: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if hash != d.sha256 {
        return Err(format!(
            "The download from {} didn't match its checksum; try again.",
            d.url
        ));
    }
    Ok(bytes)
}

fn unpack(bytes: &[u8], take: &[(&str, &str)], root: &Path) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
        if f.is_dir() {
            continue;
        }
        let name = f.name().map_err(|e| e.to_string())?.to_string();
        let Some(dest) = take.iter().find_map(|(from, to)| {
            if from.ends_with('/') {
                name.strip_prefix(from).map(|rest| format!("{to}{rest}"))
            } else {
                (name == *from).then(|| to.to_string())
            }
        }) else {
            continue;
        };
        // Archive paths are trusted (pinned hashes), but never write outside the folder.
        if dest.split('/').any(|p| p == ".." || p.is_empty()) {
            continue;
        }
        let path = root.join(&dest);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = Vec::new();
        f.read_to_end(&mut out).map_err(|e| e.to_string())?;
        std::fs::write(&path, out).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

// --- The recognizer ------------------------------------------------------------------------

type ModelNew = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type Free = unsafe extern "C" fn(*mut c_void);
type RecognizerNewGrm = unsafe extern "C" fn(*mut c_void, c_float, *const c_char) -> *mut c_void;
type AcceptShort = unsafe extern "C" fn(*mut c_void, *const i16, c_int) -> c_int;
type Text = unsafe extern "C" fn(*mut c_void) -> *const c_char;
type SetLogLevel = unsafe extern "C" fn(c_int);
type SetWords = unsafe extern "C" fn(*mut c_void, c_int);

/// Vosk, loaded, with the model.
pub struct Engine {
    model: *mut c_void,
    model_free: Free,
    recognizer_new: RecognizerNewGrm,
    recognizer_free: Free,
    accept: AcceptShort,
    result: Text,
    set_words: SetWords,
    _lib: libloading::Library,
}

// SAFETY: Vosk's model is read-only after loading and documented as shareable between threads;
// each recognizer is used by one thread at a time.
unsafe impl Send for Engine {}
unsafe impl Sync for Engine {}

impl Engine {
    pub fn load(root: &Path) -> Result<Engine, String> {
        let lib_path = root.join(LIB_FILE);
        let lib = open_library(&lib_path)
            .map_err(|e| format!("Couldn't load the speech recognizer: {e}"))?;
        // SAFETY: the symbols are Vosk's C API with these signatures (vosk_api.h, 0.3.4x), and
        // the library stays loaded as long as the Engine (`_lib`).
        unsafe {
            fn get<T: Copy>(lib: &libloading::Library, name: &[u8]) -> Result<T, String> {
                unsafe { lib.get::<T>(name) }.map(|s| *s).map_err(|e| {
                    let n = String::from_utf8_lossy(&name[..name.len() - 1]);
                    format!("the speech recognizer is missing {n}: {e}")
                })
            }
            let set_log_level: SetLogLevel = get(&lib, b"vosk_set_log_level\0")?;
            set_log_level(-1);
            let model_new: ModelNew = get(&lib, b"vosk_model_new\0")?;
            let model_path = CString::new(root.join("model").to_string_lossy().as_bytes())
                .map_err(|e| e.to_string())?;
            let model = model_new(model_path.as_ptr());
            if model.is_null() {
                return Err("The speech model couldn't be loaded; turn voice clipping off and on to download it again.".into());
            }
            Ok(Engine {
                model,
                model_free: get(&lib, b"vosk_model_free\0")?,
                recognizer_new: get(&lib, b"vosk_recognizer_new_grm\0")?,
                recognizer_free: get(&lib, b"vosk_recognizer_free\0")?,
                accept: get(&lib, b"vosk_recognizer_accept_waveform_s\0")?,
                result: get(&lib, b"vosk_recognizer_result\0")?,
                set_words: get(&lib, b"vosk_recognizer_set_words\0")?,
                _lib: lib,
            })
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: created by vosk_model_new; recognizers using it are gone (they hold an Arc).
        unsafe { (self.model_free)(self.model) }
    }
}

#[cfg(windows)]
fn open_library(path: &Path) -> Result<libloading::Library, libloading::Error> {
    use libloading::os::windows::{
        LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, Library,
    };
    // Its C++ runtime DLLs sit beside it. Windows looks in the DLL's own folder only when asked,
    // and only finds that folder in a path written with backslashes.
    let path = PathBuf::from(path.to_string_lossy().replace('/', "\\"));
    let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS;
    unsafe { Library::load_with_flags(&path, flags).map(Into::into) }
}

#[cfg(not(windows))]
fn open_library(path: &Path) -> Result<libloading::Library, libloading::Error> {
    unsafe { libloading::Library::new(path) }
}

/// Listens for one phrase.
pub struct Listener {
    engine: Arc<Engine>,
    recognizer: *mut c_void,
    pub phrase: String,
    words: Vec<String>,
    resampler: Resampler,
    buf: Vec<i16>,
}

// SAFETY: a recognizer is only used from the thread that owns the Listener.
unsafe impl Send for Listener {}

impl Listener {
    pub fn new(engine: Arc<Engine>, phrase: &str) -> Result<Listener, String> {
        let phrase = normalize_phrase(phrase);
        if phrase.is_empty() {
            return Err("The voice phrase is empty.".into());
        }
        // The phrase, common words that sound alike, or anything else as "[unk]".
        let mut list: Vec<String> = vec![phrase.clone(), "[unk]".into()];
        list.extend(
            FILLER
                .split_whitespace()
                .filter(|w| !phrase.split(' ').any(|p| p == *w))
                .map(String::from),
        );
        let grammar = serde_json::to_string(&list).map_err(|e| e.to_string())?;
        let grammar = CString::new(grammar).map_err(|e| e.to_string())?;
        // SAFETY: the model is alive (held by `engine`); the grammar is a valid C string.
        let recognizer =
            unsafe { (engine.recognizer_new)(engine.model, RATE as c_float, grammar.as_ptr()) };
        if recognizer.is_null() {
            return Err("The speech recognizer couldn't start.".into());
        }
        // Word confidences in final results.
        unsafe { (engine.set_words)(recognizer, 1) };
        Ok(Listener {
            words: phrase.split(' ').map(String::from).collect(),
            phrase,
            engine,
            recognizer,
            resampler: Resampler::default(),
            buf: Vec::new(),
        })
    }

    /// Feeds sound; true when the phrase was just heard.
    pub fn feed(&mut self, samples: &[f32], rate: u32, channels: u16) -> bool {
        self.buf.clear();
        self.resampler.push(samples, rate, channels, &mut self.buf);
        if self.buf.is_empty() {
            return false;
        }
        let e = &self.engine;
        // SAFETY: the recognizer is valid until drop; the buffer outlives the call.
        let ended =
            unsafe { (e.accept)(self.recognizer, self.buf.as_ptr(), self.buf.len() as c_int) } == 1;
        // Only finished utterances count: guesses mid-sentence give more false alarms.
        if !ended {
            return false;
        }
        let text = unsafe { CStr::from_ptr((e.result)(self.recognizer)) }
            .to_string_lossy()
            .to_string();
        serde_json::from_str::<serde_json::Value>(&text).is_ok_and(|v| heard_in(&v, &self.words))
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        // SAFETY: created by vosk_recognizer_new_grm.
        unsafe { (self.engine.recognizer_free)(self.recognizer) }
    }
}

/// Below this, the recognizer is guessing (tested: real phrases score 0.76 to 1.0).
const MIN_CONFIDENCE: f64 = 0.7;

/// Whether a final result (`{"result": [{"word", "conf"}...], "text"}`) has the phrase's words
/// in a row, each confidently.
fn heard_in(result: &serde_json::Value, words: &[String]) -> bool {
    let Some(list) = result.get("result").and_then(|r| r.as_array()) else {
        return contains_phrase(
            result.get("text").and_then(|t| t.as_str()).unwrap_or(""),
            words,
        );
    };
    let heard: Vec<(&str, f64)> = list
        .iter()
        .filter_map(|w| Some((w.get("word")?.as_str()?, w.get("conf")?.as_f64()?)))
        .collect();
    !words.is_empty()
        && heard.windows(words.len()).any(|win| {
            win.iter()
                .zip(words)
                .all(|((said, conf), want)| said == want && *conf >= MIN_CONFIDENCE)
        })
}

/// Whether the recognized words include the phrase's words in a row.
fn contains_phrase(said: &str, words: &[String]) -> bool {
    let heard: Vec<&str> = said.split_whitespace().collect();
    !words.is_empty()
        && heard
            .windows(words.len())
            .any(|w| w.iter().zip(words).all(|(a, b)| *a == b))
}

/// Mono 16 kHz 16-bit from any rate and channel count, across chunks (linear interpolation).
#[derive(Default)]
struct Resampler {
    rate: u32,
    /// Where the next output sample falls, in input samples from the current chunk's start
    /// (between -1 and 0: between the last chunk's final sample and this chunk's first).
    pos: f64,
    prev: f32,
}

impl Resampler {
    fn push(&mut self, samples: &[f32], rate: u32, channels: u16, out: &mut Vec<i16>) {
        let ch = channels.max(1) as usize;
        if rate == 0 || samples.len() < ch {
            return;
        }
        if rate != self.rate {
            *self = Resampler {
                rate,
                pos: 0.0,
                prev: 0.0,
            };
        }
        let mono: Vec<f32> = samples
            .chunks_exact(ch)
            .map(|f| f.iter().sum::<f32>() / ch as f32)
            .collect();
        let step = rate as f64 / RATE as f64;
        let n = mono.len() as f64;
        while self.pos < n - 1.0 {
            let i = self.pos.floor();
            let f = (self.pos - i) as f32;
            let a = if i < 0.0 { self.prev } else { mono[i as usize] };
            let b = mono[(i + 1.0) as usize];
            out.push(((a + (b - a) * f).clamp(-1.0, 1.0) * 32767.0) as i16);
            self.pos += step;
        }
        self.pos -= n;
        self.prev = *mono.last().expect("not empty");
    }
}

// --- In the background process -------------------------------------------------------------

/// Listens while voice clipping is on and calls `heard` when the phrase is said. Runs until
/// `shared.stop`; the microphone stream sends its sound through `shared.voice_tap`.
pub fn run(shared: Arc<Shared>, heard: impl Fn() + Send + 'static) {
    let (tx, rx): (SyncSender<Chunk>, Receiver<Chunk>) = sync_channel(64);
    let mut engine: Option<Arc<Engine>> = None;
    let mut listener: Option<Listener> = None;
    let mut failed_for: Option<String> = None;
    let mut last = Instant::now() - COOLDOWN;
    let set_status = |voice: Option<String>, error: Option<String>| {
        let mut st = shared.status.lock().unwrap();
        st.voice = voice;
        st.voice_error = error;
    };
    while !shared.stop.load(Ordering::Relaxed) {
        let s = shared.settings();
        let want = s.voice_clip && !shared.paused.load(Ordering::Relaxed);
        let phrase = normalize_phrase(&s.voice_phrase);
        if !want {
            if listener.take().is_some() {
                *shared.voice_tap.lock().unwrap() = None;
                info!("voice clipping off");
            }
            set_status(None, None);
            failed_for = None;
        } else if listener.as_ref().is_none_or(|l| l.phrase != phrase)
            && failed_for.as_deref() != Some(&phrase)
        {
            listener = None;
            let made = (|| {
                if !installed() {
                    return Err("Voice clipping needs its speech files: turn it off and on again in Settings.".to_string());
                }
                if engine.is_none() {
                    engine = Some(Arc::new(Engine::load(&dir())?));
                }
                Listener::new(engine.clone().expect("loaded"), &phrase)
            })();
            match made {
                Ok(l) => {
                    info!("voice clipping on: listening for \"{phrase}\"");
                    *shared.voice_tap.lock().unwrap() = Some(tx.clone());
                    set_status(Some(phrase.clone()), None);
                    listener = Some(l);
                    failed_for = None;
                }
                Err(e) => {
                    warning!("voice clipping: {e}");
                    set_status(None, Some(e));
                    failed_for = Some(phrase.clone());
                }
            }
        }
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok((samples, rate, channels)) => {
                if let Some(l) = &mut listener
                    && l.feed(&samples, rate, channels)
                    && last.elapsed() >= COOLDOWN
                {
                    last = Instant::now();
                    info!("heard \"{}\": saving a clip", l.phrase);
                    heard();
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    *shared.voice_tap.lock().unwrap() = None;
}

/// Shared state between the microphone stream and the listener (in `Shared`).
pub type Tap = Mutex<Option<SyncSender<Chunk>>>;

// --- Checking it with recordings -----------------------------------------------------------

/// `rewindr voice-test <phrase> +said.wav -not-said.wav ...`: downloads the recognizer if
/// needed, then checks that it hears the phrase in each "+" file and not in each "-" file.
pub fn test_files(phrase: &str, files: &[String]) -> Result<(), String> {
    if !installed() {
        println!("downloading the speech recognizer...");
        install(&|_, _| {})?;
    }
    let engine = Arc::new(Engine::load(&dir())?);
    let mut wrong = Vec::new();
    for f in files {
        let (expect, path) = match f.split_at(1) {
            ("+", p) => (true, p),
            ("-", p) => (false, p),
            _ => {
                return Err(format!(
                    "{f}: start each file with + (says the phrase) or - (doesn't)"
                ));
            }
        };
        let (samples, rate, channels) = read_wav(Path::new(path))?;
        let mut l = Listener::new(engine.clone(), phrase)?;
        let mut heard = 0;
        // Feed it in 20 ms pieces, like the microphone does, between stretches of silence (a
        // microphone hears quiet before and after someone speaks).
        let piece = (rate as usize / 50) * channels as usize;
        let silence = vec![0f32; rate as usize * channels as usize];
        for c in silence[..silence.len() / 2]
            .chunks(piece.max(1))
            .chain(samples.chunks(piece.max(1)))
            .chain(silence.chunks(piece.max(1)))
        {
            if l.feed(c, rate, channels) {
                heard += 1;
            }
        }
        println!("{path}: heard \"{}\" {heard} time(s)", l.phrase);
        if (heard > 0) != expect {
            wrong.push(path.to_string());
        }
    }
    if wrong.is_empty() {
        println!("voice test passed");
        Ok(())
    } else {
        Err(format!("wrong result for {}", wrong.join(", ")))
    }
}

/// A WAV file's samples (16-bit, 24-bit or 32-bit integer, or 32-bit float), rate and channels.
pub fn read_wav(path: &Path) -> Result<(Vec<f32>, u32, u16), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(format!("{} isn't a WAV file", path.display()));
    }
    let (mut format, mut channels, mut rate, mut bits) = (0u16, 0u16, 0u32, 0u16);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = &bytes[at + 8..(at + 8 + len).min(bytes.len())];
        if id == b"fmt " && body.len() >= 16 {
            format = u16::from_le_bytes([body[0], body[1]]);
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            bits = u16::from_le_bytes([body[14], body[15]]);
            if format == 0xFFFE && body.len() >= 26 {
                format = u16::from_le_bytes([body[24], body[25]]);
            }
        } else if id == b"data" {
            let samples: Vec<f32> = match (format, bits) {
                (1, 16) => body
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                    .collect(),
                (1, 24) => body
                    .chunks_exact(3)
                    .map(|b| (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0)
                    .collect(),
                (1, 32) => body
                    .chunks_exact(4)
                    .map(|b| i32::from_le_bytes(b.try_into().unwrap()) as f32 / 2_147_483_648.0)
                    .collect(),
                (3, 32) => body
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                    .collect(),
                _ => {
                    return Err(format!(
                        "{}: unsupported WAV format {format}/{bits}-bit",
                        path.display()
                    ));
                }
            };
            return Ok((samples, rate, channels.max(1)));
        }
        at += 8 + len + (len & 1);
    }
    Err(format!("{}: no sound in the file", path.display()))
}

/// Common words the recognizer may pick instead of the phrase. Without them, anything that
/// sounds a bit like the phrase ("flip the hat") becomes the phrase; with them, it becomes those
/// words. On 168 synthetic recordings (two voices, three speeds, with and without noise) the
/// phrase was heard 48/48 times; of 120 near misses only "click that button" still fired (9).
const FILLER: &str = "the a an and or but so to of in on at for with by from up out off over i you he she it \
    we they me him her us them my your his its our their this these those is are was were be \
    been am do does did have has had will would can could should may might go get got make \
    take come see look know think want need like say said tell give put keep let shot nice \
    good great wow yes no oh okay right left back now then here there what when where why \
    how who flip slip chip ship lip lips clap clipped clipping cut hat hit fat cat bat click \
    clicks clicked clicking button board flag grab trip tip kit kick pick quick sick that's \
    hey game play win won lost kill killed dead die run jump fire reload team enemy behind \
    top down push wait stop help one two three four five gg lol bro dude man insane crazy \
    clean watch save saved record replay moment best ever again";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrase_matching_needs_the_words_in_a_row() {
        let words: Vec<String> = ["clip", "that"].iter().map(|s| s.to_string()).collect();
        assert!(contains_phrase("clip that", &words));
        assert!(contains_phrase("[unk] clip that [unk]", &words));
        assert!(!contains_phrase("clip [unk] that", &words));
        assert!(!contains_phrase("that clip", &words));
        assert!(!contains_phrase("", &words));
    }

    #[test]
    fn a_result_counts_only_with_confident_words() {
        let words: Vec<String> = ["clip", "that"].iter().map(|s| s.to_string()).collect();
        let r = |json: &str| heard_in(&serde_json::from_str(json).unwrap(), &words);
        assert!(r(
            r#"{"result":[{"word":"wow","conf":1.0},{"word":"clip","conf":0.9},{"word":"that","conf":1.0}],"text":"wow clip that"}"#
        ));
        assert!(!r(
            r#"{"result":[{"word":"clip","conf":0.5},{"word":"that","conf":1.0}],"text":"clip that"}"#
        ));
        assert!(!r(
            r#"{"result":[{"word":"flip","conf":1.0},{"word":"that","conf":1.0}],"text":"flip that"}"#
        ));
        assert!(
            r(r#"{"text":"clip that"}"#),
            "without word details, the text decides"
        );
        assert!(!r(r#"{"text":""}"#));
    }

    #[test]
    fn resampling_keeps_time_across_chunks() {
        // One second of 48 kHz stereo in 10 ms pieces becomes one second at 16 kHz mono.
        let mut r = Resampler::default();
        let mut out = Vec::new();
        let tone: Vec<f32> = (0..48_000)
            .flat_map(|i| {
                let v = (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
                [v, v]
            })
            .collect();
        for c in tone.chunks(960) {
            r.push(c, 48_000, 2, &mut out);
        }
        assert!(
            (out.len() as i64 - 16_000).abs() <= 2,
            "{} samples",
            out.len()
        );
        // The tone survives: the peak is about half scale.
        let peak = out.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!((15_000..=17_000).contains(&peak), "peak {peak}");
        // 16 kHz in stays 16 kHz.
        let mut r = Resampler::default();
        let mut out = Vec::new();
        r.push(&vec![0.25; 1600], 16_000, 1, &mut out);
        r.push(&vec![0.25; 1600], 16_000, 1, &mut out);
        assert!((out.len() as i64 - 3200).abs() <= 1);
        assert!(out.iter().all(|&s| s == 8191));
    }

    #[test]
    fn reads_wav_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        std::fs::write(&p, rewindr::pcm::wav_f32(16_000, 1, &[0.0, 0.5, -0.5])).unwrap();
        let (s, rate, ch) = read_wav(&p).unwrap();
        assert_eq!((rate, ch), (16_000, 1));
        assert_eq!(s, vec![0.0, 0.5, -0.5]);
    }
}
