#!/usr/bin/env bash
set -euo pipefail

# Bounded validation for development machines and WSL:
# - real RTMP and SRT publishers/readers
# - 500 in-process readers
# - 32 loopback RTMP egress sessions

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"

mkdir -p .local/artifacts/latest

: > .local/artifacts/latest/run.log
# Canonical validation slices.
for mode in \
  mixed.live.rtmp.h264.a1.bf0 \
  mixed.live.srt.h265.a1.bf0 \
  mixed.live.rtmp.h264.a1.bf2
do
  echo "== $mode ==" | tee -a .local/artifacts/latest/run.log
  scripts/harness/run.sh "$mode" \
    | tee -a .local/artifacts/latest/run.log
done
