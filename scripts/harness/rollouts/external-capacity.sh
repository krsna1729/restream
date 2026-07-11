#!/usr/bin/env bash
set -euo pipefail

repo_root="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$repo_root"

scenario=${EXTERNAL_CAPACITY_SCENARIO:-mixed.live.srt.h264.a2.bf0}
artifact_dir=${EXTERNAL_CAPACITY_ARTIFACT_DIR:-.local/artifacts/external-capacity-rollout}
pass_work_dir="$artifact_dir/capacity-ok"
constrained_work_dir="$artifact_dir/constrained"
constrained_log="$artifact_dir/constrained.log"

rm -rf "$pass_work_dir" "$constrained_work_dir"
mkdir -p "$artifact_dir"

echo "=== external-capacity-rollout: capacity-ok smoke ==="
env \
  RESTREAM_EXTERNAL_FFMPEG_PERMITS="${EXTERNAL_CAPACITY_OK_PERMITS:-2}" \
  ONLY_CHECKS="${EXTERNAL_CAPACITY_OK_CHECKS:-load,ffprobe}" \
  WORK_DIR="$pass_work_dir" \
  scripts/harness/run.sh "$scenario"

echo "=== external-capacity-rollout: constrained default checks ==="
set +e
env \
  RESTREAM_EXTERNAL_FFMPEG_PERMITS="${EXTERNAL_CAPACITY_CONSTRAINED_PERMITS:-1}" \
  WORK_DIR="$constrained_work_dir" \
  scripts/harness/run.sh "$scenario" >"$constrained_log" 2>&1
status=$?
set -e

if [[ "$status" -eq 0 ]]; then
  echo "external-capacity-rollout: constrained run unexpectedly passed" >&2
  exit 1
fi

if rg -q "no new metadata-identified recording" "$constrained_log"; then
  echo "external-capacity-rollout: constrained run failed before recording metadata was visible" >&2
  exit 1
fi

rg -q "blockedByPhase=waitingForCapacity" "$constrained_log"
rg -q "backend=externalFfmpeg waitMs=[1-9][0-9]*" "$constrained_log"

constrained_db=$(find "$constrained_work_dir" -name "${scenario}.db" -print -quit)
if [[ -z "$constrained_db" || ! -f "$constrained_db" ]]; then
  echo "external-capacity-rollout: constrained run did not produce DB for $scenario" >&2
  exit 1
fi

ready_recordings=$(
  sqlite3 "$constrained_db" \
    "select count(*) from recordings where status='ready' and temp_path is not null and final_path is not null;"
)

if [[ "$ready_recordings" -lt 1 ]]; then
  echo "external-capacity-rollout: constrained run did not persist a ready recording row" >&2
  exit 1
fi

echo "external-capacity-rollout: constrained run failed causally with persisted recording metadata"
