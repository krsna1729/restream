#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT_DIR"
source "$ROOT_DIR/scripts/check/concurrency/common.sh"

LOG_DIR="$ROOT_DIR/.local/artifacts/concurrency-contract-logs"
mkdir -p "$LOG_DIR"

if [[ -z "${RESTREAM_BUILD_LOCK_FILE:-}" ]]; then
  export RESTREAM_BUILD_LOCK_FILE="/tmp/restream-build.lock"
fi

declare -A BASELINE_RUNTIME_PIDS=()

capture_runtime_baseline() {
  while read -r pid comm; do
    [[ -n "$pid" ]] || continue
    BASELINE_RUNTIME_PIDS["$pid"]="$comm"
  done < <(ps -eo pid=,comm= | awk '$2 ~ /^(restream|mediamtx|ffmpeg|ffprobe|test_harness)$/ { print $1, $2 }')
}

runtime_process_rows() {
  ps -eo pid=,comm=,args= | awk '
    $2 ~ /^(restream|mediamtx|ffmpeg|ffprobe|test_harness)$/ {
      sub(/^ +/, "", $0)
      print $0
    }
  '
}

new_runtime_pids() {
  while read -r pid comm _; do
    [[ -n "$pid" ]] || continue
    [[ -n "${BASELINE_RUNTIME_PIDS[$pid]:-}" ]] && continue
    printf '%s\n' "$pid"
  done < <(runtime_process_rows)
}

new_runtime_rows() {
  while read -r pid comm rest; do
    [[ -n "$pid" ]] || continue
    [[ -n "${BASELINE_RUNTIME_PIDS[$pid]:-}" ]] && continue
    if [[ -n "${rest:-}" ]]; then
      printf '%s %s %s\n' "$pid" "$comm" "$rest"
    else
      printf '%s %s\n' "$pid" "$comm"
    fi
  done < <(runtime_process_rows)
}

check_process_lifecycle_guards() {
  local harness="src/bin/test_harness.rs"

  awk '
    /kill_on_drop\(true\)/ { armed=1 }
    /\.spawn\(\)/ {
      if (!armed) {
        printf "process lifecycle guard failed: spawn without preceding kill_on_drop(true) near %s:%d\n", FILENAME, NR > "/dev/stderr"
        failed=1
      }
      armed=0
    }
    END { exit failed ? 1 : 0 }
  ' "$harness"
  grep -q 'async fn kill_and_wait_child' "$harness" || {
    echo "process lifecycle guard failed: missing kill_and_wait_child helper in $harness" >&2
    return 1
  }
  grep -q 'RESTREAM_BUILD_LOCK_FILE' scripts/build/resource-limit.sh || {
    echo "process lifecycle guard failed: scripts/build/resource-limit.sh must honor RESTREAM_BUILD_LOCK_FILE" >&2
    return 1
  }
}

cleanup_runtime() {
  local -a pids=()

  mapfile -t pids < <(new_runtime_pids)
  ((${#pids[@]} == 0)) && return 0

  kill -TERM -- "${pids[@]}" >/dev/null 2>&1 || true

  for _ in {1..10}; do
    mapfile -t pids < <(new_runtime_pids)
    ((${#pids[@]} == 0)) && return 0
    sleep 0.5
  done

  kill -KILL -- "${pids[@]}" >/dev/null 2>&1 || true

  for _ in {1..10}; do
    mapfile -t pids < <(new_runtime_pids)
    ((${#pids[@]} == 0)) && return 0
    sleep 0.5
  done
}

assert_no_runtime_processes() {
  local label="$1"
  local survivors

  for _ in {1..10}; do
    survivors=$(new_runtime_rows || true)
    [[ -z "$survivors" ]] && return 0
    sleep 0.5
  done

  if [[ -n "$survivors" ]]; then
    echo "runtime cleanup guard failed after $label:" >&2
    echo "$survivors" >&2
    return 1
  fi
}

run_logged() {
  local label="$1"
  shift
  local log_file="$LOG_DIR/${label}.log"

  if ! "$@" >"$log_file" 2>&1; then
    cat "$log_file"
    return 1
  fi
}

run_harness_mode() {
  local mode="$1"
  local work_dir="$2"
  local log_file="$LOG_DIR/${mode}.log"

  cleanup_runtime
  if ! RESTREAM_BIN=target/debug/restream \
    WORK_DIR="$work_dir" \
    target/debug/test_harness "$mode" >"$log_file" 2>&1; then
    cat "$log_file"
    cleanup_runtime
    assert_no_runtime_processes "$mode failure cleanup"
    return 1
  fi
  cleanup_runtime
  assert_no_runtime_processes "$mode"
}

capture_runtime_baseline
trap cleanup_runtime EXIT

run_logged history-grouping bash scripts/check/history-grouping.sh
run_logged process-lifecycle-guards check_process_lifecycle_guards

run_common_concurrency_checks run_logged
run_logged build-harness-bins scripts/build/resource-limit.sh cargo build --bin restream --bin test_harness

run_harness_mode fault.resilience .local/artifacts/concurrency-contract

run_harness_mode fault.egress-retry .local/artifacts/concurrency-fault-egress-retry

run_harness_mode fault.output-stall .local/artifacts/concurrency-fault-output-stall

run_harness_mode recovery .local/artifacts/concurrency-recovery
