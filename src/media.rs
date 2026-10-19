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

/// Small RGBA pictures of every keyframe (one a second in Rewindr's clips), for the timeline.
pub fn filmstrip_args(clip: &Path, size: (u32, u32)) -> Vec<String> {
    let mut a = head();
    a.extend([
        s("-skip_frame"),
        s("nokey"),
        s("-i"),
        clip.display().to_string(),
        s("-an"),
        s("-sn"),
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

/// A frame-exact copy of `start..end` of the clip, re-encoded at about the original quality,
/// keeping every sound track. Progress goes to stdout (`-progress`).
pub fn trim_args(
    clip: &Path,
    start: f64,
    end: f64,
    info: &Info,
    encoder: Encoder,
    out: &Path,
) -> Vec<String> {
    let encoder = export_encoder(encoder);
    let fps = info.fps.round().max(1.0) as u32;
    let bitrate = info.video_bps.unwrap_or_else(|| {
        crate::config::Quality::High.bitrate(info.width as u64 * info.height as u64, fps)
    });
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
pub fn gif_args(
    clip: &Path,
    start: f64,
    end: f64,
    width: u32,
    fps: u32,
    out: &Path,
) -> Vec<String> {
    let mut a = export_head(clip, start, end);
    a.extend([
        s("-an"),
        s("-vf"),
        format!(
            "fps={fps},scale={width}:-2:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];\
             [b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle"
        ),
        s("-loop"),
        s("0"),
        s("-f"),
        s("gif"),
        out.display().to_string(),
    ]);
    a
}

/// Vertical 1080x1920 for TikTok, Shorts and Reels: the whole picture in the middle, over a
/// blurred, zoomed copy of itself.
pub fn vertical_args(
    clip: &Path,
    start: f64,
    end: f64,
    info: &Info,
    encoder: Encoder,
    out: &Path,
) -> Vec<String> {
    let encoder = export_encoder(encoder);
    let fps = (info.fps.round() as u32).clamp(1, 60);
    let bitrate = crate::config::Quality::High
        .bitrate(1080 * 1920, fps)
        .min(16_000_000);
    let mut a = export_head(clip, start, end);
    a.extend([
        s("-filter_complex"),
        format!(
            "[0:v]fps={fps},split[bg][fg];\
             [bg]scale=270:480:force_original_aspect_ratio=increase,crop=270:480,boxblur=10:2,scale=1080:1920[b];\
             [fg]scale=1080:-2[f];[b][f]overlay=(W-w)/2:(H-h)/2,format=yuv420p[v]"
        ),
        s("-map"),
        s("[v]"),
        s("-map"),
        s("0:a:0?"),
        s("-c:v"),
        s(encoder.ffmpeg_name()),
    ]);
    a.extend(export_video_opts(encoder, bitrate, fps, false));
    a.extend(mp4_tail(out));
    a
}

/// Slow motion: `start..end` at 1/`factor` speed (2 or 4), the sound slowed to match without
/// changing its pitch.
pub fn slow_args(
    clip: &Path,
    start: f64,
    end: f64,
    factor: u32,
    info: &Info,
    encoder: Encoder,
    out: &Path,
) -> Vec<String> {
    let encoder = export_encoder(encoder);
    let factor = factor.clamp(2, 4);
    let fps = info.fps.round().max(1.0) as u32;
    let bitrate = info.video_bps.unwrap_or_else(|| {
        crate::config::Quality::High.bitrate(info.width as u64 * info.height as u64, fps)
    });
    let tempo = vec!["atempo=0.5"; if factor >= 4 { 2 } else { 1 }].join(",");
    let mut a = export_head(clip, start, end);
    a.extend([
        s("-map"),
        s("0:v:0"),
        s("-map"),
        s("0:a:0?"),
        s("-vf"),
        format!("setpts={factor}.0*PTS"),
        s("-af"),
        tempo,
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

/// Encodes `start..end` of the clip by `plan`, sound track 1 only (what Discord plays).
pub fn share_args(
    clip: &Path,
    start: f64,
    end: f64,
    plan: &SharePlan,
    encoder: Encoder,
    out: &Path,
) -> Vec<String> {
    let encoder = export_encoder(encoder);
    let fps = plan.fps.unwrap_or(60);
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
            Encoder::Nvenc,
            Path::new("out.mp4"),
        )
        .join(" ");
        assert!(a.contains("-progress pipe:1 -ss 3.250 -i in.mp4 -t 16.750 -map 0:v:0 -map 0:a? -c:v h264_nvenc"), "{a}");
        assert!(a.contains("-b:v 16660000"), "{a}");
        assert!(a.ends_with("-f mp4 out.mp4"));
        let plan = share_plan(10_000_000, 16.75, &info, 1.0).unwrap();
        let a = share_args(clip, 3.25, 20.0, &plan, Encoder::Vaapi, Path::new("d.mp4")).join(" ");
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
}
