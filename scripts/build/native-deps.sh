#!/usr/bin/env bash
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
BUILD_ROOT="${RESTREAM_BUILD_ROOT:-$ROOT/.local/build/static}"
TOOLS="$BUILD_ROOT/tools"
SOURCES="$BUILD_ROOT/src"
PREFIX="$BUILD_ROOT/prefix"
STAMPS="$BUILD_ROOT/stamps"
NATIVE_INPUT_LOCK="$ROOT/scripts/build/native/native-inputs.lock"

if [[ -z "${RESTREAM_BUILD_LOCK_HELD:-}" ]]; then
    echo "setup-static-build: run via scripts/build/resource-limit.sh ./scripts/build/native-deps.sh" >&2
    exit 2
fi

# shellcheck source=scripts/lib/debian-packages.sh
source "$ROOT/scripts/lib/debian-packages.sh"

FFMPEG_VERSION="${FFMPEG_VERSION:-n8.1.2}"
X264_COMMIT="${X264_COMMIT:-b35605ace3ddf7c1a5d67a2eb553f034aef41d55}"
X265_COMMIT="${X265_COMMIT:-e444744c03978c1fb4e037168967020cf2648427}"

if [[ ! -f "$NATIVE_INPUT_LOCK" ]]; then
    echo "missing native input lock: $NATIVE_INPUT_LOCK" >&2
    exit 1
fi
# shellcheck source=scripts/build/native/native-inputs.lock
source "$NATIVE_INPUT_LOCK"

require_locked_value() {
    local label="$1"
    local actual="$2"
    local expected="$3"
    if [[ "$actual" != "$expected" ]]; then
        echo "native input lock mismatch for $label: got '$actual', expected '$expected'" >&2
        echo "update scripts/build/native/native-inputs.lock with reviewed provenance before changing native inputs" >&2
        exit 1
    fi
}

require_locked_value FFMPEG_VERSION "$FFMPEG_VERSION" "$RESTREAM_LOCK_FFMPEG_VERSION"
require_locked_value X264_COMMIT "$X264_COMMIT" "$RESTREAM_LOCK_X264_COMMIT"
require_locked_value X265_COMMIT "$X265_COMMIT" "$RESTREAM_LOCK_X265_COMMIT"

mkdir -p "$TOOLS" "$SOURCES" "$PREFIX/bin" "$PREFIX/lib" "$STAMPS"

# Make the static prefix's pkg-config metadata take priority over any system
# copy for every native dependency built here.
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"

if command -v apt-get >/dev/null; then
    restream_debian_install_groups native-build
elif ! command -v cmake >/dev/null || ! command -v ninja >/dev/null; then
    echo "setup-static-build: cmake and ninja are required on non-apt hosts" >&2
    echo "install them manually before running this script" >&2
    exit 1
fi

for command in git cc c++ make perl pkg-config cmake ninja nasm sha256sum curl bzip2; do
    command -v "$command" >/dev/null || {
        echo "missing required command after setup: $command" >&2
        exit 1
    }
done

clone_tag() {
    local url="$1"
    local tag="$2"
    local destination="$3"
    if [[ ! -d "$destination/.git" ]]; then
        git clone --depth 1 --branch "$tag" "$url" "$destination"
    fi
}

clone_commit() {
    local url="$1"
    local commit="$2"
    local destination="$3"
    if [[ ! -d "$destination/.git" ]]; then
        git clone "$url" "$destination"
        git -C "$destination" checkout --detach "$commit"
    fi
}

# Download a release tarball, verify its SHA-256, and extract it (stripping the
# top-level directory) into $destination. A stamp records the verified hash so
# re-runs skip the download. Used for Mbed TLS, whose release tarball ships
# pre-generated sources (no Python/Perl codegen needed, unlike a git checkout).
fetch_tarball() {
    local url="$1"
    local sha256="$2"
    local destination="$3"
    local stamp="$destination/.tarball-stamp"
    if [[ -f "$stamp" && "$(cat "$stamp")" == "$sha256" ]]; then
        return
    fi
    local archive
    archive="$(mktemp)"
    curl -fLsS "$url" -o "$archive"
    printf '%s  %s\n' "$sha256" "$archive" | sha256sum -c - >/dev/null
    rm -rf "$destination"
    mkdir -p "$destination"
    tar xjf "$archive" -C "$destination" --strip-components=1
    rm -f "$archive"
    printf '%s\n' "$sha256" >"$stamp"
}

