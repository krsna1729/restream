#!/usr/bin/env bash
set -euo pipefail

APP_PORT="${APP_PORT:-3030}"
VERIFY_APP_RETRIES="${VERIFY_APP_RETRIES:-30}"
MEDIAMTX_API_URL="${MEDIAMTX_API_URL:-http://localhost:9997}"

docker compose down --remove-orphans -v
docker compose up -d --build --force-recreate --renew-anon-volumes mediamtx nginx-rtmp app

"$(dirname "$0")/wait-mediamtx.sh" "$MEDIAMTX_API_URL"

for i in $(seq 1 "$VERIFY_APP_RETRIES"); do
  if docker compose exec -T app node -e "fetch('http://localhost:${APP_PORT}/health').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(2))"; then
    echo "app backend is ready (service: app)"
    docker compose ps
    exit 0
  fi
  sleep 1
done

echo "app backend did not become ready in time"
docker compose logs app --tail=60 || true
exit 1
