#!/usr/bin/env bash
# Install the small, named system-dependency profiles used by CI.
# GitHub-hosted runners are ephemeral, so apt packages cannot be restored from
# the repository cache; all build products are cached separately by CI actions.
set -euo pipefail

profile=${1:-base}
case "$profile" in
    base)
        packages=(libssl-dev pkg-config clang)
        ;;
    browser)
        packages=(libssl-dev pkg-config clang ffmpeg)
        ;;
    live)
        packages=(libssl-dev pkg-config clang ffmpeg jq curl iproute2 util-linux sqlite3)
        ;;
    *)
        echo "ci-system-deps: unknown profile '$profile' (expected base, browser, or live)" >&2
        exit 2
        ;;
esac

if ! command -v apt-get >/dev/null; then
    echo "ci-system-deps: apt-get is required for the $profile profile" >&2
    exit 1
fi

missing=()
for package in "${packages[@]}"; do
    dpkg-query -W "$package" >/dev/null 2>&1 || missing+=("$package")
done

if ((${#missing[@]} == 0)); then
    echo "ci-system-deps: $profile profile already satisfied"
else
    echo "ci-system-deps: installing $profile profile: ${missing[*]}"
    sudo apt-get update -qq
    sudo apt-get install -y -qq "${missing[@]}"
fi

# The live profile is the CI counterpart of the documented harness runtime.
# Keep MediaMTX installation in its canonical bootstrap script rather than
# duplicating the pinned release URL here. GitHub runners intentionally use
# host networking for the harness, so they must not persist or validate the
# optional host sysctl setup.
if [[ "$profile" == "live" ]]; then
    scripts/dev/bootstrap-runtime.sh --mediamtx-only --skip-harness-host-check
fi
