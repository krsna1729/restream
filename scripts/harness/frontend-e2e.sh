#!/usr/bin/env bash
# Run the complete browser integration suite against an isolated local app.
#
# This is the canonical replacement for manually starting Restream, guessing
# which media to copy, and then invoking Playwright. It owns only its dedicated
# .local/e2e work root and the process it starts.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
WORK_ROOT="${RESTREAM_E2E_WORK_ROOT:-$ROOT/.local/e2e}"
FIXTURE="$ROOT/test/fixtures/media-library/colorbar-timer-2v16a.mp4"
APP="$ROOT/target/debug/restream"
APP_PID=""

cleanup() {
    if [[ -n "$APP_PID" ]] && kill -0 "$APP_PID" 2>/dev/null; then
        kill "$APP_PID" 2>/dev/null || true
        wait "$APP_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

if [[ "${RESTREAM_E2E_SKIP_BUILD:-0}" != "1" ]]; then
    # The SBOM belongs to a distributable build. Regenerating it for an
    # isolated browser test dirties the worktree without testing a different
    # artifact, so keep this development build side-effect free.
    (cd "$ROOT" && npm run build:frontend)
    RESTREAM_SKIP_SBOM=1 "$ROOT/scripts/build/app-native.sh"
fi

if [[ ! -x "$APP" ]]; then
    echo "frontend-e2e: expected app binary at $APP" >&2
    exit 1
fi
if [[ ! -f "$FIXTURE" ]]; then
    echo "frontend-e2e: checked-in fixture missing: $FIXTURE" >&2
    exit 1
fi

rm -rf "$WORK_ROOT"
mkdir -p "$WORK_ROOT/media" "$WORK_ROOT/logs" "$WORK_ROOT/moq"
cp "$FIXTURE" "$WORK_ROOT/media/"

# The MoQ listener (origin for the MoQ preview): a short-lived ECDSA
# self-signed certificate, which browsers accept pinned by hash (WebTransport
# allows at most 14 days). Dual-stack so the default ingest host `localhost`
# reaches it whichever address it resolves to.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
    -days 10 -subj "/CN=localhost" \
    -addext "subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1" \
    -keyout "$WORK_ROOT/moq/key.pem" -out "$WORK_ROOT/moq/cert.pem" 2>/dev/null

RESTREAM_DB_PATH="$WORK_ROOT/restream.db" \
RESTREAM_MEDIA_DIR="$WORK_ROOT/media" \
RESTREAM_LOG_DIR="$WORK_ROOT/logs" \
RESTREAM_INITIAL_ADMIN_PASSWORD=admin \
RESTREAM_MOQ_INGEST_LISTEN="[::]:4443" \
RESTREAM_MOQ_TLS_CERT="$WORK_ROOT/moq/cert.pem" \
RESTREAM_MOQ_TLS_KEY="$WORK_ROOT/moq/key.pem" \
"$APP" >"$WORK_ROOT/restream.log" 2>&1 &
APP_PID=$!

for _ in $(seq 1 30); do
    # Connection refusal is expected while the owned app starts; only the
    # eventual timeout should be user-visible, together with the app log.
    if curl -fsS http://127.0.0.1:3030/healthz >/dev/null 2>&1; then
        cd "$ROOT"
        npx playwright test "$@"
        exit $?
    fi
    sleep 1
done

echo "frontend-e2e: app did not become healthy; log follows:" >&2
cat "$WORK_ROOT/restream.log" >&2 || true
exit 1
