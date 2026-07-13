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
build_targets=()
if [[ -n "${RESTREAM_BUILD_BINS:-}" ]]; then
    read -r -a requested_bins <<<"$RESTREAM_BUILD_BINS"
    for binary in "${requested_bins[@]}"; do
        build_targets+=(--bin "$binary")
    done
else
    build_targets=(--bin restream)
fi

feature_args=()
if [[ -n "${RESTREAM_BUILD_FEATURES:-}" ]]; then
    feature_args=(--features "$RESTREAM_BUILD_FEATURES")
fi

RESTREAM_BUILD_ROOT="$BUILD_ROOT" cargo build "${cargo_args[@]}" "${build_targets[@]}" "${feature_args[@]}"

BINARY="${CARGO_TARGET_DIR:-$ROOT/target}/$binary_dir/restream"
SBOM="${RESTREAM_SBOM_PATH:-$ROOT/dist/restream-runtime.cdx.json}"
file "$BINARY"

ldd_output="$(ldd "$BINARY" 2>&1 || true)"
printf '%s\n' "$ldd_output"

if grep -Eq 'libsrt|libsrt-' <<<"$ldd_output"; then
    echo "Native linkage verification failed: $BINARY still links libsrt dynamically." >&2
    exit 1
fi

if [[ -n "${RESTREAM_BUILD_BINS:-}" ]]; then
    for binary in "${requested_bins[@]}"; do
        built="${CARGO_TARGET_DIR:-$ROOT/target}/$binary_dir/$binary"
        [[ -x "$built" ]] || {
            echo "Native build verification failed: expected executable is missing: $built" >&2
            exit 1
        }
        binary_ldd_output="$(ldd "$built" 2>&1 || true)"
        if grep -Eq 'libsrt|libsrt-' <<<"$binary_ldd_output"; then
            echo "Native linkage verification failed: $built still links libsrt dynamically." >&2
            exit 1
        fi
    done
fi

echo "Verified: $BINARY does not link libsrt dynamically."
"$ROOT/scripts/build/emit-sbom.sh" "$BINARY" "$SBOM"
