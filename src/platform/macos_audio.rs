//! What the Mac plays ("desktop audio"), through ScreenCaptureKit (macOS 13 and later): an
//! audio-only `SCStream` of the main display whose samples go into the shared desktop ring, the
//! same as WASAPI loopback on Windows and the PulseAudio monitor on Linux.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_media::{CMAudioFormatDescriptionGetStreamBasicDescription, CMSampleBuffer, CMTime};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput,
    SCStreamOutputType,
};

use kairo::pcm::PcmRing;

use crate::recorder::{Shared, now_us};

const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
/// `kAudioFormatFlagIsFloat` and `kAudioFormatFlagIsNonInterleaved` from CoreAudioTypes.
const FLAG_FLOAT: u32 = 1;
const FLAG_NON_INTERLEAVED: u32 = 1 << 5;

struct Ivars {
    shared: Arc<Shared>,
}

/// Sample buffers seen, for the self-test: video frames, audio buffers, audio buffers decoded.
static COUNTS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

/// Why the first audio buffer that couldn't be read was rejected.
static FIRST_ERROR: Mutex<Option<String>> = Mutex::new(None);

pub fn first_error() -> Option<String> {
    FIRST_ERROR.lock().unwrap().clone()
}

pub fn counts() -> (u64, u64, u64) {
    let c = |i: usize| COUNTS[i].load(Ordering::Relaxed);
    (c(0), c(1), c(2))
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and this class doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "KairoSystemAudioOutput"]
    #[ivars = Ivars]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_did_output(
            &self,
            _stream: &SCStream,
            buffer: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            // Video frames are delivered too (a stream needs a display); they're dropped here.
            if kind != SCStreamOutputType::Audio {
                COUNTS[0].fetch_add(1, Ordering::Relaxed);
                return;
            }
            COUNTS[1].fetch_add(1, Ordering::Relaxed);
            match unsafe { interleaved_f32(buffer) } {
                Ok((samples, rate, channels)) => {
                    COUNTS[2].fetch_add(1, Ordering::Relaxed);
                    push(&self.ivars().shared, &samples, rate, channels);
                }
                Err(e) => {
                    let mut first = FIRST_ERROR.lock().unwrap();
                    if first.is_none() {
                        crate::log::warning!("desktop audio: can't read a buffer: {e}");
                        *first = Some(e);
                    }
                }
            }
        }
    }
);

impl Output {
    fn new(shared: Arc<Shared>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { shared });
        unsafe { msg_send![super(this), init] }
    }
}

fn push(shared: &Shared, samples: &[f32], rate: u32, channels: u16) {
    let mut ring = shared.desktop.lock().unwrap();
    let fits = ring
        .as_ref()
        .is_some_and(|r| r.rate == rate && r.channels == channels);
    if !fits {
        let keep = shared.settings().buffer_seconds() as i64 * 1_000_000;
        *ring = Some(PcmRing::new(rate, channels, keep));
    }
    if let Some(r) = ring.as_mut() {
        r.push(samples, now_us());
    }
}

/// The sample buffer's sound as interleaved 32-bit float, with its rate and channel count.
/// ScreenCaptureKit sends planar float (one block of frames per channel, back to back), which
/// `CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer` refuses (-12737); copying the
/// block buffer's bytes works for any layout.
unsafe fn interleaved_f32(buffer: &CMSampleBuffer) -> Result<(Vec<f32>, u32, u16), String> {
    let desc = unsafe { buffer.format_description() }.ok_or("no format description")?;
    let asbd = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(&desc).as_ref() }
        .ok_or("not an audio format")?;
    let format = format!(
        "{} Hz, {} ch, {} bits, flags {:#x}",
        asbd.mSampleRate, asbd.mChannelsPerFrame, asbd.mBitsPerChannel, asbd.mFormatFlags
    );
    if asbd.mFormatFlags & FLAG_FLOAT == 0 || asbd.mBitsPerChannel != 32 {
        return Err(format!("not 32-bit float ({format})"));
    }
    let channels = asbd.mChannelsPerFrame.max(1) as usize;
    let rate = asbd.mSampleRate.round() as u32;
    let block = unsafe { buffer.data_buffer() }.ok_or_else(|| format!("no data ({format})"))?;
    let mut raw = vec![0f32; unsafe { block.data_length() } / 4];
    if raw.is_empty() {
        return Ok((raw, rate, channels as u16));
    }
    let dest = NonNull::new(raw.as_mut_ptr().cast()).ok_or("no memory")?;
    let status = unsafe { block.copy_data_bytes(0, raw.len() * 4, dest) };
    if status != 0 {
        return Err(format!(
            "CMBlockBufferCopyDataBytes returned {status} ({format})"
        ));
    }
    if asbd.mFormatFlags & FLAG_NON_INTERLEAVED == 0 || channels == 1 {
        return Ok((raw, rate, channels as u16));
    }
    // Planar: channel c's frames start at c * plane.
    let plane = raw.len() / channels;
    let frames = match unsafe { buffer.num_samples() } {
        n if n > 0 => (n as usize).min(plane),
        _ => plane,
    };
    let mut out = Vec::with_capacity(frames * channels);
    for f in 0..frames {
        for c in 0..channels {
            out.push(raw[c * plane + f]);
        }
    }
    Ok((out, rate, channels as u16))
}

