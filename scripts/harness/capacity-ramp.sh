#!/usr/bin/env bash
# Egress capacity ramp: RTMP, RTMPS and SRT fan-out from one 8 Mbit/s ingest
# into the harness's in-process sinks (MSR_PEER=sink), with Restream and the
# receiver apparatus on disjoint CPU sets. Reports per-destination delivery
# (floor 0.95) and Jain fairness at the receiver, Restream's own delivery
# view, CPU and RSS per rung, plus the highest rung where every repeat
# delivered to every destination.
set -euo pipefail

repo_root="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$repo_root"

usage() {
  cat >&2 <<'EOF'
usage: scripts/harness/capacity-ramp.sh

Environment (all optional):
  CAPACITY_PROTOCOLS       comma list of rtmp,rtmps,srt (default all three)
  CAPACITY_RTMP_OUTPUTS    RTMP ladder   (default 100,250,500,1000,2000,4000)
  CAPACITY_RTMPS_OUTPUTS   RTMPS ladder  (default 100,250,500,1000,2000)
  CAPACITY_SRT_OUTPUTS     SRT ladder    (default 50,100,150,200,300,400,600,800)
  CAPACITY_BITRATE         publisher fixture bitrate label (default 8M)
  CAPACITY_WINDOW_SECS     rated window per rung (default 30)
  CAPACITY_SETTLE_SECS     settle before the window (default 10)
  CAPACITY_REPEATS         repeats per rung (default 3)
  CAPACITY_RESTREAM_CPUS   cpuset for Restream (default: first half of online CPUs)
  CAPACITY_HARNESS_CPUS    cpuset for harness, publisher and sinks (default: the rest)
  CAPACITY_EGRESS_SHARDS   RESTREAM_EGRESS_SHARDS override (default: product default,
                           CPU-derived and clamped 2..=8)
  CAPACITY_SINK_THREADS    SRT sink thread budget (default: CPUs in the harness set)
  CAPACITY_PEER_COUNT      sink instances / ports (default 1)
  CAPACITY_STOP_AFTER_FAIL stop a protocol's ladder after a rung where no repeat
                           passed (default 1)
  CAPACITY_ARTIFACT_ROOT   output root (default .local/artifacts/capacity-ramp/<utc stamp>)
  CAPACITY_SKIP_BUILD      1 to reuse existing target/bench binaries
  CAPACITY_ALLOW_DIRTY     1 to run on a dirty worktree (recorded in provenance)

A rung passes when the receiver saw every destination at >= 0.95 of the
offered rate over the window. Results: <root>/summary.md, summary.csv,
provenance.json, and one resource-sweep directory per rung and repeat.
EOF
}

if [[ ${1:-} == "--help" || ${1:-} == "-h" ]]; then
  usage
  exit 0
