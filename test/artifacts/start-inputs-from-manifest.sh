#!/usr/bin/env bash
set -euo pipefail

MANIFEST_PATH="${1:-test/artifacts/session-4x3-last.json}"
INPUT_FILE="${INPUT_FILE:-test/colorbar-timer.mp4}"
RTMP_INGEST_BASE="${RTMP_INGEST_BASE:-rtmp://localhost:1935}"
LOG_DIR="${LOG_DIR:-test/artifacts/logs}"

command -v jq >/dev/null || { echo "jq is required"; exit 1; }
command -v ffmpeg >/dev/null || { echo "ffmpeg is required"; exit 1; }
[[ -f "$MANIFEST_PATH" ]] || { echo "Manifest not found: $MANIFEST_PATH"; exit 1; }
[[ -f "$INPUT_FILE" ]] || { echo "Input file not found: $INPUT_FILE"; exit 1; }

mkdir -p "$LOG_DIR"

expected_inputs="$(jq '.pipelines | length' "$MANIFEST_PATH")"
echo "=== Starting 1 ffmpeg multi-output publisher for $expected_inputs inputs from $MANIFEST_PATH ==="

mapfile -t stream_keys < <(jq -r '.pipelines[].streamKey' "$MANIFEST_PATH")
if [[ "${#stream_keys[@]}" -eq 0 ]]; then
  echo "No stream keys found in manifest: $MANIFEST_PATH"
  exit 1
fi

log_file="$LOG_DIR/input-tee.log"
echo "output targets: ${#stream_keys[@]}"
for idx in "${!stream_keys[@]}"; do
  n=$((idx + 1))
  echo "[$n] streamKey=${stream_keys[$idx]}"
done
echo "log=$log_file"

ffmpeg_args=(
  -nostdin
  -re
  -stream_loop -1
  -i "$INPUT_FILE"
)

for stream_key in "${stream_keys[@]}"; do
  ffmpeg_args+=(
    -map 0
    -c copy
    -f flv
    "$RTMP_INGEST_BASE/$stream_key"
  )
done

ffmpeg "${ffmpeg_args[@]}" < /dev/null > "$log_file" 2>&1 &
echo "$!" > "$LOG_DIR/input-tee.pid"

echo "Publisher started. PID file: $LOG_DIR/input-tee.pid"