clone_tag https://github.com/FFmpeg/FFmpeg.git "$FFMPEG_VERSION" "$SOURCES/ffmpeg"
clone_commit https://code.videolan.org/videolan/x264.git "$X264_COMMIT" "$SOURCES/x264"
clone_commit https://bitbucket.org/multicoreware/x265_git.git "$X265_COMMIT" "$SOURCES/x265"

require_source_commit() {
    local label="$1"
    local source_dir="$2"
    local expected="$3"
    local actual
    actual="$(git -C "$source_dir" rev-parse HEAD)"
    require_locked_value "$label resolved commit" "$actual" "$expected"
}

require_source_commit "FFmpeg" "$SOURCES/ffmpeg" "$RESTREAM_LOCK_FFMPEG_COMMIT"
require_source_commit "x264" "$SOURCES/x264" "$RESTREAM_LOCK_X264_COMMIT"
require_source_commit "x265" "$SOURCES/x265" "$RESTREAM_LOCK_X265_COMMIT"

if [[ "${RESTREAM_VERIFY_NATIVE_INPUT_LOCK_ONLY:-0}" == "1" ]]; then
    echo "Native input lock verified."
    exit 0
fi

fingerprint() {
    sha256sum | cut -d' ' -f1
}

stamp_matches() {
    local stamp="$1"
    local expected="$2"
    [[ "${RESTREAM_REBUILD_NATIVE:-0}" != "1" &&
        -f "$stamp" &&
        "$(cat "$stamp")" == "$expected" ]]
}

write_stamp() {
    local stamp="$1"
    local value="$2"
    printf '%s\n' "$value" >"$stamp"
}

reset_cmake_build_if_moved() {
    local build_dir="$1"
    local source_dir="$2"
    local cache="$build_dir/CMakeCache.txt"
    if [[ ! -f "$cache" ]]; then
        return
    fi

    local cached_source
    cached_source="$(sed -n 's/^CMAKE_HOME_DIRECTORY:INTERNAL=//p' "$cache")"
    if [[ "$cached_source" != "$source_dir" ]]; then
        echo "Discarding moved CMake build cache: $build_dir"
        rm -rf "$build_dir"
    fi
}

# ─ Microarchitecture/Optimization Level ──────────────────────────────────────
# By default, compiles with x86-64-v3 (AVX2 baseline) for wide compatibility in releases.
# You can override this by setting the RESTREAM_MARCH environment variable, e.g.:
#   RESTREAM_MARCH=native scripts/build/resource-limit.sh ./scripts/build/native-deps.sh
#
MARCH="${RESTREAM_MARCH:-x86-64-v3}"

# These flags apply to all C/C++ dependencies (x264, x265, FFmpeg):
OPT_CFLAGS="-O3 -march=$MARCH"
OPT_CXXFLAGS="-O3 -march=$MARCH"

# Security hardening: ~1-2% overhead on metadata/alloc paths; zero on SIMD codec loops.
SEC_CFLAGS="-D_FORTIFY_SOURCE=3 -fstack-protector-strong -fPIC"
SEC_CXXFLAGS="-D_FORTIFY_SOURCE=3 -fstack-protector-strong -fPIC"
BUILD_CFLAGS="$OPT_CFLAGS $SEC_CFLAGS"
BUILD_CXXFLAGS="$OPT_CXXFLAGS $SEC_CXXFLAGS"

X264_FINGERPRINT="$(
    {
        git -C "$SOURCES/x264" rev-parse HEAD
        cc --version | head -n 1
        nasm -v
        printf '%s\n' \
            "CFLAGS=$BUILD_CFLAGS" \
            "SEC_CFLAGS=$SEC_CFLAGS" \
            "--prefix=$PREFIX" \
            --enable-static \
            --enable-pic \
            --disable-opencl \
            --disable-cli
    } | fingerprint
)"
X264_STAMP="$STAMPS/x264"
if ! stamp_matches "$X264_STAMP" "$X264_FINGERPRINT" ||
    [[ ! -f "$PREFIX/lib/libx264.a" ]]; then
    pushd "$SOURCES/x264" >/dev/null
    CFLAGS="$BUILD_CFLAGS" ./configure \
        --prefix="$PREFIX" \
        --enable-static \
        --enable-pic \
        --disable-opencl \
        --disable-cli
    make -j"${BUILD_JOBS:-$(nproc)}"
    make install
    popd >/dev/null
    write_stamp "$X264_STAMP" "$X264_FINGERPRINT"
