//! ffmpeg command lines: recording into the replay buffer, and saving a clip.
//!
//! Recording writes MPEG-TS to stdout with wall-clock timestamps (`-use_wallclock_as_timestamps 1`
//! on the input, `-copyts` on the output), so every frame carries the Unix time it was captured.
//! Kairo stamps audio with the same clock, which is how the two line up when a clip is saved.

use crate::config::VideoCodec;
use std::path::Path;

/// Where frames come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Windows Graphics Capture of a monitor (HMONITOR). Frames stay on the GPU, which also
    /// scales them.
    WindowsGraphics {
        hmonitor: u64,
        /// While the screen shows HDR, its SDR white level in nits: frames are captured in float
        /// and mapped to SDR with it (`sdr_white`, a Kairo addition to ffmpeg). Without it,
        /// Windows converts them and they come out far too bright.
        sdr_white: Option<u32>,
    },
    /// Windows Desktop Duplication: output `index` of the default GPU. Frames stay on the GPU.
    Ddagrab { output: u32 },
    /// Windows GDI capture of a desktop rectangle. Slow; for machines where the others fail.
    Gdigrab { x: i32, y: i32, w: u32, h: u32 },
    /// macOS AVFoundation screen: "Capture screen N", N counting displays from 0 in
    /// `CGGetActiveDisplayList` order.
    Avfoundation { screen: u32 },
    /// X11: display name (":0") and a rectangle on it.
    X11 {
        display: String,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
    },
    /// A moving test picture, for tests without a screen.
    Test { w: u32, h: u32 },
}

impl Source {
    pub fn label(&self) -> &'static str {
        match self {
            Source::WindowsGraphics { .. } => "Windows Graphics Capture",
            Source::Ddagrab { .. } => "Desktop Duplication",
            Source::Gdigrab { .. } => "GDI",
            Source::Avfoundation { .. } => "AVFoundation",
            Source::X11 { .. } => "X11",
            Source::Test { .. } => "test picture",
        }
    }

    /// Frames arrive as Direct3D 11 textures rather than in memory.
    fn gpu_frames(&self) -> bool {
        matches!(
            self,
            Source::WindowsGraphics { .. } | Source::Ddagrab { .. }
        )
    }
}

/// A video encoder Kairo knows how to drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoder {
    /// NVIDIA.
    Nvenc,
    /// AMD.
    Amf,
    /// Windows Media Foundation with a hardware encoder (Intel, and others).
    MediaFoundation,
    /// Apple VideoToolbox.
    VideoToolbox,
    /// VA-API on Linux (Intel, AMD).
    Vaapi,
    /// x264 on the CPU: works everywhere, costs the most.
    X264,
}

impl Encoder {
    pub const ALL: [Encoder; 6] = [
        Encoder::Nvenc,
        Encoder::Amf,
        Encoder::MediaFoundation,
        Encoder::VideoToolbox,
        Encoder::Vaapi,
        Encoder::X264,
    ];

