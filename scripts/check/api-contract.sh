#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
TMP_DIR="${TMPDIR:-/tmp}/restream-api-contract-js"

cd "$ROOT_DIR"
rm -rf "$TMP_DIR"

npx tsc -p tsconfig.json --noEmit
node ./scripts/check/api-drift.mjs
npx tsc -p tsconfig.json --outDir "$TMP_DIR"
API_CONTRACT_JS_DIR="$TMP_DIR" node --test test/frontend/frontend-api-contract.test.mjs
bash scripts/check/history-grouping.sh
scripts/build/resource-limit.sh cargo test --test api -- --nocapture
scripts/build/resource-limit.sh cargo build --bin restream --bin test_harness
RESTREAM_BIN=target/debug/restream \
  RESTREAM_INITIAL_ADMIN_PASSWORD=admin \
  WORK_DIR=.local/artifacts/api-contract-smoke \
  target/debug/test_harness api-smoke
