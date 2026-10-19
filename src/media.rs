//! ffmpeg command lines and arithmetic for the clip editor: reading a clip's details, decoding
//! frames and sound for playback, and exporting trimmed or size-limited copies.

use std::path::Path;

use crate::args::Encoder;

/// What the editor needs to know about a clip.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Video bitrate in bits per second, when ffmpeg reports it.
    pub video_bps: Option<u64>,
    /// Titles of the sound tracks, in order ("" when untitled).
    pub audio_tracks: Vec<String>,
    /// The video codec's ffmpeg name ("h264", "hevc").
    pub video_codec: String,
}

fn secs(hms: &str) -> Option<f64> {
    let mut parts = hms.trim().split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

/// Reads `ffmpeg -i <clip>`'s description of the first input.
pub fn parse_info(text: &str) -> Option<Info> {
    let mut info = Info::default();
    let mut seen_video = false;
    let mut last_was_audio = false;
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("Duration:") {
            info.duration = secs(rest.split(',').next()?).unwrap_or(0.0);
        } else if l.starts_with("Stream #") && l.contains("Video:") && !seen_video {
            seen_video = true;
            last_was_audio = false;
            if let Some(after) = l.split("Video:").nth(1) {
                info.video_codec = after
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(',')
                    .to_string();
            }
            let words: Vec<&str> = l
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|w| !w.is_empty())
                .collect();
            for w in &words {
                if let Some((a, b)) = w.split_once('x')
                    && let (Ok(x), Ok(y)) = (a.parse::<u32>(), b.parse::<u32>())
                    && x >= 16
                    && y >= 16
                {
                    info.width = x;
                    info.height = y;
                    break;
                }
            }
            for pair in words.windows(2) {
                match pair[1] {
                    "fps" => info.fps = pair[0].parse().unwrap_or(0.0),
                    "kb/s" => info.video_bps = pair[0].parse::<u64>().ok().map(|k| k * 1000),
                    _ => {}
                }
            }
        } else if l.starts_with("Stream #") && l.contains("Audio:") {
            info.audio_tracks.push(String::new());
            last_was_audio = true;
        } else if l.starts_with("Stream #") {
            last_was_audio = false;
        } else if last_was_audio
            && let Some(t) = l.strip_prefix("title")
            && let Some((_, v)) = t.split_once(':')
            && let Some(last) = info.audio_tracks.last_mut()
        {
            *last = v.trim().to_string();
        }
    }
    (seen_video && info.width > 0).then_some(info)
}

/// The size frames are decoded at for playback: the clip's size, fitted into `max` (keeping the
/// aspect ratio), in even numbers.
pub fn fit(src: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let (w, h) = (src.0.max(2) as f64, src.1.max(2) as f64);
    let k = (max.0 as f64 / w).min(max.1 as f64 / h).min(1.0);
    let ew = ((w * k).round() as u32).max(2) & !1;
    let eh = ((h * k).round() as u32).max(2) & !1;
    (ew, eh)
}

fn s(v: &str) -> String {
    v.to_string()
}

fn head() -> Vec<String> {
    vec![
        s("-hide_banner"),
        s("-nostdin"),
        s("-nostats"),
        s("-loglevel"),
        s("error"),
    ]
}

/// RGBA frames of the video from `start` on, at `size`, on stdout.
pub fn video_args(clip: &Path, start: f64, size: (u32, u32), frames: Option<u32>) -> Vec<String> {
    let mut a = head();
    a.extend([s("-hwaccel"), s("auto")]);
    if start > 0.0 {
        a.extend([s("-ss"), format!("{start:.3}")]);
    }
    a.extend([s("-i"), clip.display().to_string(), s("-an"), s("-sn")]);
    if let Some(n) = frames {
        a.extend([s("-frames:v"), n.to_string()]);
    }
    a.extend([
        s("-vf"),
        format!("scale={}:{}:flags=bilinear,format=rgba", size.0, size.1),
        s("-fps_mode"),
        s("passthrough"),
        s("-f"),
        s("rawvideo"),
        s("-pix_fmt"),
        s("rgba"),
        s("pipe:1"),
    ]);
    a
}

/// Interleaved 32-bit float sound of track `track` from `start` on, on stdout.
pub fn audio_args(clip: &Path, start: f64, track: usize, rate: u32, channels: u16) -> Vec<String> {
    let mut a = head();
    if start > 0.0 {
        a.extend([s("-ss"), format!("{start:.3}")]);
    }
    a.extend([
        s("-i"),
        clip.display().to_string(),
        s("-vn"),
        s("-sn"),
        s("-map"),
        format!("0:a:{track}"),
        s("-ac"),
        channels.to_string(),
        s("-ar"),
        rate.to_string(),
        s("-f"),
        s("f32le"),
        s("pipe:1"),
    ]);
    a
}

/// Sound track `track` as 16 kHz mono 16-bit samples on stdout, for speech recognition.
pub fn speech_args(clip: &Path, track: usize) -> Vec<String> {
    let mut a = head();
    a.extend([
        s("-i"),
        clip.display().to_string(),
        s("-vn"),
        s("-sn"),
        s("-map"),
        format!("0:a:{track}"),
        s("-ac"),
        s("1"),
        s("-ar"),
        s("16000"),
        s("-f"),
        s("s16le"),
        s("pipe:1"),
    ]);
    a
}

/// Small RGBA pictures of every `every`th keyframe (one a second in Kairo's clips), for the
/// timeline.
pub fn filmstrip_args(clip: &Path, size: (u32, u32), every: u32) -> Vec<String> {
    let mut a = head();
    a.extend([
        s("-skip_frame"),
        s("nokey"),
        s("-i"),
        clip.display().to_string(),
        s("-an"),
        s("-sn"),
        s("-vf"),
        if every > 1 {
            format!(
                "select='not(mod(n\\,{every}))',scale={}:{}:flags=bilinear,format=rgba",
                size.0, size.1
            )
        } else {
            format!("scale={}:{}:flags=bilinear,format=rgba", size.0, size.1)
        },
        s("-fps_mode"),
        s("passthrough"),
        s("-f"),
        s("rawvideo"),
        s("-pix_fmt"),
        s("rgba"),
        s("pipe:1"),
    ]);
    a
}

