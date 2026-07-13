#!/usr/bin/env bash
# Build and prove the supported scratch runtime without a mount. This is the
# canonical container-release smoke used locally and by release automation.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"

IMAGE="${RESTREAM_CONTAINER_IMAGE:-restream:release-smoke}"
ARCHIVE=""

usage() {
    cat <<'EOF'
Usage: scripts/check/container-smoke.sh [--image NAME] [--archive PATH]

Builds Docker's default `runtime` target, proves it is a scratch image that
starts with no mounts, and verifies that it does not contain /tmp. When
--archive is supplied, writes a reproducible gzip-compressed OCI/Docker image
archive suitable for a GitHub Release asset.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --image)
            IMAGE=${2:?--image requires a value}
            shift 2
            ;;
        --archive)
            ARCHIVE=${2:?--archive requires a value}
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "container-smoke: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

for command in docker curl tar; do
    command -v "$command" >/dev/null || {
        echo "container-smoke: required command not found: $command" >&2
        exit 1
    }
done

name="restream-release-smoke-$$"
cleanup() {
    docker rm -f "$name" >/dev/null 2>&1 || true
}
trap cleanup EXIT

build_commit="$(git rev-parse HEAD)"
build_timestamp="$(git show -s --format=%cI HEAD)"
docker_build_args=(
    --build-arg "RESTREAM_BUILD_GIT_COMMIT=$build_commit" \
    --build-arg "RESTREAM_BUILD_TIMESTAMP=$build_timestamp" \
    --target runtime \
    -t "$IMAGE"
)
if [[ "${RESTREAM_DOCKER_GHA_CACHE:-0}" == "1" ]]; then
    docker buildx build \
        "${docker_build_args[@]}" \
        --cache-from "type=gha,scope=runtime-release" \
        --cache-to "type=gha,mode=max,scope=runtime-release" \
        --load \
        .
else
    docker build "${docker_build_args[@]}" .
fi

user="$(docker image inspect --format '{{.Config.User}}' "$IMAGE")"
if [[ "$user" != "1000:1000" ]]; then
    echo "container-smoke: expected non-root runtime user 1000:1000, got ${user:-empty}" >&2
    exit 1
fi

docker run -d --name "$name" \
    -e RESTREAM_INITIAL_ADMIN_PASSWORD=release-smoke-password \
    -p 127.0.0.1::3030 \
    "$IMAGE" >/dev/null

if [[ "$(docker inspect --format '{{json .Mounts}}' "$name")" != "[]" ]]; then
    echo "container-smoke: runtime unexpectedly requires a mount" >&2
    exit 1
fi

port="$(docker port "$name" 3030/tcp | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -n1)"
if [[ -z "$port" ]]; then
    echo "container-smoke: could not discover published HTTP port" >&2
    exit 1
fi

for _ in $(seq 1 20); do
    if curl --fail --silent --show-error "http://127.0.0.1:${port}/healthz" >/dev/null; then
        break
    fi
    sleep 1
done
curl --fail --silent --show-error "http://127.0.0.1:${port}/healthz" >/dev/null

if docker export "$name" | tar -tf - | grep -qE '^tmp/?$'; then
    echo "container-smoke: scratch runtime unexpectedly contains /tmp" >&2
    exit 1
fi

if [[ -n "$ARCHIVE" ]]; then
    mkdir -p "$(dirname "$ARCHIVE")"
    docker save "$IMAGE" | gzip -n >"$ARCHIVE"
    sha256sum "$ARCHIVE" >"${ARCHIVE}.sha256"
fi

echo "container-smoke: PASS image=$IMAGE"