    /// The name in settings and logs.
    pub fn id(self) -> &'static str {
        match self {
            Encoder::Nvenc => "nvenc",
            Encoder::Amf => "amf",
            Encoder::MediaFoundation => "mediafoundation",
            Encoder::VideoToolbox => "videotoolbox",
            Encoder::Vaapi => "vaapi",
            Encoder::X264 => "x264",
        }
    }

    pub fn from_id(id: &str) -> Option<Encoder> {
        Encoder::ALL.into_iter().find(|e| e.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Encoder::Nvenc => "NVIDIA (NVENC)",
            Encoder::Amf => "AMD (AMF)",
            Encoder::MediaFoundation => "Windows Media Foundation",
            Encoder::VideoToolbox => "Apple VideoToolbox",
            Encoder::Vaapi => "VA-API",
            Encoder::X264 => "CPU (x264)",
        }
    }

    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            Encoder::Nvenc => "h264_nvenc",
            Encoder::Amf => "h264_amf",
            Encoder::MediaFoundation => "h264_mf",
            Encoder::VideoToolbox => "h264_videotoolbox",
            Encoder::Vaapi => "h264_vaapi",
            Encoder::X264 => "libx264",
        }
    }

    /// The ffmpeg encoder for `codec`, if this kind of encoder has one (the CPU one only does
    /// H.264 in Kairo's ffmpeg).
    pub fn ffmpeg_name_for(self, codec: VideoCodec) -> Option<&'static str> {
        match codec {
            VideoCodec::H264 => Some(self.ffmpeg_name()),
            VideoCodec::Hevc => match self {
                Encoder::Nvenc => Some("hevc_nvenc"),
                Encoder::Amf => Some("hevc_amf"),
                Encoder::MediaFoundation => Some("hevc_mf"),
                Encoder::VideoToolbox => Some("hevc_videotoolbox"),
                Encoder::Vaapi => Some("hevc_vaapi"),
                Encoder::X264 => None,
            },
        }
    }

    /// Which encoders to try on this system, best first.
    pub fn candidates() -> Vec<Encoder> {
        if cfg!(windows) {
            vec![
                Encoder::Nvenc,
                Encoder::Amf,
                Encoder::MediaFoundation,
                Encoder::X264,
            ]
        } else if cfg!(target_os = "macos") {
            vec![Encoder::VideoToolbox, Encoder::X264]
        } else {
            vec![Encoder::Nvenc, Encoder::Vaapi, Encoder::X264]
        }
    }

    /// Can take Direct3D 11 frames as they are.
    fn takes_d3d11(self) -> bool {
        matches!(
            self,
            Encoder::Nvenc | Encoder::Amf | Encoder::MediaFoundation
        )
    }

    /// Rate control and latency options for a target bitrate, or for a constant quality `cq`
    /// with 1.5 times that bitrate as the ceiling (encoders that have it).
    fn options(self, bitrate: u64, fps: u32, cq: Option<u32>) -> Vec<String> {
        let b = bitrate.to_string();
        let max = (bitrate * 3 / 2).to_string();
        let buf = (bitrate * 2).to_string();
        let max_buf = (bitrate * 3).to_string();
        let q = cq.map(|q| q.to_string()).unwrap_or_default();
        // A keyframe every 2 seconds. At one a second, keyframes were 74% of a desktop clip's
        // bytes; 2 s made desktop clips 30% and game clips up to 10% smaller at the same VMAF.
        // Clips start at the keyframe at or before their start, so it's also the cut precision.
        let g = (2 * fps.max(1)).to_string();
        let v: Vec<&str> = match self {
            // Constant quality: as good a picture as the bitrate gave, in far fewer bits where a
            // scene is easy (measured: 15% smaller in a fast game, 50-60% on slower ones and a
            // desktop), with the old ceiling kept for the hardest scenes.
            Encoder::Nvenc if cq.is_some() => vec![
                "-preset",
                "p2",
                "-tune",
                "hq",
                "-rc",
                "vbr",
                "-cq",
                &q,
                "-b:v",
                "0",
                "-maxrate",
                &max,
                "-bufsize",
                &max_buf,
                "-forced-idr",
                "1",
            ],
            Encoder::Nvenc => vec![
                // p2 encodes about 1.5 times faster than p4 at the same picture (measured at
                // 1440p120 on game footage): less load on the encoder while games run.
                "-preset",
                "p2",
                "-tune",
                "ll",
                "-rc",
                "vbr",
                "-b:v",
                &b,
                "-maxrate",
                &max,
                "-bufsize",
                &buf,
                "-forced-idr",
                "1",
            ],
            Encoder::Amf => {
                vec![
                    "-usage",
                    "lowlatency",
                    "-quality",
                    "speed",
                    "-rc",
                    "vbr_peak",
                    "-b:v",
                    &b,
                    "-maxrate",
                    &max,
                    "-bufsize",
                    &buf,
                ]
            }
            Encoder::MediaFoundation => vec![
                "-hw_encoding",
                "1",
                "-scenario",
                "display_remoting",
                "-b:v",
                &b,
            ],
            Encoder::VideoToolbox => vec!["-realtime", "1", "-prio_speed", "1", "-b:v", &b],
            Encoder::Vaapi => vec!["-rc_mode", "VBR", "-b:v", &b, "-maxrate", &max],
            Encoder::X264 => vec![
                "-preset",
                "veryfast",
                "-tune",
                "zerolatency",
                "-b:v",
                &b,
                "-maxrate",
                &max,
                "-bufsize",
                &buf,
                "-pix_fmt",
                "yuv420p",
                "-sc_threshold",
                "0",
            ],
        };
        let mut out: Vec<String> = v.into_iter().map(String::from).collect();
        out.extend([
            "-g".into(),
            g.clone(),
            "-keyint_min".into(),
            g,
            "-bf".into(),
            "0".into(),
        ]);
        out
    }
}

/// What to record and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoPlan {
    pub source: Source,
    pub encoder: Encoder,
    pub codec: VideoCodec,
    pub fps: u32,
    /// The captured area's size in pixels.
    pub src_size: (u32, u32),
    /// Scale down to at most this height.
    pub max_height: Option<u32>,
    pub bitrate: u64,
    /// A constant-quality level (lower is better) for encoders that have one: `bitrate` is then
    /// only the ceiling.
    pub cq: Option<u32>,
    /// VA-API render node, for `Encoder::Vaapi`.
    pub vaapi_device: Option<String>,
}

impl VideoPlan {
    /// Whether a run of `next` can follow this one in the replay buffer: the same capture,
    /// encoder and settings, only the screen at another size. One decoder reads both, and a
    /// save fits the sizes together; any other change starts the buffer over.
    pub fn follows_at_new_size(&self, next: &VideoPlan) -> bool {
        self.source.label() == next.source.label()
            && self.encoder == next.encoder
            && self.codec == next.codec
            && self.fps == next.fps
            && self.max_height == next.max_height
            && self.cq == next.cq
            && self.vaapi_device == next.vaapi_device
            && self.src_size != next.src_size
    }
}

/// The size frames are encoded at: `src` scaled to fit `max_height` (keeping the aspect ratio),
/// rounded down to even numbers as encoders want.
pub fn output_size(src: (u32, u32), max_height: Option<u32>) -> (u32, u32) {
    let (w, h) = (src.0.max(2), src.1.max(2));
    let (w, h) = match max_height {
        Some(m) if h > m => ((w as u64 * m as u64 / h as u64) as u32, m),
        _ => (w, h),
    };
    (w & !1, h & !1)
}

fn s(v: &str) -> String {
    v.to_string()
}