/// How many keyframes to step for a filmstrip of about 150 pictures (one a second up to
/// 2.5 minutes).
pub fn strip_step(duration: f64) -> u32 {
    (duration / 150.0).ceil().max(1.0) as u32
}

/// Encoder options for an export at `bitrate`. `exact` holds the rate steady, for a size limit.
fn export_video_opts(encoder: Encoder, bitrate: u64, fps: u32, exact: bool) -> Vec<String> {
    let b = bitrate.to_string();
    let max = if exact {
        b.clone()
    } else {
        (bitrate * 3 / 2).to_string()
    };
    let buf = if exact {
        b.clone()
    } else {
        (bitrate * 2).to_string()
    };
    let v: Vec<&str> = match encoder {
        Encoder::Nvenc => {
            let rc = if exact { "cbr" } else { "vbr" };
            vec![
                "-preset", "p5", "-rc", rc, "-b:v", &b, "-maxrate", &max, "-bufsize", &buf,
            ]
        }
        Encoder::Amf => {
            let rc = if exact { "cbr" } else { "vbr_peak" };
            vec![
                "-quality", "quality", "-rc", rc, "-b:v", &b, "-maxrate", &max, "-bufsize", &buf,
            ]
        }
        Encoder::MediaFoundation => {
            let rc = if exact { "cbr" } else { "u_vbr" };
            vec!["-hw_encoding", "1", "-rate_control", rc, "-b:v", &b]
        }
        Encoder::VideoToolbox => vec!["-b:v", &b, "-maxrate", &max, "-bufsize", &buf],
        // Exports run on whole files, where x264 is fast enough and VA-API's upload isn't worth it.
        Encoder::X264 | Encoder::Vaapi => vec![
            "-preset", "veryfast", "-b:v", &b, "-maxrate", &max, "-bufsize", &buf,
        ],
    };
    let mut out: Vec<String> = v.into_iter().map(String::from).collect();
    let g = fps.max(1).to_string();
    out.extend([s("-g"), g, s("-bf"), s("0"), s("-pix_fmt"), s("yuv420p")]);
    out
}

/// The encoder to export with: exports never use VA-API (see [`export_video_opts`]).
pub fn export_encoder(e: Encoder) -> Encoder {
    if e == Encoder::Vaapi {
        Encoder::X264
    } else {
        e
    }
}

/// A colour look for exports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    #[default]
    None,
    Vivid,
    Cinematic,
    Mono,
    Vintage,
}

impl Look {
    pub const ALL: [Look; 5] = [
        Look::None,
        Look::Vivid,
        Look::Cinematic,
        Look::Mono,
        Look::Vintage,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Look::None => "None",
            Look::Vivid => "Vivid",
            Look::Cinematic => "Cinematic",
            Look::Mono => "Black & white",
            Look::Vintage => "Vintage",
        }
    }

    fn filter(self) -> Option<&'static str> {
        match self {
            Look::None => None,
            Look::Vivid => Some("eq=contrast=1.06:saturation=1.35,unsharp=5:5:0.5"),
            Look::Cinematic => Some(
                "eq=contrast=1.12:saturation=0.88:gamma=0.97,curves=preset=medium_contrast,vignette=PI/5",
            ),
            Look::Mono => Some("hue=s=0,eq=contrast=1.12"),
            Look::Vintage => Some("curves=preset=vintage,vignette=PI/4"),
        }
    }
}

impl std::fmt::Display for Look {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Music played under an export.
#[derive(Clone, Debug, PartialEq)]
pub struct Music {
    pub path: std::path::PathBuf,
    /// 0 to 1.
    pub volume: f32,
}

/// A caption picture to lay over the picture: raw RGBA in a file, shown from `start` to `end`
/// (seconds into the clip).
#[derive(Clone, Debug, PartialEq)]
pub struct Burn {
    pub path: std::path::PathBuf,
    pub width: u32,
    pub height: u32,
    pub start: f64,
    pub end: f64,
}

/// A stream of overlay frames (raw RGBA in a file) laid over the picture's corner.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayVideo {
    pub path: std::path::PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// What an export changes on the way: speed, colour, fades, the mix, music, captions.
#[derive(Clone, Debug, PartialEq)]
pub struct Effects {
    /// 1 is normal; 0.25 to 4.
    pub speed: f64,
    pub look: Look,
    /// Fade picture and sound in at the start and out at the end.
    pub fade: bool,
    pub music: Option<Music>,
    /// Volumes of the game, microphone and voice-chat tracks (1 is as recorded).
    pub game_volume: f32,
    pub mic_volume: f32,
    pub chat_volume: f32,
    /// Take the hiss and hum out of the microphone.
    pub mic_cleanup: bool,
    /// Bring the whole thing to a steady, loud-enough level.
    pub even_loudness: bool,
    /// Captions to burn in (made for the export's size just before it runs).
    pub burns: Vec<Burn>,
    /// The keys-and-clicks overlay (made for the export just before it runs).
    pub keys: Option<OverlayVideo>,
}

impl Default for Effects {
    fn default() -> Self {
        Effects {
            speed: 1.0,
            look: Look::None,
            fade: false,
            music: None,
            game_volume: 1.0,
            mic_volume: 1.0,
            chat_volume: 1.0,
            mic_cleanup: false,
            even_loudness: false,
            burns: Vec::new(),
            keys: None,
        }
    }
}

/// How long fades last, in seconds.
const FADE: f64 = 0.5;

impl Effects {
    /// Nothing to change: a plain copy.
    pub fn is_plain(&self) -> bool {
        *self == Effects::default()
    }

    /// The same effects at another speed (slow motion presets).
    pub fn at_speed(&self, speed: f64) -> Effects {
        Effects {
            speed,
            ..self.clone()
        }
    }

