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

# http_response: one read per response (length byte, then the bytes).
out=corpus/http_response
mkdir -p "$out"
i=0
for response in \
  'HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok' \
  'HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n' \
  'HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n' \
  'HTTP/1.0 503 Service Unavailable\r\n\r\nbusy' \
  'HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n'; do
  len=$(printf "$response" | wc -c)
  { printf "\\x$(printf '%02x' "$len")"; printf "$response"; } > "$out/seed-$i"
  i=$((i + 1))
done