/// ffmpeg arguments (without the program name) that record `plan` to MPEG-TS on stdout.
pub fn record_args(plan: &VideoPlan) -> Vec<String> {
    let fps = plan.fps.max(1);
    let (ow, oh) = output_size(plan.src_size, plan.max_height);
    let scaled = (ow, oh) != (plan.src_size.0 & !1, plan.src_size.1 & !1)
        || plan.src_size.0 % 2 + plan.src_size.1 % 2 > 0;
    let mut a = vec![
        s("-hide_banner"),
        s("-nostdin"),
        s("-nostats"),
        s("-loglevel"),
        s("error"),
    ];
    if plan.encoder == Encoder::Vaapi {
        a.extend([
            s("-vaapi_device"),
            plan.vaapi_device
                .clone()
                .unwrap_or_else(|| s("/dev/dri/renderD128")),
        ]);
    }
    a.extend([
        s("-thread_queue_size"),
        s("64"),
        s("-use_wallclock_as_timestamps"),
        s("1"),
    ]);
    // Filters applied after capture.
    let mut filters: Vec<String> = Vec::new();
    match &plan.source {
        Source::WindowsGraphics {
            hmonitor,
            sdr_white,
        } => {
            // The size is always given: without it, a game changing the display mode makes the
            // capture crop (or pad) to the old size instead of scaling.
            let mut g = format!(
                "gfxcapture=hmonitor={hmonitor}:max_framerate={fps}:capture_cursor=1:width={ow}:height={oh}:resize_mode=scale_aspect"
            );
            if let Some(nits) = sdr_white {
                g.push_str(&format!(":sdr_white={nits}"));
            }
            a.extend([s("-f"), s("lavfi"), s("-i"), g]);
        }
        Source::Ddagrab { output } => {
            a.extend([
                s("-f"),
                s("lavfi"),
                s("-i"),
                format!("ddagrab=output_idx={output}:framerate={fps}:draw_mouse=1"),
            ]);
        }
        Source::Gdigrab { x, y, w, h } => {
            a.extend([
                s("-f"),
                s("gdigrab"),
                s("-framerate"),
                fps.to_string(),
                s("-draw_mouse"),
                s("1"),
                s("-offset_x"),
                x.to_string(),
                s("-offset_y"),
                y.to_string(),
                s("-video_size"),
                format!("{w}x{h}"),
                s("-i"),
                s("desktop"),
            ]);
        }
        Source::Avfoundation { screen } => {
            a.extend([
                s("-f"),
                s("avfoundation"),
                s("-capture_cursor"),
                s("1"),
                s("-framerate"),
                fps.to_string(),
                s("-i"),
                format!("Capture screen {screen}:none"),
            ]);
        }
        Source::X11 {
            display,
            x,
            y,
            w,
            h,
        } => {
            a.extend([
                s("-f"),
                s("x11grab"),
                s("-framerate"),
                fps.to_string(),
                s("-draw_mouse"),
                s("1"),
                s("-video_size"),
                format!("{w}x{h}"),
                s("-i"),
                format!("{display}+{x},{y}"),
            ]);
        }
        Source::Test { w, h } => {
            a.extend([
                s("-re"),
                s("-f"),
                s("lavfi"),
                s("-i"),
                format!("testsrc2=size={w}x{h}:rate={fps}"),
            ]);
        }
    }
    // A steady frame rate: captures deliver frames irregularly (or only on change), and the
    // keyframe interval is counted in frames.
    filters.push(format!("fps={fps}"));
    let gpu = plan.source.gpu_frames();
    let gpu_scaled = matches!(plan.source, Source::WindowsGraphics { .. });
    if gpu && !plan.encoder.takes_d3d11() {
        filters.push(s("hwdownload"));
        filters.push(s("format=bgra"));
    }
    if scaled && !gpu_scaled {
        if gpu && plan.encoder.takes_d3d11() {
            // Desktop Duplication can't scale on the GPU here; do it in memory.
            filters.push(s("hwdownload"));
            filters.push(s("format=bgra"));
        }
        filters.push(format!("scale={ow}:{oh}"));
    }
    if plan.encoder == Encoder::Vaapi {
        filters.push(s("format=nv12"));
        filters.push(s("hwupload"));
    }
    a.extend([s("-vf"), filters.join(",")]);
    a.extend([
        s("-fps_mode"),
        s("passthrough"),
        s("-an"),
        s("-c:v"),
        s(plan
            .encoder
            .ffmpeg_name_for(plan.codec)
            .unwrap_or(plan.encoder.ffmpeg_name())),
    ]);
    a.extend(plan.encoder.options(plan.bitrate, fps, plan.cq));
    a.extend([
        // Stream headers before every keyframe, so a clip can start at any of them.
        s("-bsf:v"),
        s("dump_extra=freq=keyframe"),
        s("-copyts"),
        s("-muxdelay"),
        s("0"),
        s("-muxpreload"),
        s("0"),
        s("-flush_packets"),
        s("1"),
        s("-f"),
        s("mpegts"),
        s("pipe:1"),
    ]);
    a
}

/// A tiny encode that tells whether `encoder` works on this machine.
pub fn probe_args(encoder: Encoder, vaapi_device: Option<&str>) -> Vec<String> {
    let mut a = vec![s("-hide_banner"), s("-nostdin"), s("-loglevel"), s("error")];
    if encoder == Encoder::Vaapi {
        a.extend([
            s("-vaapi_device"),
            vaapi_device.unwrap_or("/dev/dri/renderD128").to_string(),
        ]);
    }
    a.extend([
        s("-f"),
        s("lavfi"),
        s("-i"),
        s("testsrc2=size=320x240:rate=30"),
    ]);
    if encoder == Encoder::Vaapi {
        a.extend([s("-vf"), s("format=nv12,hwupload")]);
    }
    a.extend([s("-frames:v"), s("5"), s("-c:v"), s(encoder.ffmpeg_name())]);
    a.extend(encoder.options(2_000_000, 30, None));
    a.extend([s("-f"), s("null"), s("-")]);
    a
}

