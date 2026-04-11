#!/usr/bin/env bash
set -euo pipefail

bash test/artifacts/setup-4x3-copy.sh
bash test/artifacts/start-inputs-from-manifest.sh "${MANIFEST_PATH:-test/artifacts/session-4x3-last.json}"
bash test/artifacts/start-outputs-from-manifest.sh "${MANIFEST_PATH:-test/artifacts/session-4x3-last.json}"
bash test/artifacts/wait-all-active.sh
bash test/artifacts/health-snapshot.sh