/// A running system-audio capture; dropping it stops the stream.
pub struct SystemAudio {
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    _queue: DispatchRetained<DispatchQueue>,
}

impl Drop for SystemAudio {
    fn drop(&mut self) {
        unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
    }
}

/// Wraps a value for handing across the completion handler's thread.
struct Sendable<T>(T);
// SAFETY: the Objective-C objects passed this way are immutable snapshots (shareable content)
// or plain flags, used on one thread at a time.
unsafe impl<T> Send for Sendable<T> {}

/// Starts capturing what the Mac plays into `shared.desktop`.
pub fn start(shared: Arc<Shared>) -> Result<SystemAudio, String> {
    if !objc2::available!(macos = 13.0) {
        return Err("Desktop audio needs macOS 13 or later; the microphone works.".into());
    }
    // Which displays may be captured (this also asks for Screen Recording permission).
    let (tx, rx) = channel();
    let handler = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let content = unsafe { Retained::retain(content) };
            let error =
                unsafe { Retained::retain(error) }.map(|e| e.localizedDescription().to_string());
            let _ = tx.send(Sendable((content, error)));
        },
    );
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
    let Sendable((content, error)) = rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| "macOS didn't answer the request for screen content".to_string())?;
    let content = content.ok_or_else(|| {
        error.unwrap_or_else(|| {
            "allow Kairo in System Settings > Privacy & Security > Screen Recording".into()
        })
    })?;
    let display = unsafe { content.displays() }
        .firstObject()
        .ok_or("no display to capture sound from")?;

    let filter = unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            &display,
            &NSArray::new(),
        )
    };
    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setCapturesAudio(true);
        config.setSampleRate(RATE as isize);
        config.setChannelCount(CHANNELS as isize);
        config.setExcludesCurrentProcessAudio(true);
        // The stream has to carry video too; keep it as small and rare as allowed.
        config.setWidth(2);
        config.setHeight(2);
        config.setMinimumFrameInterval(CMTime::new(1, 1));
    }
    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None)
    };
    let output = Output::new(shared);
    let queue = DispatchQueue::new("io.github.fsiuhgshiqoih32.kairo.system-audio", None);
    let as_output = ProtocolObject::from_ref(&*output);
    for kind in [SCStreamOutputType::Audio, SCStreamOutputType::Screen] {
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(as_output, kind, Some(&queue))
        }
        .map_err(|e| {
            format!(
                "could not set up desktop audio: {}",
                e.localizedDescription()
            )
        })?;
    }

    let (tx, rx) = channel();
    let started = RcBlock::new(move |error: *mut NSError| {
        let error =
            unsafe { Retained::retain(error) }.map(|e| e.localizedDescription().to_string());
        let _ = tx.send(Sendable(error));
    });
    unsafe { stream.startCaptureWithCompletionHandler(Some(&started)) };
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Sendable(None)) => Ok(SystemAudio {
            stream,
            _output: output,
            _queue: queue,
        }),
        Ok(Sendable(Some(e))) => Err(format!("desktop audio didn't start: {e}")),
        Err(_) => Err("desktop audio didn't start in time".into()),
    }
}
