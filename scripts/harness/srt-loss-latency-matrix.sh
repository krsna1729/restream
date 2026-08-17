#!/usr/bin/env bash
# Differential loss/latency/bitrate test matrix for
# docs/srt-pure-rust-plan.md Phase 4: libsrt vs the pure-Rust Core, under
# identical tc netem impairment, across a grid of loss levels x one-way
# network delays x target bitrates.
#
# Each cell runs in a fresh network namespace (sudo unshare --net) so tc
# qdisc state never leaks between cells and every cell can reuse the same
# port. Each implementation's caller/listener pair prints one "STATS ..."
# key=value line per role; this script just captures those lines verbatim
# into a TSV (raw stats differ in field names between libsrt and the Rust
# Core -- see test/native/srt-loss-listener.c and
# crates/srt-interop/src/bin/loss_listener.rs's doc comments -- so parsing
# into a common schema is left to a separate analysis pass, not done here).
#
# Usage:
#   scripts/harness/srt-loss-latency-matrix.sh [output.tsv]
#
# Tunables (env vars):
#   DURATION_SECS   per-cell stream duration (default 600 = 10 min/cell,
#                    the full-matrix setting; use 10 for a smoke test)
#   SRT_LATENCY_MS   fixed SRT protocol latency (SRTO_LATENCY / tsbpd_delay)
#                    for every cell -- a realistic fixed operator setting,
#                    not the netem axis (default 200)
#   BITRATE_LEVELS   space-separated target bitrates in bps (default just
#                    8000000, matching the original Phase 4 plan's
#                    loss-only design; set multiple values for a bitrate
#                    sweep, e.g. "1000000 2000000 4000000 8000000 16000000")
#   LOSS_LEVELS      space-separated loss percentages (default matrix)
#   LATENCY_LEVELS   space-separated netem one-way delays in ms (default matrix)
#   IMPLS            space-separated implementations to run: libsrt rust
set -uo pipefail

repo_root="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$repo_root"

OUT="${1:-/tmp/srt-loss-latency-matrix-$(date +%Y%m%d-%H%M%S).tsv}"

DURATION_SECS="${DURATION_SECS:-600}"
SRT_LATENCY_MS="${SRT_LATENCY_MS:-200}"
BITRATE_LEVELS="${BITRATE_LEVELS:-8000000}"
LOSS_LEVELS="${LOSS_LEVELS:-0.5 1 2 5 10 15}"
LATENCY_LEVELS="${LATENCY_LEVELS:-0 5 10 20 50 100}"
IMPLS="${IMPLS:-libsrt rust}"

PREFIX="${AGENT_SHARED_STATIC_ROOT:-$repo_root/.local/build/static}/prefix"
LIBSRT_LISTENER="$PREFIX/bin/restream-srt-loss-listener"
LIBSRT_CALLER="$PREFIX/bin/restream-srt-loss-caller"
RUST_LISTENER="$repo_root/target/debug/srt-interop-loss-listener"
RUST_CALLER="$repo_root/target/debug/srt-interop-loss-caller"

PORT=19300
CELL_TIMEOUT=$((DURATION_SECS + 30))

for bin in "$LIBSRT_LISTENER" "$LIBSRT_CALLER"; do
  if [[ " $IMPLS " == *" libsrt "* && ! -x "$bin" ]]; then
    echo "missing $bin -- run scripts/build/native-deps.sh first" >&2
    exit 1
  fi
done
for bin in "$RUST_LISTENER" "$RUST_CALLER"; do
  if [[ " $IMPLS " == *" rust "* && ! -x "$bin" ]]; then
    echo "missing $bin -- run: scripts/build/resource-limit.sh cargo build -p srt-interop" >&2
    exit 1
  fi
done

echo -e "impl\tloss_pct\tdelay_ms\tbitrate_bps\tduration_s\tcaller_rc\tlistener_rc\tcaller_stats\tlistener_stats" > "$OUT"

