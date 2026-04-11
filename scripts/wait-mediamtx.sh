#!/usr/bin/env bash
set -euo pipefail

MEDIAMTX_API_URL="${1:-${MEDIAMTX_API_URL:-http://localhost:9997}}"
RETRIES="${2:-${VERIFY_MEDIAMTX_RETRIES:-15}}"

for i in $(seq 1 "$RETRIES"); do
  if curl -fsS "$MEDIAMTX_API_URL/v3/config/global/get" >/dev/null; then
    echo "MediaMTX is ready: $MEDIAMTX_API_URL"
    exit 0
  fi
  sleep 1
done

echo "MediaMTX API did not become ready in time: $MEDIAMTX_API_URL"
exit 1
