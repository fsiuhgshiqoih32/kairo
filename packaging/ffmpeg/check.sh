#!/usr/bin/env bash
# Runs the kinds of ffmpeg command Rewindr uses against a build, so a component left out of
# build.sh fails here rather than on a user's machine.
#
#   packaging/ffmpeg/check.sh <path to ffmpeg>

set -euo pipefail
ff=$1
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
q=(-hide_banner -nostdin -loglevel error -y)
step() { echo "== $*"; }

step "record: lavfi source, fps filter, x264, keyframe headers, MPEG-TS with wall-clock time"
"$ff" "${q[@]}" -use_wallclock_as_timestamps 1 -f lavfi -i testsrc2=size=320x240:rate=30 \
  -vf fps=30 -fps_mode passthrough -an -frames:v 90 -c:v libx264 -g 30 -keyint_min 30 -sc_threshold 0 -bf 0 \
  -bsf:v dump_extra=freq=keyframe -copyts -muxdelay 0 -muxpreload 0 -f mpegts "$dir/video.ts"

step "sound: float WAV"
"$ff" "${q[@]}" -f lavfi -i sine=frequency=440:duration=3 -ac 2 -c:a pcm_f32le -f wav "$dir/game.wav"
"$ff" "${q[@]}" -f lavfi -i sine=frequency=660:duration=3 -ac 1 -c:a pcm_f32le -f wav "$dir/mic.wav"

step "save: copy video, mix and keep both tracks, AAC, faststart MP4"
"$ff" "${q[@]}" -i "$dir/video.ts" -i "$dir/game.wav" -i "$dir/mic.wav" -map 0:v:0 \
  -filter_complex "[1:a][2:a]amix=inputs=2:duration=longest:normalize=0[mix]" -map "[mix]" \
  -map 1:a:0 -map 2:a:0 -c:v copy -c:a aac -b:a 160k -metadata:s:a:0 title=Everything \
  -movflags +faststart -f mp4 "$dir/clip.mp4"

step "thumbnail: seek from the end, JPEG"
"$ff" "${q[@]}" -sseof -1 -i "$dir/clip.mp4" -frames:v 1 -vf scale=160:-2 -update 1 "$dir/thumb.jpg"
test -s "$dir/thumb.jpg"

step "editor: filmstrip, frames and sound as raw data"
"$ff" "${q[@]}" -skip_frame nokey -i "$dir/clip.mp4" -an -vf scale=64:48,format=rgba \
  -fps_mode passthrough -f rawvideo -pix_fmt rgba "$dir/strip.raw"
"$ff" "${q[@]}" -hwaccel auto -ss 1 -i "$dir/clip.mp4" -an -frames:v 10 -vf scale=160:120,format=rgba \
  -f rawvideo -pix_fmt rgba "$dir/frames.raw"
"$ff" "${q[@]}" -ss 1 -i "$dir/clip.mp4" -vn -map 0:a:0 -ac 2 -ar 48000 -f f32le "$dir/sound.raw"
test "$(wc -c < "$dir/strip.raw")" -eq $((64 * 48 * 4 * 3))
test "$(wc -c < "$dir/frames.raw")" -eq $((160 * 120 * 4 * 10))
test -s "$dir/sound.raw"

step "export: exact trim and a size-limited copy"
"$ff" "${q[@]}" -ss 0.5 -i "$dir/clip.mp4" -t 1.5 -map 0:v:0 -map "0:a?" -c:v libx264 -b:v 500k \
  -c:a aac -movflags +faststart -f mp4 "$dir/trim.mp4"
"$ff" "${q[@]}" -i "$dir/clip.mp4" -map 0:v:0 -map "0:a:0?" -vf fps=15,scale=-2:120 -c:v libx264 \
  -b:v 200k -maxrate 200k -bufsize 200k -c:a aac -b:a 64000 -f mp4 "$dir/share.mp4"

step "more formats: GIF with a palette, vertical with a blurred background, slow motion"
"$ff" "${q[@]}" -i "$dir/clip.mp4" -an   -vf "fps=10,scale=80:-2:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle"   -loop 0 -f gif "$dir/clip.gif"
test -s "$dir/clip.gif"
"$ff" "${q[@]}" -i "$dir/clip.mp4"   -filter_complex "[0:v]split[bg][fg];[bg]scale=36:64:force_original_aspect_ratio=increase,crop=36:64,boxblur=4:1,scale=144:256[b];[fg]scale=144:-2[f];[b][f]overlay=(W-w)/2:(H-h)/2,format=yuv420p[v]"   -map "[v]" -map 0:a:0 -c:v libx264 -b:v 300k -c:a aac -f mp4 "$dir/vertical.mp4"
"$ff" "${q[@]}" -i "$dir/clip.mp4" -map 0:v:0 -map 0:a:0 -vf "setpts=2.0*PTS" -af atempo=0.5   -c:v libx264 -b:v 300k -c:a aac -f mp4 "$dir/slow.mp4"

