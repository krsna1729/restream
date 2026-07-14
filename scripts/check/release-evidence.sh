#!/usr/bin/env bash
# Canonical release gate. Keep CI orchestration thin: this script owns the
# evidence required before a scratch-runtime artifact is published.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"
ARCHIVE="${1:-}"
SBOM="${2:-dist/restream-runtime.cdx.json}"
BINARY_BUNDLE="${3:-}"

for command in cargo-audit cargo-deny grype trivy; do
    command -v "$command" >/dev/null || {
        echo "release-evidence: required tool missing: $command" >&2
        exit 1
    }
done

cargo audit
cargo deny check advisories licenses bans sources

if [[ -n "$BINARY_BUNDLE" ]]; then
    for command in find ldd tar; do
        command -v "$command" >/dev/null || {
            echo "release-evidence: required bundle-inspection command missing: $command" >&2
            exit 1
        }
    done
    [[ -s "$BINARY_BUNDLE" ]] || {
        echo "release-evidence: binary bundle was not found: $BINARY_BUNDLE" >&2
        exit 1
    }

    # Certify the bytes users will download. The restream host archive contains
    # one executable plus the release SBOM and compliance files.
    bundle_root="$(mktemp -d)"
    cleanup() {
        rm -rf "$bundle_root"
    }
    trap cleanup EXIT
    tar -xzf "$BINARY_BUNDLE" -C "$bundle_root"
    mapfile -t bundled_bins < <(find "$bundle_root" -type f -perm -111 -print | sort)
    if [[ "${#bundled_bins[@]}" -ne 1 || "$(basename "${bundled_bins[0]}")" != "restream" ]]; then
        echo "release-evidence: expected exactly one executable named restream in $BINARY_BUNDLE" >&2
        exit 1
    fi
    certified_binary="${bundled_bins[0]}"
    if ldd "$certified_binary" 2>&1 | grep -Eq 'libsrt|libsrt-'; then
        echo "release-evidence: packaged binary still links libsrt dynamically: $certified_binary" >&2
        exit 1
    fi
    mapfile -t bundled_sboms < <(find "$bundle_root" -type f -name '*.sbom.cdx.json' -print | sort)
    if [[ "${#bundled_sboms[@]}" -ne 1 ]]; then
        echo "release-evidence: expected exactly one SBOM inside $BINARY_BUNDLE" >&2
        exit 1
    fi
    mkdir -p "$(dirname "$SBOM")"
    cp "${bundled_sboms[0]}" "$SBOM"
else
    # Preserve the source-certification form for local callers that are not
    # producing a downloadable bundle. Release automation always supplies the
    # bundle so its exact executable is the SBOM source.
    RESTREAM_SBOM_PATH="$SBOM" RESTREAM_BUILD_PROFILE=release \
        scripts/build/resource-limit.sh ./scripts/build/app-native.sh
fi
[[ -s "$SBOM" ]] || {
    echo "release-evidence: SBOM was not written: $SBOM" >&2
    exit 1
}

grype "sbom:$SBOM" --fail-on high
trivy sbom --exit-code 1 --severity HIGH,CRITICAL "$SBOM"

if [[ -n "$ARCHIVE" ]]; then
    if [[ -n "$BINARY_BUNDLE" ]]; then
        scripts/check/release-artifact-smoke.sh "$BINARY_BUNDLE"
    fi
    scripts/check/container-smoke.sh --image restream:release --archive "$ARCHIVE"
else
    scripts/check/container-smoke.sh --image restream:release
fi
