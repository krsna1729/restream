#!/usr/bin/env bash
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
BUILD_ROOT="${RESTREAM_BUILD_ROOT:-$ROOT/.local/build/static}"

if [[ -z "${RESTREAM_BUILD_LOCK_HELD:-}" ]]; then
    echo "build-native: run via scripts/build/resource-limit.sh ./scripts/build/app-native.sh" >&2
    exit 2
fi

if [[ ! -f "$BUILD_ROOT/env.sh" ]]; then
    "$ROOT/scripts/build/resource-limit.sh" "$ROOT/scripts/build/native-deps.sh"
fi

PROFILE="${RESTREAM_BUILD_PROFILE:-debug}"
case "$PROFILE" in
    debug)
        cargo_args=()
        binary_dir="debug"
        ;;
    release)
        cargo_args=(--release)
        binary_dir="release"
        ;;
    *)
        echo "RESTREAM_BUILD_PROFILE must be debug or release" >&2
        exit 2
        ;;
esac

cd "$ROOT"
RESTREAM_BUILD_ROOT="$BUILD_ROOT" cargo build "${cargo_args[@]}" --bin restream

BINARY="${CARGO_TARGET_DIR:-$ROOT/target}/$binary_dir/restream"
SBOM="$ROOT/sbom/restream-runtime.cdx.json"
file "$BINARY"

ldd_output="$(ldd "$BINARY" 2>&1 || true)"
printf '%s\n' "$ldd_output"

if grep -Eq 'libsrt|libsrt-' <<<"$ldd_output"; then
    echo "Native linkage verification failed: $BINARY still links libsrt dynamically." >&2
    exit 1
fi

echo "Verified: $BINARY does not link libsrt dynamically."
if [[ "${RESTREAM_SKIP_SBOM:-0}" == "1" ]]; then
    echo "Skipping SBOM emission (RESTREAM_SKIP_SBOM=1)."
else
    "$BINARY" --emit-sbom "$SBOM"
fi
