#!/usr/bin/env bash
# Builds the small ffmpeg that ships with Rewindr: only the screen capture, encoders, filters and
# formats Rewindr uses, linked so it runs without anything else installed.
#
#   packaging/ffmpeg/build.sh <out-dir>
#
# Runs on Windows (MSYS2 UCRT64), macOS (ARCH=arm64 or x86_64) and Linux. Needs a C compiler,
# make, nasm, git, curl and pkg-config; on Linux also the libxcb development headers; on Windows
# the AMF headers (MSYS2 package mingw-w64-ucrt-x86_64-amf-headers).
#
# ffmpeg is GPL here (it includes x264). Its source is the release tarball below; this script is
# the whole recipe.

set -euo pipefail
# macOS ships bash 3.2: empty arrays are expanded as ${a[@]+"${a[@]}"} below.

FFMPEG_VERSION=8.1.3
X264_REF=stable
# NVENC API 12.1 works with NVIDIA drivers from 531 on; newer headers would need newer drivers.
NV_HEADERS_REF=n12.1.14.0

out=$(mkdir -p "$1" && cd "$1" && pwd)
work=${WORK:-$PWD/ffbuild}
prefix=$work/prefix
mkdir -p "$work" "$prefix"
jobs=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)

case "$(uname -s)" in
  MINGW* | MSYS*) os=windows ;;
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *) echo "unsupported system $(uname -s)" >&2; exit 1 ;;
esac
arch=${ARCH:-$(uname -m)}
echo "Building ffmpeg $FFMPEG_VERSION for $os/$arch"

export PKG_CONFIG_PATH=$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}

cflags=""
ldflags=""
cross=()
x264_flags=()
if [ "$os" = macos ]; then
  export MACOSX_DEPLOYMENT_TARGET=11.0
  cflags="-arch $arch -mmacosx-version-min=11.0"
  ldflags="-arch $arch -mmacosx-version-min=11.0"
  if [ "$arch" != "$(uname -m)" ]; then
    cross=(--enable-cross-compile --arch="$arch" --target-os=darwin --cc="clang -arch $arch" --cxx="clang++ -arch $arch")
    x264_flags=(--host="$arch-apple-darwin")
  fi
fi

# x264, static.
if [ ! -f "$prefix/lib/libx264.a" ]; then
  rm -rf "$work/x264"
  git clone --depth 1 --branch "$X264_REF" https://code.videolan.org/videolan/x264.git "$work/x264"
  (
    cd "$work/x264"
    ./configure --prefix="$prefix" --enable-static --disable-cli --disable-opencl --enable-pic \
      --extra-cflags="$cflags" --extra-ldflags="$ldflags" ${x264_flags[@]+"${x264_flags[@]}"}
    make -j"$jobs"
    make install
  )
fi

# NVIDIA's codec headers (NVENC is loaded at run time; nothing NVIDIA is linked in).
if [ "$os" != macos ] && [ ! -f "$prefix/lib/pkgconfig/ffnvcodec.pc" ]; then
  rm -rf "$work/nv-codec-headers"
  git clone --depth 1 --branch "$NV_HEADERS_REF" https://github.com/FFmpeg/nv-codec-headers.git "$work/nv-codec-headers"
  make -C "$work/nv-codec-headers" install PREFIX="$prefix"
fi

src=$work/ffmpeg-$FFMPEG_VERSION
if [ ! -d "$src" ]; then
  curl -fsSL "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz" -o "$work/ffmpeg.tar.xz"
  tar -xf "$work/ffmpeg.tar.xz" -C "$work"
fi

