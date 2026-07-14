#!/usr/bin/env bash
# Package release executables into small role-specific archives. The host binary
# tarballs intentionally contain only the requested executable payloads and
# compliance material; the scratch OCI archive remains the portable runtime
# closure.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"
# shellcheck source=scripts/lib/release-common.sh
source "$ROOT/scripts/lib/release-common.sh"

VERSION="${1:?usage: scripts/release/package-binaries.sh <version>}"
ARCH="${RESTREAM_RELEASE_ARCH:-linux-x86_64}"
OUT_DIR="${RESTREAM_RELEASE_DIR:-dist}"

restream_release_require_version package-binaries "$VERSION"
restream_require_commands cargo npm tar

# A clean release checkout has neither node_modules nor generated public assets.
# Reuse the canonical release preparation script so packaging, CI, and local
# due diligence cannot drift on whether public/ and public/bin/ffmpeg exist.
scripts/release/prepare-build-tree.sh

# `restream-mcp` is feature-gated. Build the supported executable set through
# the native script so release packaging reuses the same static-link environment
# and linkage checks as the scratch image.
release_sbom="$OUT_DIR/restream-$VERSION.sbom.cdx.json"
RESTREAM_SBOM_PATH="$release_sbom" \
RESTREAM_BUILD_PROFILE=release \
RESTREAM_BUILD_BINS="restream restream-mcp test_harness" \
RESTREAM_BUILD_FEATURES="mcp-server,mcp-http-backend" \
    scripts/build/resource-limit.sh ./scripts/build/app-native.sh

tmp="$(mktemp -d)"
cleanup() {
    rm -rf "$tmp"
}
trap cleanup EXIT

for binary in restream restream-mcp test_harness; do
    source="target/release/$binary"
    [[ -x "$source" ]] || {
        echo "package-binaries: expected built executable is missing: $source" >&2
        exit 1
    }
done
[[ -s "$release_sbom" ]] || {
    echo "package-binaries: expected release SBOM is missing: $release_sbom" >&2
    exit 1
}

restream_stage="$tmp/restream-$VERSION-$ARCH"
mkdir -p "$restream_stage"
install -m 0755 target/release/restream "$restream_stage/restream"
install -m 0644 LICENSE.md "$restream_stage/LICENSE.md"
install -m 0644 distribution/THIRD_PARTY_COMPONENTS.md "$restream_stage/THIRD_PARTY_COMPONENTS.md"
mkdir -p "$restream_stage/licenses"
cp -a distribution/licenses/. "$restream_stage/licenses/"
install -m 0644 "$release_sbom" "$restream_stage/restream-$VERSION.sbom.cdx.json"

mcp_stage="$tmp/restream-mcp-$VERSION-$ARCH"
mkdir -p "$mcp_stage"
install -m 0755 target/release/restream-mcp "$mcp_stage/restream-mcp"

harness_stage="$tmp/test-harness-$VERSION-$ARCH"
mkdir -p "$harness_stage"
install -m 0755 target/release/test_harness "$harness_stage/test_harness"

mkdir -p "$OUT_DIR"
for package in \
    "restream-$VERSION-$ARCH" \
    "restream-mcp-$VERSION-$ARCH" \
    "test-harness-$VERSION-$ARCH"
do
    archive="$OUT_DIR/$package.tar.gz"
    tar --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner \
        -C "$tmp" -czf "$archive" "$package"
    echo "package-binaries: PASS archive=$archive"
done

scripts/release/package-runtime-image.sh "$VERSION"

echo "package-binaries: PASS sbom=$release_sbom"
