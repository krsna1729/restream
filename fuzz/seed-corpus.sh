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

# hls_playlist: a path selector byte, then a media or master playlist.
out=corpus/hls_playlist
mkdir -p "$out"
i=0
for playlist in \
  '#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:7\n#EXTINF:2.000,\nindex7.ts\n#EXT-X-DISCONTINUITY\n#EXTINF:1.5,\n../v1/index8.ts\n#EXT-X-ENDLIST\n' \
  '#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000,CODECS="avc1.64001f,mp4a.40.2"\nlow/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2500000\nhigh/index.m3u8\n'; do
  for selector in 00 01; do
    { printf "\\x$selector"; printf "$playlist"; } > "$out/seed-$i-$selector"
  done
  i=$((i + 1))
done
