#!/usr/bin/env bash
# Start the non-publishing Release workflow dry-run for the current branch (or a
# supplied ref). Branch dispatches certify without pushing GHCR images or
# creating a GitHub Release.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"
# shellcheck source=scripts/lib/release-common.sh
source "$ROOT/scripts/lib/release-common.sh"

usage() {
    cat <<'EOF'
Usage: scripts/release/dispatch-dry-run.sh [ref]

Starts the non-publishing Release workflow dry-run for the current branch, or
for the supplied ref. Branch dispatches certify the exact pushed SHA without
creating GitHub Releases or pushing GHCR images.
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

if [[ $# -gt 1 ]]; then
    usage >&2
    exit 2
fi

REF="$(restream_release_ref_or_current_branch "${1:-}")" || {
    echo "dispatch-dry-run: could not infer branch; pass a ref explicitly" >&2
    exit 2
}
restream_require_commands gh jq

gh workflow run release.yml --ref "$REF"
sleep "${RESTREAM_RELEASE_DISPATCH_SETTLE_SECS:-3}"

gh run list \
    --workflow release.yml \
    --branch "$REF" \
    --limit 1 \
    --json databaseId,status,conclusion,headSha,createdAt,url \
    | jq '.[0]'
