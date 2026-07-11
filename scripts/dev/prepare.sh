#!/usr/bin/env bash
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"

if [[ ! -d node_modules ]]; then
  echo "prepare: frontend dependencies are missing; run scripts/dev/bootstrap.sh first" >&2
  exit 2
fi

scripts/build/resource-limit.sh scripts/build/native-deps.sh
npm run build:frontend

echo "prepare: native prefix and embedded frontend assets are ready"
