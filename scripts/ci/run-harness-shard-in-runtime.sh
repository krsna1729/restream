#!/usr/bin/env bash
# Run one release harness shard inside the prebuilt CI runtime image. The host
# runner still performs checkout/artifact actions; only the live harness process
# enters the image so apt/MediaMTX setup is not repeated per matrix shard.
set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: $0 <release-shard> [harness args...]" >&2
    exit 2
fi

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"

shard=$1
shift

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
    -e "RELEASE_VERSION=${RELEASE_VERSION:-}"
    -e "RELEASE_HARNESS_SHARD_TIMEOUT=${RELEASE_HARNESS_SHARD_TIMEOUT:-}"
    -e "HARNESS_BIN=${HARNESS_BIN:-}"
    -e "RESTREAM_BIN=${RESTREAM_BIN:-}"
    -e "RESTREAM_REPO_ROOT=$ROOT"
    -v "$ROOT:$ROOT"
    -w "$ROOT"
    "$image"
)

echo "ci-harness-runtime: run shard=$shard image=$image"
exec docker "${docker_args[@]}" bash -lc 'scripts/release/harness-shard.sh "$@"' bash "$shard" "$@"