else
    echo "Using cached x264 build."
fi

X265_FINGERPRINT="$(
    {
        git -C "$SOURCES/x265" rev-parse HEAD
        c++ --version | head -n 1
        cmake --version | head -n 1
        nasm -v
        printf '%s\n' \
            "CFLAGS=$BUILD_CFLAGS" \
            "CXXFLAGS=$BUILD_CXXFLAGS" \
            "SEC_CFLAGS=$SEC_CFLAGS" \
            "-DCMAKE_BUILD_TYPE=Release" \
            "-DCMAKE_INSTALL_PREFIX=$PREFIX" \
            "-DENABLE_SHARED=OFF" \
            "-DENABLE_CLI=OFF" \
            "-DENABLE_LIBNUMA=OFF" \
            "-DENABLE_PIC=ON" \
            "x265.pc:drop-lgcc_s-for-static-link"
    } | fingerprint
)"
X265_STAMP="$STAMPS/x265"
if ! stamp_matches "$X265_STAMP" "$X265_FINGERPRINT" ||
    [[ ! -f "$PREFIX/lib/libx265.a" || ! -f "$PREFIX/lib/pkgconfig/x265.pc" ]] ||
    grep -q -- '-lgcc_s' "$PREFIX/lib/pkgconfig/x265.pc"; then
    reset_cmake_build_if_moved "$BUILD_ROOT/x265-build" "$SOURCES/x265/source"
    cmake -S "$SOURCES/x265/source" -B "$BUILD_ROOT/x265-build" -G Ninja \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_C_FLAGS="$BUILD_CFLAGS" \
        -DCMAKE_CXX_FLAGS="$BUILD_CXXFLAGS" \
        -DCMAKE_INSTALL_PREFIX="$PREFIX" \
        -DENABLE_SHARED=OFF \
        -DENABLE_CLI=OFF \
        -DENABLE_LIBNUMA=OFF \
        -DENABLE_PIC=ON
    cmake --build "$BUILD_ROOT/x265-build" --parallel "${BUILD_JOBS:-$(nproc)}"
    cmake --install "$BUILD_ROOT/x265-build"
    perl -0pi -e 's/(?:\s+-lgcc_s)+//g' "$PREFIX/lib/pkgconfig/x265.pc"
    write_stamp "$X265_STAMP" "$X265_FINGERPRINT"
else
    echo "Using cached x265 build."
fi

