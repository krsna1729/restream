#!/usr/bin/env bash
# Builds the TCP/UDP/SRT scaling-comparison tools. Not part of the
# automated test suite or CI -- these are manual investigation tools, run
# on demand. See README.md in this directory and
# docs/agent-guidance/quality/srt-scaling-first-principles-investigation-2026-08-15.md
# for what they're for and what's still open.
set -euo pipefail
cd "$(dirname "$0")"

ROOT="$(git rev-parse --show-toplevel)"
BUILD_ROOT="${RESTREAM_BUILD_ROOT:-$ROOT/.local/build/static}"
PREFIX="$BUILD_ROOT/prefix"
if [[ ! -f "$PREFIX/lib/libsrt.a" ]]; then
    echo "error: $PREFIX/lib/libsrt.a not found -- run scripts/build/native-deps.sh first" >&2
    exit 1
fi

cc -O2 -pthread -I"$PREFIX/include" -o sender_bench sender_bench.c \
    -L"$PREFIX/lib" -l:libsrt.a -l:libmbedtls.a -l:libmbedx509.a -l:libmbedcrypto.a \
    -lstdc++ -lpthread -lm
cc -O2 -pthread -I"$PREFIX/include" -o sink_bench sink_bench.c \
    -L"$PREFIX/lib" -l:libsrt.a -l:libmbedtls.a -l:libmbedx509.a -l:libmbedcrypto.a \
    -lstdc++ -lpthread -lm
cc -O2 -pthread -o tcp_sender tcp_sender.c
cc -O2 -pthread -o tcp_sink tcp_sink.c
cc -O2 -pthread -o udp_sender udp_sender.c
cc -O2 -pthread -o udp_sink udp_sink.c

echo "built: sender_bench sink_bench tcp_sender tcp_sink udp_sender udp_sink"
