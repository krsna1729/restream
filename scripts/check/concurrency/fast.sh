#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT_DIR"

source "$ROOT_DIR/scripts/check/concurrency/common.sh"

run_step() {
  local _label="$1"
  shift
  "$@"
}

run_common_concurrency_checks run_step

scripts/build/resource-limit.sh cargo test --bin test_harness -- --nocapture
