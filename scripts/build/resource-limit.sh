#!/usr/bin/env bash
# Serialize heavy commands behind a shared flock and size build parallelism from
# available RAM and CPU budget.
#
# Usage:
#   scripts/build/resource-limit.sh cargo build --release
#   scripts/build/resource-limit.sh cargo test
#   scripts/build/resource-limit.sh target/debug/test_harness mixed-anchor
#   scripts/build/resource-limit.sh ./scripts/build/native-deps.sh
#
# Defaults:
#   RESTREAM_MB_PER_JOB=500            approximate memory budget per compiler job
#   RESTREAM_CPU_RESERVE=1             CPUs to leave free when the machine has room
#   RESTREAM_MIN_JOBS=1                lower bound after memory/CPU sizing
#   RESTREAM_MAX_JOBS unset            optional hard cap
#   RESTREAM_MB_PER_TEST_THREAD=500    approximate memory budget per test thread
#
# The lockfile defaults to the repo root at .local/build/lock (gitignored). Set
# RESTREAM_BUILD_LOCK_FILE to an absolute host-global path when multiple
# worktrees share the same machine. While the lock is held, this script exports
# BUILD_JOBS, CARGO_BUILD_JOBS, CMAKE_BUILD_PARALLEL_LEVEL, MAKEFLAGS, and
# RUST_TEST_THREADS.

set -euo pipefail

ROOT_DIR="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
LOCK_FILE="${RESTREAM_BUILD_LOCK_FILE:-$ROOT_DIR/.local/build/lock}"
FLOCK_MODE="--exclusive"
WAIT_TIMEOUT=600  # 10 minutes max wait

if [[ "$LOCK_FILE" != /* ]]; then
    echo "resource-limit: RESTREAM_BUILD_LOCK_FILE must be absolute when set" >&2
    exit 2
fi

is_uint() {
    [[ "$1" =~ ^[0-9]+$ ]]
}

require_uint() {
    local name="$1"
    local value="$2"
    if ! is_uint "$value"; then
        echo "resource-limit: $name must be a non-negative integer" >&2
        exit 2
    fi
}

require_positive_uint() {
    local name="$1"
    local value="$2"
    require_uint "$name" "$value"
    if (( value == 0 )); then
        echo "resource-limit: $name must be greater than zero" >&2
        exit 2
    fi
}

read_available_mb() {
    local avail_mb
    avail_mb=$(awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo)
    if [[ -z "$avail_mb" ]]; then
        echo "resource-limit: could not read MemAvailable from /proc/meminfo" >&2
        exit 2
    fi
    echo "$avail_mb"
}

read_cpu_count() {
    nproc
}

configure_build_jobs() {
    local mb_per_job="${RESTREAM_MB_PER_JOB:-500}"
    local min_jobs="${RESTREAM_MIN_JOBS:-1}"
    local cpu_reserve="${RESTREAM_CPU_RESERVE:-1}"
    local max_jobs="${RESTREAM_MAX_JOBS:-}"

    require_positive_uint RESTREAM_MB_PER_JOB "$mb_per_job"
    require_positive_uint RESTREAM_MIN_JOBS "$min_jobs"
    require_uint RESTREAM_CPU_RESERVE "$cpu_reserve"
    if [[ -n "$max_jobs" ]]; then
        require_positive_uint RESTREAM_MAX_JOBS "$max_jobs"
    fi

    local avail_mb
    avail_mb=$(read_available_mb)

    local cpus
    cpus=$(read_cpu_count)

    local mem_jobs=$((avail_mb / mb_per_job))
    local cpu_jobs=$((cpus - cpu_reserve))
    local effective_min="$min_jobs"

    (( effective_min > cpus )) && effective_min="$cpus"
    (( mem_jobs < effective_min )) && mem_jobs="$effective_min"
    (( mem_jobs > cpus )) && mem_jobs="$cpus"
    (( cpu_jobs < effective_min )) && cpu_jobs="$effective_min"
    (( cpu_jobs > cpus )) && cpu_jobs="$cpus"

    local jobs="$mem_jobs"
    (( cpu_jobs < jobs )) && jobs="$cpu_jobs"
    if [[ -n "$max_jobs" && "$jobs" -gt "$max_jobs" ]]; then
        jobs="$max_jobs"
    fi

    export BUILD_JOBS="$jobs"
    export CARGO_BUILD_JOBS="$jobs"
    export CMAKE_BUILD_PARALLEL_LEVEL="$jobs"
    export MAKEFLAGS="-j$jobs${MAKEFLAGS:+ $MAKEFLAGS}"

    configure_rust_test_threads "$avail_mb" "$cpus" "$cpu_reserve"

    echo "resource-limit: ${avail_mb}MB available, ${cpus} CPUs, reserve ${cpu_reserve} -> $jobs build jobs, RUST_TEST_THREADS=${RUST_TEST_THREADS:-<unset>}" >&2
}

configure_rust_test_threads() {
    local avail_mb="${1:-}"
    local cpus="${2:-}"
    local cpu_reserve="${3:-${RESTREAM_CPU_RESERVE:-1}}"

    if [[ -n "${RUST_TEST_THREADS:-}" ]]; then
        require_positive_uint RUST_TEST_THREADS "$RUST_TEST_THREADS"
        return
    fi

    if [[ -z "$avail_mb" ]]; then
        avail_mb=$(read_available_mb)
    fi
    if [[ -z "$cpus" ]]; then
        cpus=$(read_cpu_count)
    fi

    local mb_per_thread="${RESTREAM_MB_PER_TEST_THREAD:-500}"
    require_positive_uint RESTREAM_MB_PER_TEST_THREAD "$mb_per_thread"

    local mem_threads=$((avail_mb / mb_per_thread))
    local cpu_threads=$((cpus - cpu_reserve))

    (( mem_threads > cpus )) && mem_threads="$cpus"
    (( cpu_threads < 1 )) && cpu_threads=1
    (( cpu_threads > cpus )) && cpu_threads="$cpus"

    local threads="$mem_threads"
    (( cpu_threads < threads )) && threads="$cpu_threads"
    (( threads < 1 )) && threads=1

    export RUST_TEST_THREADS="$threads"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --shared|-s)
            FLOCK_MODE="--shared"
            shift
            ;;
        --timeout|-t)
            if [[ $# -lt 2 || ! "$2" =~ ^[0-9]+$ ]]; then
                echo "resource-limit: --timeout requires a non-negative integer seconds value" >&2
                exit 2
            fi
            WAIT_TIMEOUT="$2"
            shift 2
            ;;
        --)
            shift
            break
            ;;
        *)
            break
            ;;
    esac
done

if [[ $# -eq 0 ]]; then
    echo "resource-limit: no command specified" >&2
    echo "usage: scripts/build/resource-limit.sh [--shared] [--timeout N] COMMAND..." >&2
    exit 1
fi

if [[ -n "${RESTREAM_BUILD_LOCK_HELD:-}" ]]; then
    if [[ -z "${BUILD_JOBS:-}" ]]; then
        configure_build_jobs
    else
        configure_rust_test_threads
    fi
    exec "$@"
fi

mkdir -p "$(dirname "$LOCK_FILE")"
exec 9>"$LOCK_FILE"

if ! flock --nonblock "$FLOCK_MODE" 9 2>/dev/null; then
    echo "resource-limit: waiting for another build to finish (timeout ${WAIT_TIMEOUT}s)..." >&2
    flock "$FLOCK_MODE" --timeout "$WAIT_TIMEOUT" 9
fi

configure_build_jobs
export RESTREAM_BUILD_LOCK_HELD=1
exec "$@"
