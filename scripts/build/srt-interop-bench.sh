#!/usr/bin/env bash
# Build the SRT interop binaries used by the Rust-vs-libsrt benchmark matrix.
# The binaries intentionally target the host's x86-64-v3 baseline so the Rust
# side gets the same ISA baseline as the native libsrt build.
set -euo pipefail

repo_root="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$repo_root"

if [[ "$(uname -m)" != "x86_64" ]]; then
    echo "srt-interop-bench: x86-64-v3 requires an x86_64 host" >&2
    exit 2
fi

cpu_flags="$(awk '/^flags[[:space:]]*:/{print; exit}' /proc/cpuinfo)"
required_features=(
    ssse3 sse4_1 sse4_2 popcnt xsave avx avx2 bmi1 bmi2 f16c fma movbe
)
missing_features=()
for feature in "${required_features[@]}"; do
    if [[ " $cpu_flags " != *" $feature "* ]]; then
        missing_features+=("$feature")
    fi
done

# Linux reports AMD's LZCNT support as ABM rather than lzcnt.
if [[ " $cpu_flags " != *" lzcnt "* && " $cpu_flags " != *" abm "* ]]; then
    missing_features+=("lzcnt/abm")
fi
if (( ${#missing_features[@]} > 0 )); then
    echo "srt-interop-bench: CPU lacks x86-64-v3 features: ${missing_features[*]}" >&2
    exit 2
fi

target_rustflags="${CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS:-}"
target_rustflags+=" -C target-cpu=x86-64-v3"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="$target_rustflags"

scripts/build/resource-limit.sh cargo build --profile bench -p srt-interop "$@"

target_root="${CARGO_TARGET_DIR:-$repo_root/target}"
if [[ "$target_root" != /* ]]; then
    target_root="$repo_root/$target_root"
fi
bin_dir="$target_root/release"
for backend in mio tokio smol monoio glommio compio; do
    for role in caller listener; do
        binary="$bin_dir/srt-interop-loss-$role-$backend"
        if [[ ! -x "$binary" ]]; then
            echo "srt-interop-bench: expected binary missing: $binary" >&2
            exit 1
        fi
    done
done

echo "srt-interop-bench: ready in $bin_dir (opt-level=3, lto=thin, target-cpu=x86-64-v3)"
