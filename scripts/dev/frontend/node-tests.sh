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
  test/frontend/frontend-architecture-layering.test.mjs
  test/frontend/frontend-dashboard-contract.test.mjs
  test/frontend/dashboard-contract/output-mutations.test.mjs
  test/frontend/dashboard-contract/pipeline-mutations.test.mjs
  test/frontend/dashboard-contract/runtime-modes.test.mjs
  test/frontend/dashboard-contract/runtime-polling.test.mjs
  test/frontend/frontend-core-helpers.test.mjs
  test/frontend/frontend-diagnostics.test.mjs
  test/frontend/frontend-incidents.test.mjs
  test/frontend/frontend-engineer-telemetry.test.mjs
  test/frontend/frontend-ops-navigation.test.mjs
  test/frontend/frontend-pipeline-workspace.test.mjs
  test/frontend/frontend-history-stream.test.mjs
  test/frontend/frontend-log-stream.test.mjs
  test/frontend/frontend-log-stream-interleaving.property.test.mjs
  test/frontend/frontend-history-helpers.test.mjs
  test/frontend/frontend-overview-activity-stream.test.mjs
  test/frontend/frontend-publisher-health-contract.test.mjs
  test/frontend/frontend-status-stream.test.mjs
  test/frontend/frontend-pipeline-route-body.test.mjs
  test/frontend/frontend-media-render.test.mjs
  test/frontend/frontend-status-render.test.mjs
  test/frontend/frontend-settings-render.test.mjs
  test/frontend/frontend-settings-srt-ingest.test.mjs
  test/frontend/history-nearby-render.test.mjs
  test/frontend/overview-activity-render.test.mjs
  test/frontend/overview-view-model.test.mjs
  test/frontend/pipeline-operate-view-model.test.mjs
  test/frontend/pipeline-output-overview.property.test.mjs
  test/frontend/pipeline-inputs-view-model.test.mjs
  test/frontend/frontend-chaos-scenarios.test.mjs
  test/frontend/frontend-output-scenarios.test.mjs
  test/frontend/frontend-pipeline-info-scenarios.test.mjs
  test/frontend/frontend-dom-render.test.mjs
)

# Only modules Node's fake-DOM harness genuinely cannot exercise stay here.
# `npm run test:frontend:coverage:all` (no excludes) is the way to check
# whether a module belongs on this list: if it already shows non-trivial
# coverage there, Node is exercising it fine and it should not be excluded.
NODE_COVERAGE_EXCLUDES=(
  # Side-effecting bootstrap entry point; runs initDashboardApp() and
  # startDashboardRuntime() at import time, so it is never imported by a
  # unit test. Proven at the app/browser-integration layer instead
  # (test:e2e, the `playwright` CI job).
  "web/ts/app/dashboard-entry.ts"
  # Real hls.js/video-element playback logic; near-zero reachable surface
  # under Node's fake DOM. Covered by test/frontend/hls-player.spec.ts.
  "web/ts/features/hls-player.ts"
  # Real preview <video>/<audio> element wiring; near-zero reachable
  # surface under Node's fake DOM. Covered by
  # test/frontend/frontend-browser-dom.spec.ts,
  # test/frontend/hls-player.spec.ts, and
  # test/frontend/redesign/seed-scale.spec.ts.
  "web/ts/features/input-preview.ts"
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
