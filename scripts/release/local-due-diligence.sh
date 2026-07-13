#!/usr/bin/env bash
# Run the local release confidence loop with the same canonical scripts that CI
# uses. This is intentionally a wrapper, not a second implementation of release
# evidence.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"
# shellcheck source=scripts/lib/release-common.sh
source "$ROOT/scripts/lib/release-common.sh"

usage() {
    cat <<'EOF'
Usage: scripts/release/local-due-diligence.sh <version> [--skip-harness]

Runs local release due diligence:
  1. format, frontend, API contract, test hygiene, fixture discipline
  2. full live harness suite unless --skip-harness is supplied
  3. package every supported Linux binary
  4. release evidence, packaged-frontend smoke, scratch container smoke

The evidence step requires cargo-audit, cargo-deny, grype, trivy, and Docker.
EOF
}

VERSION=""
SKIP_HARNESS=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-harness)
            SKIP_HARNESS=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        -*)
            echo "local-due-diligence: unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
        *)
            if [[ -n "$VERSION" ]]; then
                echo "local-due-diligence: version already supplied: $VERSION" >&2
                usage >&2
                exit 2
            fi
            VERSION="$1"
            shift
            ;;
    esac
done

if [[ -z "$VERSION" ]]; then
    usage >&2
    exit 2
fi
restream_release_require_version local-due-diligence "$VERSION"

sha="$(git rev-parse --short=12 HEAD)"
run_id="local-release-${VERSION}-${sha}"
arch="${RESTREAM_RELEASE_ARCH:-linux-x86_64}"
bundle="${RESTREAM_RELEASE_DIR:-dist}/restream-${VERSION}-${arch}.tar.gz"
oci="${RESTREAM_RELEASE_DIR:-dist}/restream-${VERSION}-oci.tar.gz"
sbom="${RESTREAM_RELEASE_DIR:-dist}/restream-${VERSION}.sbom.cdx.json"

cargo fmt --all --check
scripts/release/prepare-build-tree.sh
npm run test:frontend
scripts/check/api-contract.sh
scripts/check/test-hygiene.sh
scripts/check/fixture-discipline.sh

if [[ "$SKIP_HARNESS" -eq 0 ]]; then
    BENCH_BUILD="${BENCH_BUILD:-if-needed}" \
        scripts/harness/run.sh suite -- --run-id "$run_id" --continue-on-fail
else
    echo "local-due-diligence: skipping full live harness by request"
fi

scripts/release/package-binaries.sh "$VERSION"
scripts/check/release-evidence.sh "$oci" "$sbom" "$bundle"

echo "local-due-diligence: PASS version=$VERSION sha=$(git rev-parse HEAD)"
