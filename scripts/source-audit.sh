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

echo ""
echo "Checking file size limits..."
SOURCE_LINE_LIMIT=2000
LARGE_FILE_REPORT=target/source-audit-large-files.jsonl
: > "$LARGE_FILE_REPORT"
SOURCE_FILE_FIND_ARGS=(
    src
    public/ts
    test
    -path test/artifacts -prune
    -o
    -path test/fixtures -prune
    -o
    -type f
    \( -name '*.rs' -o -name '*.ts' -o -name '*.mjs' -o -name '*.js' \)
    -print0
)
while IFS= read -r -d '' file; do
    lines=$(wc -l < "$file" | tr -d ' ')
    printf '{"file":"%s","lines":%s,"limit":%s}\n' \
        "$file" "$lines" "$SOURCE_LINE_LIMIT" >> "$LARGE_FILE_REPORT"
    if [ "$lines" -gt "$SOURCE_LINE_LIMIT" ]; then
        echo "FAIL: $file has $lines lines (limit: $SOURCE_LINE_LIMIT)" >&2
        FAILED=1
    fi
done < <(find "${SOURCE_FILE_FIND_ARGS[@]}")

LARGEST_FILES=$(
    sort -t: -k2,2nr <(
        while IFS= read -r -d '' file; do
            lines=$(wc -l < "$file" | tr -d ' ')
            printf '%s:%s\n' "$file" "$lines"
        done < <(find "${SOURCE_FILE_FIND_ARGS[@]}")
    ) | head -20
)

if [ "$FAILED" -eq 0 ]; then
    echo "OK: All audited source/test files are at or below ${SOURCE_LINE_LIMIT} lines."
fi

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
LARGEST_FILES_JSON=$(
    awk -F: '
        BEGIN { print "[" }
        {
            gsub(/\\/,"\\\\",$1);
            gsub(/"/,"\\\"",$1);
            printf "%s    {\"file\":\"%s\",\"lines\":%s,\"limit\":%s}", sep, $1, $2, limit;
            sep = ",\n";
        }
        END { print "\n  ]" }
    ' limit="$SOURCE_LINE_LIMIT" <<< "$LARGEST_FILES"
)

cat > target/source-audit.json <<EOF
{
  "sourceLineLimit": ${SOURCE_LINE_LIMIT},
  "largestFiles": ${LARGEST_FILES_JSON},
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
