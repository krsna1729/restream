#!/usr/bin/env bash
# Publish the already-certified scratch image. The workflow deliberately calls
# this only after release-evidence.sh has completed the full live-harness gate.
set -euo pipefail

VERSION="${1:?usage: scripts/release/publish-image.sh <version>}"
SOURCE_IMAGE="${RESTREAM_CONTAINER_IMAGE:-restream:release}"
REGISTRY_IMAGE="${RESTREAM_REGISTRY_IMAGE:?RESTREAM_REGISTRY_IMAGE is required}"

docker image inspect "$SOURCE_IMAGE" >/dev/null || {
    echo "publish-image: certified image is missing: $SOURCE_IMAGE" >&2
    exit 1
}

for tag in "$VERSION" latest; do
    docker tag "$SOURCE_IMAGE" "$REGISTRY_IMAGE:$tag"
    docker push "$REGISTRY_IMAGE:$tag"
done

echo "publish-image: PASS image=$REGISTRY_IMAGE version=$VERSION"