step "montage: clips crossfaded into one, one without sound, faded in and out"
"$ff" "${q[@]}" -i "$dir/clip.mp4" -an -c:v copy -f mp4 "$dir/quiet.mp4"
fit="scale=160:120:force_original_aspect_ratio=decrease,pad=160:120:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,format=yuv420p,settb=AVTB"
"$ff" "${q[@]}" -i "$dir/clip.mp4" -i "$dir/trim.mp4" -i "$dir/quiet.mp4" -filter_complex   "[0:v]$fit[v0];[1:v]$fit[v1];[2:v]$fit[v2];[0:a:0]aformat=sample_rates=48000:channel_layouts=stereo[a0];[1:a:0]aformat=sample_rates=48000:channel_layouts=stereo[a1];anullsrc=r=48000:cl=stereo,atrim=duration=3[a2];[v0][v1]xfade=transition=fade:duration=0.5:offset=2.5[x1];[x1][v2]xfade=transition=fade:duration=0.5:offset=3.5[x2];[a0][a1]acrossfade=d=0.5[y1];[y1][a2]acrossfade=d=0.5[y2];[x2]fade=t=in:st=0:d=0.4,fade=t=out:st=6:d=0.5[v];[y2]afade=t=in:st=0:d=0.4,afade=t=out:st=6:d=0.5,volume=1.0[a]"   -map "[v]" -map "[a]" -c:v libx264 -b:v 300k -c:a aac -movflags +faststart -f mp4 "$dir/montage.mp4"
test -s "$dir/montage.mp4"
"$ff" "${q[@]}" -i "$dir/clip.mp4" -i "$dir/trim.mp4" -filter_complex   "[0:v]$fit[v0];[1:v]$fit[v1];[0:a:0]apad=whole_dur=3[a0];[1:a:0]anull[a1];[v0][a0][v1][a1]concat=n=2:v=1:a=1[v][a]"   -map "[v]" -map "[a]" -c:v libx264 -b:v 300k -c:a aac -f mp4 "$dir/joined.mp4"
test -s "$dir/joined.mp4"

step "effects: speed, freeze frame, colour, zoom, reverse, music mixed under the clip, mic clean-up"
"$ff" "${q[@]}" -f lavfi -i sine=frequency=330:duration=4 -ac 2 -c:a flac -f flac "$dir/music.flac"
"$ff" "${q[@]}" -i "$dir/clip.mp4" -i "$dir/music.flac" -filter_complex "[0:v]setpts=PTS/1.5,eq=contrast=1.08:saturation=1.25,vignette,unsharp,tpad=stop_mode=clone:stop_duration=0.5,zoompan=z=1.1:d=1:s=320x240:fps=30[v];[0:a:0]atempo=1.5,afftdn,highpass=f=80,apad=whole_dur=2.5[g];[1:a]volume=0.4,afade=t=out:st=2:d=0.5[m];[g][m]amix=inputs=2:duration=first:normalize=0,loudnorm=I=-14:TP=-1.5,alimiter[a]" -map "[v]" -map "[a]" -c:v libx264 -b:v 300k -c:a aac -f mp4 "$dir/effects.mp4"
test -s "$dir/effects.mp4"
"$ff" "${q[@]}" -i "$dir/clip.mp4" -t 1 -vf "reverse,hue=s=0,curves=preset=vintage,gblur=sigma=2,drawbox=x=10:y=10:w=50:h=20:color=white@0.5:t=fill" -af "areverse,dynaudnorm,lowpass=f=8000" -map 0:v:0 -map 0:a:0 -c:v libx264 -b:v 300k -c:a aac -f mp4 "$dir/reverse.mp4"
test -s "$dir/reverse.mp4"

step "captions: raw RGBA pictures laid over the picture for their time"
"$ff" "${q[@]}" -f lavfi -i testsrc2=size=120x40:rate=1 -frames:v 1 -f rawvideo -pix_fmt rgba "$dir/cap.rgba"
test "$(wc -c < "$dir/cap.rgba")" -eq $((120 * 40 * 4))
"$ff" "${q[@]}" -i "$dir/clip.mp4" -f rawvideo -pix_fmt rgba -video_size 120x40 -i "$dir/cap.rgba" -filter_complex "[0:v][1:v]overlay=x=(W-w)/2:y=H-h-H/14:enable='between(t,0.5,1.5)'[v]" -map "[v]" -map 0:a:0 -c:v libx264 -b:v 300k -c:a aac -f mp4 "$dir/captioned.mp4"
test -s "$dir/captioned.mp4"

