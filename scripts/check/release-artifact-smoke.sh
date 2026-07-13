#!/usr/bin/env bash
# Prove the downloadable binary bundle starts outside the source tree and serves
# embedded frontend assets. This catches releases that only worked because
# `public/` or other build-tree files happened to be present in CI.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"

BUNDLE="${1:-}"
if [[ -z "$BUNDLE" ]]; then
    echo "usage: scripts/check/release-artifact-smoke.sh <restream-*-linux-x86_64.tar.gz>" >&2
    exit 2
fi
[[ -s "$BUNDLE" ]] || {
    echo "release-artifact-smoke: bundle not found: $BUNDLE" >&2
    exit 1
}

for command in curl python3 tar; do
    command -v "$command" >/dev/null || {
        echo "release-artifact-smoke: required command not found: $command" >&2
        exit 1
    }
done

tmp="$(mktemp -d)"
app_pid=""
cleanup() {
    if [[ -n "$app_pid" ]] && kill -0 "$app_pid" 2>/dev/null; then
        kill "$app_pid" 2>/dev/null || true
        wait "$app_pid" 2>/dev/null || true
    fi
    rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

tar -xzf "$BUNDLE" -C "$tmp"
mapfile -t roots < <(find "$tmp" -mindepth 1 -maxdepth 1 -type d -print | sort)
if [[ "${#roots[@]}" -ne 1 ]]; then
    echo "release-artifact-smoke: expected one top-level directory in $BUNDLE" >&2
    exit 1
fi
stage="${roots[0]}"
run="$stage/run"
[[ -x "$run" ]] || {
    echo "release-artifact-smoke: missing executable bundle runner: $run" >&2
    exit 1
}

port="$(python3 - <<'PY'
import socket
with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
    s.bind(("127.0.0.1", 0))
    print(s.getsockname()[1])
PY
)"
rtmp_port="$(python3 - <<'PY'
import socket
with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
    s.bind(("127.0.0.1", 0))
    print(s.getsockname()[1])
PY
)"
srt_port="$(python3 - <<'PY'
import socket
with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
    s.bind(("127.0.0.1", 0))
    print(s.getsockname()[1])
PY
)"

runtime="$tmp/runtime"
mkdir -p "$runtime/media" "$runtime/logs" "$runtime/cwd"
(
    cd "$runtime/cwd"
    RESTREAM_HTTP_BIND_ADDR=127.0.0.1 \
    RESTREAM_HTTP_PORT="$port" \
    RESTREAM_RTMP_PORT="$rtmp_port" \
    RESTREAM_SRT_PORT="$srt_port" \
    RESTREAM_DB_PATH="$runtime/restream.db" \
    RESTREAM_MEDIA_DIR="$runtime/media" \
    RESTREAM_LOG_DIR="$runtime/logs" \
    RESTREAM_INITIAL_ADMIN_PASSWORD=release-smoke-password \
        "$run" restream >"$runtime/restream.log" 2>&1 &
    app_pid=$!
    echo "$app_pid" >"$runtime/app.pid"
)
app_pid="$(cat "$runtime/app.pid")"

base_url="http://127.0.0.1:$port"
for _ in $(seq 1 30); do
    if curl --fail --silent --show-error "$base_url/healthz" >/dev/null 2>&1; then
        break
    fi
    sleep 1
done
if ! curl --fail --silent --show-error "$base_url/healthz" >/dev/null; then
    echo "release-artifact-smoke: app did not become healthy; log follows" >&2
    cat "$runtime/restream.log" >&2 || true
    exit 1
fi

assert_contains() {
    local path="$1"
    local expected="$2"
    local out="$runtime/asset-${path//[^A-Za-z0-9_.-]/_}"
    curl --fail --silent --show-error "$base_url$path" -o "$out"
    if ! grep -Fq -- "$expected" "$out"; then
        echo "release-artifact-smoke: $path did not contain expected marker: $expected" >&2
        exit 1
    fi
}

assert_nonempty() {
    local path="$1"
    local out="$runtime/asset-${path//[^A-Za-z0-9_.-]/_}"
    curl --fail --silent --show-error "$base_url$path" -o "$out"
    [[ -s "$out" ]] || {
        echo "release-artifact-smoke: $path was empty" >&2
        exit 1
    }
}

assert_contains "/login" "Restream Login"
assert_contains "/output.css" "--color-base-100"
assert_contains "/base-path.js" "__RESTREAM_BASE_PATH__"
assert_contains "/js/features/dashboard-entry.js" "dashboard"
assert_contains "/js/lib/hls.min.js" "Hls"
assert_nonempty "/logo.png"

cookie_jar="$runtime/cookies.txt"
curl --fail --silent --show-error \
    -c "$cookie_jar" \
    -H 'Content-Type: application/json' \
    -d '{"password":"release-smoke-password"}' \
    "$base_url/api/v1/auth/login" >/dev/null
curl --fail --silent --show-error -b "$cookie_jar" "$base_url/" -o "$runtime/index.html"
if ! grep -Fq -- "js/features/dashboard-entry.js" "$runtime/index.html"; then
    echo "release-artifact-smoke: authenticated index did not contain dashboard asset reference" >&2
    exit 1
fi

echo "release-artifact-smoke: PASS bundle=$BUNDLE"