    /// How long `start..end` of a clip lasts once exported.
    pub fn length(&self, start: f64, end: f64) -> f64 {
        (end - start).max(0.05) / self.speed.clamp(0.25, 4.0)
    }

    fn mixes_tracks(&self) -> bool {
        self.game_volume != 1.0
            || self.mic_volume != 1.0
            || self.chat_volume != 1.0
            || self.mic_cleanup
    }
}

/// `atempo` steps for a speed: each step does at most 2x either way.
fn tempo_chain(speed: f64) -> Vec<String> {
    let mut left = speed.clamp(0.25, 4.0);
    let mut steps = Vec::new();
    while left >= 2.0 - 1e-9 {
        steps.push(s("atempo=2.0"));
        left /= 2.0;
    }
    while left <= 0.5 + 1e-9 {
        steps.push(s("atempo=0.5"));
        left /= 0.5;
    }
    if (left - 1.0).abs() > 1e-6 {
        steps.push(format!("atempo={left:.4}"));
    }
    steps
}

/// The filter graph for an export: the picture through speed, look and fades into `shape`
/// (the format's own layout, from `[pre]` to `[v]`), and with `sound`, the mix through speed,
/// fades, music (input 1) and levelling into `[a]`. `length` is the output's length.
fn effects_graph(
    eff: &Effects,
    info: &Info,
    shape: &str,
    out_fps: u32,
    length: f64,
    sound: bool,
) -> String {
    let mut parts = Vec::new();
    let mut pic = Vec::new();
    if (eff.speed - 1.0).abs() > 1e-6 {
        pic.push(format!("setpts=PTS/{:.4}", eff.speed.clamp(0.25, 4.0)));
    }
    pic.push(format!("fps={out_fps}"));
    if let Some(l) = eff.look.filter() {
        pic.push(s(l));
    }
    if eff.fade && length > FADE * 2.5 {
        pic.push(format!("fade=t=in:st=0:d={FADE}"));
        pic.push(format!("fade=t=out:st={:.3}:d={FADE}", length - FADE));
    }
    parts.push(format!("[0:v]{}[pre]", pic.join(",")));
    if eff.burns.is_empty() && eff.keys.is_none() {
        parts.push(s(shape));
    } else {
        // Captions over the finished picture, each for its time (in the export's timeline),
        // then the keys in the bottom-left corner.
        let shaped = shape.strip_suffix("[v]").unwrap_or(shape);
        parts.push(format!("{shaped}[sv]"));
        let first = if eff.music.is_some() { 2 } else { 1 };
        let mut last = s("sv");
        let n = eff.burns.len() + eff.keys.is_some() as usize;
        for (i, b) in eff.burns.iter().enumerate() {
            let to = if i + 1 == n { s("v") } else { format!("c{i}") };
            parts.push(format!(
                "[{last}][{}:v]overlay=x=(W-w)/2:y=H-h-H/14:enable='between(t,{:.3},{:.3})'[{to}]",
                first + i,
                b.start,
                b.end
            ));
            last = to;
        }
        if eff.keys.is_some() {
            parts.push(format!(
                "[{last}][{}:v]overlay=x=W*3/100:y=H-h-H*5/100:eof_action=pass[v]",
                first + eff.burns.len()
            ));
        }
    }
    if !sound {
        return parts.join(";");
    }
    let track = |name: &str| info.audio_tracks.iter().position(|t| t == name);
    // The separate tracks the clip has, each with its level (the mic cleaned up if asked).
    let clean = if eff.mic_cleanup {
        "afftdn=nr=12,highpass=f=90,"
    } else {
        ""
    };
    let parts_in: Vec<(usize, String)> = [
        ("Game", eff.game_volume, ""),
        ("Microphone", eff.mic_volume, clean),
        ("Voice chat", eff.chat_volume, ""),
    ]
    .into_iter()
    .filter_map(|(name, v, pre)| track(name).map(|i| (i, format!("{pre}volume={v:.2}"))))
    .collect();
    let base = if eff.mixes_tracks() && parts_in.len() >= 2 {
        let mut g = String::new();
        let mut labels = String::new();
        for (k, (i, chain)) in parts_in.iter().enumerate() {
            g.push_str(&format!("[0:a:{i}]{chain}[t{k}];"));
            labels.push_str(&format!("[t{k}]"));
        }
        format!(
            "{g}{labels}amix=inputs={}:duration=longest:normalize=0",
            parts_in.len()
        )
    } else if info.audio_tracks.is_empty() {
        format!("anullsrc=r=48000:cl=stereo,atrim=0:{length:.3}")
    } else {
        format!("[0:a:0]volume={:.2}", eff.game_volume)
    };
    let mut snd = vec![s("aformat=sample_rates=48000:channel_layouts=stereo")];
    snd.extend(tempo_chain(eff.speed));
    if eff.fade && length > FADE * 2.5 {
        snd.push(format!("afade=t=in:st=0:d={FADE}"));
        snd.push(format!("afade=t=out:st={:.3}:d={FADE}", length - FADE));
    }
    parts.push(format!("{base},{}[mix]", snd.join(",")));
    let mut last = s("mix");
    if let Some(m) = &eff.music {
        let tail = (length - 1.5).max(0.0);
        parts.push(format!(
            "[1:a]aformat=sample_rates=48000:channel_layouts=stereo,volume={:.2},atrim=0:{length:.3},\
             afade=t=out:st={tail:.3}:d=1.5[mus]",
            m.volume.clamp(0.0, 2.0)
        ));
        parts.push(s(
            "[mix][mus]amix=inputs=2:duration=first:normalize=0[withmusic]",
        ));
        last = s("withmusic");
    }
    if eff.even_loudness {
        parts.push(format!(
            "[{last}]loudnorm=I=-16:TP=-1.5:LRA=11,aresample=48000[a]"
        ));
    } else {
        parts.push(format!("[{last}]anull[a]"));
    }
    parts.join(";")
}

/// The start of an export with effects: the clip's part, then the music (looped) if any.
fn effects_head(clip: &Path, start: f64, end: f64, eff: &Effects) -> Vec<String> {
    let mut a = export_head(clip, start, end);
    if let Some(m) = &eff.music {
        a.extend([
            s("-stream_loop"),
            s("-1"),
            s("-i"),
            m.path.display().to_string(),
        ]);
    }
    for b in &eff.burns {
        a.extend([
            s("-f"),
            s("rawvideo"),
            s("-pix_fmt"),
            s("rgba"),
            s("-video_size"),
            format!("{}x{}", b.width, b.height),
            s("-i"),
            b.path.display().to_string(),
        ]);
    }
    if let Some(k) = &eff.keys {
        a.extend([
            s("-f"),
            s("rawvideo"),
            s("-pix_fmt"),
            s("rgba"),
            s("-video_size"),
            format!("{}x{}", k.width, k.height),
            s("-framerate"),
            k.fps.to_string(),
            s("-i"),
            k.path.display().to_string(),
        ]);
    }
    a
}

/// Where captions shown at `start..end` of the clip land in an export of `range` at `speed`,
/// or `None` if they're outside it.
pub fn burn_window(start: f64, end: f64, range: (f64, f64), speed: f64) -> Option<(f64, f64)> {
    let (a, b) = (start.max(range.0), end.min(range.1));
    (b > a).then(|| ((a - range.0) / speed, (b - range.0) / speed))
}

/// A frame-exact copy of `start..end` of the clip, re-encoded at about the original quality.
/// Without effects it keeps every sound track; with them, one mixed track. Progress goes to
/// stdout (`-progress`).
pub fn trim_args(
    clip: &Path,
    start: f64,
    end: f64,
    info: &Info,
    eff: &Effects,
    encoder: Encoder,
    out: &Path,
) -> Vec<String> {
    let encoder = export_encoder(encoder);
    let fps = info.fps.round().max(1.0) as u32;
    let bitrate = info.video_bps.unwrap_or_else(|| {
        crate::config::Quality::High.bitrate(info.width as u64 * info.height as u64, fps)
    });
    if !eff.is_plain() {
        let length = eff.length(start, end);
        let mut a = effects_head(clip, start, end, eff);
        a.extend([
            s("-filter_complex"),
            effects_graph(eff, info, "[pre]null[v]", fps.min(120), length, true),
            s("-map"),
            s("[v]"),
            s("-map"),
            s("[a]"),
            s("-c:v"),
            s(encoder.ffmpeg_name()),
        ]);
        a.extend(export_video_opts(encoder, bitrate, fps.min(120), false));
        a.extend(mp4_tail(out));
        return a;
    }
    let mut a = vec![s("-y")];
    a.extend(head());
    a.extend([s("-progress"), s("pipe:1")]);
    a.extend([
        s("-ss"),
        format!("{start:.3}"),
        s("-i"),
        clip.display().to_string(),
        s("-t"),
        format!("{:.3}", (end - start).max(0.05)),
        s("-map"),
        s("0:v:0"),
        s("-map"),
        s("0:a?"),
        s("-c:v"),
        s(encoder.ffmpeg_name()),
    ]);
    a.extend(export_video_opts(encoder, bitrate, fps, false));
    a.extend([s("-c:a"), s("aac"), s("-b:a"), s("160k")]);
    a.extend([
        s("-movflags"),
        s("+faststart"),
        s("-f"),
        s("mp4"),
        out.display().to_string(),
    ]);
    a
}

/// The start of an export: overwrite, quiet, progress on stdout, `start..end` of the clip.
fn export_head(clip: &Path, start: f64, end: f64) -> Vec<String> {
    let mut a = vec![s("-y")];
    a.extend(head());
    a.extend([s("-progress"), s("pipe:1")]);
    // Both on the input: the output can be longer than the part read (slow motion).
    a.extend([
        s("-ss"),
        format!("{start:.3}"),
        s("-t"),
        format!("{:.3}", (end - start).max(0.05)),
        s("-i"),
        clip.display().to_string(),
    ]);
    a
}

fn mp4_tail(out: &Path) -> Vec<String> {
    vec![
        s("-c:a"),
        s("aac"),
        s("-b:a"),
        s("160k"),
        s("-movflags"),
        s("+faststart"),
        s("-f"),
        s("mp4"),
        out.display().to_string(),
    ]
}

/// A GIF of `start..end`: `width` pixels wide at `fps`, with a palette made for this clip.
#[allow(clippy::too_many_arguments)]
pub fn gif_args(
    clip: &Path,
    start: f64,
    end: f64,
    info: &Info,
    eff: &Effects,
    width: u32,
    fps: u32,
    out: &Path,
) -> Vec<String> {
    // A GIF has no sound, and overlays don't survive its palette.
    let eff = Effects {
        music: None,
        burns: Vec::new(),
        keys: None,
        ..eff.clone()
    };
    let shape = format!(
        "[pre]scale={width}:-2:flags=lanczos,split[ga][gb];[ga]palettegen=stats_mode=diff[p];\
         [gb][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle[v]"
    );
    let length = eff.length(start, end);
    let mut a = export_head(clip, start, end);
    a.extend([
        s("-filter_complex"),
        effects_graph(&eff, info, &shape, fps, length, false),
        s("-map"),
        s("[v]"),
        s("-an"),
        s("-loop"),
        s("0"),
        s("-f"),
        s("gif"),
        out.display().to_string(),
    ]);
    a
}

/// A frame for phones or feeds: `size` (1080x1920, 1080x1080, 1080x1350), the whole picture in
/// the middle over a blurred, zoomed copy of itself.
#[allow(clippy::too_many_arguments)]
pub fn framed_args(
    clip: &Path,
    start: f64,
    end: f64,
    info: &Info,
    eff: &Effects,
    size: (u32, u32),
    encoder: Encoder,
    out: &Path,
) -> Vec<String> {
    let encoder = export_encoder(encoder);
    let (w, h) = size;
    let fps = (info.fps.round() as u32).clamp(1, 60);
    let bitrate = crate::config::Quality::High
        .bitrate(w as u64 * h as u64, fps)
        .min(16_000_000);
    let (bw, bh) = (w / 4, h / 4);
    let shape = format!(
        "[pre]split[bg][fg];\
         [bg]scale={bw}:{bh}:force_original_aspect_ratio=increase,crop={bw}:{bh},boxblur=10:2,scale={w}:{h}[b];\
         [fg]scale={w}:{h}:force_original_aspect_ratio=decrease[f];[b][f]overlay=(W-w)/2:(H-h)/2,format=yuv420p[v]"
    );
    let length = eff.length(start, end);
    let mut a = effects_head(clip, start, end, eff);
    a.extend([
        s("-filter_complex"),
        effects_graph(eff, info, &shape, fps, length, true),
        s("-map"),
        s("[v]"),
        s("-map"),
        s("[a]"),
        s("-c:v"),
        s(encoder.ffmpeg_name()),
    ]);
    a.extend(export_video_opts(encoder, bitrate, fps, false));
    a.extend(mp4_tail(out));
    a
}

/// How a clip is squeezed under a size limit.
#[derive(Clone, Debug, PartialEq)]
pub struct SharePlan {
    pub video_bps: u64,
    pub audio_bps: u64,
    /// Scale down to this height, if smaller than the clip's.
    pub height: Option<u32>,
    /// Drop to this frame rate, if lower than the clip's.
    pub fps: Option<u32>,
}

/// The lowest video bitrate worth sending; below it, ask for a shorter clip instead.
const MIN_VIDEO_BPS: u64 = 250_000;

/// Picks bitrate, size and frame rate so `duration` seconds fit in `limit_bytes`. The `scale`
/// below 1 retries with a smaller budget when an encode came out too big.
pub fn share_plan(
    limit_bytes: u64,
    duration: f64,
    src: &Info,
    scale: f64,
) -> Result<SharePlan, String> {
    let duration = duration.max(0.5);
    // 8% for the container and the encoder overshooting.
    let total = limit_bytes as f64 * 8.0 * 0.92 * scale / duration;
    let audio_bps: u64 = if total > 1_500_000.0 { 128_000 } else { 64_000 };
    let video = total - audio_bps as f64;
    if video < MIN_VIDEO_BPS as f64 {
        let fits = limit_bytes as f64 * 8.0 * 0.92 / (MIN_VIDEO_BPS + 64_000) as f64;
        return Err(format!(
            "{duration:.0} s is too long to look good in {} MB. Trim it to {:.0} s or less first.",
            limit_bytes / 1_000_000,
            fits.floor()
        ));
    }
    let video_bps = video as u64;
    let target_h = if video_bps < 700_000 {
        480
    } else if video_bps < 1_800_000 {
        720
    } else if video_bps < 5_000_000 {
        1080
    } else {
        u32::MAX
    };
    let height = (target_h < src.height).then_some(target_h);
    let fps = (src.fps > 31.0 && video_bps < 3_000_000).then_some(30);
    Ok(SharePlan {
        video_bps,
        audio_bps,
        height,
        fps,
    })
}

/// A copy for a share link: at most 1080p and 60 fps, at most 12 Mbit/s (plenty for a browser),
/// small enough for `limit_bytes`.
pub fn web_plan(limit_bytes: u64, duration: f64, src: &Info) -> Result<SharePlan, String> {
    let mut p = share_plan(limit_bytes, duration, src, 1.0)?;
    p.video_bps = p.video_bps.min(12_000_000);
    if src.height > 1080 {
        p.height = Some(p.height.map_or(1080, |h| h.min(1080)));
    }
    if src.fps > 61.0 {
        p.fps = Some(p.fps.map_or(60, |f| f.min(60)));
    }
    Ok(p)
}

/// Encodes `start..end` of the clip by `plan`, sound track 1 only (what Discord plays), with
/// `eff` applied when there are any.
#[allow(clippy::too_many_arguments)]
pub fn share_args(
    clip: &Path,
    start: f64,
    end: f64,
    info: &Info,
    eff: &Effects,
    plan: &SharePlan,
    encoder: Encoder,
    out: &Path,
) -> Vec<String> {
    let encoder = export_encoder(encoder);
    let fps = plan.fps.unwrap_or(60);
    if !eff.is_plain() {
        let out_fps = plan.fps.unwrap_or((info.fps.round() as u32).clamp(1, 60));
        let shape = match plan.height {
            Some(h) => format!("[pre]scale=-2:{h}[v]"),
            None => s("[pre]null[v]"),
        };
        let length = eff.length(start, end);
        let mut a = effects_head(clip, start, end, eff);
        a.extend([
            s("-filter_complex"),
            effects_graph(eff, info, &shape, out_fps, length, true),
            s("-map"),
            s("[v]"),
            s("-map"),
            s("[a]"),
            s("-c:v"),
            s(encoder.ffmpeg_name()),
        ]);
        a.extend(export_video_opts(encoder, plan.video_bps, out_fps, true));
        a.extend([s("-c:a"), s("aac"), s("-b:a"), plan.audio_bps.to_string()]);
        a.extend([
            s("-movflags"),
            s("+faststart"),
            s("-f"),
            s("mp4"),
            out.display().to_string(),
        ]);
        return a;
    }
    let mut filters = Vec::new();
    if let Some(f) = plan.fps {
        filters.push(format!("fps={f}"));
    }
    if let Some(h) = plan.height {
        filters.push(format!("scale=-2:{h}"));
    }
    let mut a = vec![s("-y")];
    a.extend(head());
    a.extend([s("-progress"), s("pipe:1")]);
    a.extend([
        s("-ss"),
        format!("{start:.3}"),
        s("-i"),
        clip.display().to_string(),
        s("-t"),
        format!("{:.3}", (end - start).max(0.05)),
        s("-map"),
        s("0:v:0"),
        s("-map"),
        s("0:a:0?"),
    ]);
    if !filters.is_empty() {
        a.extend([s("-vf"), filters.join(",")]);
    }
    a.extend([s("-c:v"), s(encoder.ffmpeg_name())]);
    a.extend(export_video_opts(encoder, plan.video_bps, fps, true));
    a.extend([s("-c:a"), s("aac"), s("-b:a"), plan.audio_bps.to_string()]);
    a.extend([
        s("-movflags"),
        s("+faststart"),
        s("-f"),
        s("mp4"),
        out.display().to_string(),
    ]);
    a
}

/// Seconds encoded so far, from one line of ffmpeg's `-progress` output.
pub fn progress_secs(line: &str) -> Option<f64> {
    let v = line
        .strip_prefix("out_time_us=")
        .or_else(|| line.strip_prefix("out_time_ms="))?;
    v.trim()
        .parse::<i64>()
        .ok()
        .filter(|v| *v >= 0)
        .map(|us| us as f64 / 1e6)
}

/// "0:07.25" for the editor's timeline.
pub fn format_time(t: f64) -> String {
    let t = t.max(0.0);
    let m = (t / 60.0).floor();
    format!("{}:{:05.2}", m as u64, t - m * 60.0)
}

/// One clip in a montage.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    pub path: std::path::PathBuf,
    pub duration: f64,
    pub has_audio: bool,
}

