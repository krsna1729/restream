#!/usr/bin/env bash
# Publish a release by pushing a v* tag, but only after a supplied branch
# workflow_dispatch dry-run has completed successfully against this exact HEAD.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"
# shellcheck source=scripts/lib/release-common.sh
source "$ROOT/scripts/lib/release-common.sh"

usage() {
    cat <<'EOF'
Usage: scripts/release/tag-and-publish.sh <vX.Y.Z> <successful-dry-run-id>

The dry-run must be a completed successful Release workflow run whose head SHA
matches the current checkout. Pushing the tag triggers the publishing workflow:
GitHub Release assets plus GHCR <tag> and latest images.
EOF
}

TAG="${1:-}"
RUN_ID="${2:-}"
if [[ "$TAG" == "-h" || "$TAG" == "--help" ]]; then
    usage
    exit 0
fi
if [[ -z "$TAG" || -z "$RUN_ID" ]]; then
    usage >&2
    exit 2
fi
restream_release_require_tag tag-and-publish "$TAG"
restream_require_commands gh jq git
restream_release_require_clean_checkout tag-and-publish

head_sha="$(git rev-parse HEAD)"
run_json="$(gh run view "$RUN_ID" --json status,conclusion,headSha,workflowName,event,url,jobs)"
status="$(jq -r '.status' <<<"$run_json")"
conclusion="$(jq -r '.conclusion' <<<"$run_json")"
run_sha="$(jq -r '.headSha' <<<"$run_json")"
workflow="$(jq -r '.workflowName' <<<"$run_json")"
event="$(jq -r '.event' <<<"$run_json")"
url="$(jq -r '.url' <<<"$run_json")"

if [[ "$workflow" != "Release" ]]; then
    echo "tag-and-publish: run $RUN_ID is workflow '$workflow', expected Release" >&2
    exit 1
fi
if [[ "$event" != "workflow_dispatch" ]]; then
    echo "tag-and-publish: run $RUN_ID event is '$event', expected workflow_dispatch dry-run" >&2
    exit 1
fi
if [[ "$status" != "completed" || "$conclusion" != "success" ]]; then
    echo "tag-and-publish: dry-run is not green: status=$status conclusion=$conclusion url=$url" >&2
    exit 1
fi
for required_job in "Package and release evidence" "Certify release dry-run"; do
    job_conclusion="$(jq -r --arg name "$required_job" '[.jobs[] | select(.name == $name) | .conclusion][0] // "missing"' <<<"$run_json")"
    if [[ "$job_conclusion" != "success" ]]; then
        echo "tag-and-publish: required release job '$required_job' was '$job_conclusion', expected success: $url" >&2
        exit 1
    fi
done
if [[ "$run_sha" != "$head_sha" ]]; then
    echo "tag-and-publish: dry-run SHA $run_sha does not match HEAD $head_sha" >&2
    exit 1
fi
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    echo "tag-and-publish: local tag already exists: $TAG" >&2
    exit 1
fi
if git ls-remote --exit-code --tags origin "refs/tags/$TAG" >/dev/null 2>&1; then
    echo "tag-and-publish: remote tag already exists: $TAG" >&2
    exit 1
fi

git tag -a "$TAG" -m "Release $TAG"
git push origin "refs/tags/$TAG"

echo "tag-and-publish: pushed $TAG at $head_sha"
echo "tag-and-publish: monitor with: gh run list --workflow release.yml --branch $TAG --limit 1"