step "editor cuts: parts of a clip joined back to back, every sound track kept exact"
"$ff" "${q[@]}" -ss 0.2 -t 2.4 -i "$dir/clip.mp4" -filter_complex   "[0:v]split=2[v0][v1];[0:a:0]asplit=2[a0_0][a0_1];[v0]trim=start=0:end=0.8,setpts=PTS-STARTPTS[pv0];[a0_0]atrim=start=0:end=0.8,asetpts=PTS-STARTPTS[pa0_0];[v1]trim=start=1.6:end=2.4,setpts=PTS-STARTPTS[pv1];[a0_1]atrim=start=1.6:end=2.4,asetpts=PTS-STARTPTS[pa0_1];[pv0][pa0_0][pv1][pa0_1]concat=n=2:v=1:a=1[v][a0]"   -map "[v]" -map "[a0]" -c:v libx264 -b:v 300k -metadata:s:a:0 title=Game -metadata:s:a:0 handler_name=Game -c:a pcm_s16le -f mov "$dir/cut.mov"
# Reading a file with no output "fails", so take the text first, then look in it.
probe=$("$ff" -hide_banner -i "$dir/cut.mov" 2>&1 || true)
grep -q "handler_name *: Game" <<<"$probe" || { echo "track name lost"; exit 1; }
"$ff" "${q[@]}" -ss 0.2 -i "$dir/clip.mp4" -t 1 -map 0:v:0 -map "0:a?" -c copy -avoid_negative_ts make_zero   -movflags +faststart -f mp4 "$dir/copy.mp4"
test -s "$dir/copy.mp4"

step "text: frames of an animation from their moment, laid at a place, then the still"
"$ff" "${q[@]}" -f lavfi -i testsrc2=size=60x20:rate=30 -frames:v 6 -f rawvideo -pix_fmt rgba "$dir/in.rgba"
"$ff" "${q[@]}" -i "$dir/clip.mp4" -f rawvideo -pix_fmt rgba -video_size 60x20 -framerate 30 -itsoffset 0.5   -i "$dir/in.rgba" -f rawvideo -pix_fmt rgba -video_size 120x40 -i "$dir/cap.rgba" -filter_complex   "[0:v][1:v]overlay=x=40:y=30:eof_action=pass:enable='between(t,0.5,0.7)'[c0];[c0][2:v]overlay=x=30:y=20:enable='between(t,0.7,2)'[v]"   -map "[v]" -map 0:a:0 -c:v libx264 -b:v 300k -c:a aac -f mp4 "$dir/text.mp4"
test -s "$dir/text.mp4"

step "webcam: laid in a corner of a saved clip, starting a little late"
"$ff" "${q[@]}" -use_wallclock_as_timestamps 1 -f lavfi -i testsrc2=size=64x36:rate=30 -frames:v 60   -vf fps=30,scale=-2:36,format=yuv420p -c:v libx264 -g 30 -bf 0 -copyts -f mpegts "$dir/cam.ts"
"$ff" "${q[@]}" -i "$dir/video.ts" -itsoffset 0.25 -i "$dir/cam.ts" -i "$dir/game.wav"   -filter_complex "[1:v]scale=80:-2[cam];[0:v][cam]overlay=x=W-w-W*2/100:y=H-h-W*2/100:eof_action=pass[v]"   -map "[v]" -map 2:a:0 -c:v libx264 -b:v 300k -c:a aac -movflags +faststart -f mp4 "$dir/webcam.mp4"
test -s "$dir/webcam.mp4"
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) cam=dshow ;;
  Darwin) cam=avfoundation ;;
  *) cam=v4l2 ;;
esac
devices=$("$ff" -hide_banner -devices 2>/dev/null || true)
grep -qw "$cam" <<<"$devices" || { echo "no $cam camera input"; exit 1; }

step "screenshot: one frame of the stream as JPEG"
"$ff" "${q[@]}" -i "$dir/video.ts" -vf "select=eq(n\,80)" -frames:v 1 -q:v 2 -update 1 "$dir/shot.jpg"
test -s "$dir/shot.jpg"

step "encoder probe: null output"
"$ff" "${q[@]}" -f lavfi -i testsrc2=size=320x240:rate=30 -frames:v 5 -c:v libx264 -f null -

echo "All checks passed."