fi
[[ $# -eq 0 ]] || { usage; exit 2; }

protocols="${CAPACITY_PROTOCOLS:-rtmp,rtmps,srt}"
rtmp_outputs="${CAPACITY_RTMP_OUTPUTS:-100,250,500,1000,2000,4000}"
rtmps_outputs="${CAPACITY_RTMPS_OUTPUTS:-100,250,500,1000,2000}"
srt_outputs="${CAPACITY_SRT_OUTPUTS:-50,100,150,200,300,400,600,800}"
bitrate="${CAPACITY_BITRATE:-8M}"
window_secs="${CAPACITY_WINDOW_SECS:-30}"
settle_secs="${CAPACITY_SETTLE_SECS:-10}"
repeats="${CAPACITY_REPEATS:-3}"
stop_after_fail="${CAPACITY_STOP_AFTER_FAIL:-1}"
peer_count="${CAPACITY_PEER_COUNT:-1}"
root="${CAPACITY_ARTIFACT_ROOT:-.local/artifacts/capacity-ramp/$(date -u +%Y%m%dT%H%M%SZ)}"

# --- CPU split -------------------------------------------------------------
online_cpus() {
  # Expand /sys online list ("0-7,16-23") into one CPU per line.
  tr ',' '\n' </sys/devices/system/cpu/online | while IFS=- read -r lo hi; do
    seq "$lo" "${hi:-$lo}"
  done
}
mapfile -t cpus < <(online_cpus)
total_cpus=${#cpus[@]}
if (( total_cpus < 4 )); then
  echo "capacity-ramp: need at least 4 online CPUs (have $total_cpus)" >&2
  exit 2
fi
half=$(( total_cpus / 2 ))
join_cpus() { local IFS=,; echo "$*"; }
restream_cpus="${CAPACITY_RESTREAM_CPUS:-$(join_cpus "${cpus[@]:0:half}")}"
harness_cpus="${CAPACITY_HARNESS_CPUS:-$(join_cpus "${cpus[@]:half}")}"
count_cpus() {
  tr ',' '\n' <<<"$1" | while IFS=- read -r lo hi; do seq "$lo" "${hi:-$lo}"; done | wc -l
}
harness_cpu_count=$(count_cpus "$harness_cpus")
sink_threads="${CAPACITY_SINK_THREADS:-$harness_cpu_count}"

# --- Preflight ---------------------------------------------------------------
for process in restream mediamtx ffmpeg; do
  if pgrep -x "$process" >/dev/null; then
    echo "capacity-ramp: a '$process' process is running; stop it first (measurements must not share the host)" >&2
    exit 3
  fi
done
if [[ "${CAPACITY_ALLOW_DIRTY:-0}" != 1 ]] && ! git diff --quiet HEAD --; then
  echo "capacity-ramp: worktree is dirty; commit, stash, or set CAPACITY_ALLOW_DIRTY=1" >&2
  exit 3
fi
if (( $(ulimit -n) < 65536 )); then
  ulimit -n 65536 2>/dev/null || {
    echo "capacity-ramp: open-file limit $(ulimit -n) < 65536 and cannot be raised" >&2
    exit 3
  }
fi
warn_sysctl() {
  local key=$1 want=$2 have
  have=$(sysctl -n "$key" 2>/dev/null || echo 0)
  if (( have < want )); then
    echo "capacity-ramp: warning: $key=$have (< $want recommended)" >&2
  fi
}
warn_sysctl net.core.somaxconn 4096
warn_sysctl net.core.rmem_max 8388608
warn_sysctl net.core.wmem_max 8388608

# Listener ports below the ephemeral range, so no rung's outbound sockets can
# collide with a sink or Restream listener.
ephemeral_low=$(cut -f1 /proc/sys/net/ipv4/ip_local_port_range)
port_base=$(( ephemeral_low > 26000 ? 21000 : 11000 ))
export RESTREAM_HTTP=$port_base RESTREAM_RTMP=$((port_base + 1)) RESTREAM_SRT=$((port_base + 2))
export MTX_API=$((port_base + 10)) MTX_HLS=$((port_base + 11))
export MTX_RTMP=$((port_base + 100)) MTX_RTMPS=$((port_base + 200)) MTX_SRT=$((port_base + 300))

mkdir -p "$root"
if [[ "${CAPACITY_SKIP_BUILD:-0}" != 1 ]]; then
  export RESTREAM_BUILD_LOCK_FILE="${RESTREAM_BUILD_LOCK_FILE:-/tmp/restream-build.lock}"
  scripts/build/bench-harness.sh >"$root/build.log" 2>&1 || {
    echo "capacity-ramp: build failed; see $root/build.log" >&2
    exit 4
  }
fi
[[ -x target/bench/restream && -x target/bench/test_harness ]] || {
  echo "capacity-ramp: target/bench binaries missing (run scripts/build/bench-harness.sh)" >&2
  exit 4
}

# --- Provenance --------------------------------------------------------------
python3 - "$root/provenance.json" <<EOF
import json, os, platform, subprocess, sys
def sh(*cmd):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, check=False).stdout.strip()
    except OSError:
        return ""
lscpu = {}
for line in sh("lscpu").splitlines():
    key, _, value = line.partition(":")
    lscpu[key.strip()] = value.strip()
meminfo = open("/proc/meminfo").read().split("\n")[0]
json.dump({
    "commit": sh("git", "rev-parse", "HEAD"),
    # Tracked changes only, matching the preflight check; untracked scratch
    # files do not change the build.
    "dirty": sh("git", "status", "--porcelain", "--untracked-files=no") != "",
    "kernel": platform.release(),
    "cpu_model": lscpu.get("Model name", ""),
    "online_cpus": $total_cpus,
    "numa_nodes": lscpu.get("NUMA node(s)", ""),
    "hypervisor": lscpu.get("Hypervisor vendor", "bare-metal?"),
    "memory": meminfo,
    "rustc": sh("rustc", "--version"),
    "ffmpeg": sh("ffmpeg", "-version").split("\n")[0],
    "restream_cpus": "$restream_cpus",
    "harness_cpus": "$harness_cpus",
    "egress_shards": "${CAPACITY_EGRESS_SHARDS:-default}",
    "sink_threads": $sink_threads,
    "peer_count": $peer_count,
    "bitrate": "$bitrate",
    "window_secs": $window_secs,
    "settle_secs": $settle_secs,
    "repeats": $repeats,
    "ladders": {"rtmp": "$rtmp_outputs", "rtmps": "$rtmps_outputs", "srt": "$srt_outputs"},
}, open(sys.argv[1], "w"), indent=2)
EOF
echo "capacity-ramp: artifacts in $root (restream cpus $restream_cpus, harness cpus $harness_cpus)"

# --- Ramp --------------------------------------------------------------------
scenario_for() {
  case $1 in
    rtmp) echo egress-growth-source-same ;;
    rtmps) echo egress-growth-source-rtmps ;;
    srt) echo egress-growth-source-srt ;;
    *) echo "capacity-ramp: unknown protocol $1" >&2; exit 2 ;;
  esac
}
ladder_for() {
  case $1 in
    rtmp) echo "$rtmp_outputs" ;;
    rtmps) echo "$rtmps_outputs" ;;
    srt) echo "$srt_outputs" ;;
  esac
}