# Component names are configure's, which differ from some run-time names: dump_extradata is
# the dump_extra filter, pcm_f32le the f32le muxer. Unknown names are ignored silently.
common=(
  --prefix="$prefix"
  --disable-everything --disable-autodetect --disable-doc --disable-debug --disable-network
  --disable-ffplay --disable-ffprobe
  --enable-gpl --enable-libx264
  --enable-protocol=file,pipe
  --enable-indev=lavfi
  --enable-demuxer=mov,mpegts,wav,matroska,image2,mp3,ogg,flac,aac,rawvideo
  --enable-muxer=mp4,mov,mpegts,rawvideo,pcm_f32le,pcm_s16le,wav,image2,gif,null,flac
  --enable-decoder=h264,hevc,aac,pcm_f32le,pcm_s16le,pcm_s32le,pcm_s24le,rawvideo,bmp,mjpeg,png,wrapped_avframe,mp3,mp3float,flac,vorbis,opus,alac
  --enable-encoder=libx264,aac,mjpeg,gif,rawvideo,pcm_f32le,pcm_s16le,wrapped_avframe,flac
  --enable-parser=h264,hevc,aac,mjpeg,mpegaudio,flac,vorbis,opus
  --enable-bsf=dump_extradata,h264_mp4toannexb,hevc_mp4toannexb,aac_adtstoasc,extract_extradata
  --enable-filter=fps,scale,format,null,anull,copy,crop,pad,setpts,asetpts,trim,atrim,select,aselect,asplit,amix,aresample,aformat,testsrc2,sine,showinfo,hwdownload,hwupload,hwmap,split,palettegen,paletteuse,atempo,overlay,boxblur,concat,xfade,fade,afade,acrossfade,anullsrc,setsar,settb,volume,apad,tpad,reverse,areverse,eq,hue,vignette,unsharp,curves,zoompan,gblur,drawbox,afftdn,loudnorm,dynaudnorm,highpass,lowpass,alimiter,pan,amerge
  --pkg-config-flags=--static
  --extra-cflags="-I$prefix/include $cflags"
  --extra-ldflags="-L$prefix/lib $ldflags"
)

case "$os" in
  windows)
    platform=(
      --enable-w32threads
      --enable-d3d11va --enable-dxva2 --enable-mediafoundation --enable-amf
      --enable-ffnvcodec --enable-nvenc
      --enable-encoder=h264_nvenc,h264_amf,h264_mf,hevc_nvenc,hevc_amf,hevc_mf
      --enable-filter=gfxcapture,ddagrab,scale_d3d11
      --enable-indev=gdigrab,dshow
      --enable-hwaccel=h264_d3d11va,h264_d3d11va2,h264_dxva2,hevc_d3d11va,hevc_d3d11va2,hevc_dxva2
      --extra-ldflags=-static
    )
    ;;
  macos)
    platform=(
      --enable-pthreads
      --enable-avfoundation --enable-videotoolbox
      --enable-encoder=h264_videotoolbox,hevc_videotoolbox
      --enable-indev=avfoundation
      --enable-hwaccel=h264_videotoolbox,hevc_videotoolbox
    )
    ;;
  linux)
    platform=(
      --enable-pthreads
      --enable-libxcb --enable-libxcb-shm --enable-libxcb-xfixes
      --enable-indev=xcbgrab,v4l2
      --enable-ffnvcodec --enable-nvenc
      --enable-encoder=h264_nvenc,hevc_nvenc
    )
    ;;
esac

(
  cd "$src"
  ./configure "${common[@]}" "${platform[@]}" ${cross[@]+"${cross[@]}"} || { tail -n 60 ffbuild/config.log; exit 1; }
  make -j"$jobs"
)

exe=ffmpeg
[ "$os" = windows ] && exe=ffmpeg.exe
cp "$src/$exe" "$out/$exe"
strip "$out/$exe" 2>/dev/null || true
cp "$src/COPYING.GPLv2" "$out/FFMPEG-LICENSE.txt"
cat > "$out/FFMPEG-SOURCE.txt" <<EOF
The ffmpeg program shipped with Rewindr is FFmpeg $FFMPEG_VERSION, built with x264 ($X264_REF)
and licensed under the GNU GPL version 2 or later (see FFMPEG-LICENSE.txt).

Source code: https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz
x264 source: https://code.videolan.org/videolan/x264/-/tree/$X264_REF
Build recipe: packaging/ffmpeg/build.sh in https://github.com/fsiuhgshiqoih32/rewindr
EOF
"$out/$exe" -hide_banner -version | head -n 1 || true
ls -la "$out"
