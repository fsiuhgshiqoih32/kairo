//! ffmpeg command lines: recording into the replay buffer, and saving a clip.
//!
//! Recording writes MPEG-TS to stdout with wall-clock timestamps (`-use_wallclock_as_timestamps 1`
//! on the input, `-copyts` on the output), so every frame carries the Unix time it was captured.
//! Rewindr stamps audio with the same clock, which is how the two line up when a clip is saved.

use std::path::Path;

/// Where frames come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Windows Graphics Capture of a monitor (HMONITOR). Frames stay on the GPU, which also
    /// scales them.
    WindowsGraphics { hmonitor: u64 },
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

/// A video encoder Rewindr knows how to drive.
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

    /// Rate control and latency options for a target bitrate.
    fn options(self, bitrate: u64, fps: u32) -> Vec<String> {
        let b = bitrate.to_string();
        let max = (bitrate * 3 / 2).to_string();
        let buf = (bitrate * 2).to_string();
        let g = fps.max(1).to_string();
        let v: Vec<&str> = match self {
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
        // One keyframe a second: clips start at a keyframe, so this is the cutting precision.
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
    pub fps: u32,
    /// The captured area's size in pixels.
    pub src_size: (u32, u32),
    /// Scale down to at most this height.
    pub max_height: Option<u32>,
    pub bitrate: u64,
    /// VA-API render node, for `Encoder::Vaapi`.
    pub vaapi_device: Option<String>,
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
        Source::WindowsGraphics { hmonitor } => {
            let mut g =
                format!("gfxcapture=hmonitor={hmonitor}:max_framerate={fps}:capture_cursor=1");
            if scaled {
                g.push_str(&format!(":width={ow}:height={oh}:resize_mode=scale_aspect"));
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
        s(plan.encoder.ffmpeg_name()),
    ]);
    a.extend(plan.encoder.options(plan.bitrate, fps));
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
    a.extend(encoder.options(2_000_000, 30));
    a.extend([s("-f"), s("null"), s("-")]);
    a
}

/// Joins the saved video with the sound tracks (each a WAV starting where the video starts, with a
/// title) into an MP4. With several, the first track is their mix, which is what players and
/// Discord play; each source follows as its own track for editing.
pub fn save_args(video: &Path, audio: &[(&Path, &str)], out: &Path) -> Vec<String> {
    let mut a = vec![
        s("-y"),
        s("-hide_banner"),
        s("-nostdin"),
        s("-loglevel"),
        s("error"),
    ];
    a.extend([s("-i"), video.display().to_string()]);
    for (p, _) in audio {
        a.extend([s("-i"), p.display().to_string()]);
    }
    a.extend([s("-map"), s("0:v:0")]);
    let mut titles: Vec<&str> = Vec::new();
    if audio.len() > 1 {
        let n = audio.len();
        let inputs: String = (1..=n).map(|i| format!("[{i}:a]")).collect();
        a.extend([
            s("-filter_complex"),
            format!("{inputs}amix=inputs={n}:duration=longest:normalize=0[mix]"),
            s("-map"),
            s("[mix]"),
        ]);
        titles.push("Everything");
    }
    for (i, (_, title)) in audio.iter().enumerate() {
        a.extend([s("-map"), format!("{}:a:0", i + 1)]);
        titles.push(title);
    }
    a.extend([s("-c:v"), s("copy")]);
    if !audio.is_empty() {
        a.extend([s("-c:a"), s("aac"), s("-b:a"), s("160k")]);
    }
    for (i, t) in titles.iter().enumerate() {
        a.extend([format!("-metadata:s:a:{i}"), format!("title={t}")]);
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

/// A small picture of the clip's last moment, for the notification.
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

    fn plan(
        source: Source,
        encoder: Encoder,
        src: (u32, u32),
        max_height: Option<u32>,
    ) -> VideoPlan {
        VideoPlan {
            source,
            encoder,
            fps: 60,
            src_size: src,
            max_height,
            bitrate: 12_000_000,
            vaapi_device: None,
        }
    }

    fn joined(a: &[String]) -> String {
        a.join(" ")
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
            Source::WindowsGraphics { hmonitor: 65537 },
            Encoder::Nvenc,
            (2560, 1440),
            None,
        )));
        assert!(a.contains("-use_wallclock_as_timestamps 1 -f lavfi -i gfxcapture=hmonitor=65537:max_framerate=60"), "{a}");
        assert!(!a.contains("hwdownload"), "{a}");
        assert!(a.contains("-vf fps=60 "), "{a}");
        assert!(a.contains("-c:v h264_nvenc"));
        assert!(a.contains("-g 60 -keyint_min 60 -bf 0"));
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
            Source::WindowsGraphics { hmonitor: 1 },
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
        let a = joined(&save_args(v, &[(d, "Game"), (m, "Microphone")], o));
        assert!(a.contains("-i v.ts -i desktop.wav -i mic.wav -map 0:v:0 -filter_complex [1:a][2:a]amix=inputs=2"), "{a}");
        assert!(
            a.contains("-map [mix] -map 1:a:0 -map 2:a:0 -c:v copy -c:a aac"),
            "{a}"
        );
        assert!(
            a.contains("-metadata:s:a:0 title=Everything -disposition:a:0 default"),
            "{a}"
        );
        assert!(
            a.contains("-metadata:s:a:2 title=Microphone -disposition:a:2 0"),
            "{a}"
        );
        assert!(a.ends_with("-movflags +faststart -f mp4 out.mp4"));
        let a = joined(&save_args(v, &[(d, "Game")], o));
        assert!(
            a.contains("-map 0:v:0 -map 1:a:0 -c:v copy -c:a aac"),
            "{a}"
        );
        assert!(!a.contains("amix"));
        let a = joined(&save_args(v, &[], o));
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