tls_cert="$PWD/test/fixtures/tls/mediamtx-rtmps-cert.pem"
tls_key="$PWD/test/fixtures/tls/mediamtx-rtmps-key.pem"

run_rung() {
  local protocol=$1 outputs=$2 rep=$3 dir=$4
  mkdir -p "$dir"
  local shards_env=()
  if [[ -n "${CAPACITY_EGRESS_SHARDS:-}" ]]; then
    shards_env=(RESTREAM_EGRESS_SHARDS="$CAPACITY_EGRESS_SHARDS")
  fi
  env "${shards_env[@]}" \
    RESTREAM_CPUSET="$restream_cpus" \
    MSR_PEER=sink PEER_COUNT="$peer_count" \
    HARNESS_SRT_SINK_THREADS="$sink_threads" \
    RESTREAM_BIN="$PWD/target/bench/restream" WORK_DIR="$dir" \
    RESOURCE_SWEEP_SCENARIOS="$(scenario_for "$protocol")" \
    RESOURCE_SWEEP_INGEST_COUNTS=1 RESOURCE_SWEEP_EGRESS_COUNTS="$outputs" \
    RESOURCE_SWEEP_BITRATE="$bitrate" \
    RESOURCE_SWEEP_RTMPS_CERT="$tls_cert" RESOURCE_SWEEP_RTMPS_KEY="$tls_key" \
    RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM="$tls_cert" \
    RESOURCE_SWEEP_SETTLE_SECS="$settle_secs" RESOURCE_SWEEP_SAMPLE_SECS="$window_secs" \
    taskset -c "$harness_cpus" target/bench/test_harness resource-sweep --no-netns \
    >"$dir/harness.log" 2>&1
}

rung_passed() {
  python3 - "$1/resource-sweep-results.csv" "$2" <<'EOF'
import csv, sys
try:
    rows = list(csv.DictReader(open(sys.argv[1])))
except OSError:
    sys.exit(1)
outputs = int(sys.argv[2])
ok = any(int(r["delivery_delivered"] or 0) >= outputs for r in rows)
sys.exit(0 if ok else 1)
EOF
}

IFS=, read -ra protocol_list <<<"$protocols"
for protocol in "${protocol_list[@]}"; do
  IFS=, read -ra ladder <<<"$(ladder_for "$protocol")"
  for outputs in "${ladder[@]}"; do
    passes=0
    for rep in $(seq 1 "$repeats"); do
      dir="$root/$protocol/$outputs/rep$rep"
      echo "[capacity-ramp] $protocol outputs=$outputs rep=$rep start $(date -u +%H:%M:%S)"
      if ! run_rung "$protocol" "$outputs" "$rep" "$dir"; then
        echo "[capacity-ramp] $protocol outputs=$outputs rep=$rep harness failed (see $dir/harness.log)"
      fi
      if rung_passed "$dir" "$outputs"; then
        passes=$((passes + 1))
      fi
      sleep 5 # let sockets from this rung drain before the next binds
    done
    echo "[capacity-ramp] $protocol outputs=$outputs passed $passes/$repeats"
    if [[ "$stop_after_fail" == 1 && $passes -eq 0 ]]; then
      echo "[capacity-ramp] $protocol: stopping ladder after a rung with no passing repeat"
      break
    fi
  done
done

python3 scripts/harness/capacity-ramp-summary.py "$root"
echo "capacity-ramp: summary at $root/summary.md"
