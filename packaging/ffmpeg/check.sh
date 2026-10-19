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

step "screenshot: one frame of the stream as JPEG"
"$ff" "${q[@]}" -i "$dir/video.ts" -vf "select=eq(n\,80)" -frames:v 1 -q:v 2 -update 1 "$dir/shot.jpg"
test -s "$dir/shot.jpg"

step "encoder probe: null output"
"$ff" "${q[@]}" -f lavfi -i testsrc2=size=320x240:rate=30 -frames:v 5 -c:v libx264 -f null -

echo "All checks passed."
