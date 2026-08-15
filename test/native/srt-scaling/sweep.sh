#!/usr/bin/env bash
# Sweeps SRT receiver port_count at 600/900/1200 real 8Mbps connections on
# stock libsrt, looking for the smallest port_count that gives a clean
# result. UNRESOLVED as of 2026-08-15 -- see
# docs/agent-guidance/quality/srt-scaling-first-principles-investigation-2026-08-15.md
# for why an earlier run of this same idea gave a false-clean answer (a
# sender pacing bug under-called sendmsg() by ~50x, so "0 errors" meant
# "barely sent anything," not "delivered cleanly"). sender_bench.c now
# reports pct_of_target directly -- read that column, not just the error
# counts, before trusting any port_count as "clean."
set -uo pipefail
cd "$(dirname "$0")"

BASE_PORT=40000
CHECKPOINTS="600,900,1200"
HOLD=10
BITRATE=1000000   # bytes/sec = 8Mbps, matches the real MSR fixture bitrate
SENDER_THREADS=6
WORKER_THREADS=6
SENDER_LOCAL_PORTS=6   # mirrors restream's own SrtEgressMuxerPorts sender-side sharding;
                       # without this the sender itself bottlenecks on one shared multiplexer
REPS=5
LOAD_CEILING=3.0
TUNED_RCVBUF=192000000

OUT="$(pwd)/sweep-results.csv"
echo "port_count,rep,checkpoint,requested,connected,failed,connect_p50_ms,connect_p95_ms,connect_p99_ms,steady_bytes_sent,steady_send_attempts,steady_send_errors,steady_would_block,target_bytes,pct_of_target,elapsed_connect_s" > "$OUT"

wait_for_idle_host() {
    local iters=0
    while (( iters < 60 )); do
        local load1 over
        load1=$(awk '{print $1}' /proc/loadavg)
        over=$(awk -v l="$load1" -v c="$LOAD_CEILING" 'BEGIN{print (l > c) ? 1 : 0}')
        local live
        live=$(pgrep -f "srt-scaling/sink_bench" 2>/dev/null | wc -l)
        if [[ "$over" == "0" && "$live" == "0" ]]; then
            return 0
        fi
        sleep 5
        iters=$((iters + 1))
    done
}

run_cell() {
    local port_count="$1" rep="$2" port_base="$3" local_port_base="$4"

    wait_for_idle_host

    local sink_log="/tmp/srt-scaling-sink-p${port_count}-r${rep}.log"
    local sender_log="/tmp/srt-scaling-sender-p${port_count}-r${rep}.log"

    ./sink_bench "$port_base" "$port_count" "$WORKER_THREADS" "$TUNED_RCVBUF" > "$sink_log" 2>&1 &
    local sink_pid=$!
    sleep 2

    ./sender_bench 127.0.0.1 "$port_base" "$port_count" "$SENDER_THREADS" "$BITRATE" "$CHECKPOINTS" "$HOLD" \
        "$SENDER_LOCAL_PORTS" "$local_port_base" \
        > "$sender_log" 2>&1
    local sender_rc=$?

    kill -9 "$sink_pid" 2>/dev/null
    wait "$sink_pid" 2>/dev/null

    while IFS= read -r line; do
        [[ "$line" == checkpoint,* ]] && continue
        echo "${port_count},${rep},${line}" >> "$OUT"
    done < "$sender_log"

    echo "[srt-scaling-sweep] done port_count=${port_count} rep=${rep} sender_rc=${sender_rc}"
    sleep 5
}

port_offset=0
for port_count in 2 4 8 16; do
    for rep in $(seq 1 "$REPS"); do
        port_base=$((BASE_PORT + port_offset * 20))
        local_port_base=$((50000 + port_offset * 20))
        port_offset=$((port_offset + 1))
        run_cell "$port_count" "$rep" "$port_base" "$local_port_base"
    done
done

echo "[srt-scaling-sweep] ALL DONE"
