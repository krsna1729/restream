#!/usr/bin/env bash
set -euo pipefail

API_URL="${API_URL:-http://localhost:3030}"
OUTPUT_BASE_URL="${OUTPUT_BASE_URL:-rtmp://localhost:1936/live}"
LABEL_PREFIX="${LABEL_PREFIX:-Input}"
PIPELINE_PREFIX="${PIPELINE_PREFIX:-Pipeline}"

RUN_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
MANIFEST_PATH="${MANIFEST_PATH:-test/artifacts/session-4x3-last.json}"

command -v curl >/dev/null || { echo "curl is required"; exit 1; }
command -v jq >/dev/null || { echo "jq is required"; exit 1; }

tmp_manifest="$(mktemp)"
echo '{"generatedAt":"'"$RUN_TS"'","apiUrl":"'"$API_URL"'","pipelines":[]}' > "$tmp_manifest"

echo "=== Creating 4 stream keys and pipelines ==="
for i in 1 2 3 4; do
  key_resp="$(curl -sf -X POST "$API_URL/stream-keys" -H "Content-Type: application/json" -d "{\"label\":\"$LABEL_PREFIX $i\"}")"
  stream_key="$(echo "$key_resp" | jq -r '.streamKey.key')"

  pipe_resp="$(curl -sf -X POST "$API_URL/pipelines" -H "Content-Type: application/json" -d "{\"name\":\"$PIPELINE_PREFIX $i\",\"streamKey\":\"$stream_key\"}")"
  pipeline_id="$(echo "$pipe_resp" | jq -r '.pipeline.id')"

  outputs='[]'
  echo "Creating outputs for pipeline $i ($pipeline_id)"
  for j in 1 2 3; do
    out_resp="$(curl -sf -X POST "$API_URL/pipelines/$pipeline_id/outputs" -H "Content-Type: application/json" -d "{\"name\":\"Output $i-$j\",\"url\":\"$OUTPUT_BASE_URL/out${i}_${j}\",\"encoding\":\"copy\"}")"
    output_id="$(echo "$out_resp" | jq -r '.output.id')"
    outputs="$(echo "$outputs" | jq --arg id "$output_id" --arg name "Output $i-$j" --arg url "$OUTPUT_BASE_URL/out${i}_${j}" '. + [{id:$id,name:$name,url:$url,encoding:"copy"}]')"
    echo "  Created output $i-$j: $output_id"
  done

  next_manifest="$(mktemp)"
  jq --arg pid "$pipeline_id" --arg pName "$PIPELINE_PREFIX $i" --arg sk "$stream_key" --argjson outs "$outputs" '.pipelines += [{pipelineId:$pid,name:$pName,streamKey:$sk,outputs:$outs}]' "$tmp_manifest" > "$next_manifest"
  mv "$next_manifest" "$tmp_manifest"
done

mkdir -p "$(dirname "$MANIFEST_PATH")"
mv "$tmp_manifest" "$MANIFEST_PATH"

echo
echo "Saved manifest: $MANIFEST_PATH"
echo "Pipelines: $(jq '.pipelines | length' "$MANIFEST_PATH")"
echo "Outputs: $(jq '[.pipelines[].outputs[]] | length' "$MANIFEST_PATH")"
