#!/usr/bin/env bash
# Source-wide audit automation to enforce clean layering, prevent growth of
# god files, and catch un-centralized environment variables.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
mkdir -p target

echo "=== Restream Source Audit ==="
FAILED=0

# 1. Check for forbidden imports in media modules
echo "Checking forbidden imports in src/media/..."
FORBIDDEN_IMPORTS=$(grep -rn "use crate::api" src/media/ || true)
if [ -n "$FORBIDDEN_IMPORTS" ]; then
    echo "FAIL: Media modules must not import api types:" >&2
    echo "$FORBIDDEN_IMPORTS" >&2
    FAILED=1
else
    echo "OK: No forbidden imports found."
fi

# 2. Check for god file growth
check_size() {
    local file="$1"
    local max_lines="$2"
    if [ ! -f "$file" ]; then
        echo "WARN: File $file does not exist"
        return
    fi
    local lines
    lines=$(wc -l < "$file" | tr -d ' ')
    echo "File $file: $lines lines (limit: $max_lines)"
    if [ "$lines" -gt "$max_lines" ]; then
        echo "FAIL: File $file exceeds size limit of $max_lines lines!" >&2
        FAILED=1
    fi
}

echo ""
echo "Checking file size limits..."
check_size "src/media/engine.rs" 6587
check_size "src/bin/test_harness.rs" 10282

# 3. Check for raw std::env::var usage outside src/config.rs and tests
echo ""
echo "Checking for inline std::env::var usage outside src/config.rs..."
# Exclude config.rs, main.rs (tokio runtime limits), test harness,
# tests, benches, test_fixtures.rs, lib.rs (config-chain helpers),
# planner (BackendPolicy::from_env), and restream-mcp (separate binary).
RAW_ENV_VARS=$(grep -rn "std::env::var" src/ \
    | grep -v "src/config.rs" \
    | grep -v "src/main.rs" \
    | grep -v "src/lib.rs" \
    | grep -v "src/planner/" \
    | grep -v "src/bin/test_harness" \
    | grep -v "src/bin/restream-mcp.rs" \
    | grep -v "tests/" \
    | grep -v "benches/" \
    | grep -v "test_fixtures.rs" || true)

if [ -n "$RAW_ENV_VARS" ]; then
    echo "WARN: Found raw std::env::var usage outside src/config.rs (please refactor to AppConfig):"
    echo "$RAW_ENV_VARS"
else
    echo "OK: No raw std::env::var usage found outside configuration module."
fi

ROUTE_MODULE_COUNT=$(find src/api -maxdepth 1 -type f -name '*.rs' | wc -l | tr -d ' ')
DB_REPOSITORY_COUNT=$(find src/db -maxdepth 1 -type f -name '*_repo.rs' | wc -l | tr -d ' ')
FEATURE_CFG_COUNT=$(grep -R "\#\\[cfg(feature" -n src Cargo.toml 2>/dev/null || true)
FEATURE_CFG_COUNT=$(printf "%s" "$FEATURE_CFG_COUNT" | sed '/^$/d' | wc -l | tr -d ' ')
MEDIA_API_IMPORT_COUNT=$(grep -rn "use crate::api" src/media/ 2>/dev/null || true)
MEDIA_API_IMPORT_COUNT=$(printf "%s" "$MEDIA_API_IMPORT_COUNT" | sed '/^$/d' | wc -l | tr -d ' ')
ENGINE_LINES=$(wc -l < src/media/engine.rs | tr -d ' ')
HARNESS_LINES=$(wc -l < src/bin/test_harness.rs | tr -d ' ')

cat > target/source-audit.json <<EOF
{
  "largeFiles": {
    "src/media/engine.rs": {
      "lines": ${ENGINE_LINES},
      "limit": 6587
    },
    "src/bin/test_harness.rs": {
      "lines": ${HARNESS_LINES},
      "limit": 10282
    }
  },
  "moduleSummary": {
    "apiRouteModules": ${ROUTE_MODULE_COUNT},
    "dbRepositoryModules": ${DB_REPOSITORY_COUNT},
    "featureCfgSites": ${FEATURE_CFG_COUNT}
  },
  "forbiddenImports": {
    "mediaImportsApi": ${MEDIA_API_IMPORT_COUNT}
  }
}
EOF
echo "Wrote target/source-audit.json"

echo ""
if [ "$FAILED" -eq 1 ]; then
    echo "=== AUDIT FAILED ==="
    exit 1
else
    echo "=== AUDIT PASSED ==="
    exit 0
fi