/// The webcam laid over a clip as it's saved.
#[derive(Clone, Debug, PartialEq)]
pub struct CamOverlay<'a> {
    /// The webcam's recording (MPEG-TS).
    pub path: &'a Path,
    /// When it starts, in seconds after the clip's start (negative: before).
    pub offset: f64,
    /// Its width in the clip, in pixels.
    pub width: u32,
    /// One of `config::WEBCAM_CORNERS`.
    pub corner: &'a str,
}

/// How a save encodes the picture again rather than copying it.
#[derive(Clone, Debug, PartialEq)]
pub struct Reencode<'a> {
    pub encoder: Encoder,
    pub bitrate: u64,
    pub fps: u32,
    /// The webcam in a corner.
    pub cam: Option<CamOverlay<'a>>,
    /// Every picture fitted into this size, with black bars where it doesn't fill it: a clip
    /// from before and after a game changed the display mode.
    pub fit: Option<(u32, u32)>,
}

/// Where the overlay filter puts the webcam: a corner, inset by 2% of the width.
fn corner_xy(corner: &str) -> (&'static str, &'static str) {
    let x = if corner.ends_with("left") {
        "W*2/100"
    } else {
        "W-w-W*2/100"
    };
    let y = if corner.starts_with("top") {
        "W*2/100"
    } else {
        "H-h-W*2/100"
    };
    (x, y)
}

/// Joins the saved video with the sound tracks (each a WAV starting where the video starts, with a
/// title) into an MP4. With several, the first track is their mix, which is what players and
/// Discord play; each source follows as its own track for editing.
/// `hevc`: the video is H.265, tagged so Apple's players and browsers open it.
pub fn save_args(video: &Path, audio: &[(&Path, &str)], hevc: bool, out: &Path) -> Vec<String> {
    save_reencoded_args(video, None, audio, hevc, out)
}

/// [`save_args`], encoding the picture again where `re` says: to lay the webcam over a corner,
/// or to fit pictures of different sizes into one.
pub fn save_reencoded_args(
    video: &Path,
    re: Option<&Reencode>,
    audio: &[(&Path, &str)],
    hevc: bool,
    out: &Path,
) -> Vec<String> {
    let mut a = vec![
        s("-y"),
        s("-hide_banner"),
        s("-nostdin"),
        s("-loglevel"),
        s("error"),
    ];
    // Said rather than guessed: a save sends the stream through a pipe (`pipe:0`).
    a.extend([s("-f"), s("mpegts"), s("-i"), video.display().to_string()]);
    let cam = re.and_then(|r| r.cam.as_ref());
    let fit = re.and_then(|r| r.fit);
    if let Some(c) = cam {
        if c.offset < 0.0 {
            a.extend([s("-ss"), format!("{:.3}", -c.offset)]);
        } else if c.offset > 0.0 {
            a.extend([s("-itsoffset"), format!("{:.3}", c.offset)]);
        }
        a.extend([s("-i"), c.path.display().to_string()]);
    }
    let first_audio = 1 + usize::from(cam.is_some());
    for (p, _) in audio {
        // A session's WAV can pass 4 GiB (its header can't say so) or be left unfinished by a
        // crash (its header says 0): read to the end either way.
        a.extend([
            s("-ignore_length"),
            s("1"),
            s("-i"),
            p.display().to_string(),
        ]);
    }
    let mut graph: Vec<String> = Vec::new();
    let base = match fit {
        Some((w, h)) => {
            let (w, h) = (w.max(2) & !1, h.max(2) & !1);
            let out = if cam.is_some() { "[fit]" } else { "[v]" };
            graph.push(format!(
                "[0:v]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1{out}"
            ));
            "[fit]"
        }
        None => "[0:v]",
    };
    if let Some(c) = cam {
        let (x, y) = corner_xy(c.corner);
        graph.push(format!(
            "[1:v]scale={}:-2[cam];{base}[cam]overlay=x={x}:y={y}:eof_action=pass[v]",
            c.width.max(2) & !1
        ));
    }
    let mut titles: Vec<&str> = Vec::new();
    if audio.len() > 1 {
        let n = audio.len();
        let inputs: String = (first_audio..first_audio + n)
            .map(|i| format!("[{i}:a]"))
            .collect();
        graph.push(format!(
            "{inputs}amix=inputs={n}:duration=longest:normalize=0[mix]"
        ));
    }
    if !graph.is_empty() {
        a.extend([s("-filter_complex"), graph.join(";")]);
    }
    a.extend([s("-map"), s(if re.is_some() { "[v]" } else { "0:v:0" })]);
    if audio.len() > 1 {
        a.extend([s("-map"), s("[mix]")]);
        titles.push("Everything");
    }
    for (i, (_, title)) in audio.iter().enumerate() {
        a.extend([s("-map"), format!("{}:a:0", first_audio + i)]);
        titles.push(title);
    }
    let mut tag = hevc;
    match re {
        Some(r) => {
            let hevc_name = if hevc {
                r.encoder.ffmpeg_name_for(VideoCodec::Hevc)
            } else {
                None
            };
            tag = hevc_name.is_some();
            a.extend([s("-c:v"), s(hevc_name.unwrap_or(r.encoder.ffmpeg_name()))]);
            a.extend(crate::media::export_video_opts(
                r.encoder, r.bitrate, r.fps, false,
            ));
        }
        None => a.extend([s("-c:v"), s("copy")]),
    }
    if tag {
        a.extend([s("-tag:v"), s("hvc1")]);
    }
    if !audio.is_empty() {
        a.extend([s("-c:a"), s("aac"), s("-b:a"), s("160k")]);
    }
    for (i, t) in titles.iter().enumerate() {
        // The mix (or a lone track) keeps full quality. A source kept beside it, for editing,
        // needs less, and a microphone is one voice: mono at 64k.
        if titles.len() > 1 {
            match *t {
                "Everything" => {}
                "Microphone" => {
                    a.extend([format!("-b:a:{i}"), s("64k"), format!("-ac:a:{i}"), s("1")])
                }
                "Voice chat" => a.extend([format!("-b:a:{i}"), s("96k")]),
                _ => a.extend([format!("-b:a:{i}"), s("128k")]),
            }
        }
        // MP4 keeps a track's handler name but not its title: write both.
        a.extend([format!("-metadata:s:a:{i}"), format!("title={t}")]);
        a.extend([format!("-metadata:s:a:{i}"), format!("handler_name={t}")]);
        let d = if i == 0 { "default" } else { "0" };
        a.extend([format!("-disposition:a:{i}"), s(d)]);
    }
    a.extend([
        s("-movflags"),
        s("+faststart"),
        s("-f"),
        s("mp4"),
        out.display().to_string(),
    ]);
    a
}

