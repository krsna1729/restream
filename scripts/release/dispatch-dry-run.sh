#!/usr/bin/env bash
# Start the non-publishing Release workflow dry-run for the current branch (or a
# supplied ref). Branch dispatches certify without pushing GHCR images or
# creating a GitHub Release.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"

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

REF="${1:-$(git branch --show-current)}"
if [[ -z "$REF" ]]; then
    echo "dispatch-dry-run: could not infer branch; pass a ref explicitly" >&2
    exit 2
fi

for command in gh jq; do
    command -v "$command" >/dev/null || {
        echo "dispatch-dry-run: required command not found: $command" >&2
        exit 1
    }
done

gh workflow run release.yml --ref "$REF"
sleep "${RESTREAM_RELEASE_DISPATCH_SETTLE_SECS:-3}"

gh run list \
    --workflow release.yml \
    --branch "$REF" \
    --limit 1 \
    --json databaseId,status,conclusion,headSha,createdAt,url \
    | jq '.[0]'