run_cell() {
  local impl="$1" loss_pct="$2" delay_ms="$3" bitrate_bps="$4"
  local listener_bin caller_bin
  if [[ "$impl" == "libsrt" ]]; then
    listener_bin="$LIBSRT_LISTENER"
    caller_bin="$LIBSRT_CALLER"
  else
    listener_bin="$RUST_LISTENER"
    caller_bin="$RUST_CALLER"
  fi

  # -u (name only, don't create): the cell runs as root via sudo, and this
  # sandboxed environment does not let root write into a file the
  # unprivileged mktemp call already created -- the file must be created
  # fresh by the root-context process itself (plain `>` redirection).
  local listener_log caller_log
  listener_log="$(mktemp -u)"
  caller_log="$(mktemp -u)"

  local has_loss=0
  awk -v l="$loss_pct" 'BEGIN{exit !(l>0)}' && has_loss=1

  local netem_cmd=""
  if [[ "$has_loss" == "1" || "$delay_ms" != "0" ]]; then
    netem_cmd="tc qdisc add dev lo root netem"
    if [[ "$has_loss" == "1" ]]; then
      netem_cmd="$netem_cmd loss ${loss_pct}%"
    fi
    if [[ "$delay_ms" != "0" ]]; then
      netem_cmd="$netem_cmd delay ${delay_ms}ms"
    fi
  fi

  # No `set -e` here: the caller legitimately exits 1 on a connect timeout
  # (rare but possible under high loss) and the script must still reap the
  # listener and record both real exit codes in that case, not abort mid-cell
  # and leave the listener an orphan / lose the listener_rc entirely.
  timeout "$CELL_TIMEOUT" sudo unshare --net bash -c "
    ip link set lo up
    ${netem_cmd:-true}
    '$listener_bin' '$PORT' '$DURATION_SECS' '$SRT_LATENCY_MS' > '$listener_log' 2>&1 &
    lpid=\$!
    sleep 0.3
    '$caller_bin' 127.0.0.1 '$PORT' '$DURATION_SECS' '$SRT_LATENCY_MS' '$bitrate_bps' > '$caller_log' 2>&1
    caller_rc=\$?
    wait \$lpid
    listener_rc=\$?
    echo \"\$caller_rc \$listener_rc\" > '${caller_log}.rc'
  "
  local outer_rc=$?

  local caller_rc="timeout" listener_rc="timeout"
  if [[ -f "${caller_log}.rc" ]]; then
    read -r caller_rc listener_rc < "${caller_log}.rc"
  elif [[ "$outer_rc" != "0" ]]; then
    caller_rc="outer_rc=$outer_rc"
    listener_rc="outer_rc=$outer_rc"
  fi

  local caller_stats listener_stats
  caller_stats="$(grep '^STATS ' "$caller_log" 2>/dev/null || echo "NO_STATS")"
  listener_stats="$(grep '^STATS ' "$listener_log" 2>/dev/null || echo "NO_STATS")"

  echo -e "${impl}\t${loss_pct}\t${delay_ms}\t${bitrate_bps}\t${DURATION_SECS}\t${caller_rc}\t${listener_rc}\t${caller_stats}\t${listener_stats}" >> "$OUT"
  echo "[matrix] impl=${impl} loss=${loss_pct}% delay=${delay_ms}ms bitrate=${bitrate_bps} caller_rc=${caller_rc} listener_rc=${listener_rc}"
  if [[ "$caller_stats" == "NO_STATS" || "$listener_stats" == "NO_STATS" ]]; then
    echo "[matrix]   caller_log: $(tail -5 "$caller_log" 2>/dev/null)"
    echo "[matrix]   listener_log: $(tail -5 "$listener_log" 2>/dev/null)"
  fi

  sudo rm -f "$listener_log" "$caller_log" "${caller_log}.rc"
}

total=0
for impl in $IMPLS; do
  for loss_pct in $LOSS_LEVELS; do
    for delay_ms in $LATENCY_LEVELS; do
      for bitrate_bps in $BITRATE_LEVELS; do
        total=$((total + 1))
      done
    done
  done
done

echo "[matrix] running $total cells, ${DURATION_SECS}s each, output: $OUT"

n=0
for impl in $IMPLS; do
  for loss_pct in $LOSS_LEVELS; do
    for delay_ms in $LATENCY_LEVELS; do
      for bitrate_bps in $BITRATE_LEVELS; do
        n=$((n + 1))
        echo "[matrix] cell $n/$total"
        run_cell "$impl" "$loss_pct" "$delay_ms" "$bitrate_bps"
      done
    done
  done
done

echo "[matrix] done, results in $OUT"