/// Each montage clip fades in from and out to black over this long.
pub const MONTAGE_FADE: f64 = 0.3;

/// Joins `pieces` into one video, in order: each scaled into `size` (letterboxed if its shape
/// differs) at `fps`, dipping to black between clips, sound faded with the picture. Clips
/// without sound get silence. Progress goes to stdout.
pub fn montage_args(
    pieces: &[Piece],
    size: (u32, u32),
    fps: u32,
    encoder: Encoder,
    out: &Path,
) -> Vec<String> {
    let encoder = export_encoder(encoder);
    let (w, h) = (size.0.max(2) & !1, size.1.max(2) & !1);
    let fps = fps.clamp(1, 60);
    let mut a = vec![s("-y")];
    a.extend(head());
    a.extend([s("-progress"), s("pipe:1")]);
    for p in pieces {
        a.extend([s("-i"), p.path.display().to_string()]);
    }
    let fit = format!(
        "scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,\
         setsar=1,fps={fps},format=yuv420p"
    );
    let mut graph = Vec::new();
    let mut joined = String::new();
    for (i, p) in pieces.iter().enumerate() {
        let d = p.duration.max(MONTAGE_FADE * 2.0 + 0.1);
        let fade = MONTAGE_FADE;
        let out_at = d - fade;
        graph.push(format!(
            "[{i}:v]{fit},fade=t=in:st=0:d={fade},fade=t=out:st={out_at:.3}:d={fade}[v{i}]"
        ));
        let sound = if p.has_audio {
            format!(
                "[{i}:a:0]aformat=sample_rates=48000:channel_layouts=stereo,apad=whole_dur={d:.3},\
                 atrim=0:{d:.3}"
            )
        } else {
            format!("anullsrc=r=48000:cl=stereo,atrim=0:{d:.3}")
        };
        graph.push(format!(
            "{sound},afade=t=in:st=0:d={fade},afade=t=out:st={out_at:.3}:d={fade}[a{i}]"
        ));
        joined.push_str(&format!("[v{i}][a{i}]"));
    }
    graph.push(format!("{joined}concat=n={}:v=1:a=1[v][a]", pieces.len()));
    let bitrate = crate::config::Quality::High
        .bitrate(w as u64 * h as u64, fps)
        .min(16_000_000);
    a.extend([
        s("-filter_complex"),
        graph.join(";"),
        s("-map"),
        s("[v]"),
        s("-map"),
        s("[a]"),
        s("-c:v"),
        s(encoder.ffmpeg_name()),
    ]);
    a.extend(export_video_opts(encoder, bitrate, fps, false));
    a.extend(mp4_tail(out));
    a
}

