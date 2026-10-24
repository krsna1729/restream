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
        dev-workstation)
            # This preserves scripts/dev/bootstrap.sh's historical package
            # surface while moving ownership of the list out of the bootstrap
            # orchestrator.
            printf '%s\n' \
                build-essential \
                ca-certificates \
                clang \
                curl \
                ffmpeg \
                git \
                jq \
                mold \
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
    local apt_log
    apt_log=$(mktemp)

    local status=0
    restream_retry "apt-get update" "$apt_attempts" "$apt_retry_delay" \
        restream_debian_apt "$apt_log" "$apt_timeout" update &&
        restream_retry "apt-get install" "$apt_attempts" "$apt_retry_delay" \
            restream_debian_apt "$apt_log" "$apt_timeout" install -y "${missing[@]}" ||
        status=$?
    rm -f "$apt_log"
    return "$status"
}

# restream_debian_apt LOG LIMIT ARGS...: one bounded, non-interactive apt-get
# run. Its output goes to LOG and is printed only on failure, so a run that
# hits the time limit shows where it stopped. A run killed mid-install can
# leave dpkg half-configured, so each install attempt first finishes pending
# configuration.
restream_debian_apt() {
    local log=$1
    local limit=$2
    shift 2
    local status=0
    echo "apt-get $1 (timeout ${limit}; output shown on failure)"
    restream_with_timeout_as_root "apt-get $1" "$limit" bash -c '
        export DEBIAN_FRONTEND=noninteractive
        if [[ "$1" == install ]]; then
            dpkg --force-confdef --force-confold --configure -a
        fi
        exec apt-get -q \
            -o Acquire::Retries=3 \
            -o Acquire::http::Timeout=30 \
            -o Acquire::https::Timeout=30 \
            -o DPkg::Lock::Timeout=120 \
            -o Dpkg::Options::=--force-confdef \
            -o Dpkg::Options::=--force-confold \
            "$@"
    ' bash "$@" >"$log" 2>&1 || status=$?
    if ((status != 0)); then
        echo "apt-get $1 failed (exit $status); last output:" >&2
        tail -n 40 "$log" >&2
        restream_debian_demote_mirror
    fi
    return "$status"
}

# GitHub's Ubuntu runners fetch through the apt `mirror` method from
# /etc/apt/apt-mirrors.txt, which falls over to the next mirror on errors but
# not on a download that only crawls; such stalls on the first (Azure) mirror
# are what time installs out. After a failed run, drop the first mirror so the
# next attempt fetches from the next one. Hosts without that list are untouched,
# and the last mirror is never dropped.
restream_debian_demote_mirror() {
    local list=/etc/apt/apt-mirrors.txt
    [[ -f "$list" ]] || return 0
    # One predicate for counting and dropping: a mirror entry is a line whose
    # first field exists and is not a comment.
    restream_run_as_root bash -c '
        awk '\''
            FNR == NR { if (NF && $1 !~ /^#/) entries++; next }
            NF && $1 !~ /^#/ && !dropped && entries > 1 {
                dropped = 1
                print "debian-packages: dropping apt mirror " $1 > "/dev/stderr"
                next
            }
            { print }
        '\'' "$1" "$1" >"$1.new" && mv "$1.new" "$1"
    ' bash "$list"
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
