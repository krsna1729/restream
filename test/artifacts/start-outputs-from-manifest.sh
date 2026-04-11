#!/usr/bin/env bash
set -euo pipefail

API_URL="${API_URL:-http://localhost:3030}"
MANIFEST_PATH="${1:-test/artifacts/session-4x3-last.json}"

command -v curl >/dev/null || { echo "curl is required"; exit 1; }
command -v jq >/dev/null || { echo "jq is required"; exit 1; }
[[ -f "$MANIFEST_PATH" ]] || { echo "Manifest not found: $MANIFEST_PATH"; exit 1; }

echo "=== Starting outputs from $MANIFEST_PATH ==="
count=0
ok=0
MAX_RETRIES="${MAX_RETRIES:-30}"
RETRY_DELAY_SEC="${RETRY_DELAY_SEC:-1}"

while IFS='|' read -r pipeline_id output_id; do
  count=$((count + 1))
  attempt=0
  started=0

  while [[ "$attempt" -lt "$MAX_RETRIES" ]]; do
    attempt=$((attempt + 1))
    resp_file="$(mktemp)"
    status="$(curl -s -o "$resp_file" -w '%{http_code}' -X POST "$API_URL/pipelines/$pipeline_id/outputs/$output_id/start")"
    err_msg="$(jq -r '.error // empty' "$resp_file" 2>/dev/null || true)"

    if [[ "$status" == "200" || "$status" == "201" ]]; then
      ok=$((ok + 1))
      started=1
      echo "[$count] $pipeline_id/$output_id -> $status (attempt $attempt)"
      rm -f "$resp_file"
      break
    fi

    if [[ "$status" == "409" ]]; then
      if [[ "$err_msg" == *"already has a running job"* ]]; then
        ok=$((ok + 1))
        started=1
        echo "[$count] $pipeline_id/$output_id -> 409 already running (attempt $attempt)"
        rm -f "$resp_file"
        break
      fi
      if [[ "$err_msg" == *"input is not available yet"* ]]; then
        rm -f "$resp_file"
        sleep "$RETRY_DELAY_SEC"
        continue
      fi
    fi

    echo "[$count] $pipeline_id/$output_id -> $status (attempt $attempt)"
    cat "$resp_file" || true
    rm -f "$resp_file"
    break
  done

  if [[ "$started" -ne 1 ]]; then
    echo "[$count] $pipeline_id/$output_id failed to start after $MAX_RETRIES attempts"
  fi

done < <(jq -r '.pipelines[] | .pipelineId as $pid | .outputs[] | "\($pid)|\(.id)"' "$MANIFEST_PATH")

echo "Started/Already-running outputs: $ok/$count"
