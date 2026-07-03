#!/usr/bin/env bash
set -euo pipefail

ROOT=${WORK_DIR:-"test/artifacts/mixed/fast-breadth-parallel-$(date +%Y%m%dT%H%M%S)"}
BIN=${HARNESS_BIN:-target/bench/test_harness}
MODE=${HARNESS_MODE:-mixed.fast-breadth}
N_PER_GROUP_VALUE=${N_PER_GROUP:-1}
SKIP_LOAD_VALUE=${SKIP_LOAD:-1}
COLLECT_FAILURES_VALUE=${COLLECT_FAILURES:-1}
HARNESS_ARGS=${HARNESS_ARGS:---no-netns}
REQUIRE_CLEAN_RUNTIME_VALUE=${REQUIRE_CLEAN_RUNTIME:-0}

if [[ ! -x "$BIN" ]]; then
  echo "missing harness binary at $BIN; build it first with scripts/build-bench-harness.sh or scripts/resource-limit cargo build --profile bench --bin test_harness" >&2
  exit 1
fi

if [[ "$BIN" == "target/bench/test_harness" ]] \
  && [[ -x target/release/test_harness && -x target/release/restream ]] \
  && { [[ ! -x target/bench/test_harness ]] || [[ target/release/test_harness -nt target/bench/test_harness ]] || [[ ! -x target/bench/restream ]] || [[ target/release/restream -nt target/bench/restream ]]; }; then
  mkdir -p target/bench
  cp target/release/test_harness target/bench/test_harness
  cp target/release/restream target/bench/restream
fi

mkdir -p "$ROOT"

runtime_rows=$(ps -eo pid=,comm=,args= | awk '$2 ~ /^(restream|mediamtx|ffmpeg)$/ { sub(/^ +/, "", $0); print }')
if [[ -n "$runtime_rows" ]]; then
  printf '%s\n' "$runtime_rows" >"$ROOT/preexisting-runtime-processes.txt"
  if [[ "$REQUIRE_CLEAN_RUNTIME_VALUE" == "1" ]]; then
    echo "refusing to start fast-breadth parallel run while runtime processes are already running" >&2
    cat "$ROOT/preexisting-runtime-processes.txt" >&2
    exit 1
  fi
  echo "warning: continuing beside pre-existing runtime processes; baseline saved to $ROOT/preexisting-runtime-processes.txt" >&2
fi

groups=(live-rtmp live-srt file-ingest)
bases=(43000 48000 53000)
group_pids=()
group_dirs=()
group_statuses=()

run_group() {
  local group=$1
  local base=$2
  local group_dir=$3

  mkdir -p "$group_dir"
  (
    export WORK_DIR="$group_dir"
    export ASSERTION_LOG="$group_dir/assertions.jsonl"
    export TIMING_LOG="$group_dir/timing.jsonl"
    export SUMMARY_LOG="$group_dir/summary.txt"
    export RSS_SUMMARY="$group_dir/rss-summary.csv"
    export MIXED_FAST_BREADTH_GROUPS="$group"
    export N_PER_GROUP="$N_PER_GROUP_VALUE"
    export SKIP_LOAD="$SKIP_LOAD_VALUE"
    export COLLECT_FAILURES="$COLLECT_FAILURES_VALUE"

    export RESTREAM_HTTP=$((base + 30))
    export RESTREAM_RTMP=$((base + 35))
    export RESTREAM_SRT=$((base + 80))
    export MTX_RTMP=$((base + 135))
    export MTX_SRT=$((base + 180))
    export MTX_HLS=$((base + 190))
    export MTX_API=$((base + 197))
    export SINK_PORT=$((base + 300))
    export HLS_PUT_PORT=$((base + 600))
    export FFMPEG_SRT_SINK_BASE=$((base + 800))
    export FFMPEG_SIGNAL_SINK_BASE=$((base + 2000))

    if [[ -n "$HARNESS_ARGS" ]]; then
      # shellcheck disable=SC2086
      exec "$BIN" "$MODE" $HARNESS_ARGS
    else
      exec "$BIN" "$MODE"
    fi
  ) >"$group_dir/result.json" 2>"$group_dir/stderr.log" &
  group_pids+=("$!")
  group_dirs+=("$group_dir")
}

for i in "${!groups[@]}"; do
  run_group "${groups[$i]}" "${bases[$i]}" "$ROOT/${groups[$i]}"
done

overall_status=0
for i in "${!group_pids[@]}"; do
  if wait "${group_pids[$i]}"; then
    group_statuses+=("ok")
  else
    group_statuses+=("fail")
    overall_status=1
  fi
done

: >"$ROOT/assertions.jsonl"
: >"$ROOT/timing.jsonl"
: >"$ROOT/report-index.txt"

for i in "${!groups[@]}"; do
  group=${groups[$i]}
  group_dir=${group_dirs[$i]}
  status=${group_statuses[$i]}
  [[ -f "$group_dir/assertions.jsonl" ]] && cat "$group_dir/assertions.jsonl" >>"$ROOT/assertions.jsonl"
  [[ -f "$group_dir/timing.jsonl" ]] && cat "$group_dir/timing.jsonl" >>"$ROOT/timing.jsonl"
  printf '%s\t%s\t%s\t%s\n' \
    "$group" \
    "$status" \
    "$group_dir/result.json" \
    "$group_dir/stderr.log" >>"$ROOT/report-index.txt"
done

printf 'fast-breadth parallel artifacts: %s\n' "$ROOT"
printf 'batch\tstatus\tresult\tstderr\n'
cat "$ROOT/report-index.txt"

exit "$overall_status"
