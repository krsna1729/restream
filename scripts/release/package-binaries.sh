#!/usr/bin/env bash
# Package every supported Linux executable with the same runtime-rootfs builder
# used by the smoke-tested scratch container. A raw ELF would silently depend
# on the builder's glibc loader, so this bundle has one portable launch
# contract.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"

VERSION="${1:?usage: scripts/release/package-binaries.sh <version>}"
ARCH="${RESTREAM_RELEASE_ARCH:-linux-x86_64}"
OUT_DIR="${RESTREAM_RELEASE_DIR:-dist}"

if [[ ! "$VERSION" =~ ^v?[0-9][0-9A-Za-z._+-]*$ ]]; then
    echo "package-binaries: version must be a filename-safe release label: $VERSION" >&2
    exit 2
fi

for command in cargo npm tar sha256sum; do
    command -v "$command" >/dev/null || {
        echo "package-binaries: required command not found: $command" >&2
        exit 1
    }
done

# A clean release checkout has neither node_modules nor generated public assets.
# Reuse the canonical release preparation script so packaging, CI, and local
# due diligence cannot drift on whether public/ and public/bin/ffmpeg exist.
scripts/release/prepare-build-tree.sh

# `restream-mcp` is feature-gated. Build the supported executable set through
# the native script so release packaging reuses the same static-link environment
# and linkage checks as the scratch image.
# Keep packaging clean-checkout friendly. app-native.sh emits an SBOM as part
# of its linkage proof, but release-evidence.sh later regenerates the certified
# SBOM from the exact bundled executable; writing this intermediate proof under
# dist/ prevents local release packaging from mutating the checked-in snapshot.
build_sbom="$OUT_DIR/restream-$VERSION.build.sbom.cdx.json"
RESTREAM_SBOM_PATH="$build_sbom" \
RESTREAM_BUILD_PROFILE=release \
RESTREAM_BUILD_BINS="restream restream-mcp test_harness" \
RESTREAM_BUILD_FEATURES="mcp-server,mcp-http-backend" \
    scripts/build/resource-limit.sh ./scripts/build/app-native.sh

tmp="$(mktemp -d)"
stage="$tmp/restream-$VERSION-$ARCH"
cleanup() {
    rm -rf "$tmp"
}
trap cleanup EXIT

mkdir -p "$stage/bin" "$stage/rootfs"
bash scripts/build/runtime-rootfs.sh target/release/restream "$stage/rootfs"
cp -a distribution "$stage/distribution"

for binary in restream restream-mcp test_harness; do
    source="target/release/$binary"
    [[ -x "$source" ]] || {
        echo "package-binaries: expected built executable is missing: $source" >&2
        exit 1
    }
    install -m 0755 "$source" "$stage/bin/$binary"
done

cat >"$stage/run" <<'EOF'
#!/bin/sh
# Run a bundled executable through the loader and libraries certified in the
# scratch image. The harness tools remain diagnostic tools; run them from a
# source checkout when they need fixtures, MediaMTX, or host network setup.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
binary=${1:---help}
case "$binary" in
  restream|restream-mcp|test_harness)
    shift
    ;;
  -h|--help)
    cat <<USAGE
Usage: ./run <binary> [arguments...]

Available binaries:
  restream           media-server runtime
  restream-mcp       MCP server (feature-enabled build)
  test_harness       live integration harness
USAGE
    exit 0
    ;;
  *)
    echo "unknown bundled binary: $binary" >&2
    exit 2
    ;;
esac

exec "$root/rootfs/lib64/ld-linux-x86-64.so.2" \
  --library-path "$root/rootfs/lib/x86_64-linux-gnu:$root/rootfs/usr/lib/x86_64-linux-gnu" \
  "$root/bin/$binary" "$@"
EOF
chmod 0755 "$stage/run"

# Prove that the packaged loader can start a non-server CLI before publishing
# it. `test_harness` deliberately rejects an unknown mode with exit 1; that
# expected parser result catches a missing dynamic library without binding a
# network port.
set +e
"$stage/run" test_harness --help >/dev/null 2>&1
loader_status=$?
set -e
[[ "$loader_status" -eq 1 ]] || {
    echo "package-binaries: bundled loader probe failed with $loader_status" >&2
    exit 1
}

cat >"$stage/README.txt" <<EOF
Restream $VERSION for $ARCH

Run the server:
  ./run restream

The bundle contains every supported Linux executable and the loader/library
closure from the scratch image that was smoke-tested for this release. It does
not require host FFmpeg, SRT, or C/C++ runtime packages.

License texts, the third-party component index, and source information are in
the distribution/ directory and must remain beside the binaries.

test_harness is included for inspection and debugging. Its read-only catalog
inspection commands live under `./run test_harness catalog ...`. Run live
integration tests from a source checkout with scripts/harness/run.sh; they also
require committed fixtures, MediaMTX, and the documented host setup.
EOF

mkdir -p "$OUT_DIR"
archive="$OUT_DIR/restream-$VERSION-$ARCH.tar.gz"
tar --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner \
    -C "$tmp" -czf "$archive" "$(basename "$stage")"
sha256sum "$archive" >"$archive.sha256"

echo "package-binaries: PASS archive=$archive"