/// ffmpeg arguments that list the cameras (on stderr) where ffmpeg can: DirectShow on Windows,
/// AVFoundation on macOS. Linux cameras are found in /sys instead.
pub fn camera_list_args() -> Vec<String> {
    let mut a = vec![s("-hide_banner"), s("-nostdin")];
    if cfg!(target_os = "macos") {
        a.extend([s("-f"), s("avfoundation"), s("-list_devices"), s("true")]);
        a.extend([s("-i"), s("")]);
    } else {
        a.extend([s("-list_devices"), s("true"), s("-f"), s("dshow")]);
        a.extend([s("-i"), s("dummy")]);
    }
    a
}

/// The cameras in ffmpeg's DirectShow device list. Newer ffmpeg marks each device "(video)" or
/// "(audio)"; older ones list them under a heading instead.
pub fn dshow_cameras(text: &str) -> Vec<String> {
    let mut video_part = false;
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.contains("DirectShow video devices") {
            video_part = true;
            continue;
        }
        if line.contains("DirectShow audio devices") {
            video_part = false;
            continue;
        }
        if line.contains("Alternative name") {
            continue;
        }
        let Some(open) = line.find('"') else { continue };
        let Some(len) = line[open + 1..].find('"') else {
            continue;
        };
        let name = &line[open + 1..open + 1 + len];
        let rest = line[open + 2 + len..].trim();
        let is_video = match rest {
            "(video)" => true,
            "(audio)" | "(none)" => false,
            _ => video_part && rest.is_empty(),
        };
        if is_video && !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// The cameras in ffmpeg's AVFoundation device list (screens left out).
pub fn avfoundation_cameras(text: &str) -> Vec<String> {
    let mut video_part = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.contains("AVFoundation video devices") {
            video_part = true;
            continue;
        }
        if line.contains("AVFoundation audio devices") {
            video_part = false;
            continue;
        }
        if !video_part {
            continue;
        }
        // "[AVFoundation indev @ 0x...] [0] FaceTime HD Camera"
        let Some(at) = line.find("] [") else { continue };
        let Some((_, name)) = line[at + 3..].split_once("] ") else {
            continue;
        };
        let name = name.trim();
        if !name.starts_with("Capture screen") && !name.is_empty() {
            out.push(name.to_string());
        }
    }
    out
}

/// ffmpeg arguments that record the camera `device` (a DirectShow or AVFoundation name, or a
/// /dev/video path) to MPEG-TS on stdout: small and cheap, 360p at 30 fps, stamped with the wall
/// clock like the screen so the two line up.
pub fn webcam_args(device: &str) -> Vec<String> {
    let mut a = vec![
        s("-hide_banner"),
        s("-nostdin"),
        s("-nostats"),
        s("-loglevel"),
        s("error"),
        s("-thread_queue_size"),
        s("64"),
        s("-use_wallclock_as_timestamps"),
        s("1"),
    ];
    if cfg!(windows) {
        a.extend([s("-f"), s("dshow"), s("-rtbufsize"), s("64M")]);
        a.extend([s("-i"), format!("video={device}")]);
    } else if cfg!(target_os = "macos") {
        a.extend([s("-f"), s("avfoundation"), s("-framerate"), s("30")]);
        a.extend([s("-i"), format!("{device}:none")]);
    } else {
        a.extend([s("-f"), s("v4l2"), s("-i"), device.to_string()]);
    }
    a.extend(
        [
            "-vf",
            "fps=30,scale=-2:360,format=yuv420p",
            "-fps_mode",
            "passthrough",
            "-an",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-b:v",
            "1000k",
            "-maxrate",
            "1500k",
            "-bufsize",
            "2000k",
            "-g",
            "30",
            "-keyint_min",
            "30",
            "-bf",
            "0",
            "-bsf:v",
            "dump_extra=freq=keyframe",
            "-copyts",
            "-muxdelay",
            "0",
            "-muxpreload",
            "0",
            "-flush_packets",
            "1",
            "-f",
            "mpegts",
            "pipe:1",
        ]
        .map(s),
    );
    a
}