FFMPEG_FINGERPRINT="$(
    {
        git -C "$SOURCES/ffmpeg" rev-parse HEAD
        cc --version | head -n 1
        nasm -v
        printf '%s\n' \
            "CFLAGS=$BUILD_CFLAGS" \
            "CXXFLAGS=$BUILD_CXXFLAGS" \
            "SEC_CFLAGS=$SEC_CFLAGS" \
            "$X264_FINGERPRINT" \
            "$X265_FINGERPRINT" \
            "--prefix=$PREFIX" \
            --pkg-config-flags=--static \
            --enable-static \
            --disable-shared \
            --enable-pic \
            --disable-programs \
            --disable-doc \
            --disable-debug \
            --disable-autodetect \
            --disable-network \
            --enable-x86asm \
            --disable-everything \
            --enable-gpl \
            --enable-libx264 \
            --enable-libx265 \
            --enable-avcodec \
            --enable-avformat \
            --enable-avfilter \
            --enable-swscale \
            --enable-swresample \
            --enable-protocol=file,pipe \
            --enable-demuxer=mpegts,matroska,mov \
            --enable-muxer=mpegts,matroska,mov,null \
            --enable-decoder=h264,hevc,aac,mp3,ac3,eac3 \
            --enable-encoder=aac,ac3,libx264,libx265,wrapped_avframe,pcm_s16le \
            --enable-parser=h264,hevc,aac,ac3 \
            --enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,aac_adtstoasc \
            --enable-filter=scale,crop,transpose,format,aformat,aresample,pan
    } | fingerprint
)"
FFMPEG_STAMP="$STAMPS/ffmpeg"
if ! stamp_matches "$FFMPEG_STAMP" "$FFMPEG_FINGERPRINT" ||
    [[ ! -f "$PREFIX/lib/libavcodec.a" ]]; then
    pushd "$SOURCES/ffmpeg" >/dev/null
    PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig" CFLAGS="$BUILD_CFLAGS" CXXFLAGS="$BUILD_CXXFLAGS" ./configure \
        --prefix="$PREFIX" \
        --pkg-config-flags=--static \
        --enable-static \
        --disable-shared \
        --enable-pic \
        --disable-programs \
        --disable-doc \
        --disable-debug \
        --disable-autodetect \
        --disable-network \
        --enable-x86asm \
        --extra-cflags="$BUILD_CFLAGS" \
        --extra-cxxflags="$BUILD_CXXFLAGS" \
        --disable-everything \
        --enable-gpl \
        --enable-libx264 \
        --enable-libx265 \
        --enable-avcodec \
        --enable-avformat \
        --enable-avfilter \
        --enable-swscale \
        --enable-swresample \
        --enable-protocol=file,pipe \
        --enable-demuxer=mpegts,matroska,mov \
        --enable-muxer=mpegts,matroska,mov,null \
        --enable-decoder=h264,hevc,aac,mp3,ac3,eac3 \
        --enable-encoder=aac,ac3,libx264,libx265,wrapped_avframe,pcm_s16le \
        --enable-parser=h264,hevc,aac,ac3 \
        --enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,aac_adtstoasc \
        --enable-filter=scale,crop,transpose,format,aformat,aresample,pan

    if ! grep -q '^#define HAVE_X86ASM 1$' config.h; then
        echo "FFmpeg configured without standalone x86 assembly" >&2
        exit 1
    fi

    make -j"${BUILD_JOBS:-$(nproc)}"
    make install
    make distclean
    popd >/dev/null
    write_stamp "$FFMPEG_STAMP" "$FFMPEG_FINGERPRINT"
else
    echo "Using cached FFmpeg build."
fi

# ─ Standalone ffmpeg binary for embedding (public/bin/ffmpeg) ────────────────
# The external transcoder spawns this as a subprocess. It must match the same
# FFmpeg version as the static libraries above so protocol/codec feature sets
# are consistent. Built in a separate directory to avoid disturbing the library
# build.  Strip the result to keep the embedded binary small.
FFMPEG_BIN_STAMP="$STAMPS/ffmpeg-${FFMPEG_VERSION}-built"
FFMPEG_BIN_DEST="$ROOT/public/bin/ffmpeg"
FFMPEG_BIN_BDIR="$BUILD_ROOT/ffmpeg-standalone-build"

if [[ ! -x "$FFMPEG_BIN_DEST" ]] &&
    stamp_matches "$FFMPEG_BIN_STAMP" "$FFMPEG_FINGERPRINT" &&
    [[ -x "$FFMPEG_BIN_BDIR/ffmpeg_stripped" ]]; then
    mkdir -p "$(dirname "$FFMPEG_BIN_DEST")"
    cp "$FFMPEG_BIN_BDIR/ffmpeg_stripped" "$FFMPEG_BIN_DEST"
    chmod +x "$FFMPEG_BIN_DEST"
    echo "Restored cached standalone ffmpeg to $FFMPEG_BIN_DEST"
fi

