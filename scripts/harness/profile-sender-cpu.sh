#!/usr/bin/env bash
# On-CPU/off-CPU profile of one pinned sender CPU, with host state restored.
#
# The substrate and WI3.6 ladders measure CPU per datagram; when a stage is
# CPU-bound and the reason is not obvious, this is the next step (Brendan Gregg
# style): sample the pinned CPU, fold the stacks, and report where the samples
# land. It raises `kernel.perf_event_paranoid` only for the duration of the run
# and restores the previous value on every exit path, so a measurement host is
# left as it was found.
#
# Usage:
#   scripts/harness/profile-sender-cpu.sh <cpu> <seconds> <out-dir> [perf args...]
#
# Example (sender pinned to CPU 0, 10 s window, folded output in .local/artifacts):
#   scripts/harness/profile-sender-cpu.sh 0 10 .local/artifacts/wi3-prof
set -euo pipefail

CPU="${1:?usage: profile-sender-cpu.sh <cpu> <seconds> <out-dir> [perf args...]}"
SECS="${2:?seconds}"
OUT="${3:?out dir}"
shift 3

PARANOID_FILE=/proc/sys/kernel/perf_event_paranoid
ORIGINAL="$(cat "$PARANOID_FILE")"

restore() {
  if [[ "$(cat "$PARANOID_FILE" 2>/dev/null || echo)" != "$ORIGINAL" ]]; then
    sudo -n sysctl -qw "kernel.perf_event_paranoid=${ORIGINAL}" 2>/dev/null \
      || echo "[profile] WARNING: could not restore perf_event_paranoid=${ORIGINAL}" >&2
    echo "[profile] restored kernel.perf_event_paranoid=${ORIGINAL}"
  fi
}
trap restore EXIT INT TERM

if [[ "$ORIGINAL" -gt 1 ]]; then
  sudo -n sysctl -qw kernel.perf_event_paranoid=1
  echo "[profile] kernel.perf_event_paranoid ${ORIGINAL} -> 1 (restored on exit)"
fi

mkdir -p "$OUT"
DATA="$OUT/perf.data"
sudo -n rm -f "$DATA"
sudo -n perf record -C "$CPU" -F 997 -g -o "$DATA" "$@" -- sleep "$SECS"
sudo -n chmod 644 "$DATA"
sudo -n perf script -i "$DATA" > "$OUT/perf-script.txt"
sudo -n perf report --stdio -i "$DATA" --no-children --sort=symbol --percent-limit 0.5 \
  > "$OUT/perf-report.txt" 2>/dev/null || true

# Folded stacks (flamegraph-equivalent, text) plus an inclusive-cost summary.
python3 - "$OUT" <<'PY'
import collections, pathlib, re, sys
out = pathlib.Path(sys.argv[1])
stacks = collections.Counter()
cur = []
def flush():
    if cur:
        stacks[";".join(reversed(cur))] += 1
        cur.clear()
for line in (out / "perf-script.txt").read_text(errors="replace").splitlines():
    if not line.strip():
        flush(); continue
    indented = line[0].isspace()
    m = re.search(r'^\s+[0-9a-f]+\s+(\S+)\s+\((\S+)\)', line) if indented \
        else re.search(r'^\S+\s+\d+.*?:\s+[0-9a-f]+\s+(\S+)\s+\((\S+)\)', line)
    if not m:
        continue
    if not indented:
        flush()
    cur.append(re.sub(r'\+0x[0-9a-f]+$', '', m.group(1)))
flush()
total = sum(stacks.values()) or 1
with (out / "perf-folded.txt").open("w") as handle:
    for stack, count in stacks.most_common():
        handle.write(f"{stack} {count}\n")
print(f"[profile] samples={total} folded={out/'perf-folded.txt'}")
print(f"[profile] top stacks:")
for stack, count in stacks.most_common(8):
    print(f"  {100*count/total:6.2f}%  " + " <- ".join(reversed(stack.split(';')[-8:])))
PY