/// A big still from `at` seconds into `clip`, for the library's banner.
pub fn hero_args(clip: &Path, at: f64, out: &Path) -> Vec<String> {
    vec![
        s("-y"),
        s("-hide_banner"),
        s("-nostdin"),
        s("-loglevel"),
        s("error"),
        s("-ss"),
        format!("{:.2}", at.max(0.0)),
        s("-i"),
        clip.display().to_string(),
        s("-frames:v"),
        s("1"),
        s("-vf"),
        s("scale=1280:-2"),
        s("-q:v"),
        s("3"),
        s("-update"),
        s("1"),
        out.display().to_string(),
    ]
}

/// A small picture of the clip's last moment, for the notification.
pub fn thumbnail_args(clip: &Path, out: &Path) -> Vec<String> {
    vec![
        s("-y"),
        s("-hide_banner"),
        s("-nostdin"),
        s("-loglevel"),
        s("error"),
        s("-sseof"),
        s("-1"),
        s("-i"),
        clip.display().to_string(),
        s("-frames:v"),
        s("1"),
        s("-vf"),
        s("scale=320:-2"),
        s("-update"),
        s("1"),
        out.display().to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_cameras_in_device_lists() {
        let new = "[dshow @ 0000] \"Integrated Camera\" (video)\n\
                   [dshow @ 0000]   Alternative name \"@device_pnp_\\\\?\\usb#vid\"\n\
                   [dshow @ 0000] \"OBS Virtual Camera\" (none)\n\
                   [dshow @ 0000] \"Microphone (Realtek Audio)\" (audio)\n\
                   [dshow @ 0000] \"Logi C920\" (video)\n";
        assert_eq!(dshow_cameras(new), vec!["Integrated Camera", "Logi C920"]);
        let old = "[dshow @ 0000] DirectShow video devices (some may be both video and audio devices)\n\
                   [dshow @ 0000]  \"USB Camera\"\n\
                   [dshow @ 0000] DirectShow audio devices\n\
                   [dshow @ 0000]  \"Microphone\"\n";
        assert_eq!(dshow_cameras(old), vec!["USB Camera"]);
        let mac = "[AVFoundation indev @ 0x1] AVFoundation video devices:\n\
                   [AVFoundation indev @ 0x1] [0] FaceTime HD Camera\n\
                   [AVFoundation indev @ 0x1] [1] Capture screen 0\n\
                   [AVFoundation indev @ 0x1] AVFoundation audio devices:\n\
                   [AVFoundation indev @ 0x1] [0] MacBook Pro Microphone\n";
        assert_eq!(avfoundation_cameras(mac), vec!["FaceTime HD Camera"]);
    }

    #[test]
    fn the_webcam_goes_in_a_corner_of_the_saved_clip() {
        let cam = Reencode {
            encoder: Encoder::Nvenc,
            bitrate: 20_000_000,
            fps: 60,
            cam: Some(CamOverlay {
                path: Path::new("cam.ts"),
                offset: -0.4,
                width: 641,
                corner: "top-left",
            }),
            fit: None,
        };
        let audio = [
            (Path::new("Game.wav"), "Game"),
            (Path::new("Microphone.wav"), "Microphone"),
        ];
        let a = save_reencoded_args(
            Path::new("video.ts"),
            Some(&cam),
            &audio,
            false,
            Path::new("out.mp4"),
        )
        .join(" ");
        assert!(
            a.contains(
                "-i video.ts -ss 0.400 -i cam.ts -ignore_length 1 -i Game.wav -ignore_length 1 -i Microphone.wav"
            ),
            "{a}"
        );
        assert!(
            a.contains("[1:v]scale=640:-2[cam];[0:v][cam]overlay=x=W*2/100:y=W*2/100:eof_action=pass[v];[2:a][3:a]amix=inputs=2"),
            "{a}"
        );
        assert!(
            a.contains("-map [v] -map [mix] -map 2:a:0 -map 3:a:0 -c:v h264_nvenc"),
            "{a}"
        );
        assert!(!a.contains("-tag:v"));

        // Without a webcam it's the plain copy, and H.265 on the CPU falls back to H.264.
        let plain = save_args(
            Path::new("video.ts"),
            &audio[..1],
            true,
            Path::new("out.mp4"),
        );
        assert!(
            plain
                .join(" ")
                .contains("-map 0:v:0 -map 1:a:0 -c:v copy -tag:v hvc1")
        );
        let late = Reencode {
            encoder: Encoder::X264,
            cam: Some(CamOverlay {
                offset: 0.25,
                corner: "bottom-right",
                ..cam.cam.clone().unwrap()
            }),
            ..cam.clone()
        };
        let a = save_reencoded_args(
            Path::new("v.ts"),
            Some(&late),
            &[],
            true,
            Path::new("o.mp4"),
        )
        .join(" ");
        assert!(a.contains("-itsoffset 0.250 -i cam.ts"), "{a}");
        assert!(a.contains("overlay=x=W-w-W*2/100:y=H-h-W*2/100"), "{a}");
        assert!(a.contains("-c:v libx264") && !a.contains("hvc1"), "{a}");
    }

    fn plan(
        source: Source,
        encoder: Encoder,
        src: (u32, u32),
        max_height: Option<u32>,
    ) -> VideoPlan {
        VideoPlan {
            source,
            encoder,
            codec: VideoCodec::H264,
            fps: 60,
            src_size: src,
            max_height,
            bitrate: 12_000_000,
            cq: None,
            vaapi_device: None,
        }
    }

    fn joined(a: &[String]) -> String {
        a.join(" ")
    }

    #[test]
    fn an_hdr_screen_is_captured_with_its_sdr_white() {
        let hdr = Source::WindowsGraphics {
            hmonitor: 65537,
            sdr_white: Some(240),
        };
        let a = joined(&record_args(&plan(
            hdr,
            Encoder::Nvenc,
            (2560, 1440),
            Some(1080),
        )));
        assert!(a.contains("resize_mode=scale_aspect:sdr_white=240"), "{a}");
        let sdr = Source::WindowsGraphics {
            hmonitor: 65537,
            sdr_white: None,
        };
        let a = joined(&record_args(&plan(
            sdr,
            Encoder::Nvenc,
            (2560, 1440),
            Some(1080),
        )));
        assert!(!a.contains("sdr_white"), "{a}");
    }

    #[test]
    fn only_a_new_screen_size_keeps_the_replay_going() {
        let wgc = Source::WindowsGraphics {
            hmonitor: 7,
            sdr_white: None,
        };
        let desktop = plan(wgc.clone(), Encoder::Nvenc, (2560, 1440), Some(1080));
        // The game at 1920x1440, stretched.
        let game = plan(wgc.clone(), Encoder::Nvenc, (1920, 1440), Some(1080));
        assert!(desktop.follows_at_new_size(&game));
        assert!(!desktop.follows_at_new_size(&desktop));
        // New settings, another encoder or another capture start over.
        let mut hevc = game.clone();
        hevc.codec = VideoCodec::Hevc;
        assert!(!desktop.follows_at_new_size(&hevc));
        let mut faster = game.clone();
        faster.fps = 120;
        assert!(!desktop.follows_at_new_size(&faster));
        assert!(!desktop.follows_at_new_size(&plan(wgc, Encoder::X264, (1920, 1440), Some(1080))));
        let dda = plan(
            Source::Ddagrab { output: 0 },
            Encoder::Nvenc,
            (1920, 1440),
            Some(1080),
        );
        assert!(!desktop.follows_at_new_size(&dda));
    }

    #[test]
    fn a_clip_across_sizes_is_fitted_to_one() {
        let re = Reencode {
            encoder: Encoder::X264,
            bitrate: 8_000_000,
            fps: 60,
            cam: None,
            fit: Some((1921, 1080)),
        };
        let a = joined(&save_reencoded_args(
            Path::new("video.ts"),
            Some(&re),
            &[(Path::new("Game.wav"), "Game")],
            false,
            Path::new("out.mp4"),
        ));
        assert!(
            a.contains(
                "[0:v]scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,setsar=1[v]"
            ),
            "{a}"
        );
        assert!(a.contains("-map [v] -map 1:a:0 -c:v libx264"), "{a}");
        // With the webcam too: it goes over the fitted picture.
        let both = Reencode {
            cam: Some(CamOverlay {
                path: Path::new("cam.ts"),
                offset: 0.0,
                width: 480,
                corner: "top-left",
            }),
            ..re
        };
        let a = joined(&save_reencoded_args(
            Path::new("video.ts"),
            Some(&both),
            &[],
            false,
            Path::new("out.mp4"),
        ));
        assert!(
            a.contains("setsar=1[fit];[1:v]scale=480:-2[cam];[fit][cam]overlay="),
            "{a}"
        );
    }

    #[test]
    fn sizes() {
        assert_eq!(output_size((2560, 1440), None), (2560, 1440));
        assert_eq!(output_size((2560, 1440), Some(1080)), (1920, 1080));
        assert_eq!(output_size((5120, 1440), Some(720)), (2560, 720));
        assert_eq!(output_size((1366, 768), Some(1080)), (1366, 768));
        assert_eq!(output_size((1365, 767), None), (1364, 766));
    }

    #[test]
    fn gpu_capture_straight_into_nvenc() {
        let a = joined(&record_args(&plan(
            Source::WindowsGraphics {
                hmonitor: 65537,
                sdr_white: None,
            },
            Encoder::Nvenc,
            (2560, 1440),
            None,
        )));
        assert!(a.contains("-use_wallclock_as_timestamps 1 -f lavfi -i gfxcapture=hmonitor=65537:max_framerate=60"), "{a}");
        // One argument with no spaces in it (a stray run of them once got in).
        let filter = record_args(&plan(
            Source::WindowsGraphics {
                hmonitor: 65537,
                sdr_white: None,
            },
            Encoder::Nvenc,
            (2560, 1440),
            None,
        ))
        .into_iter()
        .find(|x| x.starts_with("gfxcapture="))
        .unwrap();
        assert!(!filter.contains(' '), "{filter}");
        // Even at the screen's own size: a display-mode change then scales, not crops.
        assert!(
            a.contains(":width=2560:height=1440:resize_mode=scale_aspect"),
            "{a}"
        );
        assert!(!a.contains("hwdownload"), "{a}");
        assert!(a.contains("-vf fps=60 "), "{a}");
        assert!(a.contains("-c:v h264_nvenc"));
        // A keyframe every 2 seconds.
        assert!(a.contains("-g 120 -keyint_min 120 -bf 0"), "{a}");
        assert!(
            a.contains("-tune ll -rc vbr -b:v 12000000 -maxrate 18000000 -bufsize 24000000"),
            "{a}"
        );
        // With a quality level: constant quality under the same ceiling.
        let mut p = plan(
            Source::WindowsGraphics {
                hmonitor: 65537,
                sdr_white: None,
            },
            Encoder::Nvenc,
            (2560, 1440),
            None,
        );
        p.cq = Some(21);
        let a = joined(&record_args(&p));
        assert!(
            a.contains(
                "-preset p2 -tune hq -rc vbr -cq 21 -b:v 0 -maxrate 18000000 -bufsize 36000000 -forced-idr 1 -g 120 -keyint_min 120 -bf 0"
            ),
            "{a}"
        );
        assert!(
            a.contains("-copyts -muxdelay 0 -muxpreload 0 -flush_packets 1 -f mpegts pipe:1"),
            "{a}"
        );
        assert!(
            !a.contains("-t "),
            "-t would cut a wall-clock stream at once"
        );
    }

    #[test]
    fn graphics_capture_scales_on_the_gpu() {
        let a = joined(&record_args(&plan(
            Source::WindowsGraphics {
                hmonitor: 1,
                sdr_white: None,
            },
            Encoder::Nvenc,
            (2560, 1440),
            Some(1080),
        )));
        assert!(
            a.contains(":width=1920:height=1080:resize_mode=scale_aspect"),
            "{a}"
        );
        assert!(!a.contains("scale=1920"), "{a}");
    }

    #[test]
    fn duplication_scales_in_memory_and_cpu_encoders_download() {
        let a = joined(&record_args(&plan(
            Source::Ddagrab { output: 1 },
            Encoder::Nvenc,
            (2560, 1440),
            Some(1080),
        )));
        assert!(a.contains("ddagrab=output_idx=1:framerate=60:draw_mouse=1"));
        assert!(
            a.contains("-vf fps=60,hwdownload,format=bgra,scale=1920:1080 "),
            "{a}"
        );
        let a = joined(&record_args(&plan(
            Source::Ddagrab { output: 0 },
            Encoder::X264,
            (1920, 1080),
            None,
        )));
        assert!(a.contains("-vf fps=60,hwdownload,format=bgra "), "{a}");
        assert!(a.contains("-c:v libx264 -preset veryfast"));
    }

    #[test]
    fn x11_with_vaapi() {
        let mut p = plan(
            Source::X11 {
                display: ":0".into(),
                x: 1920,
                y: 0,
                w: 2560,
                h: 1440,
            },
            Encoder::Vaapi,
            (2560, 1440),
            None,
        );
        p.vaapi_device = Some("/dev/dri/renderD129".into());
        let a = joined(&record_args(&p));
        assert!(
            a.starts_with(
                "-hide_banner -nostdin -nostats -loglevel error -vaapi_device /dev/dri/renderD129"
            ),
            "{a}"
        );
        assert!(
            a.contains("-f x11grab -framerate 60 -draw_mouse 1 -video_size 2560x1440 -i :0+1920,0"),
            "{a}"
        );
        assert!(a.contains("-vf fps=60,format=nv12,hwupload "), "{a}");
    }

    #[test]
    fn save_mixes_several_tracks() {
        let v = Path::new("v.ts");
        let (d, m, o) = (
            Path::new("desktop.wav"),
            Path::new("mic.wav"),
            Path::new("out.mp4"),
        );
        let a = joined(&save_args(v, &[(d, "Game"), (m, "Microphone")], false, o));
        assert!(
            a.contains(
                "-f mpegts -i v.ts -ignore_length 1 -i desktop.wav -ignore_length 1 -i mic.wav -filter_complex [1:a][2:a]amix=inputs=2"
            ),
            "{a}"
        );
        assert!(
            a.contains("-map 0:v:0 -map [mix] -map 1:a:0 -map 2:a:0 -c:v copy -c:a aac"),
            "{a}"
        );
        assert!(
            a.contains(
                "-metadata:s:a:0 title=Everything -metadata:s:a:0 handler_name=Everything \
                 -disposition:a:0 default"
            ),
            "{a}"
        );
        assert!(
            a.contains("handler_name=Microphone -disposition:a:2 0"),
            "{a}"
        );
        // The mix at full quality, the sources beside it smaller, the mic in mono.
        assert!(a.contains("-c:a aac -b:a 160k -metadata:s:a:0"), "{a}");
        assert!(a.contains("-b:a:1 128k -metadata:s:a:1 title=Game"), "{a}");
        assert!(
            a.contains("-b:a:2 64k -ac:a:2 1 -metadata:s:a:2 title=Microphone"),
            "{a}"
        );
        assert!(a.ends_with("-movflags +faststart -f mp4 out.mp4"));
        let a = joined(&save_args(v, &[(d, "Game")], false, o));
        assert!(!a.contains("-b:a:0"), "a lone track keeps 160k: {a}");
        assert!(
            a.contains("-map 0:v:0 -map 1:a:0 -c:v copy -c:a aac"),
            "{a}"
        );
        assert!(!a.contains("amix"));
        let a = joined(&save_args(v, &[], true, o));
        assert!(a.contains("-c:v copy -tag:v hvc1"), "{a}");
        assert!(!a.contains("-c:a"));
    }

    #[test]
    fn encoder_ids_round_trip() {
        for e in Encoder::ALL {
            assert_eq!(Encoder::from_id(e.id()), Some(e));
        }
        assert_eq!(Encoder::from_id("auto"), None);
    }
}
