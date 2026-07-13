#!/usr/bin/env bash
# Shared shell primitives for repo scripts. Source this file from scripts that
# need common command checks, root escalation, bounded network/setup commands,
# or repo-root discovery; keep user-facing orchestration in the caller.

restream_repo_root() {
    if [[ -n "${RESTREAM_REPO_ROOT:-}" ]]; then
        printf '%s\n' "$RESTREAM_REPO_ROOT"
    elif command -v git >/dev/null 2>&1 && git rev-parse --show-toplevel >/dev/null 2>&1; then
        git rev-parse --show-toplevel
    else
        cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd
    fi
}

restream_require_command() {
    local command_name=$1
    command -v "$command_name" >/dev/null 2>&1 || {
        echo "required command not found: $command_name" >&2
        return 1
    }
}

restream_require_commands() {
    local command_name
    for command_name in "$@"; do
        restream_require_command "$command_name"
    done
}

restream_run_as_root() {
    if [[ "$(id -u)" -eq 0 ]]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        sudo "$@"
    else
        echo "need sudo to run as root: $*" >&2
        return 1
    fi
}

restream_with_timeout() {
    local label=$1
    local limit=$2
    shift 2
    restream_require_command timeout
    echo "$label (timeout ${limit})"
    timeout --kill-after="${RESTREAM_TIMEOUT_KILL_AFTER:-30s}" "$limit" "$@"
}

restream_retry() {
    local label=$1
    local attempts=$2
    local delay=$3
    shift 3

    local attempt
    for ((attempt = 1; attempt <= attempts; attempt++)); do
        if "$@"; then
            return 0
        fi
        echo "$label attempt $attempt/$attempts failed" >&2
        if ((attempt < attempts)); then
            sleep "$delay"
        fi
    done
    return 1
}

restream_format_elapsed() {
    local total=$1
    printf '%dm%02ds' $((total / 60)) $((total % 60))
}
