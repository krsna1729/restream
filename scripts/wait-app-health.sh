#!/usr/bin/env bash
set -euo pipefail

APP_HEALTH_URL="${1:-${APP_HEALTH_URL:-http://localhost:3030/healthz}}"
RETRIES="${2:-${VERIFY_APP_RETRIES:-30}}"

for i in $(seq 1 "$RETRIES"); do
  if curl -fsS "$APP_HEALTH_URL" >/dev/null; then
    echo "App is ready: $APP_HEALTH_URL"
    exit 0
  fi
  sleep 1
done

echo "App readiness did not become ready in time: $APP_HEALTH_URL"
exit 1
