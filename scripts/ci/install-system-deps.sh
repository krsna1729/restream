#!/usr/bin/env bash
# Thin CI adapter for canonical Debian package groups. Package names live in
# scripts/lib/debian-packages.sh; this script only maps workflow-facing profiles
# to those groups and invokes runtime bootstrap when a profile needs MediaMTX.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
# shellcheck source=scripts/lib/debian-packages.sh
source "$ROOT/scripts/lib/debian-packages.sh"

PRINT_ONLY=0
if [[ "${1:-}" == "--print" ]]; then
    PRINT_ONLY=1
    shift
fi

profile=${1:-base}
bootstrap_mediamtx=0
groups=()
case "$profile" in
    base|build)
        groups=(rust-build)
        ;;
    browser|browser-build)
        groups=(rust-build media-tools)
        ;;
    live|live-build)
        groups=(rust-build harness-runtime)
        bootstrap_mediamtx=1
        ;;
    harness-runtime|live-runtime)
        groups=(harness-runtime)
        bootstrap_mediamtx=1
        ;;
    native-build)
        groups=(rust-build native-build)
        ;;
    *)
        echo "ci-system-deps: unknown profile '$profile'" >&2
        echo "expected one of: base, build, browser, browser-build, live, live-build, harness-runtime, live-runtime, native-build" >&2
        exit 2
        ;;
esac

if [[ "$PRINT_ONLY" == "1" ]]; then
    printf 'profile=%s\n' "$profile"
    printf 'groups=%s\n' "${groups[*]}"
    printf 'packages='
    restream_debian_packages_for_groups "${groups[@]}" | paste -sd' ' -
    printf 'bootstrap_mediamtx=%s\n' "$bootstrap_mediamtx"
    exit 0
fi

echo "ci-system-deps: profile=$profile groups=${groups[*]}"
restream_debian_install_groups "${groups[@]}"

if [[ "$bootstrap_mediamtx" == "1" ]]; then
    # The live profiles are the CI counterpart of the documented harness
    # runtime. Keep MediaMTX installation in its canonical bootstrap script
    # rather than duplicating the pinned release URL here.
    restream_with_timeout "ci-system-deps: bootstrap runtime media peer" 5m \
        "$ROOT/scripts/dev/bootstrap-runtime.sh" --mediamtx-only --skip-harness-host-check
fi
