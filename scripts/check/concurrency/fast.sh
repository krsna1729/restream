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
run_step "lib-rtmp-feed-wake-after-media" \
  scripts/build/resource-limit.sh cargo test feed_wake_delivers_media_after_idle_when_factory_start_is_delayed --lib -- --nocapture
run_step "lib-srt-ingress-two-owners" \
  scripts/build/resource-limit.sh cargo test two_owners_share_the_port_and_each_session_command_reaches_its_owner --lib -- --nocapture

scripts/build/resource-limit.sh cargo test api_runtime_views::status::tests::health --lib
scripts/build/resource-limit.sh cargo test --bin test_harness -- --nocapture
