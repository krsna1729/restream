#!/usr/bin/env bash
# Run an arbitrary harness-side command inside the prebuilt CI runtime image.
# The checkout and prepared target/bench binaries stay on the host workspace;
# only the process environment moves into the image so PR jobs avoid repeated
# apt/MediaMTX setup.
set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: $0 <command> [args...]" >&2
    exit 2
fi

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"

default_image="ghcr.io/${GITHUB_REPOSITORY:-krsna1729/restream}-ci-harness-runtime:ubuntu24"
image="${RESTREAM_CI_HARNESS_IMAGE:-$default_image}"
container_home="$ROOT/.local/ci-container-home"
mkdir -p "$container_home"

if [[ "${RESTREAM_CI_HARNESS_PULL:-1}" == "1" ]]; then
    token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
    if [[ "$image" == ghcr.io/* && -n "$token" ]]; then
        echo "ci-harness-runtime: logging in to ghcr.io for runtime image pull"
        printf '%s\n' "$token" | docker login ghcr.io --username "${GITHUB_ACTOR:-oauth2}" --password-stdin
    fi
    echo "ci-harness-runtime: pulling $image"
    docker pull "$image"
fi

docker_args=(
    run
    --rm
    --network host
    --user "$(id -u):$(id -g)"
    -e "HOME=$container_home"
    -e "BENCH_BUILD=${BENCH_BUILD:-never}"
    -e "GITHUB_SHA=${GITHUB_SHA:-}"
    -e "RESTREAM_REPO_ROOT=$ROOT"
    -e "TEST_HARNESS_USE_HOST_NET=${TEST_HARNESS_USE_HOST_NET:-}"
    -e "INTERNAL_BACKEND_SMOKE_CONTINUE_ON_FAIL=${INTERNAL_BACKEND_SMOKE_CONTINUE_ON_FAIL:-}"
    -e "INTERNAL_BACKEND_SMOKE_ARTIFACT_DIR=${INTERNAL_BACKEND_SMOKE_ARTIFACT_DIR:-}"
    -v "$ROOT:$ROOT"
    -w "$ROOT"
    "$image"
)

echo "ci-harness-runtime: run command=$* image=$image"
exec docker "${docker_args[@]}" "$@"
