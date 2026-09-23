#!/usr/bin/env bash
set -euo pipefail

repo_root="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$repo_root"

usage() {
  cat >&2 <<'EOF'
usage: scripts/harness/wi37-capacity.sh

Runs the bounded WI3.7 plaintext shard/fanout ladder. The arm is intentionally
small: requested SRT shards 1..4, fanout 10,20,30,40,50,60,80, then stop on
a receiver apparatus limit or a sender-capacity classification.

Environment:
  WI37_ARTIFACT_ROOT    output root (default .local/artifacts/wi37-capacity)
  WI37_SKIP_BUILD       skip the feature-enabled bench build when set to 1
  WI37_SKIP_CRYPTO      skip the bounded AES-128/AES-256 cells (default 0)
  WI37_CRYPTO_SHARDS    bounded crypto cell shard count (default 1)
  WI37_CRYPTO_OUTPUTS   bounded crypto cell fanout (default 10)
  WI37_RECEIVER_QUEUE_HORIZON_MS
                        receiver datapath queue horizon (default 10000)
  WI37_WINDOW_SECS      rated window (default 30)
  WI37_NETNS             receiver namespace (default RESTREAM_BENCH_NETNS)
  WI37_DEST_BASE        receiver peer address (default RESOURCE_SWEEP_SRT_PEER_HOSTS)
EOF
}

if [[ ${1:-} == "--help" || ${1:-} == "-h" ]]; then
  usage
  exit 0
fi
if [[ $# -ne 0 ]]; then
  usage
  exit 2
fi

artifact_root="${WI37_ARTIFACT_ROOT:-.local/artifacts/wi37-capacity}"
window_secs="${WI37_WINDOW_SECS:-30}"
netns="${WI37_NETNS:-${RESTREAM_BENCH_NETNS:-}}"
dest_base="${WI37_DEST_BASE:-${RESOURCE_SWEEP_SRT_PEER_HOSTS:-10.53.1.1}}"
receiver_queue_horizon_ms="${WI37_RECEIVER_QUEUE_HORIZON_MS:-10000}"
mkdir -p "$artifact_root"

if [[ ${WI37_SKIP_BUILD:-0} != 1 ]]; then
  RESTREAM_BENCH_FEATURES=wi37-shard-bench \
    scripts/build/bench-harness.sh
fi

capacity_field() {
  local artifact=$1
  local field=$2
  python3 - "$artifact" "$field" <<'PY'
import json, sys
path, field = sys.argv[1:]
try:
    value = json.load(open(path)).get("capacity", {}).get(field)
except (OSError, ValueError):
    value = None
if isinstance(value, bool):
    print("true" if value else "false")
elif value is None:
    print("null")
else:
    print(value)
PY
}

classify_artifact() {
  local artifact=$1
  capacity_field "$artifact" classification
}

shard_cpus_for() {
  case "$1" in
    1) printf '0' ;;
    2) printf '0,1' ;;
    3) printf '0,1,2' ;;
    4) printf '0,1,2,3' ;;
    *) return 1 ;;
  esac
}

peer_cpus_for() {
  case "$1" in
    1) printf '2-5' ;;
    2) printf '3-5' ;;
    3) printf '4-5' ;;
    4) printf '5' ;;
    *) return 1 ;;
  esac
}

online_cpus="$(getconf _NPROCESSORS_ONLN 2>/dev/null || nproc)"
if (( online_cpus < 6 )); then
  echo "wi37-capacity: requires at least 6 online CPUs for the fixed disjoint arm layout (got $online_cpus)" >&2
  exit 1
fi

