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

Installs locked frontend dependencies when node_modules is absent, ensures the
pinned native prefix exists, then builds generated public/ frontend assets so
release/harness builds can embed them from a clean checkout.

Environment:
  RESTREAM_RELEASE_NPM_CI=if-needed|always|never  (default: if-needed)
  RESTREAM_RELEASE_FRONTEND_BUILD=if-needed|always|never  (default: if-needed)
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

native_state_ready() {
    [[ -f .local/build/static/env.sh ]] &&
        [[ -f .local/build/static/prefix/lib/libavcodec.a ]] &&
        [[ -f .local/build/static/prefix/lib/libavformat.a ]] &&
        [[ -x .local/build/static/prefix/bin/restream-ffmpeg-capabilities ]] &&
        [[ -x public/bin/ffmpeg ]]
}

if native_state_ready; then
    echo "prepare-build-tree: reusing existing native prefix"
else
    scripts/build/resource-limit.sh scripts/build/native-deps.sh
fi

frontend_assets_exist() {
    [[ -s public/index.html ]] &&
        [[ -s public/login.html ]] &&
        [[ -s public/output.css ]] &&
        [[ -s public/js/app/dashboard-entry.js ]] &&
        [[ -s public/js/app/dashboard-v2-entry.js ]] &&
        [[ -s public/js/app/dashboard-v2-checkpoints-entry.js ]] &&
        [[ -s public/js/app/dashboard-v2-jsx-runtime.js ]] &&
        [[ -s public/js/lib/hls.min.js ]]
}

frontend_assets_fresh() {
    frontend_assets_exist || return 1
    local newest_input
    newest_input="$(
        find web scripts/dev/frontend package.json package-lock.json tsconfig.json \
            tsconfig.v2.json vite.v2.config.ts \
            -type f -not -path '*/node_modules/*' -printf '%T@\n' 2>/dev/null |
            sort -nr |
            head -n1
    )"
    [[ -n "$newest_input" ]] || return 1

    local oldest_output
    oldest_output="$(
        find public/index.html public/login.html public/output.css public/js \
            -type f -printf '%T@\n' 2>/dev/null |
            sort -n |
            head -n1
    )"
    [[ -n "$oldest_output" ]] || return 1

    awk -v input="$newest_input" -v output="$oldest_output" 'BEGIN { exit !(output >= input) }'
}

frontend_build_mode="${RESTREAM_RELEASE_FRONTEND_BUILD:-if-needed}"
case "$frontend_build_mode" in
    always)
        npm run build:frontend
        ;;
    if-needed)
        if frontend_assets_fresh; then
            echo "prepare-build-tree: reusing generated frontend assets"
        else
            npm run build:frontend
        fi
        ;;
    never)
        ;;
    *)
        echo "prepare-build-tree: invalid RESTREAM_RELEASE_FRONTEND_BUILD=$frontend_build_mode" >&2
        exit 2
        ;;
esac

for asset in \
    public/index.html \
    public/login.html \
    public/output.css \
    public/js/app/dashboard-entry.js \
    public/js/app/dashboard-v2-entry.js \
    public/js/app/dashboard-v2-checkpoints-entry.js \
    public/js/app/dashboard-v2-jsx-runtime.js \
    public/js/lib/hls.min.js \
    public/bin/ffmpeg; do
    [[ -s "$asset" ]] || {
        echo "prepare-build-tree: required generated asset is missing: $asset" >&2
        exit 1
    }
done

echo "prepare-build-tree: release build tree is ready"
