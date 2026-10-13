#!/usr/bin/env bash
# `RESTREAM_BIN` shim for the Q-032 core-segregation A/B. `REAL_RESTREAM` is
# the binary to run.
#
# Segregated (default): each thread class gets its own CPU set.
#   CONTROL `SEG_CONTROL_CPUS` (default 0): `restream-tokio` and every other
#           thread the two classes below do not name.
#   MEDIA   `SEG_MEDIA_CPUS` (default 1): `restream-media`.
#   HOT     `SEG_HOT_CPUS` (default 2-7): egress shards (`egress-shard-<n>`,
#           including their SRT Owners), SRT ingress Owners (`srt-in-...`)
#           and RTMP ingress owners (`restream-rtmp-c...`).
# Restream boots on the union of the three sets in both arms, so every
# CPU-derived size (`HostCapacity`) is the same. Pools grow at run time and a
# thread takes its name after it starts, so the shim re-reads every thread's
# name each 0.5 s and re-pins a thread whenever its name changed (a reused
# thread ID counts as a new thread: the key includes its start time). Child
# processes (FFmpeg) keep the mask of the thread that spawned them. The
# packet contract's `restreamCpusAllowed` reads the main thread's mask, which
# this arm pins to CONTROL, so it shows CONTROL's set, not the union.
# Shared (`SEG_SHARED=1`, the A arm): every thread stays on the union.
# Run the harness itself outside these CPUs (for example
# `taskset -c 8-11 scripts/harness/run.sh ...`), so its peers do not compete.
set -euo pipefail
real=${REAL_RESTREAM:?REAL_RESTREAM is required}
control=${SEG_CONTROL_CPUS:-0}
media=${SEG_MEDIA_CPUS:-1}
hot=${SEG_HOT_CPUS:-2-7}
union="$control,$media,$hot"
if [[ ${SEG_SHARED:-0} != 1 ]]; then
  # This shell's pid becomes Restream's at the `exec` below, so the harness
  # measures Restream itself; the pinner runs beside it until it exits.
  target=$$
  (
    # Pin nothing until the exec happened and boot has sized every pool
    # from the union (`HostCapacity` reads the affinity mask once).
    binary=$(readlink -f "$real")
    until [[ $(readlink /proc/"$target"/exe 2>/dev/null) == "$binary" ]]; do
      kill -0 "$target" 2>/dev/null || exit 0
      sleep 0.05
    done
    sleep 2
    declare -A pinned
    while kill -0 "$target" 2>/dev/null; do
      for task in /proc/"$target"/task/*; do
        tid=${task##*/}
        comm=$(cat "$task/comm" 2>/dev/null) || continue
        stat=$(cat "$task/stat" 2>/dev/null) || continue
        read -ra fields <<<"${stat##*) }"
        key="${fields[19]}:$comm" # field 22, the thread's start time
        [[ ${pinned[$tid]:-} == "$key" ]] && continue
        case $comm in
          restream-media*) cpus=$media ;;
          egress-shard-* | srt-in-* | restream-rtmp-c*) cpus=$hot ;;
          *) cpus=$control ;;
        esac
        taskset -pc "$cpus" "$tid" >/dev/null 2>&1 && pinned[$tid]=$key
      done
      sleep 0.5
    done
  ) </dev/null >/dev/null 2>&1 &
fi
exec taskset -c "$union" "$real" "$@"
