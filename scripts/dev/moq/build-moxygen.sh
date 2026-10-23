#!/usr/bin/env bash
# Build the moxygen relay the `moq-moxygen-interop` harness mode is pinned to.
#
# moxygen (facebookexperimental/moxygen) publishes no release binaries, so the
# relay is built from a pinned commit with its own getdeps, which also pins
# folly, fizz, mvfst, wangle and proxygen (build/deps/github_hashes/). The
# result is `.local/bin/moxygen-<commit>/moqrelayserver`, statically linked
# against those dependencies, with a `.commit` stamp; an installed relay from
# the pinned commit is kept.
#
# Host workarounds, none of which change moxygen's protocol code:
# - CMAKE_POLICY_VERSION_MINIMUM=3.5: CMake 4 refuses boost 1.83's
#   pre-3.5 cmake_minimum_required.
# - CFLAGS=-std=gnu17: GCC 15 defaults to C23, which libunwind's sources reject.
# - liburing from a pinned commit on CPATH: folly's zero-copy RX needs newer
#   io_uring headers (zcrx ctrl) than a distro liburing 2.14 provides.
#
# Needs ~6 GB under the scratch path and roughly 30-60 min on 3 jobs. Install
# the system packages getdeps lists first, e.g.:
#   sudo python3 <checkout>/build/fbcode_builder/getdeps.py install-system-deps --recursive moxygen
set -euo pipefail

MOXYGEN_COMMIT="c392ebfc4d1dc39220fac3087b503416d06c47b9"
LIBURING_COMMIT="f33bc2f24cf3b38df8553aab5f9714fc5cf0aff4"

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
TARGET_DIR="$ROOT/.local/bin/moxygen-${MOXYGEN_COMMIT:0:8}"
TARGET="$TARGET_DIR/moqrelayserver"
SCRATCH="${MOXYGEN_SCRATCH:-$HOME/.cache/restream-moxygen}"
JOBS="${MOXYGEN_BUILD_JOBS:-3}"

case "${1:-}" in
    "") ;;
    -h|--help)
        sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
        exit 0
        ;;
    *) echo "build-moxygen: unknown argument: $1" >&2; exit 2 ;;
esac

if [[ -x "$TARGET" && "$(cat "$TARGET.commit" 2>/dev/null)" == "$MOXYGEN_COMMIT" ]]; then
    echo "build-moxygen: $TARGET already built from $MOXYGEN_COMMIT"
    exit 0
fi

# checkout URL COMMIT DIR: a shallow fetch of exactly COMMIT.
checkout() {
    local url=$1 commit=$2 dir=$3
    if [[ "$(git -C "$dir" rev-parse HEAD 2>/dev/null)" != "$commit" ]]; then
        rm -rf "$dir"
        git init -q "$dir"
        git -C "$dir" fetch -q --depth 1 "$url" "$commit"
        git -C "$dir" checkout -q FETCH_HEAD
    fi
}

mkdir -p "$SCRATCH"
checkout https://github.com/axboe/liburing.git "$LIBURING_COMMIT" "$SCRATCH/liburing-src"
# Reuse the install only when its stamp names the pinned commit; an interrupted
# or older install is rebuilt.
if [[ "$(cat "$SCRATCH/liburing/.commit" 2>/dev/null)" != "$LIBURING_COMMIT" ]]; then
    rm -rf "$SCRATCH/liburing"
    (cd "$SCRATCH/liburing-src" &&
        ./configure --prefix="$SCRATCH/liburing" >/dev/null &&
        make -j"$JOBS" -C src >/dev/null &&
        make -C src install >/dev/null)
    printf '%s\n' "$LIBURING_COMMIT" >"$SCRATCH/liburing/.commit"
fi

checkout https://github.com/facebookexperimental/moxygen.git "$MOXYGEN_COMMIT" "$SCRATCH/moxygen-src"
# CMake 4 reads CMAKE_POLICY_VERSION_MINIMUM from the environment for every
# project getdeps configures, the dependencies included.
export CMAKE_POLICY_VERSION_MINIMUM=3.5 CFLAGS="-std=gnu17"
export CPATH="$SCRATCH/liburing/include" LIBRARY_PATH="$SCRATCH/liburing/lib"
(cd "$SCRATCH/moxygen-src" &&
    python3 build/fbcode_builder/getdeps.py build --allow-system-packages --src-dir=. \
        --scratch-path "$SCRATCH/scratch" --num-jobs "$JOBS" --no-tests --free-up-disk \
        --build-type Release \
        --cmake-target moqrelayserver moxygen)

built="$SCRATCH/scratch/build/moxygen/moxygen/relay/moqrelayserver"
[[ -x "$built" ]] || built="$SCRATCH/scratch/installed/moxygen/bin/moqrelayserver"
if [[ ! -x "$built" ]]; then
    echo "build-moxygen: no moqrelayserver under $SCRATCH/scratch" >&2
    exit 1
fi
mkdir -p "$TARGET_DIR"
install -m 0755 "$built" "$TARGET.partial"
mv -f "$TARGET.partial" "$TARGET"
printf '%s\n' "$MOXYGEN_COMMIT" >"$TARGET.commit"
echo "build-moxygen: installed $TARGET ($MOXYGEN_COMMIT)"
