#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

continue_on_fail=${INTERNAL_BACKEND_SMOKE_CONTINUE_ON_FAIL:-0}
dry_run=${INTERNAL_BACKEND_SMOKE_DRY_RUN:-0}

failures=0

run_case() {
  local label=$1
  shift

  echo "=== internal-backend-smoke: $label ==="
  printf 'command:'
  printf ' %q' "$@"
  printf '\n'

  if [[ "$dry_run" == "1" || "$dry_run" == "true" ]]; then
    return 0
  fi

  if "$@"; then
    return 0
  fi

  failures=$((failures + 1))
  if [[ "$continue_on_fail" != "1" && "$continue_on_fail" != "true" ]]; then
    exit 1
  fi
}

run_case "internal video preset timestamp/file loop" \
  env RESTREAM_INTERNAL_VIDEO_PRESETS=1 \
    ONLY_CHECKS=ffprobe,decode-scan \
    scripts/run-bench-harness.sh mixed.asset.file.h264.a1.bf0

run_case "internal video preset live startup" \
  env RESTREAM_INTERNAL_VIDEO_PRESETS=1 \
    ONLY_CHECKS=load,ffprobe \
    scripts/run-bench-harness.sh mixed.live.srt.h264.a1.bf0

run_case "internal HEVC-to-H264 codec edge" \
  env RESTREAM_INTERNAL_VIDEO_PRESETS=0 \
    RESTREAM_INTERNAL_HEVC_TO_H264=1 \
    ONLY_CHECKS=load,ffprobe,stage-sharing \
    scripts/run-bench-harness.sh mixed.live.srt.h265.a2.bf2

if [[ "$failures" -gt 0 ]]; then
  echo "internal-backend-smoke: $failures case(s) failed" >&2
  exit 1
fi

echo "internal-backend-smoke: all cases passed"
