#!/usr/bin/env bash
set -euo pipefail

repo_root="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$repo_root"

continue_on_fail=${INTERNAL_BACKEND_SMOKE_CONTINUE_ON_FAIL:-0}
dry_run=${INTERNAL_BACKEND_SMOKE_DRY_RUN:-0}
artifact_dir=${INTERNAL_BACKEND_SMOKE_ARTIFACT_DIR:-.local/artifacts/internal-backend-smoke}
summary_tsv="$artifact_dir/summary.tsv"

unexpected_failures=0
allowed_failures=0

mkdir -p "$artifact_dir"
printf 'label\tstatus\tallowed_failure\treason\tcommand\n' >"$summary_tsv"

run_case() {
  local label=$1
  local allowed_failure=$2
  local reason=$3
  shift
  shift
  shift

  echo "=== internal-backend-smoke: $label ==="
  printf 'command:'
  printf ' %q' "$@"
  printf '\n'

  local command
  printf -v command '%q ' "$@"
  command=${command% }

  if [[ "$dry_run" == "1" || "$dry_run" == "true" ]]; then
    printf '%s\t%s\t%s\t%s\t%s\n' \
      "$label" "dry-run" "$allowed_failure" "$reason" "$command" >>"$summary_tsv"
    return 0
  fi

  if "$@"; then
    printf '%s\t%s\t%s\t%s\t%s\n' \
      "$label" "passed" "$allowed_failure" "$reason" "$command" >>"$summary_tsv"
    return 0
  fi

  if [[ "$allowed_failure" == "1" || "$allowed_failure" == "true" ]]; then
    allowed_failures=$((allowed_failures + 1))
    printf '%s\t%s\t%s\t%s\t%s\n' \
      "$label" "allowed-failure" "$allowed_failure" "$reason" "$command" >>"$summary_tsv"
  else
    unexpected_failures=$((unexpected_failures + 1))
    printf '%s\t%s\t%s\t%s\t%s\n' \
      "$label" "unexpected-failure" "$allowed_failure" "$reason" "$command" >>"$summary_tsv"
  fi

  if [[ "$continue_on_fail" != "1" && "$continue_on_fail" != "true" ]]; then
    exit 1
  fi
}

run_case "internal video preset timestamp/file loop" \
  false \
  "Phase 16 file-loop timestamp proof is green" \
  env RESTREAM_INTERNAL_VIDEO_PRESETS=1 \
    ONLY_CHECKS=ffprobe,decode-scan \
    scripts/harness/run.sh mixed.asset.file.h264.a1.bf0

run_case "internal video preset live startup" \
  false \
  "Phase 16 SRT decode-scan matrix and RSS proofs are green" \
  env RESTREAM_INTERNAL_VIDEO_PRESETS=1 \
    ONLY_CHECKS=load,ffprobe,decode-scan \
    scripts/harness/run.sh mixed.live.srt.h264.a1.bf0

run_case "internal HEVC-to-H264 codec edge" \
  false \
  "Phase 16 HEVC RTMP selected-audio decode-scan proof is green" \
  env RESTREAM_INTERNAL_VIDEO_PRESETS=0 \
    RESTREAM_INTERNAL_HEVC_TO_H264=1 \
    ONLY_CHECKS=load,ffprobe,decode-scan,stage-sharing \
    scripts/harness/run.sh mixed.live.srt.h265.a2.bf2

echo "internal-backend-smoke: wrote $summary_tsv"

if [[ "$unexpected_failures" -gt 0 ]]; then
  echo "internal-backend-smoke: $unexpected_failures unexpected failure(s)" >&2
  exit 1
fi

if [[ "$allowed_failures" -gt 0 ]]; then
  echo "internal-backend-smoke: $allowed_failures allowed failure(s)"
else
  echo "internal-backend-smoke: all cases passed"
fi
