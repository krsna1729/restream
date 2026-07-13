#!/usr/bin/env bash
# Prepare a clean checkout for release-style Rust builds.
#
# Release CI used to run the live harness before packaging, while only the
# packaging script generated public/. That meant the harness compile failed in a
# clean runner with `#[derive(RustEmbed)] folder ... public/ does not exist`.
# Keep this as a canonical script so CI, local due diligence, and packaging all
# share the same generated-asset contract instead of depending on whatever files
# happened to exist in a developer tree.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"

usage() {
    cat <<'EOF'
Usage: scripts/release/prepare-build-tree.sh

Installs locked frontend dependencies when node_modules is absent, then runs
scripts/dev/prepare.sh so release/harness builds can embed generated public/
assets and the pinned native prefix from a clean checkout.

Environment:
  RESTREAM_RELEASE_NPM_CI=if-needed|always|never  (default: if-needed)
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi
if [[ $# -gt 0 ]]; then
    usage >&2
    exit 2
fi

for command in npm cargo; do
    command -v "$command" >/dev/null || {
        echo "prepare-build-tree: required command not found: $command" >&2
        exit 1
    }
done

npm_ci_mode="${RESTREAM_RELEASE_NPM_CI:-if-needed}"
case "$npm_ci_mode" in
    always)
        npm ci --include=optional
        ;;
    if-needed)
        if [[ ! -d node_modules ]]; then
            npm ci --include=optional
        else
            echo "prepare-build-tree: reusing existing node_modules"
        fi
        ;;
    never)
        ;;
    *)
        echo "prepare-build-tree: invalid RESTREAM_RELEASE_NPM_CI=$npm_ci_mode" >&2
        exit 2
        ;;
esac

scripts/dev/prepare.sh

for asset in \
    public/index.html \
    public/login.html \
    public/output.css \
    public/js/features/dashboard-entry.js \
    public/js/lib/hls.min.js \
    public/bin/ffmpeg; do
    [[ -s "$asset" ]] || {
        echo "prepare-build-tree: required generated asset is missing: $asset" >&2
        exit 1
    }
done

echo "prepare-build-tree: release build tree is ready"
