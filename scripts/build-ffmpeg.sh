#!/usr/bin/env bash
#
# build-ffmpeg.sh [prefix]
#
# Builds the static ffmpeg the `portable` release links against, trimmed to the
# formats kirie can actually open.
#
# `ffmpeg-next/build` builds ffmpeg itself, but `ffmpeg-sys-next` hardcodes its
# `./configure` line and offers no hook to pass flags — so it ships *everything*
# ffmpeg has: ~500 decoders, every muxer, every network protocol. That is around
# 19-21 MB of a 46 MB release binary, for a program that opens six container
# types from local disk.
#
# Building it here instead and pointing the crate at the result via `FFMPEG_DIR`
# (with `ffmpeg-next/static` rather than `/build`) keeps the same static linkage
# with only the pieces kirie reaches.
#
# The allow-list below is derived from what kirie actually opens:
#   * containers  — `VIDEO_EXTS` in crates/kirie/src/compat/resolve.rs
#   * video texs  — .tex files carrying an mp4 (docs/format-tex.md §7.3)
#   * audio       — the tracks those containers carry, decoded for reactivity
# Anything outside it (ProRes, DNxHD, WMV/ASF, FLV, Theora, RealMedia, image
# sequences, every network protocol) is unreachable from kirie's own code.

set -euo pipefail

VERSION="${FFMPEG_VERSION:-7.1.1}"
PREFIX="${1:-$HOME/.cache/kirie/ffmpeg-static}"
BUILD_DIR="${FFMPEG_BUILD_DIR:-/var/tmp/kirie-ffmpeg-build}"
# nproc is GNU; macOS answers with sysctl.
JOBS="${JOBS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"

log() { printf 'build-ffmpeg: %s\n' "$*"; }

# Already built for this version? The marker records what the prefix holds, so
# a version bump rebuilds and a repeat run does not.
marker="$PREFIX/.kirie-ffmpeg-version"
if [ "${FORCE:-}" != 1 ] && [ -f "$marker" ] && [ "$(cat "$marker")" = "$VERSION" ]; then
    log "already built at $PREFIX (ffmpeg $VERSION)"
    exit 0
fi

for tool in nasm pkg-config make cc; do
    command -v "$tool" >/dev/null || { echo "build-ffmpeg: $tool is required" >&2; exit 1; }
done

mkdir -p "$BUILD_DIR"
cd "$BUILD_DIR"

src="ffmpeg-$VERSION"
if [ ! -d "$src" ]; then
    log "fetching ffmpeg $VERSION"
    curl -fL --retry 3 -o "$src.tar.xz" "https://ffmpeg.org/releases/ffmpeg-$VERSION.tar.xz"
    tar xf "$src.tar.xz"
    rm -f "$src.tar.xz"
fi
cd "$src"

log "configuring (trimmed)"
./configure \
    --prefix="$PREFIX" \
    --enable-static --disable-shared --enable-pic \
    --disable-everything \
    --disable-autodetect --disable-programs --disable-doc \
    --disable-network --disable-avdevice --disable-avfilter --disable-postproc \
    --disable-encoders --disable-muxers --disable-devices --disable-filters \
    --disable-bsfs --disable-debug --enable-stripping \
    --enable-avcodec --enable-avformat --enable-swscale --enable-swresample \
    --enable-pthreads \
    --enable-demuxer=mov,matroska,avi \
    --enable-decoder=h264,hevc,vp8,vp9,av1,mpeg4,mjpeg,png \
    --enable-decoder=aac,mp3,vorbis,opus,ac3,eac3,flac \
    --enable-decoder=pcm_s16le,pcm_s16be,pcm_u8,pcm_f32le \
    --enable-parser=h264,hevc,vp8,vp9,av1,aac,mpegaudio,opus,flac,vorbis,png \
    --enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,extract_extradata,vp9_superframe \
    --enable-protocol=file \
    ${FFMPEG_EXTRA_FLAGS:-} \
    >/dev/null

log "building with $JOBS jobs"
make -j"$JOBS" >/dev/null
make install >/dev/null
printf '%s' "$VERSION" > "$marker"

# BSD find has no -printf, so ask the files themselves.
total=$(find "$PREFIX/lib" -name '*.a' -exec wc -c {} + | awk 'END {print $1}')
log "installed to $PREFIX ($(( total / 1048576 )) MB of static libs)"
