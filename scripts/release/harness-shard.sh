#!/usr/bin/env bash
# Run or inspect one release harness shard. The shard catalog lives in
# scripts/lib/release-shards.sh so CI, docs, and local release due diligence do
# not grow separate shard/timeout maps.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"
# shellcheck source=scripts/lib/release-shards.sh
source "$ROOT/scripts/lib/release-shards.sh"

usage() {
    cat <<'EOF' >&2
usage:
  scripts/release/harness-shard.sh <shard>
  scripts/release/harness-shard.sh run <shard>
  scripts/release/harness-shard.sh list
  scripts/release/harness-shard.sh timeout <shard>
  scripts/release/harness-shard.sh explain <shard>
EOF
}

command_name="${1:-}"
case "$command_name" in
    ""|-h|--help)
        usage
        [[ -n "$command_name" ]] && exit 0 || exit 2
        ;;
    list)
        restream_release_shard_list
        exit 0
        ;;
    timeout)
        shard="${2:-}"
        [[ -n "$shard" ]] || { usage; exit 2; }
        restream_release_shard_exists "$shard" || { echo "harness-shard: unknown shard '$shard'" >&2; exit 2; }
        restream_release_shard_timeout "$shard"
        exit 0
        ;;
    explain)
        shard="${2:-}"
        [[ -n "$shard" ]] || { usage; exit 2; }
        restream_release_shard_explain "$shard"
        exit 0
        ;;
    run)
        shard="${2:-}"
        [[ -n "$shard" ]] || { usage; exit 2; }
        ;;
    *)
        # Preserve the original CI/user interface: a bare shard name runs it.
        shard="$command_name"
        ;;
esac

restream_release_shard_exists "$shard" || {
    echo "harness-shard: unknown shard '$shard'" >&2
    usage
    exit 2
}

safe_shard="${shard//[^A-Za-z0-9_.-]/-}"
run_id="${RELEASE_HARNESS_RUN_ID:-release-${GITHUB_SHA:-local}-$safe_shard}"
common_args=(--no-netns --run-id "$run_id")
shard_started_at=$SECONDS
shard_timeout="${RELEASE_HARNESS_SHARD_TIMEOUT:-}"

if [[ "${RELEASE_HARNESS_SHARD_TIMEOUT_ACTIVE:-0}" != "1" ]]; then
    restream_require_command timeout
    shard_timeout="${shard_timeout:-$(restream_release_shard_timeout "$shard")}"
    echo "[release-shard] timeout $shard: $shard_timeout"
    set +e
    RELEASE_HARNESS_SHARD_TIMEOUT_ACTIVE=1 \
        timeout --kill-after=30s "$shard_timeout" "$0" run "$shard"
    status=$?
    set -e
    elapsed=$((SECONDS - shard_started_at))
    if [[ "$status" -eq 124 || "$status" -eq 137 || "$status" -eq 143 ]]; then
        echo "[release-shard] TIMEOUT $shard after $(restream_format_elapsed "$elapsed") (limit $shard_timeout)" >&2
    fi
    exit "$status"
fi

run_mode() {
    local mode=$1
    shift || true
    echo "[release-shard] run $shard: $mode"
    # Release CI uploads the JSON artifacts separately; printing the full
    # pretty JSON for every successful mode makes 22 parallel shard logs hard
    # to scan and mixes machine payloads with progress lines. Keep progress,
    # failure diagnostics, and artifact paths in stdout/stderr, but suppress
    # redundant success payloads in this release wrapper.
    TEST_HARNESS_SUPPRESS_SUCCESS_JSON=1 scripts/harness/run.sh "$mode" -- "${common_args[@]}" "$@"
}

while IFS=$'\t' read -r kind value; do
    case "$kind" in
        mode)
            run_mode "$value"
            ;;
        bitrate)
            BITRATE_SWEEP_CONFIGS="$value" run_mode bitrate-sweep
            ;;
        resource)
            RESOURCE_SWEEP_SCENARIOS="$value" run_mode resource-sweep
            ;;
        msr-smoke)
            MSR_OUTPUT_COUNTS=1 \
            MSR_FFPROBE_SAMPLE_COUNT=1 \
            MSR_SINK_SAMPLE_SECS=1 \
            run_mode "$value"
            ;;
        *)
            echo "harness-shard: invalid plan row for $shard: $kind $value" >&2
            exit 1
            ;;
    esac
done < <(restream_release_shard_plan "$shard")

echo "[release-shard] PASS $shard ($(restream_format_elapsed "$((SECONDS - shard_started_at))"))"
