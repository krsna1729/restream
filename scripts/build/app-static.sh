#!/usr/bin/env bash
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
BUILD_ROOT="${RESTREAM_BUILD_ROOT:-$ROOT/.local/build/static}"

if [[ -z "${RESTREAM_BUILD_LOCK_HELD:-}" ]]; then
    echo "build-static: run via scripts/build/resource-limit.sh ./scripts/build/app-static.sh" >&2
    exit 2
fi

if [[ ! -f "$BUILD_ROOT/env.sh" ]]; then
    "$ROOT/scripts/build/resource-limit.sh" "$ROOT/scripts/build/native-deps.sh"
fi

# shellcheck source=/dev/null
source "$BUILD_ROOT/env.sh"

PROFILE="${RESTREAM_BUILD_PROFILE:-release}"
if [[ "$PROFILE" != "release" && "$PROFILE" != "fast-release" ]]; then
    echo "RESTREAM_BUILD_PROFILE must be release or fast-release" >&2
    exit 2
fi

cd "$ROOT"
cargo rustc --profile "$PROFILE" --bin restream -- \
    -C target-feature=+crt-static \
    -C relocation-model=static \
    -C linker=cc \
    -C link-arg=-fuse-ld=bfd \
    -C link-arg=-static \
    -C link-arg=-no-pie

BINARY="$CARGO_TARGET_DIR/$PROFILE/restream"
SBOM="$ROOT/sbom/restream-runtime.cdx.json"
file "$BINARY"
"$BUILD_ROOT/prefix/bin/restream-ffmpeg-capabilities"

ldd_output="$(ldd "$BINARY" 2>&1 || true)"
if grep -Eq "not a dynamic executable|statically linked" <<<"$ldd_output"; then
    echo "Verified: $BINARY is statically linked."
else
    echo "Static verification failed:" >&2
    echo "$ldd_output" >&2
    exit 1
fi

# Linkage alone is not a release contract.  Static glibc/native-library builds
# have regressed by producing an ELF that passed ldd yet crashed before CLI
# parsing.  Keep this cheap, dependency-free process smoke beside the linker
# check so CI/release automation cannot publish that false-positive artifact.
if ! "$BINARY" --help >/dev/null 2>&1; then
    echo "Static runtime smoke failed: $BINARY cannot start (--help)." >&2
    exit 1
fi
echo "Verified: $BINARY starts successfully."

if [[ "${RESTREAM_SKIP_SBOM:-0}" == "1" ]]; then
    echo "Skipping SBOM emission (RESTREAM_SKIP_SBOM=1)."
else
    "$BINARY" --emit-sbom "$SBOM"
fi
