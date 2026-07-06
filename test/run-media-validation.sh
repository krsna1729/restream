#!/usr/bin/env bash
set -euo pipefail

# Bounded validation for development machines and WSL:
# - real RTMP and SRT publishers/readers
# - 500 in-process readers
# - 32 loopback RTMP egress sessions

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

mkdir -p test/artifacts/latest
scripts/build-bench-harness.sh

: > test/artifacts/latest/run.log
# Canonical validation slices.
for mode in \
  mixed.live.rtmp.h264.a1.bf0 \
  mixed.live.srt.h265.a1.bf0 \
  mixed.live.rtmp.h264.a1.bf2
do
  echo "== $mode ==" | tee -a test/artifacts/latest/run.log
  target/bench/test_harness "$mode" \
    | tee -a test/artifacts/latest/run.log
done
