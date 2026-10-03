#!/usr/bin/env bash
# Package the shipped runtime image archive from the already-built release
# binary. This keeps release certification on one Rust build: native packaging
# produces the executable bytes, and Docker only assembles the runtime image
# around those exact bytes.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"
# shellcheck source=scripts/lib/release-common.sh
source "$ROOT/scripts/lib/release-common.sh"

VERSION="${1:?usage: scripts/release/package-runtime-image.sh <version>}"
OUT_DIR="${RESTREAM_RELEASE_DIR:-dist}"
IMAGE="${RESTREAM_CONTAINER_IMAGE:-restream:release}"
ARCHIVE="$OUT_DIR/restream-$VERSION-oci.tar.gz"
# The runtime image needs a seccomp profile that permits io_uring (Docker's
# default denies it), so the supported profile ships NEXT TO the image archive:
# users must not need a source checkout to obtain it.
SECCOMP_PROFILE_SOURCE="distribution/docker/restream-seccomp.json"
SECCOMP_PROFILE="$OUT_DIR/restream-$VERSION-seccomp.json"
BINARY="${RESTREAM_RELEASE_BINARY:-target/release/restream}"

restream_release_require_version package-runtime-image "$VERSION"
restream_require_commands docker gzip

[[ -x "$BINARY" ]] || {
    echo "package-runtime-image: expected built executable is missing: $BINARY" >&2
    exit 1
}

tmp="$(mktemp -d)"
cleanup() {
    rm -rf "$tmp"
}
trap cleanup EXIT

payload="$tmp/release-payload"
mkdir -p "$payload"

cp "$BINARY" "$payload/restream"

build_commit="$(git rev-parse HEAD)"
build_timestamp="$(git show -s --format=%cI HEAD)"
docker_build_args=(
    --file Dockerfile
    --target runtime-artifact
    --build-context "release_payload=$payload"
    --build-arg "RESTREAM_BUILD_GIT_COMMIT=$build_commit"
    --build-arg "RESTREAM_BUILD_TIMESTAMP=$build_timestamp"
    -t "$IMAGE"
    .
)
if [[ "${RESTREAM_DOCKER_GHA_CACHE:-0}" == "1" ]]; then
    docker buildx build \
        "${docker_build_args[@]}" \
        --cache-from "type=gha,scope=runtime-release-package" \
        --cache-to "type=gha,mode=max,scope=runtime-release-package" \
        --load
else
    docker build "${docker_build_args[@]}"
fi

mkdir -p "$OUT_DIR"
docker save "$IMAGE" | gzip -n >"$ARCHIVE"
[[ -s "$SECCOMP_PROFILE_SOURCE" ]] || {
    echo "package-runtime-image: missing supported seccomp profile: $SECCOMP_PROFILE_SOURCE" >&2
    exit 1
}
install -m 0644 "$SECCOMP_PROFILE_SOURCE" "$SECCOMP_PROFILE"
echo "package-runtime-image: PASS archive=$ARCHIVE seccomp=$SECCOMP_PROFILE image=$IMAGE"
