#!/usr/bin/env bash
# Regression checks for the resource-limit wrapper's libtest concurrency policy.
#
# Each test uses an isolated lock file so tests do not block each other.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"

pass_count=0
fail_count=0

pass() {
    local label="$1"
    (( pass_count++ ))
    echo "  PASS  $label"
}

fail() {
    local label="$1"
    local detail="$2"
    (( fail_count++ ))
    echo "  FAIL  $label: $detail"
}

run_wrapper() {
    local lock_file
    lock_file=$(mktemp /tmp/restream-check-XXXXXX.lock)
    RESTREAM_BUILD_LOCK_FILE="$lock_file" \
    RESTREAM_REPO_ROOT="$ROOT" \
        scripts/build/resource-limit.sh --shared --timeout 5 \
        bash -c 'echo "RUST_TEST_THREADS=${RUST_TEST_THREADS:-}"' 2>/dev/null
    rm -f "$lock_file" 2>/dev/null || true
}

echo "=== resource-limit concurrency policy checks ==="
echo ""

# 1. Memory-derived budget: with a per-thread budget far larger than
#    available memory, the derived count should be 1.
echo "[budget-memory-derived]"
result=$(RESTREAM_MB_PER_TEST_THREAD=999999 run_wrapper 2>/dev/null || true)
if [[ "$result" == "RUST_TEST_THREADS=1" ]]; then
    pass "memory-derived budget caps at 1 with huge per-thread budget"
else
    fail "memory-derived budget caps at 1" "got: $result"
fi

# 2. Nested invocation: when RESTREAM_BUILD_LOCK_HELD and BUILD_JOBS are
#    already exported, the wrapper still derives test threads.
echo "[nested-invocation]"
lock_file=$(mktemp /tmp/restream-check-XXXXXX.lock)
result=$(
    RESTREAM_BUILD_LOCK_FILE="$lock_file" \
    RESTREAM_REPO_ROOT="$ROOT" \
    RESTREAM_BUILD_LOCK_HELD=1 \
    BUILD_JOBS=2 \
    RESTREAM_MB_PER_TEST_THREAD=500 \
        scripts/build/resource-limit.sh --shared --timeout 5 \
        bash -c 'echo "RUST_TEST_THREADS=${RUST_TEST_THREADS:-}"' 2>/dev/null || true
)
rm -f "$lock_file" 2>/dev/null || true
if [[ "$result" == "RUST_TEST_THREADS="* ]] && [[ -n "${result#RUST_TEST_THREADS=}" ]]; then
    pass "nested wrapper derivation works: $result"
else
    fail "nested wrapper derivation" "got: $result"
fi

# 3. Explicit RUST_TEST_THREADS override is preserved (skips memory/CPU
#    derivation entirely).
echo "[explicit-override]"
result=$(RUST_TEST_THREADS=2 run_wrapper 2>/dev/null || true)
if [[ "$result" == "RUST_TEST_THREADS=2" ]]; then
    pass "explicit RUST_TEST_THREADS=2 preserved"
else
    fail "explicit RUST_TEST_THREADS=2" "got: $result"
fi

# 4. Invalid explicit override (RUST_TEST_THREADS=0) causes exit 2.
echo "[invalid-override-rejected]"
lock_file=$(mktemp /tmp/restream-check-XXXXXX.lock)
exit_code=0
RESTREAM_BUILD_LOCK_FILE="$lock_file" \
RESTREAM_REPO_ROOT="$ROOT" \
RUST_TEST_THREADS=0 \
    scripts/build/resource-limit.sh --shared --timeout 5 \
    bash -c 'true' 2>/dev/null || exit_code=$?
rm -f "$lock_file" 2>/dev/null || true
if (( exit_code == 2 )); then
    pass "invalid override (RUST_TEST_THREADS=0) rejected with exit 2"
else
    fail "invalid override rejected" "exit code: $exit_code (expected 2)"
fi

echo ""
echo "=== result: $pass_count passed, $fail_count failed ==="
exit "$fail_count"