if ! stamp_matches "$FFMPEG_BIN_STAMP" "$FFMPEG_FINGERPRINT" ||
    [[ ! -x "$FFMPEG_BIN_DEST" ]]; then
    echo "Building standalone ffmpeg $FFMPEG_VERSION binary..."
    if [[ -f "$SOURCES/ffmpeg/config.h" ]]; then
        make -C "$SOURCES/ffmpeg" distclean
    fi
    mkdir -p "$FFMPEG_BIN_BDIR"
    pushd "$FFMPEG_BIN_BDIR" > /dev/null

    PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig" CFLAGS="$BUILD_CFLAGS" CXXFLAGS="$BUILD_CXXFLAGS" \
    "$SOURCES/ffmpeg/configure" \
        --prefix="$FFMPEG_BIN_BDIR/out" \
        --pkg-config-flags=--static \
        --enable-static \
        --disable-shared \
        --enable-pic \
        --disable-ffprobe \
        --disable-ffplay \
        --disable-doc \
        --disable-debug \
        --disable-autodetect \
        --disable-network \
        --enable-x86asm \
        --extra-cflags="$BUILD_CFLAGS -I$PREFIX/include" \
        --extra-ldflags="-static -L$PREFIX/lib" \
        --disable-everything \
        --enable-gpl \
        --enable-libx264 \
        --enable-libx265 \
        --enable-avcodec \
        --enable-avformat \
        --enable-avfilter \
        --enable-swscale \
        --enable-swresample \
        --enable-protocol=file,pipe \
        --enable-demuxer=mpegts,matroska,mov \
        --enable-muxer=mpegts,matroska,mov,null \
        --enable-decoder=h264,hevc,aac,mp3,ac3,eac3 \
        --enable-encoder=aac,ac3,libx264,libx265,wrapped_avframe,pcm_s16le \
        --enable-parser=h264,hevc,aac,ac3 \
        --enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,aac_adtstoasc \
        --enable-filter=scale,crop,transpose,format,aformat,aresample,pan

    make -j"${BUILD_JOBS:-$(nproc)}" ffmpeg_g
    strip -s ffmpeg_g -o ffmpeg_stripped
    mkdir -p "$(dirname "$FFMPEG_BIN_DEST")"
    cp ffmpeg_stripped "$FFMPEG_BIN_DEST"
    chmod +x "$FFMPEG_BIN_DEST"
    popd > /dev/null

    write_stamp "$FFMPEG_BIN_STAMP" "$FFMPEG_FINGERPRINT"
    echo "Standalone ffmpeg installed to $FFMPEG_BIN_DEST"
else
    echo "Using cached standalone ffmpeg build ($FFMPEG_BIN_DEST)."
fi

CAPABILITIES="$PREFIX/bin/restream-ffmpeg-capabilities"
if [[ ! -x "$CAPABILITIES" ||
    "$ROOT/test/native/ffmpeg-capabilities.c" -nt "$CAPABILITIES" ||
    "$PREFIX/lib/libavformat.a" -nt "$CAPABILITIES" ||
    "$PREFIX/lib/libavcodec.a" -nt "$CAPABILITIES" ||
    "$PREFIX/lib/libavutil.a" -nt "$CAPABILITIES" ]]; then
    PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig" cc -O2 -static \
        $(PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig" pkg-config --cflags libavformat libavcodec libavutil) \
        "$ROOT/test/native/ffmpeg-capabilities.c" \
        $(PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig" pkg-config --static --libs libavformat libavcodec libavutil) \
        -o "$CAPABILITIES"
fi

NATIVE_BUILD_ID="$(
    sha256sum \
        "$PREFIX/lib/libx264.a" \
        "$PREFIX/lib/libx265.a" \
        "$PREFIX/lib/libavcodec.a" \
        "$PREFIX/lib/libavformat.a" \
        "$PREFIX/lib/libavfilter.a" \
        "$PREFIX/lib/libswscale.a" \
        "$PREFIX/lib/libswresample.a" \
        "$PREFIX/lib/libavutil.a" |
        sha256sum |
        cut -d' ' -f1
)"

cat >"$BUILD_ROOT/env.sh" <<EOF
export RESTREAM_BUILD_ROOT="$BUILD_ROOT"
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"
export RESTREAM_STATIC_FFMPEG=1
export RESTREAM_FULLY_STATIC=1
export RESTREAM_NATIVE_BUILD_ID="$NATIVE_BUILD_ID"
export CARGO_TARGET_DIR="$BUILD_ROOT/cargo-target"
EOF

echo
echo "Static build environment is ready."
echo "Build with: scripts/build/resource-limit.sh ./scripts/build/app-static.sh"
echo "Faster iteration: RESTREAM_BUILD_PROFILE=fast-release scripts/build/resource-limit.sh ./scripts/build/app-static.sh"
echo "Force native rebuild: RESTREAM_REBUILD_NATIVE=1 scripts/build/resource-limit.sh ./scripts/build/native-deps.sh"
echo "Environment: $BUILD_ROOT/env.sh"
