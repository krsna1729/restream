#!/usr/bin/env bash
# Source-wide audit automation to enforce clean layering, prevent growth of
# god files, and catch un-centralized environment variables.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

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
check_size "src/api.rs" 7730
check_size "src/media/engine.rs" 6210
check_size "src/bin/test_harness.rs" 10255

# 3. Check for raw std::env::var usage outside src/config.rs and tests
echo ""
echo "Checking for inline std::env::var usage outside src/config.rs..."
# Exclude config.rs, main.rs (tokio runtime limits), tests, benches, and test harness
RAW_ENV_VARS=$(grep -rn "std::env::var" src/ \
    | grep -v "src/config.rs" \
    | grep -v "src/main.rs" \
    | grep -v "tests/" \
    | grep -v "benches/" \
    | grep -v "test_fixtures.rs" || true)

if [ -n "$RAW_ENV_VARS" ]; then
    echo "WARN: Found raw std::env::var usage outside src/config.rs (please refactor to AppConfig):"
    echo "$RAW_ENV_VARS"
else
    echo "OK: No raw std::env::var usage found outside configuration module."
fi

echo ""
if [ "$FAILED" -eq 1 ]; then
    echo "=== AUDIT FAILED ==="
    exit 1
else
    echo "=== AUDIT PASSED ==="
    exit 0
fi
