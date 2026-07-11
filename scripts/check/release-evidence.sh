#!/usr/bin/env bash
# Canonical release gate. Keep CI orchestration thin: this script owns the
# evidence required before a scratch-runtime artifact is published.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"
ARCHIVE="${1:-}"

for command in cargo-audit cargo-deny grype trivy; do
    command -v "$command" >/dev/null || {
        echo "release-evidence: required tool missing: $command" >&2
        exit 1
    }
done

cargo audit
cargo deny check advisories licenses bans sources

RESTREAM_BUILD_PROFILE=release scripts/build/resource-limit.sh ./scripts/build/app-native.sh
git diff --exit-code -- sbom/restream-runtime.cdx.json

grype "sbom:sbom/restream-runtime.cdx.json" --fail-on high
trivy sbom --exit-code 1 --severity HIGH,CRITICAL sbom/restream-runtime.cdx.json

if [[ -n "$ARCHIVE" ]]; then
    scripts/check/container-smoke.sh --image restream:release --archive "$ARCHIVE"
else
    scripts/check/container-smoke.sh --image restream:release
fi
