#!/usr/bin/env bash
# Source-wide audit automation to enforce clean layering, prevent growth of
# god files, and catch un-centralized environment variables.

set -euo pipefail

ROOT_DIR="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
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

echo ""
echo "Checking file size limits..."
SOURCE_LINE_LIMIT=999
SOURCE_LINE_WARNING=800
FRONTEND_SOURCE_LINE_LIMIT=2000

SOURCE_ROOTS=()
for root in src web/ts test tests benches; do
    if [ -d "$root" ]; then
        SOURCE_ROOTS+=("$root")
    fi
done

find_audited_source_files() {
    if [ -f build.rs ]; then
        printf 'build.rs\0'
    fi
    find "${SOURCE_ROOTS[@]}" \
        \( -path '*/.local/artifacts' -o -path '*/.local/artifacts/*' \
            -o -path 'test/fixtures' -o -path 'test/fixtures/*' \) -prune \
        -o -type f \
        \( -name '*.rs' -o -name '*.ts' -o -name '*.mjs' -o -name '*.js' \) \
        -print0
}

classify_source_file() {
    local file="$1"
    case "$file" in
        build.rs)
            printf 'build-script'
            ;;
        benches/*.rs)
            printf 'benchmark'
            ;;
        tests/*.rs)
            printf 'integration-test'
            ;;
        src/bin/test_harness.rs|src/bin/test_harness/*.rs|test/harness/*.rs)
            printf 'harness'
            ;;
        src/*/tests/*.rs|src/*_test/*.rs|src/*_tests/*.rs|src/*/test.rs|src/*/tests.rs|src/*_test.rs|src/*_tests.rs)
            printf 'dedicated-test'
            ;;
        src/*.rs)
            printf 'production'
            ;;
        test/*.rs)
            printf 'harness'
            ;;
        web/ts/*|test/*)
            printf 'frontend-test-or-source'
            ;;
        *)
            printf 'other'
            ;;
    esac
}

declare -A RUST_CLASS_COUNTS=(
    [build-script]=0
    [production]=0
    [dedicated-test]=0
    [harness]=0
    [benchmark]=0
    [integration-test]=0
)
SOURCE_SIZE_FAILED=0
SOURCE_SIZE_WARNINGS=0
while IFS= read -r -d '' file; do
    lines=$(awk 'END { print NR }' "$file")
    classification=$(classify_source_file "$file")
    if [ "${file##*.}" = rs ]; then
        file_limit=$SOURCE_LINE_LIMIT
        warning_threshold=$SOURCE_LINE_WARNING
        RUST_CLASS_COUNTS["$classification"]=$((RUST_CLASS_COUNTS["$classification"] + 1))
        if [ "$lines" -gt "$file_limit" ]; then
            echo "FAIL [$classification]: $file has $lines raw lines (Rust hard maximum: $file_limit; 1000 fails)" >&2
            FAILED=1
            SOURCE_SIZE_FAILED=1
        elif [ "$lines" -ge "$warning_threshold" ]; then
            SOURCE_SIZE_WARNINGS=$((SOURCE_SIZE_WARNINGS + 1))
            echo "WARN [$classification]: $file has $lines raw lines (Rust pressure band: ${warning_threshold}-${file_limit})" >&2
        fi
    else
        file_limit=$FRONTEND_SOURCE_LINE_LIMIT
        if [ "$lines" -gt "$file_limit" ]; then
            echo "FAIL [$classification]: $file has $lines raw lines (frontend hard maximum: $file_limit; 2001 fails)" >&2
            FAILED=1
            SOURCE_SIZE_FAILED=1
        fi
    fi
done < <(find_audited_source_files)

echo "Line policies: Rust hard maximum ${SOURCE_LINE_LIMIT} (warn at ${SOURCE_LINE_WARNING}); TypeScript/JavaScript hard maximum ${FRONTEND_SOURCE_LINE_LIMIT}."
echo "Audited Rust files by responsibility:"
for classification in build-script production dedicated-test harness benchmark integration-test; do
    printf '  %-16s %s\n' "$classification:" "${RUST_CLASS_COUNTS[$classification]}"
done

if [ "$SOURCE_SIZE_FAILED" -eq 0 ]; then
    echo "OK: All audited files are within their language-specific raw-line maximum."
fi
if [ "$SOURCE_SIZE_WARNINGS" -gt 0 ]; then
    echo "WARN: ${SOURCE_SIZE_WARNINGS} Rust file(s) are in the ${SOURCE_LINE_WARNING}-${SOURCE_LINE_LIMIT} pressure band."
    echo "      Near-cap clustering is architectural pressure, not success; split by ownership before adding more code."
fi

# 3. Check for raw std::env::var usage outside src/config.rs and tests
echo ""
echo "Checking for inline std::env::var usage outside src/config.rs..."
# Exclude config.rs, main.rs (tokio runtime limits), test harness,
# tests, benches, test_fixtures.rs, lib.rs (config-chain helpers),
# ffmpeg_extract.rs (the documented FFMPEG_BIN_PATH fallback), planner
# (BackendPolicy::from_env), and restream-mcp (separate binary).
RAW_ENV_VARS=$(grep -rn "std::env::var" src/ \
    | grep -v "src/config.rs" \
    | grep -v "src/main.rs" \
    | grep -v "src/lib.rs" \
    | grep -v "src/ffmpeg_extract.rs" \
    | grep -v "src/planner/" \
    | grep -v "src/bin/test_harness" \
    | grep -v "src/bin/restream-mcp.rs" \
    | grep -v "tests/" \
    | grep -v "benches/" \
    | grep -v "test_fixtures.rs" || true)

if [ -n "$RAW_ENV_VARS" ]; then
    echo "FAIL: Found raw std::env::var usage outside approved config/test harness owners:" >&2
    echo "$RAW_ENV_VARS" >&2
    FAILED=1
else
    echo "OK: No raw std::env::var usage found outside configuration module."
fi

echo ""
echo "Checking API stage-start guardrails..."
API_MANUAL_STAGE_STARTS=$(grep -REn \
    "Command::new|ensure_ffmpeg|ffmpeg_bin_path|get_or_create_transcoder|get_or_create_h264_transcoder|spawn_ffmpeg|run_.*ffmpeg|external_transcoder" \
    src/api/ || true)
if [ -n "$API_MANUAL_STAGE_STARTS" ]; then
    echo "FAIL: API route/view modules must not manually start FFmpeg/transcoder stages:" >&2
    echo "$API_MANUAL_STAGE_STARTS" >&2
    FAILED=1
else
    echo "OK: API modules do not start FFmpeg/transcoder stages."
fi

echo ""
echo "Checking harness status-schema guardrails..."
HARNESS_STATE_FIELD_READS=$(grep -REn '\["state"\]' src/bin/test_harness/ src/bin/test_harness.rs || true)
if [ -n "$HARNESS_STATE_FIELD_READS" ]; then
    echo "FAIL: Harness reads a non-schema output status field named state:" >&2
    echo "$HARNESS_STATE_FIELD_READS" >&2
    FAILED=1
else
    echo "OK: Harness does not read the removed output status state field."
fi

echo ""
if [ "$FAILED" -eq 1 ]; then
    echo "=== AUDIT FAILED ==="
    exit 1
else
    echo "=== AUDIT PASSED ==="
    exit 0
fi
