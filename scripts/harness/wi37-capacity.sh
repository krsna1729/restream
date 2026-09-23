#!/usr/bin/env bash
set -euo pipefail

repo_root="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$repo_root"

usage() {
  cat >&2 <<'EOF'
usage: scripts/harness/wi37-capacity.sh

Runs the bounded WI3.7 plaintext shard/fanout ladder. The default arm is
requested SRT shards 1..4, fanout 10,20,30,40,50,60,80. Each arm stops after
a receiver apparatus limit or a sender-capacity classification. Existing
complete cells are preserved as attempts; WI37_RESUME=1 reuses only cells
whose artifact and contract match the current clean build provenance.

Environment:
  WI37_ARTIFACT_ROOT    output root (default .local/artifacts/wi37-capacity)
  WI37_SKIP_BUILD       skip the feature-enabled bench build when set to 1
  WI37_RESUME           skip matching complete cells when set to 1 (default 0)
  WI37_SHARDS           comma-separated shard arms (default 1,2,3,4)
  WI37_FANOUTS          comma-separated fanouts (default 10,20,30,40,50,60,80)
  WI37_SKIP_CRYPTO      skip AES repeats when set to 1 (default 0)
  WI37_CRYPTO_SHARDS    bounded crypto cell shard count (default 1)
  WI37_CRYPTO_OUTPUTS   bounded crypto cell fanout (default 10)
  WI37_CRYPTO_REPEATS   repeats per AES mode, alternating order (default 3)
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
resume="${WI37_RESUME:-0}"
shards_csv="${WI37_SHARDS:-1,2,3,4}"
fanouts_csv="${WI37_FANOUTS:-10,20,30,40,50,60,80}"
crypto_repeats="${WI37_CRYPTO_REPEATS:-3}"
mkdir -p "$artifact_root"

if [[ ! "$window_secs" =~ ^[0-9]+([.][0-9]+)?$ || "$window_secs" == 0 ]]; then
  echo "wi37-capacity: WI37_WINDOW_SECS must be positive" >&2
  exit 2
fi
if [[ ! "$resume" =~ ^[01]$ ]]; then
  echo "wi37-capacity: WI37_RESUME must be 0 or 1" >&2
  exit 2
fi
if [[ ! "$crypto_repeats" =~ ^[1-9][0-9]*$ ]]; then
  echo "wi37-capacity: WI37_CRYPTO_REPEATS must be a positive integer" >&2
  exit 2
fi

parse_csv() {
  local raw=$1
  local -n output=$2
  local value
  IFS=',' read -r -a values <<< "$raw"
  for value in "${values[@]}"; do
    if [[ ! "$value" =~ ^[0-9]+$ ]]; then
      echo "wi37-capacity: invalid integer in CSV: $value" >&2
      exit 2
    fi
    output+=("$value")
  done
}

shard_arms=()
fanout_arms=()
parse_csv "$shards_csv" shard_arms
parse_csv "$fanouts_csv" fanout_arms
if ((${#shard_arms[@]} == 0 || ${#fanout_arms[@]} == 0)); then
  echo "wi37-capacity: WI37_SHARDS and WI37_FANOUTS must not be empty" >&2
  exit 2
fi
for shards in "${shard_arms[@]}"; do
  if (( shards < 1 || shards > 4 )); then
    echo "wi37-capacity: shard arms must be in 1..4 (got $shards)" >&2
    exit 2
  fi
done
for fanout in "${fanout_arms[@]}"; do
  if (( fanout < 1 )); then
    echo "wi37-capacity: fanouts must be positive (got $fanout)" >&2
    exit 2
  fi
done

if [[ ${WI37_SKIP_BUILD:-0} != 1 ]]; then
  RESTREAM_BENCH_FEATURES=wi37-shard-bench \
    scripts/build/bench-harness.sh
fi

if [[ ! -x target/bench/test_harness ]]; then
  echo "wi37-capacity: target/bench/test_harness is missing; build or unset WI37_SKIP_BUILD" >&2
  exit 1
fi

git_sha="$(git rev-parse HEAD)"
test_harness_sha256="$(sha256sum target/bench/test_harness | awk '{print $1}')"
restream_sha256="$(sha256sum target/bench/restream | awk '{print $1}')"
features="wi37-shard-bench"
build_provenance_path="target/bench/build-provenance.json"
if [[ ! -s "$build_provenance_path" ]]; then
  echo "wi37-capacity: missing $build_provenance_path; rebuild the clean feature bench" >&2
  exit 1
fi
python3 - "$build_provenance_path" "$git_sha" "$features" <<'PY'
import json, sys
path, git_sha, features = sys.argv[1:]
try:
    provenance = json.load(open(path))
except (OSError, ValueError) as error:
    raise SystemExit(f"wi37-capacity: invalid build provenance: {error}")
if provenance.get("gitSha") != git_sha:
    raise SystemExit("wi37-capacity: build provenance gitSha does not match HEAD")
if provenance.get("gitDirty") is not False:
    raise SystemExit("wi37-capacity: rating requires build provenance gitDirty=false")
if provenance.get("features") != features:
    raise SystemExit("wi37-capacity: build provenance feature set does not match WI3.7")
PY

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

write_provenance() {
  local path=$1
  python3 - "$path" "$git_sha" "$test_harness_sha256" "$restream_sha256" "$features" "$build_provenance_path" <<'PY'
import json, sys
path, git_sha, bench_sha, restream_sha, features, build_path = sys.argv[1:]
build = json.load(open(build_path))
provenance = {
    "contractVersion": 2,
    "gitSha": git_sha,
    "benchSha256": bench_sha,
    "restreamSha256": restream_sha,
    "features": features,
    "buildProvenance": {
        "gitSha": build["gitSha"],
        "gitDirty": build["gitDirty"],
        "features": build["features"],
    },
}
with open(path, "w") as handle:
    json.dump(provenance, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
}

write_contract() {
  local path=$1
  local crypto=$2
  local repeat=$3
  local shards=$4
  local fanout=$5
  local shard_cpus=$6
  local control_cpu=$7
  local peer_cpus=$8
  python3 - "$path" "$git_sha" "$test_harness_sha256" "$restream_sha256" "$features" "$crypto" "$repeat" "$shards" "$fanout" "$shard_cpus" "$control_cpu" "$peer_cpus" "$window_secs" "$receiver_queue_horizon_ms" "$dest_base" "$netns" <<'PY'
import json, sys
(
    path, git_sha, bench_sha, restream_sha, features, crypto, repeat, shards,
    fanout, shard_cpus, control_cpu, peer_cpus, window_secs, queue_horizon,
    dest_base, netns,
) = sys.argv[1:]
contract = {
    "contractVersion": 2,
    "gitSha": git_sha,
    "benchSha256": bench_sha,
    "restreamSha256": restream_sha,
    "features": features,
    "buildProvenance": {
        "gitSha": git_sha,
        "gitDirty": False,
        "features": features,
    },
    "crypto": crypto,
    "repeat": None if repeat == "-" else int(repeat),
    "requestedShards": int(shards),
    "outputs": int(fanout),
    "bitrate": "8M",
    "windowSecs": float(window_secs),
    "receiverQueueHorizonMs": int(queue_horizon),
    "destBase": dest_base,
    "netns": netns,
    "shardCpus": shard_cpus,
    "controlRestreamCpus": control_cpu,
    "harnessCpus": control_cpu,
    "receiverPeerCpus": peer_cpus,
    "controlRestreamHarnessShare": True,
}
with open(path, "w") as handle:
    json.dump(contract, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
}

artifact_matches_contract() {
  local run_dir=$1
  local artifact="$run_dir/egress-duty.json"
  local contract="$run_dir/contract.json"
  [[ -s "$artifact" && -s "$contract" ]] || return 1
  python3 - "$artifact" "$contract" "$git_sha" "$test_harness_sha256" "$restream_sha256" "$features" "$build_provenance_path" <<'PY'
import json, sys
(
    artifact_path, contract_path, current_git_sha, current_bench_sha,
    current_restream_sha, current_features, build_path,
) = sys.argv[1:]
try:
    artifact = json.load(open(artifact_path))
    contract = json.load(open(contract_path))
    build = json.load(open(build_path))
except (OSError, ValueError):
    raise SystemExit(1)
expected = {
    "contractVersion": 2,
    "gitSha": current_git_sha,
    "benchSha256": current_bench_sha,
    "restreamSha256": current_restream_sha,
    "features": current_features,
    "buildProvenance": {
        "gitSha": current_git_sha,
        "gitDirty": False,
        "features": current_features,
    },
}
if any(contract.get(key) != value for key, value in expected.items()):
    raise SystemExit(1)
if build.get("gitSha") != current_git_sha or build.get("gitDirty") is not False:
    raise SystemExit(1)
if build.get("features") != current_features:
    raise SystemExit(1)
if artifact.get("verdict") is None or not isinstance(artifact.get("capacity"), dict):
    raise SystemExit(1)
config = artifact.get("config") or {}
checks = {
    "crypto": config.get("crypto"),
    "requestedShards": config.get("requestedShards"),
    "outputs": config.get("outputs"),
    "bitrate": config.get("bitrate"),
    "windowSecs": config.get("windowSecs"),
    "receiverQueueHorizonMs": config.get("receiverQueueHorizonMs"),
    "destBase": config.get("destBase"),
    "netns": config.get("netns"),
    "shardCpus": ",".join(str(value) for value in config.get("shardCpus", [])),
    "controlRestreamCpus": config.get("restreamCpusExplicit", config.get("restreamCpus")),
    "harnessCpus": config.get("harnessCpus"),
    "receiverPeerCpus": config.get("peerCpus"),
}
for key, value in checks.items():
    expected_value = contract.get(key)
    if key == "windowSecs":
        if value is None or expected_value is None or abs(float(value) - float(expected_value)) > 1e-9:
            raise SystemExit(1)
    elif str(value) != str(expected_value):
        raise SystemExit(1)
if contract.get("controlRestreamHarnessShare") is not True:
    raise SystemExit(1)
raise SystemExit(0)
PY
}


matching_run_dir() {
  local canonical_dir=$1
  local candidate
  if artifact_matches_contract "$canonical_dir"; then
    printf '%s' "$canonical_dir"
    return 0
  fi
  shopt -s nullglob
  for candidate in "$canonical_dir"/attempt-*; do
    if artifact_matches_contract "$candidate"; then
      printf '%s' "$candidate"
      shopt -u nullglob
      return 0
    fi
  done
  shopt -u nullglob
  return 1
}


verdict_field() {
  local artifact=$1
  local field=$2
  python3 - "$artifact" "$field" <<'PY'
import json, sys
try:
    value = json.load(open(sys.argv[1])).get(sys.argv[2])
except (OSError, ValueError):
    value = "invalid-json"
print(value if value is not None else "null")
PY
}

emit_cell() {
  local crypto=$1
  local repeat=$2
  local shards=$3
  local fanout=$4
  local status=$5
  local run_dir=$6
  local artifact="$run_dir/egress-duty.json"
  local verdict classification apparatus
  if [[ ! -s "$artifact" ]]; then
    echo -e "$crypto\t$repeat\t$shards\t$fanout\t$status\tmissing\tmissing\tmissing\t$artifact"
    echo "wi37-capacity: missing artifact for crypto=$crypto repeat=$repeat shards=$shards fanout=$fanout; see $run_dir/run.log" >&2
    return 1
  fi
  verdict=$(verdict_field "$artifact" verdict)
  classification=$(classify_artifact "$artifact")
  apparatus=$(capacity_field "$artifact" apparatusValid)
  echo -e "$crypto\t$repeat\t$shards\t$fanout\t$status\t$verdict\t$classification\t$apparatus\t$artifact"
  if [[ "$classification" == "null" || "$apparatus" == "null" ]]; then
    echo "wi37-capacity: artifact lacks a capacity verdict for crypto=$crypto repeat=$repeat shards=$shards fanout=$fanout; see $run_dir/run.log" >&2
    return 1
  fi
  if [[ "$classification" == "receiver-apparatus-limited" || "$classification" == "sender-saturated" || "$apparatus" != "true" ]]; then
    echo "wi37-capacity: stopping arm shards=$shards fanout=$fanout classification=$classification apparatusValid=$apparatus" >&2
    return 2
  fi
  if (( status != 0 )); then
    echo "wi37-capacity: nonzero run without a stop classification; see $run_dir/run.log" >&2
    return "$status"
  fi
  return 0
}

run_cell() {
  local crypto=$1
  local repeat=$2
  local shards=$3
  local fanout=$4
  local canonical_dir=$5
  local run_id=$6
  local shard_cpus control_cpu peer_cpus run_dir matching log port_base status
  shard_cpus=$(shard_cpus_for "$shards")
  control_cpu="$shards"
  peer_cpus=$(peer_cpus_for "$shards")

  if (( resume == 1 )) && matching=$(matching_run_dir "$canonical_dir"); then
    echo "wi37-capacity: resume crypto=$crypto repeat=$repeat shards=$shards fanout=$fanout artifact=$matching/egress-duty.json" >&2
    emit_cell "$crypto" "$repeat" "$shards" "$fanout" 0 "$matching"
    return $?
  fi

  run_dir="$canonical_dir"
  if [[ -e "$canonical_dir/egress-duty.json" || -e "$canonical_dir/contract.json" || -e "$canonical_dir/run.log" ]]; then
    run_dir="$canonical_dir/attempt-$(date -u +%Y%m%dT%H%M%SZ)-$$-$run_id"
  fi
  mkdir -p "$run_dir"
  write_contract "$run_dir/contract.json" "$crypto" "$repeat" "$shards" "$fanout" "$shard_cpus" "$control_cpu" "$peer_cpus"
  log="$run_dir/run.log"
  port_base=$((18000 + run_id * 200))

  set +e
  TEST_HARNESS_ARTIFACT_DIR="$run_dir" \
  EGRESS_DUTY_WORK_DIR="$run_dir/work" \
  EGRESS_DUTY_OUTPUTS="$fanout" \
  EGRESS_DUTY_REQUESTED_SHARDS="$shards" \
  EGRESS_DUTY_SHARD_CPUS="$shard_cpus" \
  EGRESS_DUTY_RESTREAM_CPUS="$control_cpu" \
  EGRESS_DUTY_HARNESS_CPUS="$control_cpu" \
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
  emit_cell "$crypto" "$repeat" "$shards" "$fanout" "$status" "$run_dir"
}

online_cpus="$(getconf _NPROCESSORS_ONLN 2>/dev/null || nproc)"
if (( online_cpus < 6 )); then
  echo "wi37-capacity: requires at least 6 online CPUs for the fixed layout (got $online_cpus)" >&2
  exit 1
fi
write_provenance "$artifact_root/provenance.json"

printf 'crypto\trepeat\tshards\tfanout\tstatus\tverdict\tclassification\tapparatusValid\tartifact\n'
run_id=0
for shards in "${shard_arms[@]}"; do
  for fanout in "${fanout_arms[@]}"; do
    run_id=$((run_id + 1))
    run_dir="$artifact_root/shards-${shards}/fanout-${fanout}"
    if run_cell plain - "$shards" "$fanout" "$run_dir" "$run_id"; then
      :
    else
      status=$?
      if (( status == 2 )); then
        break
      fi
      exit "$status"
    fi
  done
done

if [[ ${WI37_SKIP_CRYPTO:-0} != 1 ]]; then
  crypto_shards="${WI37_CRYPTO_SHARDS:-1}"
  crypto_fanout="${WI37_CRYPTO_OUTPUTS:-10}"
  if [[ ! "$crypto_shards" =~ ^[1-4]$ || ! "$crypto_fanout" =~ ^[1-9][0-9]*$ ]]; then
    echo "wi37-capacity: invalid WI37_CRYPTO_SHARDS or WI37_CRYPTO_OUTPUTS" >&2
    exit 2
  fi
  for ((repeat = 1; repeat <= crypto_repeats; repeat++)); do
    if (( repeat % 2 == 1 )); then
      crypto_order=(128 256)
    else
      crypto_order=(256 128)
    fi
    for crypto in "${crypto_order[@]}"; do
      run_id=$((run_id + 1))
      run_dir="$artifact_root/crypto-${crypto}/repeat-${repeat}"
      if run_cell "$crypto" "$repeat" "$crypto_shards" "$crypto_fanout" "$run_dir" "$run_id"; then
        :
      else
        status=$?
        if (( status == 2 )); then
          echo "wi37-capacity: crypto cell classified as a stop; continuing repeat sequence" >&2
          continue
        fi
        exit "$status"
      fi
    done
  done
fi
