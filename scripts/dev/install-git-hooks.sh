#!/usr/bin/env bash
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
HOOKS_DIR="$ROOT/.githooks"

if ! git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "install-git-hooks: $ROOT is not a Git worktree" >&2
    exit 1
fi

chmod +x "$HOOKS_DIR"/*

current_hooks_path="$(git -C "$ROOT" config --get core.hooksPath || true)"

# Cloud Agent / Cursor wraps hooks with a dispatcher under
# ~/.cursor/agent-hooks/... and chains the previous hooksPath via
# .cursor-original-hooks-path. Point that file at .githooks so both run;
# overwriting core.hooksPath would drop Cursor's commit-msg trailers.
if [[ -n "$current_hooks_path" && -f "$current_hooks_path/.dispatcher" && -f "$current_hooks_path/.cursor-original-hooks-path" ]]; then
    printf '%s\n' "$HOOKS_DIR" >"$current_hooks_path/.cursor-original-hooks-path"
    echo "install-git-hooks: chained $HOOKS_DIR through Cursor dispatcher at $current_hooks_path"
else
    git -C "$ROOT" config core.hooksPath "$HOOKS_DIR"
    echo "install-git-hooks: Git hooks installed to $HOOKS_DIR"
fi
