#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
TMP_BASE="${TMPDIR:-/tmp}"
BUILD_DIR="$(mktemp -d "${TMP_BASE}/restream-frontend-node-test-js.XXXXXX")"
KEEP_BUILD_DIR="${FRONTEND_NODE_TEST_KEEP_BUILD_DIR:-0}"

cleanup() {
  if [[ "$KEEP_BUILD_DIR" != "1" ]]; then
    rm -rf "$BUILD_DIR"
  fi
}

trap cleanup EXIT

cd "$ROOT_DIR"

npx tsc -p tsconfig.frontend-node-test.json --outDir "$BUILD_DIR"
cp test/support/frontend-v2-node-stubs/app/*.js "$BUILD_DIR/app/"

export FRONTEND_MODULES_DIR="$BUILD_DIR"
export TMPDIR="$TMP_BASE"

TEST_FILES=(
  test/frontend/frontend-api-contract.test.mjs
  test/frontend/frontend-dashboard-contract.test.mjs
  test/frontend/frontend-core-helpers.test.mjs
  test/frontend/frontend-diagnostics.test.mjs
  test/frontend/frontend-incidents.test.mjs
  test/frontend/frontend-engineer-telemetry.test.mjs
  test/frontend/frontend-ops-navigation.test.mjs
  test/frontend/frontend-pipeline-workspace.test.mjs
  test/frontend/frontend-history-stream.test.mjs
  test/frontend/frontend-log-stream.test.mjs
  test/frontend/frontend-history-helpers.test.mjs
  test/frontend/frontend-overview-activity-stream.test.mjs
  test/frontend/frontend-publisher-health-contract.test.mjs
  test/frontend/frontend-status-stream.test.mjs
  test/frontend/frontend-settings-render.test.mjs
  test/frontend/history-nearby-render.test.mjs
  test/frontend/overview-activity-render.test.mjs
  test/frontend/overview-view-model.test.mjs
  test/frontend/pipeline-operate-view-model.test.mjs
  test/frontend/pipeline-inputs-view-model.test.mjs
  test/frontend/frontend-chaos-scenarios.test.mjs
  test/frontend/frontend-output-scenarios.test.mjs
  test/frontend/frontend-pipeline-info-scenarios.test.mjs
  test/frontend/frontend-dom-render.test.mjs
)

NODE_COVERAGE_EXCLUDES=(
  "web/ts/core/api.ts"
  "web/ts/core/state.ts"
  "web/ts/features/control-room.ts"
  "web/ts/app/dashboard-entry.ts"
  "web/ts/features/diagnostics.ts"
  "web/ts/features/editor.ts"
  "web/ts/features/graph.ts"
  "web/ts/features/hls-player.ts"
  "web/ts/features/input-preview.ts"
  "web/ts/features/media-library.ts"
  "web/ts/features/metric-format.ts"
  "web/ts/features/metrics.ts"
  "web/ts/features/pipeline-dependencies.ts"
  "web/ts/features/settings.ts"
  "web/ts/history/render.ts"
  "web/ts/history/state.ts"
)

if [[ "${1:-}" == "--coverage" ]]; then
  COVERAGE_ARGS=(
    "--enable-source-maps"
    "--experimental-test-coverage"
    "--test"
    "--test-coverage-exclude=test/**"
  )
  for file in "${NODE_COVERAGE_EXCLUDES[@]}"; do
    COVERAGE_ARGS+=("--test-coverage-exclude=$file")
  done
  node --experimental-default-type=module "${COVERAGE_ARGS[@]}" "${TEST_FILES[@]}"
  exit 0
fi

if [[ "${1:-}" == "--coverage-all" ]]; then
  node \
    --experimental-default-type=module \
    --enable-source-maps \
    --experimental-test-coverage \
    --test \
    --test-coverage-exclude='test/**' \
    "${TEST_FILES[@]}"
  exit 0
fi

node --experimental-default-type=module --enable-source-maps --test "${TEST_FILES[@]}"
