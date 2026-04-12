#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

docker compose --profile host up -d mediamtx nginx-rtmp
"$ROOT_DIR/scripts/ensure-deps.sh"
npm run dev &
app_pid=$!

"$ROOT_DIR/scripts/wait-app-health.sh" "${APP_HEALTH_URL:-http://localhost:3030/healthz}" "${VERIFY_APP_RETRIES:-30}"

wait "$app_pid"
