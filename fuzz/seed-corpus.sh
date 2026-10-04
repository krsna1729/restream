#!/usr/bin/env bash
# Seeds fuzz/corpus/<target> from checked-in fixtures, so coverage-guided
# fuzzing starts from valid streams instead of random bytes (a random input
# almost never carries a valid PAT and PMT, so PES and probe parsing would
# stay unexplored). Each seed is a mode byte (see the target) followed by the
# start of a fixture: 20 TS packets carry the PAT, PMT and first PES.
set -euo pipefail
cd "$(dirname "$0")"
out=corpus/ts_demux
mkdir -p "$out"
for fixture in ../test/fixtures/transport/av-marker-*.ts; do
  name=$(basename "$fixture" .ts)
  for mode in 01 0e; do
    { printf "\\x$mode"; head -c 3760 "$fixture"; } > "$out/seed-$name-$mode"
  done
done
