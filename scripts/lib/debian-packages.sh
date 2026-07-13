#!/usr/bin/env bash
# Canonical Debian/Ubuntu package groups for CI and developer bootstraps.
# Callers choose groups; this file owns package names and apt retry/timeout
# behavior so CI, runtime bootstrap, and dev bootstrap cannot drift.

RESTREAM_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/common.sh
source "$RESTREAM_LIB_DIR/common.sh"

restream_debian_packages_for_group() {
    local group=$1
    case "$group" in
        rust-build)
            printf '%s\n' libssl-dev pkg-config clang
            ;;
        media-tools)
            printf '%s\n' ca-certificates curl ffmpeg
            ;;
        harness-runtime)
            printf '%s\n' ca-certificates curl ffmpeg iproute2 jq sqlite3 util-linux
            ;;
        native-build)
            printf '%s\n' build-essential bzip2 cmake curl file git mold nasm ninja-build perl pkg-config
            ;;
        ffmpeg-dev-headers)
            printf '%s\n' \
                libavcodec-dev \
                libavdevice-dev \
                libavfilter-dev \
                libavformat-dev \
                libavutil-dev \
                libswresample-dev \
                libswscale-dev
            ;;
        dev-workstation)
            # This preserves scripts/dev/bootstrap.sh's historical package
            # surface while moving ownership of the list out of the bootstrap
            # orchestrator.
            printf '%s\n' \
                build-essential \
                bzip2 \
                ca-certificates \
                clang \
                cmake \
                curl \
                ffmpeg \
                file \
                git \
                jq \
                libavcodec-dev \
                libavdevice-dev \
                libavfilter-dev \
                libavformat-dev \
                libavutil-dev \
                libswresample-dev \
                libswscale-dev \
                mold \
                nasm \
                ninja-build \
                perl \
                pkg-config \
                iproute2 \
                sqlite3 \
                tzdata \
                util-linux
            ;;
        *)
            echo "unknown Debian package group: $group" >&2
            return 2
            ;;
    esac
}

restream_debian_packages_for_groups() {
    local seen=" "
    local group package
    for group in "$@"; do
        while IFS= read -r package; do
            [[ -n "$package" ]] || continue
            if [[ "$seen" != *" $package "* ]]; then
                seen+="$package "
                printf '%s\n' "$package"
            fi
        done < <(restream_debian_packages_for_group "$group")
    done
}

restream_debian_install_packages() {
    if ! command -v apt-get >/dev/null 2>&1; then
        echo "apt-get is required; install dependencies manually on this distro" >&2
        return 1
    fi

    local missing=()
    local package
    for package in "$@"; do
        dpkg-query -W "$package" >/dev/null 2>&1 || missing+=("$package")
    done

    if ((${#missing[@]} == 0)); then
        echo "debian-packages: packages already present"
        return 0
    fi

    echo "debian-packages: installing: ${missing[*]}"
    local apt_timeout="${RESTREAM_APT_TIMEOUT:-5m}"
    local apt_attempts="${RESTREAM_APT_ATTEMPTS:-3}"
    local apt_retry_delay="${RESTREAM_APT_RETRY_DELAY:-5}"

    restream_retry "apt-get update" "$apt_attempts" "$apt_retry_delay" \
        restream_with_timeout "apt-get update" "$apt_timeout" \
        restream_run_as_root apt-get update -qq
    restream_retry "apt-get install" "$apt_attempts" "$apt_retry_delay" \
        restream_with_timeout "apt-get install" "$apt_timeout" \
        restream_run_as_root apt-get install -y -qq "${missing[@]}"
}

restream_debian_install_groups() {
    local packages=()
    mapfile -t packages < <(restream_debian_packages_for_groups "$@")
    if ((${#packages[@]} == 0)); then
        echo "debian-packages: no packages requested"
        return 0
    fi
    restream_debian_install_packages "${packages[@]}"
}
