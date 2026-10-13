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

scripts/dev/frontend/compile-for-node.sh "$BUILD_DIR"

export FRONTEND_MODULES_DIR="$BUILD_DIR"
export TMPDIR="$TMP_BASE"

TEST_FILES=(test/frontend/*.test.mjs)

if [[ "${1:-}" == "--coverage" ]]; then
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
