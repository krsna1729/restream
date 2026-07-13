#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: scripts/build/emit-sbom.sh <restream-binary> <sbom-path>" >&2
    exit 2
fi

BINARY="$1"
SBOM="$2"

if [[ "${RESTREAM_SKIP_SBOM:-0}" == "1" ]]; then
    echo "Skipping SBOM emission (RESTREAM_SKIP_SBOM=1)."
    exit 0
fi

mkdir -p "$(dirname "$SBOM")"
"$BINARY" --emit-sbom "$SBOM"
