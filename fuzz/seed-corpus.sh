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

# fmp4_ingest: the init segment's length (2 bytes, big-endian), the init,
# then one media segment, as FFmpeg's fMP4 HLS muxer writes them
# (`styp sidx moof mdat`): the canonical H.264 fixture re-encoded tiny, with
# B-frames (composition offsets), so seeds stay a few KiB.
out=corpus/fmp4_ingest
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
ffmpeg -hide_banner -loglevel error -nostdin -i ../test/fixtures/transport/correctness-h264.ts \
  -t 1 -vf scale=64:36 -c:v libx264 -preset ultrafast -bf 2 -g 6 -c:a aac -b:a 24k \
  -f hls -hls_segment_type fmp4 -hls_time 0.2 -hls_list_size 0 "$work/index.m3u8"
init_len=$(wc -c < "$work/init.mp4")
for segment in "$work"/index*.m4s; do
  name=$(basename "$segment" .m4s)
  { printf "\\x$(printf '%02x' $((init_len >> 8)))\\x$(printf '%02x' $((init_len & 255)))"
    cat "$work/init.mp4" "$segment"; } > "$out/seed-$name"
done

# moq_ingest: the catalog's length (2 bytes, big-endian), a hang catalog
# (H.264 with inline parameter sets, AAC-LC 48 kHz stereo), then frames as
# (track byte, 2-byte big-endian length, frame): an IDR access unit with
# its SPS/PPS and an AAC frame, each behind a legacy varint timestamp.
out=corpus/moq_ingest
mkdir -p "$out"
catalog='{"video":{"renditions":{"video":{"codec":"avc1.42001e","codedWidth":64,"codedHeight":36,"container":{"kind":"legacy"}}}},"audio":{"renditions":{"audio":{"codec":"mp4a.40.2","sampleRate":48000,"numberOfChannels":2,"description":"1190","container":{"kind":"legacy"}}}}}'
catalog_len=${#catalog}
video='\x00\x00\x00\x00\x01\x67\x42\x00\x1e\xf4\x05\x01\xec\x80\x00\x00\x00\x01\x68\xce\x06\xe2\x00\x00\x00\x01\x65\x88\x84\x00'
{ printf "\\x$(printf '%02x' $((catalog_len >> 8)))\\x$(printf '%02x' $((catalog_len & 255)))"
  printf '%s' "$catalog"
  printf '\x00\x00\x1e'; printf "$video"
  printf '\x01\x00\x05\x00\x21\x10\x05\x00'; } > "$out/seed-av"

# The catalog that panicked hang 0.21.16's H.264 codec parser (`avc1.4Ψ01e`
# is six bytes but five characters), from the CI fuzz smoke crash.
printf '%s' 'AQt7InZpZGVvIjp7InJlbmRpdGlvbnMiOnsiK3ZpZGVvIjp7ImNvZGVjIjoiYXZjMS40zqgwMWUiLCJjb2RlZFdpZHRoIjo2NCwiY29kZWRIZWlnaHQiOjM2LCJmcmE7ZXJhdGUiAAAAAACQADA6eyJraW5kIjoibGVvYWN5In19fXssIm51bWJlck9mQ2hhBwBubmVscyI69////2VzY2FyY2hpdmU6OiIwNTk1IiwiY29udGFpbg==' \
  | base64 -d > "$out/regression-h264-non-ascii-codec"
# MSF-only publishers: both catalog drafts reach the upstream MSF -> hang
# mapping, then production LOC video/AAC conversion (no legacy timestamp).
for version in '1' '"draft-01"'; do
  for codec in avc3.42001e hev1.1.6.L93.B0; do
    catalog="{\"version\":$version,\"tracks\":[{\"name\":\"video\",\"packaging\":\"loc\",\"role\":\"video\",\"codec\":\"$codec\"},{\"name\":\"audio\",\"packaging\":\"loc\",\"role\":\"audio\",\"codec\":\"mp4a.40.2\",\"samplerate\":48000,\"channelConfig\":\"2\"}]}"
    catalog_len=${#catalog}
    if [[ "$codec" == avc3.* ]]; then
      video='\x02\x10\x00\x00\x00\x00\x01\x67\x42\x00\x1e\xf4\x05\x01\xec\x80\x00\x00\x00\x01\x68\xce\x06\xe2\x00\x00\x00\x01\x65\x88\x84\x00'
      video_len=32
    else
      video='\x02\x10\x00\x00\x00\x00\x01\x26\x01\xaf'
      video_len=10
    fi
    { printf "\\x$(printf '%02x' $((catalog_len >> 8)))\\x$(printf '%02x' $((catalog_len & 255)))"
      printf '%s' "$catalog"
      printf "\\x00\\x00\\x$(printf '%02x' "$video_len")"; printf "$video"
      printf '\x01\x00\x05\x02\x10\x00\x21\x10'
      # Valid timescale override (90 kHz, one second), then zero
      # timescale, missing timestamp and truncated property block.
      printf '\x01\x00\x0d\x0a\x08\x80\x01\x5f\x90\x08\x80\x01\x5f\x90\x21\x10'
      printf '\x01\x00\x05\x04\x08\x00\x08\x01'
      printf '\x00\x00\x01\x00'
      printf '\x00\x00\x03\x05\x10\x01'
    } > "$out/seed-msf-$version-$codec"
  done
done

# Shared draft-01 initialization resolves before upstream AAC conversion.
# The 65-track input crosses MAX_TRACKS without a large corpus allocation.
for track_count in 2 65; do
  tracks=''
  for ((index = 0; index < track_count; index++)); do
    if [[ -n "$tracks" ]]; then tracks+=','; fi
    tracks+="{\"name\":\"audio-$index\",\"packaging\":\"loc\",\"role\":\"audio\",\"codec\":\"mp4a.40.2\",\"initRef\":\"aac\"}"
  done
  catalog="{\"version\":\"draft-01\",\"initDataList\":[{\"id\":\"aac\",\"type\":\"inline\",\"data\":\"EZA=\"}],\"tracks\":[$tracks]}"
  catalog_len=${#catalog}
  { printf "\\x$(printf '%02x' $((catalog_len >> 8)))\\x$(printf '%02x' $((catalog_len & 255)))"
    printf '%s' "$catalog"
    printf '\x00\x00\x05\x02\x10\x00\x21\x10'
    printf '\x01\x00\x05\x02\x10\x00\x21\x10'
  } > "$out/seed-msf-shared-init-$track_count"
done
