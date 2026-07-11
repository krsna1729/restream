#!/usr/bin/env bash
# Canonical release gate. Keep CI orchestration thin: this script owns the
# evidence required before a scratch-runtime artifact is published.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"
ARCHIVE="${1:-}"
SBOM="${2:-sbom/restream-runtime.cdx.json}"

for command in cargo-audit cargo-deny grype trivy; do
    command -v "$command" >/dev/null || {
        echo "release-evidence: required tool missing: $command" >&2
        exit 1
    }
done

cargo audit
cargo deny check advisories licenses bans sources

RESTREAM_SBOM_PATH="$SBOM" RESTREAM_BUILD_PROFILE=release \
    scripts/build/resource-limit.sh ./scripts/build/app-native.sh
[[ -s "$SBOM" ]] || {
    echo "release-evidence: SBOM was not written: $SBOM" >&2
    exit 1
}

grype "sbom:$SBOM" --fail-on high
trivy sbom --exit-code 1 --severity HIGH,CRITICAL "$SBOM"

if [[ -n "$ARCHIVE" ]]; then
    scripts/check/container-smoke.sh --image restream:release --archive "$ARCHIVE"
else
    scripts/check/container-smoke.sh --image restream:release
fi
