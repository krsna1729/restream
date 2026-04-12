#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

APP_RETRIES="${VERIFY_APP_RETRIES:-30}"
APP_HEALTH_URL="${APP_HEALTH_URL:-http://localhost:3030/healthz}"

docker compose --profile container up -d --build --force-recreate --renew-anon-volumes pause mediamtx-pod nginx-rtmp app
"$ROOT_DIR/scripts/wait-app-health.sh" "$APP_HEALTH_URL" "$APP_RETRIES"
echo "Container app is ready: $APP_HEALTH_URL"
docker compose ps