printf 'crypto\tshards\tfanout\tstatus\tverdict\tclassification\tapparatusValid\tartifact\n'
run_id=0
for shards in 1 2 3 4; do
  shard_cpus=$(shard_cpus_for "$shards")
  peer_cpus=$(peer_cpus_for "$shards")
  for fanout in 10 20 30 40 50 60 80; do
    run_id=$((run_id + 1))
    run_dir="$artifact_root/shards-${shards}/fanout-${fanout}"
    mkdir -p "$run_dir"
    log="$run_dir/run.log"
    port_base=$((18000 + run_id * 200))

    set +e
    TEST_HARNESS_ARTIFACT_DIR="$run_dir" \
    EGRESS_DUTY_WORK_DIR="$run_dir/work" \
    EGRESS_DUTY_OUTPUTS="$fanout" \
    EGRESS_DUTY_REQUESTED_SHARDS="$shards" \
    EGRESS_DUTY_SHARD_CPUS="$shard_cpus" \
    EGRESS_DUTY_RESTREAM_CPUS="$shards" \
    EGRESS_DUTY_HARNESS_CPUS="$shards" \
    EGRESS_DUTY_PEER_CPUS="$peer_cpus" \
    EGRESS_DUTY_CAPACITY_MODE=1 \
    EGRESS_DUTY_CRYPTO=plain \
    EGRESS_DUTY_BITRATE=8M \
    EGRESS_DUTY_WINDOW_SECS="$window_secs" \
    EGRESS_DUTY_PORT_BASE="$port_base" \
    EGRESS_DUTY_DEST_BASE="$dest_base" \
    EGRESS_DUTY_NETNS="$netns" \
    EGRESS_DUTY_RECEIVER_QUEUE_HORIZON_MS="$receiver_queue_horizon_ms" \
    RESTREAM_BENCH_FEATURES=wi37-shard-bench \
    BENCH_BUILD=never \
      scripts/harness/run.sh egress-duty >"$log" 2>&1
    status=$?
    set -e

    artifact="$run_dir/egress-duty.json"
    if [[ ! -s "$artifact" ]]; then
      echo -e "plain\t$shards\t$fanout\t$status\tmissing\tmissing\tmissing\t$artifact"
      echo "wi37-capacity: missing artifact for shards=$shards fanout=$fanout; see $log" >&2
      exit 1
    fi
    verdict=$(python3 - "$artifact" <<'PY'
import json, sys
try:
    value = json.load(open(sys.argv[1])).get("verdict")
except (OSError, ValueError):
    value = "invalid-json"
print(value if value is not None else "null")
PY
)
    classification=$(classify_artifact "$artifact")
    apparatus=$(capacity_field "$artifact" apparatusValid)
    echo -e "plain\t$shards\t$fanout\t$status\t$verdict\t$classification\t$apparatus\t$artifact"

    if [[ "$classification" == "null" || "$apparatus" == "null" ]]; then
      echo "wi37-capacity: artifact lacks a capacity verdict for shards=$shards fanout=$fanout; see $log" >&2
      exit 1
    fi
    if [[ "$classification" == "receiver-apparatus-limited" || "$classification" == "sender-saturated" || "$apparatus" != "true" ]]; then
      echo "wi37-capacity: stopping arm shards=$shards fanout=$fanout classification=$classification apparatusValid=$apparatus" >&2
      break
    fi
    if (( status != 0 )); then
      echo "wi37-capacity: nonzero run without a stop classification; see $log" >&2
      exit "$status"
    fi
  done
done

if [[ ${WI37_SKIP_CRYPTO:-0} != 1 ]]; then
  crypto_shards="${WI37_CRYPTO_SHARDS:-1}"
  crypto_fanout="${WI37_CRYPTO_OUTPUTS:-10}"
  shard_cpus=$(shard_cpus_for "$crypto_shards")
  peer_cpus=$(peer_cpus_for "$crypto_shards")
  for crypto in 128 256; do
    run_id=$((run_id + 1))
    run_dir="$artifact_root/crypto-${crypto}"
    mkdir -p "$run_dir"
    log="$run_dir/run.log"
    port_base=$((18000 + run_id * 200))

    set +e
    TEST_HARNESS_ARTIFACT_DIR="$run_dir" \
    EGRESS_DUTY_WORK_DIR="$run_dir/work" \
    EGRESS_DUTY_OUTPUTS="$crypto_fanout" \
    EGRESS_DUTY_REQUESTED_SHARDS="$crypto_shards" \
    EGRESS_DUTY_SHARD_CPUS="$shard_cpus" \
    EGRESS_DUTY_RESTREAM_CPUS="$crypto_shards" \
    EGRESS_DUTY_HARNESS_CPUS="$crypto_shards" \
    EGRESS_DUTY_PEER_CPUS="$peer_cpus" \
    EGRESS_DUTY_CAPACITY_MODE=1 \
    EGRESS_DUTY_CRYPTO="$crypto" \
    EGRESS_DUTY_BITRATE=8M \
    EGRESS_DUTY_WINDOW_SECS="$window_secs" \
    EGRESS_DUTY_PORT_BASE="$port_base" \
    EGRESS_DUTY_DEST_BASE="$dest_base" \
    EGRESS_DUTY_NETNS="$netns" \
    EGRESS_DUTY_RECEIVER_QUEUE_HORIZON_MS="$receiver_queue_horizon_ms" \
    RESTREAM_BENCH_FEATURES=wi37-shard-bench \
    BENCH_BUILD=never \
      scripts/harness/run.sh egress-duty >"$log" 2>&1
    status=$?
    set -e

    artifact="$run_dir/egress-duty.json"
    if [[ ! -s "$artifact" ]]; then
      echo -e "$crypto\t$crypto_shards\t$crypto_fanout\t$status\tmissing\tmissing\tmissing\t$artifact"
      echo "wi37-capacity: missing AES-$crypto artifact; see $log" >&2
      exit 1
    fi
    verdict=$(python3 - "$artifact" <<'PY'
import json, sys
try:
    value = json.load(open(sys.argv[1])).get("verdict")
except (OSError, ValueError):
    value = "invalid-json"
print(value if value is not None else "null")
PY
)
    classification=$(classify_artifact "$artifact")
    apparatus=$(capacity_field "$artifact" apparatusValid)
    echo -e "$crypto\t$crypto_shards\t$crypto_fanout\t$status\t$verdict\t$classification\t$apparatus\t$artifact"
    if [[ "$classification" == "null" || "$apparatus" == "null" ]]; then
      echo "wi37-capacity: AES-$crypto artifact lacks a capacity verdict; see $log" >&2
      exit 1
    fi
    if (( status != 0 )) && [[ "$classification" != "receiver-apparatus-limited" && "$classification" != "sender-saturated" && "$apparatus" == "true" ]]; then
      echo "wi37-capacity: nonzero AES-$crypto run without a classified stop; see $log" >&2
      exit "$status"
    fi
  done
fi
