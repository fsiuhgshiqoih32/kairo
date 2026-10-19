//! What the Mac plays ("desktop audio"), through ScreenCaptureKit (macOS 13 and later): an
//! audio-only `SCStream` of the main display whose samples go into the shared desktop ring, the
//! same as WASAPI loopback on Windows and the PulseAudio monitor on Linux.

use std::ptr::{NonNull, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::channel;
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{
    CMAudioFormatDescriptionGetStreamBasicDescription, CMBlockBuffer, CMSampleBuffer, CMTime,
};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput,
    SCStreamOutputType,
};

use rewindr::pcm::PcmRing;

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

pub fn counts() -> (u64, u64, u64) {
    let c = |i: usize| COUNTS[i].load(Ordering::Relaxed);
    (c(0), c(1), c(2))
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and this class doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "RewindrSystemAudioOutput"]
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
            if let Some((samples, rate, channels)) = unsafe { interleaved_f32(buffer) } {
                COUNTS[2].fetch_add(1, Ordering::Relaxed);
                push(&self.ivars().shared, &samples, rate, channels);
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

/// An `AudioBufferList` with room for several channel buffers (it's declared with one).
#[repr(C)]
struct BufferList {
    list: AudioBufferList,
    more: [AudioBuffer; 7],
}

/// The sample buffer's sound as interleaved 32-bit float, with its rate and channel count.
unsafe fn interleaved_f32(buffer: &CMSampleBuffer) -> Option<(Vec<f32>, u32, u16)> {
    let desc = unsafe { buffer.format_description() }?;
    let asbd = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(&desc).as_ref() }?;
    if asbd.mFormatFlags & FLAG_FLOAT == 0 || asbd.mBitsPerChannel != 32 {
        return None;
    }
    let channels = asbd.mChannelsPerFrame.max(1) as usize;
    let rate = asbd.mSampleRate.round() as u32;
    let mut lists: BufferList = unsafe { std::mem::zeroed() };
    let mut block: *mut CMBlockBuffer = null_mut();
    let status = unsafe {
        buffer.audio_buffer_list_with_retained_block_buffer(
            null_mut(),
            &mut lists.list,
            size_of::<BufferList>(),
            None,
            None,
            0,
            &mut block,
        )
    };
    // The block buffer owns the memory the list points into; release it when done.
    let _block = NonNull::new(block).map(|b| unsafe { CFRetained::from_raw(b) });
    if status != 0 {
        return None;
    }
    let count = (lists.list.mNumberBuffers as usize).min(8);
    let base = &lists.list.mBuffers as *const AudioBuffer;
    let buffers: Vec<&[f32]> = (0..count)
        .map(|i| {
            let b = unsafe { &*base.add(i) };
            let len = b.mDataByteSize as usize / 4;
            if b.mData.is_null() || len == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(b.mData as *const f32, len) }
            }
        })
        .collect();
    let out = if asbd.mFormatFlags & FLAG_NON_INTERLEAVED != 0 {
        // One buffer per channel: interleave them.
        let frames = buffers.iter().map(|b| b.len()).min().unwrap_or(0);
        let mut v = Vec::with_capacity(frames * buffers.len());
        for f in 0..frames {
            for b in &buffers {
                v.push(b[f]);
            }
        }
        (v, rate, buffers.len().max(1) as u16)
    } else {
        (buffers.first()?.to_vec(), rate, channels as u16)
    };
    Some(out)
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
            "allow Rewindr in System Settings > Privacy & Security > Screen Recording".into()
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
    let queue = DispatchQueue::new("io.github.fsiuhgshiqoih32.rewindr.system-audio", None);
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
