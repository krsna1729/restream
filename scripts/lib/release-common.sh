#!/usr/bin/env bash
# Shared release-script checks. Release scripts remain user-facing verbs; this
# file owns the repeated validation rules so local due diligence, packaging,
# tagging, and publishing do not drift on what a safe version/ref means.

RESTREAM_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/common.sh
source "$RESTREAM_LIB_DIR/common.sh"

restream_release_require_version() {
    local caller=$1
    local version=$2
    if [[ -z "$version" ]]; then
        echo "$caller: version is required" >&2
        return 2
    fi
    if [[ ! "$version" =~ ^v?[0-9][0-9A-Za-z._+-]*$ ]]; then
        echo "$caller: version must be filename-safe: $version" >&2
        return 2
    fi
}

restream_release_require_tag() {
    local caller=$1
    local tag=$2
    if [[ -z "$tag" ]]; then
        echo "$caller: tag is required" >&2
        return 2
    fi
    if [[ ! "$tag" =~ ^v[0-9][0-9A-Za-z._-]*$ ]]; then
        echo "$caller: tag must start with v and be release-safe, got: $tag" >&2
        return 2
    fi
}

restream_release_require_clean_checkout() {
    local caller=$1
    if [[ -n "$(git status --porcelain)" ]]; then
        echo "$caller: checkout must be clean before tagging" >&2
        git status --short >&2
        return 1
    fi
}

restream_release_ref_or_current_branch() {
    local ref="${1:-$(git branch --show-current)}"
    if [[ -z "$ref" ]]; then
        echo "could not infer branch; pass a ref explicitly" >&2
        return 2
    fi
    printf '%s\n' "$ref"
}
