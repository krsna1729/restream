#!/usr/bin/env bash
set -euo pipefail

# Generates the checked-in bench transport fixtures under
# `test/fixtures/transport/`, which is where `src/test_fixtures.rs`'s
# `bench_transport_fixture` reads them. Keep the two in sync: writing to
# `test/fixtures/` produced files the fixture API could not see.
#
# Bitrate labels are not encoder settings. `bench-h264-8m*` is the canonical
# WI3.4 contract workload and must carry ~8.0 Mbps of MPEG-TS payload
# (bytes over the media span, not the `-b:v` video target): x264's VBR output
# plus AAC plus TS overhead lands ~16% above `-b:v`, so 8 Mbps of payload
# needs `-b:v 6860k`. The 8 Mbps H.264 fixtures are asserted by
# `tests/fixtures.rs::canonical_8m_bench_fixtures_carry_the_stated_payload_rate`;
# re-tune the target there if a codec/toolchain update moves the effective
# rate. The 1.5M/4M H.264 and all H.265 rows remain encoder-shape fixtures
# (their labels approximate the video target, not the whole-file rate).

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
OUT="$ROOT/test/fixtures/transport"

mkdir -p "$OUT"

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i mandelbrot=size=1920x1080:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -t 8 -map 0:v -map 1:a \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 60 -bf 2 \
  -b:v 1500k -maxrate 1500k -bufsize 3000k \
  -c:a aac -b:a 64k \
  -f mpegts "$OUT/bench-h264-1_5m.ts"

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i mandelbrot=size=1920x1080:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -t 8 -map 0:v -map 1:a \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 60 -bf 2 \
  -b:v 4000k -maxrate 4000k -bufsize 8000k \
  -c:a aac -b:a 64k \
  -f mpegts "$OUT/bench-h264-4m.ts"

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i mandelbrot=size=1920x1080:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -t 8 -map 0:v -map 1:a \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 60 -bf 2 \
  -b:v 6860k -maxrate 6860k -bufsize 13720k \
  -c:a aac -b:a 64k \
  -f mpegts "$OUT/bench-h264-8m.ts"

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i mandelbrot=size=1920x1080:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -f lavfi -i sine=frequency=880:sample_rate=48000 \
  -filter_complex '[2:a]pan=stereo|c0=c0|c1=c0[a2]' \
  -t 8 -map 0:v -map 1:a -map '[a2]' \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 60 -bf 2 \
  -b:v 1500k -maxrate 1500k -bufsize 3000k \
  -c:a aac -b:a 64k \
  -f mpegts "$OUT/bench-h264-1_5m-2a.ts"

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i mandelbrot=size=1920x1080:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -f lavfi -i sine=frequency=880:sample_rate=48000 \
  -filter_complex '[2:a]pan=stereo|c0=c0|c1=c0[a2]' \
  -t 8 -map 0:v -map 1:a -map '[a2]' \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 60 -bf 2 \
  -b:v 4000k -maxrate 4000k -bufsize 8000k \
  -c:a aac -b:a 64k \
  -f mpegts "$OUT/bench-h264-4m-2a.ts"

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i mandelbrot=size=1920x1080:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -f lavfi -i sine=frequency=880:sample_rate=48000 \
  -filter_complex '[2:a]pan=stereo|c0=c0|c1=c0[a2]' \
  -t 8 -map 0:v -map 1:a -map '[a2]' \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 60 -bf 2 \
  -b:v 6860k -maxrate 6860k -bufsize 13720k \
  -c:a aac -b:a 64k \
  -f mpegts "$OUT/bench-h264-8m-2a.ts"

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i mandelbrot=size=1920x1080:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -t 8 -map 0:v -map 1:a \
  -c:v libx265 -preset ultrafast -tune zerolatency \
  -x265-params 'log-level=none:bitrate=1500:vbv-maxrate=1500:vbv-bufsize=3000:strict-cbr=1:keyint=60:min-keyint=60:no-scenecut=1' \
  -g 60 -bf 0 \
  -c:a aac -b:a 64k \
  -f mpegts "$OUT/bench-h265-1_5m.ts"

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i mandelbrot=size=1920x1080:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -t 8 -map 0:v -map 1:a \
  -c:v libx265 -preset ultrafast -tune zerolatency \
  -x265-params 'log-level=none:bitrate=4000:vbv-maxrate=4000:vbv-bufsize=8000:strict-cbr=1:keyint=60:min-keyint=60:no-scenecut=1' \
  -g 60 -bf 0 \
  -c:a aac -b:a 64k \
  -f mpegts "$OUT/bench-h265-4m.ts"

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i mandelbrot=size=1920x1080:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -t 8 -map 0:v -map 1:a \
  -c:v libx265 -preset ultrafast -tune zerolatency \
  -x265-params 'log-level=none:bitrate=8000:vbv-maxrate=8000:vbv-bufsize=16000:strict-cbr=1:keyint=60:min-keyint=60:no-scenecut=1' \
  -g 60 -bf 0 \
  -c:a aac -b:a 64k \
  -f mpegts "$OUT/bench-h265-8m.ts"

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i mandelbrot=size=1920x1080:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -f lavfi -i sine=frequency=880:sample_rate=48000 \
  -filter_complex '[2:a]pan=stereo|c0=c0|c1=c0[a2]' \
  -t 8 -map 0:v -map 1:a -map '[a2]' \
  -c:v libx265 -preset ultrafast -tune zerolatency \
  -x265-params 'log-level=none:bitrate=1500:vbv-maxrate=1500:vbv-bufsize=3000:strict-cbr=1:keyint=60:min-keyint=60:no-scenecut=1' \
  -g 60 -bf 0 \
  -c:a aac -b:a 64k \
  -f mpegts "$OUT/bench-h265-1_5m-2a.ts"

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i mandelbrot=size=1920x1080:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -f lavfi -i sine=frequency=880:sample_rate=48000 \
  -filter_complex '[2:a]pan=stereo|c0=c0|c1=c0[a2]' \
  -t 8 -map 0:v -map 1:a -map '[a2]' \
  -c:v libx265 -preset ultrafast -tune zerolatency \
  -x265-params 'log-level=none:bitrate=4000:vbv-maxrate=4000:vbv-bufsize=8000:strict-cbr=1:keyint=60:min-keyint=60:no-scenecut=1' \
  -g 60 -bf 0 \
  -c:a aac -b:a 64k \
  -f mpegts "$OUT/bench-h265-4m-2a.ts"

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i mandelbrot=size=1920x1080:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -f lavfi -i sine=frequency=880:sample_rate=48000 \
  -filter_complex '[2:a]pan=stereo|c0=c0|c1=c0[a2]' \
  -t 8 -map 0:v -map 1:a -map '[a2]' \
  -c:v libx265 -preset ultrafast -tune zerolatency \
  -x265-params 'log-level=none:bitrate=8000:vbv-maxrate=8000:vbv-bufsize=16000:strict-cbr=1:keyint=60:min-keyint=60:no-scenecut=1' \
  -g 60 -bf 0 \
  -c:a aac -b:a 64k \
  -f mpegts "$OUT/bench-h265-8m-2a.ts"
