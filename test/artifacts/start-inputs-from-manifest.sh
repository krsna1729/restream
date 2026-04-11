#!/usr/bin/env bash
set -euo pipefail

MANIFEST_PATH="${1:-test/artifacts/session-4x3-last.json}"
INPUT_FILE="${INPUT_FILE:-test/colorbar-timer.mp4}"
RTMP_INGEST_BASE="${RTMP_INGEST_BASE:-rtmp://localhost:1935}"
RTSP_INGEST_BASE="${RTSP_INGEST_BASE:-rtsp://localhost:8554}"
SRT_INGEST_BASE="${SRT_INGEST_BASE:-srt://localhost:8890?streamid=publish:}"
INPUT_PROTOCOLS="${INPUT_PROTOCOLS:-rtmp,rtsp,srt}"
LOG_DIR="${LOG_DIR:-test/artifacts/logs}"

command -v jq >/dev/null || { echo "jq is required"; exit 1; }
command -v ffmpeg >/dev/null || { echo "ffmpeg is required"; exit 1; }
[[ -f "$MANIFEST_PATH" ]] || { echo "Manifest not found: $MANIFEST_PATH"; exit 1; }
[[ -f "$INPUT_FILE" ]] || { echo "Input file not found: $INPUT_FILE"; exit 1; }

mkdir -p "$LOG_DIR"

expected_inputs="$(jq '.pipelines | length' "$MANIFEST_PATH")"
echo "=== Starting $expected_inputs ffmpeg input publishers from $MANIFEST_PATH ==="

mapfile -t stream_keys < <(jq -r '.pipelines[].streamKey' "$MANIFEST_PATH")
if [[ "${#stream_keys[@]}" -eq 0 ]]; then
  echo "No stream keys found in manifest: $MANIFEST_PATH"
  exit 1
fi

IFS=',' read -r -a protocols <<< "$INPUT_PROTOCOLS"
if [[ "${#protocols[@]}" -eq 0 ]]; then
  echo "No input protocols configured"
  exit 1
fi

build_target_url() {
  local protocol="$1"
  local stream_key="$2"
  case "$protocol" in
    rtmp)
      printf '%s/%s' "$RTMP_INGEST_BASE" "$stream_key"
      ;;
    rtsp)
      printf '%s/%s' "$RTSP_INGEST_BASE" "$stream_key"
      ;;
    srt)
      printf '%s%s' "$SRT_INGEST_BASE" "$stream_key"
      ;;
    *)
      echo "Unsupported input protocol: $protocol" >&2
      return 1
      ;;
  esac
}

normalize_protocol() {
  local value="$1"
  value="$(printf '%s' "$value" | tr '[:upper:]' '[:lower:]')"
  value="${value#${value%%[![:space:]]*}}"
  value="${value%${value##*[![:space:]]}}"
  printf '%s' "$value"
}

for idx in "${!stream_keys[@]}"; do
  n=$((idx + 1))
  stream_key="${stream_keys[$idx]}"
  protocol_index=$((idx % ${#protocols[@]}))
  protocol="${protocols[$protocol_index]}"
  protocol="$(normalize_protocol "$protocol")"
  target_url="$(build_target_url "$protocol" "$stream_key")"
  log_file="$LOG_DIR/input-$n-$protocol.log"
  pid_file="$LOG_DIR/input-$n.pid"

  case "$protocol" in
    rtmp)
      ffmpeg_args=(
        -nostdin
        -re
        -stream_loop -1
        -i "$INPUT_FILE"
        -map 0
        -c copy
        -f flv
        "$target_url"
      )
      ;;
    rtsp)
      ffmpeg_args=(
        -nostdin
        -re
        -stream_loop -1
        -i "$INPUT_FILE"
        -map 0
        -c copy
        -f rtsp
        -rtsp_transport tcp
        "$target_url"
      )
      ;;
    srt)
      ffmpeg_args=(
        -nostdin
        -re
        -stream_loop -1
        -i "$INPUT_FILE"
        -map 0
        -c copy
        -f mpegts
        "$target_url"
      )
      ;;
  esac

  echo "[$n/$expected_inputs] protocol=$protocol streamKey=$stream_key target=$target_url"
  ffmpeg "${ffmpeg_args[@]}" < /dev/null > "$log_file" 2>&1 &
  echo "$!" > "$pid_file"
  echo "  pid=$(cat "$pid_file") log=$log_file"
done

echo "All input publishers started. PID files: $LOG_DIR/input-*.pid"
