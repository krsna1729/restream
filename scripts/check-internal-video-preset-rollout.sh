#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

baseline=${INTERNAL_VIDEO_PRESET_RSS_BASELINE:-test/harness/baselines/internal-video-presets-rss.csv}
threshold=${INTERNAL_VIDEO_PRESET_RSS_THRESHOLD_PCT:-20}
checks=${INTERNAL_VIDEO_PRESET_CHECKS:-load,ffprobe,decode-scan}

scenarios=(
  mixed.live.srt.h264.a1.bf0
  mixed.live.srt.h264.a1.bf2
  mixed.live.srt.h264.a2.bf0
  mixed.live.srt.h264.a2.bf2
)

for scenario in "${scenarios[@]}"; do
  echo "=== internal-video-preset-rollout: $scenario ==="
  env \
    RESTREAM_INTERNAL_VIDEO_PRESETS=1 \
    ONLY_CHECKS="$checks" \
    RSS_BASELINE="$baseline" \
    RSS_BASELINE_THRESHOLD_PCT="$threshold" \
    scripts/run-bench-harness.sh "$scenario"
done
