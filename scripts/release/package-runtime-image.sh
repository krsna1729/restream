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

context="$tmp/context"
rootfs="$context/rootfs"
mkdir -p "$context" "$rootfs"

scripts/build/runtime-rootfs.sh "$BINARY" "$rootfs"
cp "$BINARY" "$context/restream"
cp -a distribution "$context/distribution"

cat >"$context/Dockerfile" <<'EOF'
FROM scratch
COPY rootfs/ /
COPY --chown=1000:1000 rootfs/.restream /.restream
COPY restream /restream
COPY distribution/ /usr/share/doc/restream/distribution/
ARG RESTREAM_BUILD_GIT_COMMIT
ARG RESTREAM_BUILD_TIMESTAMP
LABEL org.opencontainers.image.source="https://github.com/krsna1729/restream" \
    org.opencontainers.image.revision="${RESTREAM_BUILD_GIT_COMMIT}" \
    org.opencontainers.image.created="${RESTREAM_BUILD_TIMESTAMP}" \
    org.opencontainers.image.licenses="MIT AND GPL-2.0-or-later AND MPL-2.0 AND Apache-2.0"
EXPOSE 3030 1935 10080/udp
USER 1000:1000
ENV RESTREAM_HTTP_BIND_ADDR=0.0.0.0
ENTRYPOINT ["/restream"]
EOF

build_commit="$(git rev-parse HEAD)"
build_timestamp="$(git show -s --format=%cI HEAD)"
docker_build_args=(
    --build-arg "RESTREAM_BUILD_GIT_COMMIT=$build_commit"
    --build-arg "RESTREAM_BUILD_TIMESTAMP=$build_timestamp"
    -t "$IMAGE"
    "$context"
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
echo "package-runtime-image: PASS archive=$ARCHIVE image=$IMAGE"
