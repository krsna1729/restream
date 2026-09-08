#!/usr/bin/env bash
# Idempotent Cloud Agent install helpers: shared BtbN FFmpeg + git hooks.
# Point the environment `install` field at this script so pre-commit clippy
# can run without compiling the release static prefix from scratch.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"

"$ROOT/scripts/dev/fetch-btbn-ffmpeg.sh"
"$ROOT/scripts/dev/install-git-hooks.sh"

if [[ ! -d node_modules ]]; then
    echo "cloud-agent-install: installing npm dependencies"
    npm ci --include=optional
fi

echo "cloud-agent-install: ready for local fmt/clippy via staged-gate-router"
