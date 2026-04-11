#!/usr/bin/env bash
set -euo pipefail

API_URL="${API_URL:-http://localhost:3030}"
MANIFEST_PATH="${MANIFEST_PATH:-test/artifacts/session-4x3-last.json}"
TIMEOUT_SEC="${TIMEOUT_SEC:-180}"
POLL_SEC="${POLL_SEC:-2}"

command -v curl >/dev/null || { echo "curl is required"; exit 1; }
command -v jq >/dev/null || { echo "jq is required"; exit 1; }

if [[ ! -f "$MANIFEST_PATH" ]]; then
  echo "Manifest not found: $MANIFEST_PATH"
  exit 1
fi

expected_inputs="$(jq '.pipelines | length' "$MANIFEST_PATH")"
expected_outputs="$(jq '[.pipelines[].outputs[]] | length' "$MANIFEST_PATH")"

echo "Waiting for all streams green (inputs=$expected_inputs outputs=$expected_outputs)"

deadline=$(( $(date +%s) + TIMEOUT_SEC ))
while true; do
  now="$(date +%s)"
  if (( now > deadline )); then
    echo "Timed out waiting for all streams to become green"
    if [[ -n "${health_json:-}" ]]; then
      echo "---- Input status summary ----"
      printf '%s' "$health_json" | jq -r '.pipelines | to_entries[] | "\(.key) input=\(.value.input.status) online=\(.value.input.online) ready=\(.value.input.ready) readers=\(.value.input.readers)"'
      echo "---- Output mismatch details (non-on only) ----"
      printf '%s' "$health_json" | jq -r '
        .pipelines
        | to_entries[] as $p
        | $p.value.outputs
        | to_entries[]
        | select(.value.status != "on")
        | "\($p.key)/\(.key) status=\(.value.status) jobStatus=\(.value.jobStatus // "null") jobId=\(.value.jobId // "null") bytesIn=\(.value.bytesReceived // 0) bytesOut=\(.value.bytesSent // 0) remote=\(.value.remoteAddr // "null")"
      '
    fi
    exit 1
  fi

  health_json="$(curl -sf "$API_URL/health")" || {
    sleep "$POLL_SEC"
    continue
  }

  # /health uses an outputs object map keyed by outputId per pipeline.
  input_on="$(printf '%s' "$health_json" | jq '[.pipelines[] | select(.input.status=="on")] | length')"
  input_warning="$(printf '%s' "$health_json" | jq '[.pipelines[] | select(.input.status=="warning")] | length')"
  output_active="$(printf '%s' "$health_json" | jq '[.pipelines[].outputs | to_entries[] | select(.value.status=="on" or .value.status=="warning")] | length')"
  output_on="$(printf '%s' "$health_json" | jq '[.pipelines[].outputs | to_entries[] | select(.value.status=="on")] | length')"
  output_warning="$(printf '%s' "$health_json" | jq '[.pipelines[].outputs | to_entries[] | select(.value.status=="warning")] | length')"

  echo "Status now: inputs on=$input_on/$expected_inputs warning=$input_warning | outputs on=$output_on/$expected_outputs warning=$output_warning active=$output_active/$expected_outputs"

  if [[ "$input_on" -eq "$expected_inputs" && "$output_on" -eq "$expected_outputs" ]]; then
    echo "All expected inputs and outputs are green (on)"
    break
  fi

  sleep "$POLL_SEC"
done
