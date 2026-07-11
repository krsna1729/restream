#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
TMP_DIR="${TMPDIR:-/tmp}/restream-history-grouping-js"

cd "$ROOT_DIR"
rm -rf "$TMP_DIR"

npx tsc -p tsconfig.json --outDir "$TMP_DIR"
API_CONTRACT_JS_DIR="$TMP_DIR" node --test \
  test/frontend/history-nearby-render.test.mjs \
  test/frontend/overview-activity-render.test.mjs \
  test/frontend/frontend-chaos-scenarios.test.mjs