/// How long a montage of `pieces` runs.
pub fn montage_length(pieces: &[Piece]) -> f64 {
    pieces
        .iter()
        .map(|p| p.duration.max(MONTAGE_FADE * 2.0 + 0.1))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'x.mp4':
  Metadata:
    major_brand     : isom
  Duration: 00:00:30.30, start: 0.000000, bitrate: 16829 kb/s
  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(tv, bt709, progressive), 1920x1440 [SAR 1:1 DAR 4:3], 16660 kb/s, 60 fps, 60 tbr, 90k tbn (default)
      Metadata:
        handler_name    : VideoHandler
  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp, 160 kb/s (default)
      Metadata:
        title           : Everything
  Stream #0:2[0x3](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp, 160 kb/s
      Metadata:
        title           : Game
At least one output file must be specified";

    #[test]
    fn reads_clip_details() {
        let i = parse_info(SAMPLE).unwrap();
        assert!((i.duration - 30.3).abs() < 1e-9);
        assert_eq!((i.width, i.height), (1920, 1440));
        assert_eq!(i.fps, 60.0);
        assert_eq!(i.video_bps, Some(16_660_000));
        assert_eq!(i.video_codec, "h264");
        assert_eq!(
            i.audio_tracks,
            vec!["Everything".to_string(), "Game".to_string()]
        );
        assert_eq!(
            parse_info("x.mp4: Invalid data found when processing input"),
            None
        );
    }

    #[test]
    fn fits_playback_size() {
        assert_eq!(fit((1920, 1440), (1280, 720)), (960, 720));
        assert_eq!(fit((2560, 1440), (1280, 720)), (1280, 720));
        assert_eq!(fit((640, 480), (1280, 720)), (640, 480));
    }

    #[test]
    fn share_plans() {
        let src = Info {
            duration: 30.0,
            width: 2560,
            height: 1440,
            fps: 60.0,
            ..Default::default()
        };
        // 30 s in 10 MB: about 2.3 Mbit/s of video, so 1080p30.
        let p = share_plan(10_000_000, 30.0, &src, 1.0).unwrap();
        assert!((2_200_000..2_400_000).contains(&p.video_bps), "{p:?}");
        assert_eq!(p.height, Some(1080));
        assert_eq!(p.fps, Some(30));
        // 15 s in 50 MB keeps everything.
        let p = share_plan(50_000_000, 15.0, &src, 1.0).unwrap();
        assert_eq!((p.height, p.fps), (None, None));
        // 2 minutes in 10 MB is 480p.
        let p = share_plan(10_000_000, 120.0, &src, 1.0).unwrap();
        assert_eq!(p.height, Some(480));
        // 5 minutes doesn't fit.
        let e = share_plan(10_000_000, 300.0, &src, 1.0).unwrap_err();
        assert!(e.contains("Trim it to 234 s"), "{e}");
        // Total bits stay under the limit.
        let p = share_plan(10_000_000, 45.0, &src, 1.0).unwrap();
        assert!(((p.video_bps + p.audio_bps) as f64 * 45.0 / 8.0) < 10_000_000.0 * 0.93);
    }

    #[test]
    fn export_command_lines() {
        let info = parse_info(SAMPLE).unwrap();
        let clip = Path::new("in.mp4");
        let a = trim_args(
            clip,
            3.25,
            20.0,
            &info,
            &Effects::default(),
            Encoder::Nvenc,
            Path::new("out.mp4"),
        )
        .join(" ");
        assert!(a.contains("-progress pipe:1 -ss 3.250 -i in.mp4 -t 16.750 -map 0:v:0 -map 0:a? -c:v h264_nvenc"), "{a}");
        assert!(a.contains("-b:v 16660000"), "{a}");
        assert!(a.ends_with("-f mp4 out.mp4"));
        let plan = share_plan(10_000_000, 16.75, &info, 1.0).unwrap();
        let a = share_args(
            clip,
            3.25,
            20.0,
            &info,
            &Effects::default(),
            &plan,
            Encoder::Vaapi,
            Path::new("d.mp4"),
        )
        .join(" ");
        assert!(
            a.contains("-map 0:a:0? -vf scale=-2:1080 -c:v libx264"),
            "{a}"
        );
        assert!(a.contains("-c:a aac -b:a 128000"), "{a}");
        let v = video_args(clip, 1.5, (960, 720), Some(1)).join(" ");
        assert!(
            v.contains("-ss 1.500 -i in.mp4 -an -sn -frames:v 1 -vf scale=960:720"),
            "{v}"
        );
        let au = audio_args(clip, 0.0, 1, 44_100, 2).join(" ");
        assert!(
            au.contains("-i in.mp4 -vn -sn -map 0:a:1 -ac 2 -ar 44100 -f f32le pipe:1"),
            "{au}"
        );
        assert!(!au.contains("-ss"));
    }

    #[test]
    fn progress_and_times() {
        assert_eq!(progress_secs("out_time_us=1500000"), Some(1.5));
        assert_eq!(progress_secs("out_time_ms=2000000"), Some(2.0));
        assert_eq!(progress_secs("out_time_us=N/A"), None);
        assert_eq!(progress_secs("frame=12"), None);
        assert_eq!(format_time(7.25), "0:07.25");
        assert_eq!(format_time(125.5), "2:05.50");
    }

    #[test]
    fn montage_joins_clips_with_fades_and_fills_missing_sound() {
        let pieces = [
            Piece {
                path: "a.mp4".into(),
                duration: 30.0,
                has_audio: true,
            },
            Piece {
                path: "b.mp4".into(),
                duration: 12.5,
                has_audio: false,
            },
            Piece {
                path: "c.mp4".into(),
                duration: 8.0,
                has_audio: true,
            },
        ];
        let a = montage_args(&pieces, (1920, 1080), 60, Encoder::X264, Path::new("m.mp4"));
        let j = a.join(" ");
        assert!(j.contains("-i a.mp4 -i b.mp4 -i c.mp4"), "{j}");
        let graph = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert!(graph.contains("[v0][a0][v1][a1][v2][a2]concat=n=3:v=1:a=1[v][a]"));
        // The clip without sound gets silence of its length; the others are padded to theirs.
        assert!(graph.contains("anullsrc=r=48000:cl=stereo,atrim=0:12.500"));
        assert!(graph.contains(
            "[0:a:0]aformat=sample_rates=48000:channel_layouts=stereo,apad=whole_dur=30.000"
        ));
        assert!(!graph.contains("[1:a:0]"));
        // Each fades out to black just before its end.
        assert!(graph.contains("fade=t=out:st=29.700:d=0.3[v0]"));
        assert!(
            graph.contains("scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080")
        );
        assert!(j.contains("-map [v] -map [a] -c:v libx264"));
        assert!(j.ends_with("-movflags +faststart -f mp4 m.mp4"));
        assert_eq!(montage_length(&pieces), 50.5);
    }

    fn three_tracks() -> Info {
        Info {
            duration: 30.0,
            width: 1920,
            height: 1080,
            fps: 60.0,
            video_bps: Some(20_000_000),
            audio_tracks: vec!["Everything".into(), "Game".into(), "Microphone".into()],
            video_codec: "h264".into(),
        }
    }

    fn graph(a: &[String]) -> &str {
        &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1]
    }

    #[test]
    fn plain_trims_keep_every_track() {
        let a = trim_args(
            Path::new("c.mp4"),
            2.0,
            7.0,
            &three_tracks(),
            &Effects::default(),
            Encoder::X264,
            Path::new("o.mp4"),
        );
        assert!(a.join(" ").contains("-map 0:v:0 -map 0:a?"));
        assert!(!a.contains(&s("-filter_complex")));
    }

    #[test]
    fn effects_change_speed_look_fades_and_the_mix() {
        let eff = Effects {
            speed: 0.25,
            look: Look::Cinematic,
            fade: true,
            game_volume: 0.6,
            mic_volume: 1.4,
            chat_volume: 1.0,
            mic_cleanup: true,
            even_loudness: true,
            music: Some(Music {
                path: "song.mp3".into(),
                volume: 0.3,
            }),
            burns: Vec::new(),
            keys: None,
        };
        let a = trim_args(
            Path::new("c.mp4"),
            2.0,
            7.0,
            &three_tracks(),
            &eff,
            Encoder::X264,
            Path::new("o.mp4"),
        );
        let j = a.join(" ");
        assert!(j.contains("-i c.mp4 -stream_loop -1 -i song.mp3"), "{j}");
        let g = graph(&a);
        // 5 s at quarter speed: 20 s out, faded out from 19.5 s.
        assert!(g.starts_with("[0:v]setpts=PTS/0.2500,fps=60,eq="), "{g}");
        assert!(g.contains("fade=t=out:st=19.500:d=0.5"), "{g}");
        assert!(
            g.contains("[0:a:1]volume=0.60[t0];[0:a:2]afftdn=nr=12,highpass=f=90,volume=1.40[t1];[t0][t1]amix=inputs=2"),
            "{g}"
        );
        assert!(g.contains("atempo=0.5,atempo=0.5"), "{g}");
        assert!(
            g.contains(
                "[1:a]aformat=sample_rates=48000:channel_layouts=stereo,volume=0.30,atrim=0:20.000"
            ),
            "{g}"
        );
        assert!(
            g.contains("loudnorm=I=-16:TP=-1.5:LRA=11,aresample=48000[a]"),
            "{g}"
        );
        assert!(j.contains("-map [v] -map [a]"));
    }

    #[test]
    fn speeds_split_into_tempo_steps() {
        assert!(tempo_chain(1.0).is_empty());
        assert_eq!(tempo_chain(4.0), ["atempo=2.0", "atempo=2.0"]);
        assert_eq!(tempo_chain(1.5), ["atempo=1.5000"]);
        assert_eq!(tempo_chain(0.25), ["atempo=0.5", "atempo=0.5"]);
        assert_eq!(Effects::default().at_speed(2.0).length(0.0, 10.0), 5.0);
    }

    #[test]
    fn framed_and_gif_exports_take_effects() {
        let eff = Effects {
            look: Look::Mono,
            ..Default::default()
        };
        let sq = framed_args(
            Path::new("c.mp4"),
            0.0,
            4.0,
            &three_tracks(),
            &eff,
            (1080, 1080),
            Encoder::X264,
            Path::new("o.mp4"),
        );
        let g = graph(&sq);
        assert!(g.contains("[0:v]fps=60,hue=s=0"), "{g}");
        assert!(
            g.contains("scale=1080:1080:force_original_aspect_ratio=decrease"),
            "{g}"
        );
        assert!(g.contains("[0:a:0]volume=1.00"), "{g}");
        let gif = gif_args(
            Path::new("c.mp4"),
            0.0,
            4.0,
            &three_tracks(),
            &eff,
            480,
            15,
            Path::new("o.gif"),
        );
        assert!(graph(&gif).contains("[0:v]fps=15,hue=s=0"));
        assert!(!graph(&gif).contains("[a]"));
        // Silent clips get silence to carry music or fades.
        let quiet = Info {
            audio_tracks: Vec::new(),
            ..three_tracks()
        };
        let a = trim_args(
            Path::new("c.mp4"),
            0.0,
            3.0,
            &quiet,
            &Effects {
                fade: true,
                ..Default::default()
            },
            Encoder::X264,
            Path::new("o.mp4"),
        );
        assert!(graph(&a).contains("anullsrc=r=48000:cl=stereo,atrim=0:3.000"));
    }

    #[test]
    fn captions_are_laid_over_the_finished_picture() {
        let eff = Effects {
            music: Some(Music {
                path: "m.mp3".into(),
                volume: 0.5,
            }),
            burns: vec![
                Burn {
                    path: "c0.rgba".into(),
                    width: 400,
                    height: 80,
                    start: 0.5,
                    end: 1.5,
                },
                Burn {
                    path: "c1.rgba".into(),
                    width: 300,
                    height: 80,
                    start: 2.0,
                    end: 3.0,
                },
            ],
            ..Default::default()
        };
        let a = framed_args(
            Path::new("c.mp4"),
            0.0,
            4.0,
            &three_tracks(),
            &eff,
            (1080, 1920),
            Encoder::X264,
            Path::new("o.mp4"),
        );
        let j = a.join(" ");
        assert!(
            j.contains(
                "-i m.mp3 -f rawvideo -pix_fmt rgba -video_size 400x80 -i c0.rgba -f rawvideo"
            ),
            "{j}"
        );
        let g = graph(&a);
        assert!(g.contains("format=yuv420p[sv];[sv][2:v]overlay=x=(W-w)/2:y=H-h-H/14:enable='between(t,0.500,1.500)'[c0];[c0][3:v]overlay"), "{g}");
        assert!(g.contains("between(t,2.000,3.000)'[v]"), "{g}");
        assert_eq!(burn_window(5.0, 7.0, (4.0, 10.0), 2.0), Some((0.5, 1.5)));
        assert_eq!(burn_window(1.0, 3.0, (4.0, 10.0), 1.0), None);
        assert_eq!(burn_window(3.5, 4.5, (4.0, 10.0), 1.0), Some((0.0, 0.5)));
    }

    #[test]
    fn the_keys_overlay_goes_in_the_corner_after_captions() {
        let eff = Effects {
            burns: vec![Burn {
                path: "c0.rgba".into(),
                width: 400,
                height: 80,
                start: 0.5,
                end: 1.5,
            }],
            keys: Some(OverlayVideo {
                path: "k.rgba".into(),
                width: 344,
                height: 172,
                fps: 15,
            }),
            ..Default::default()
        };
        let a = trim_args(
            Path::new("c.mp4"),
            0.0,
            4.0,
            &three_tracks(),
            &eff,
            Encoder::X264,
            Path::new("o.mp4"),
        );
        let j = a.join(" ");
        assert!(
            j.contains("-video_size 344x172 -framerate 15 -i k.rgba"),
            "{j}"
        );
        let g = graph(&a);
        assert!(g.contains("[sv][1:v]overlay=x=(W-w)/2"), "{g}");
        assert!(
            g.contains("[c0][2:v]overlay=x=W*3/100:y=H-h-H*5/100:eof_action=pass[v]"),
            "{g}"
        );
    }
}
