#!/usr/bin/env bash
# Retained compiler-independent entrypoint and primitives shared only by the
# other two retained bootstraps. Package/profile names live in the data file.
set -euo pipefail

RESTREAM_BOOTSTRAP_SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/install-system-deps.sh"
ROOT="${RESTREAM_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
RESTREAM_PACKAGE_DATA="$ROOT/scripts/data/debian-packages.tsv"

restream_run_as_root() {
    if [[ "$(id -u)" -eq 0 ]]; then "$@"
    elif command -v sudo >/dev/null 2>&1; then sudo "$@"
    else echo "need sudo to run as root: $*" >&2; return 1; fi
}

restream_debian_packages_for_groups() {
    local group key rest package found seen=' '
    for group in "$@"; do
        found=0
        while read -r key rest; do
            [[ "$key" == "$group" ]] || continue
            found=1
            for package in $rest; do
                if [[ "$seen" != *" $package "* ]]; then
                    seen+="$package "
                    printf '%s\n' "$package"
                fi
            done
        done < "$RESTREAM_PACKAGE_DATA"
        if [[ "$found" == 0 ]]; then echo "unknown Debian package group: $group" >&2; return 2; fi
    done
}

restream_demote_mirror() {
    local list=/etc/apt/apt-mirrors.txt
    [[ -f "$list" ]] || return 0
    # Only this retained entrypoint is executed across the privilege boundary.
    restream_run_as_root "$RESTREAM_BOOTSTRAP_SELF" --demote-mirror
}

restream_apt_attempt() {
    export DEBIAN_FRONTEND=noninteractive
    if [[ "$1" == install ]]; then dpkg --force-confdef --force-confold --configure -a || return; fi
    exec apt-get -q -o Acquire::Retries=3 -o Acquire::http::Timeout=30 \
        -o Acquire::https::Timeout=30 -o DPkg::Lock::Timeout=120 \
        -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold "$@"
}

restream_debian_install_packages() (
    command -v apt-get >/dev/null || { echo 'apt-get is required; install dependencies manually on this distro' >&2; exit 1; }
    local missing=() package
    for package in "$@"; do dpkg-query -W "$package" >/dev/null 2>&1 || missing+=("$package"); done
    if ((${#missing[@]} == 0)); then echo 'debian-packages: packages already present'; exit 0; fi
    echo "debian-packages: installing: ${missing[*]}"
    local limit="${RESTREAM_APT_TIMEOUT:-5m}" attempts="${RESTREAM_APT_ATTEMPTS:-3}" delay="${RESTREAM_APT_RETRY_DELAY:-5}"
    local log action attempt status success
    log="$(mktemp)"
    trap 'rm -f "$log"' EXIT
    for action in update install; do
        success=0
        for ((attempt=1; attempt<=attempts; attempt++)); do
            local argv=("$action")
            [[ "$action" != install ]] || argv+=(-y "${missing[@]}")
            echo "apt-get $action (timeout $limit; output shown on failure)"
            status=0
            restream_run_as_root timeout --kill-after="${RESTREAM_TIMEOUT_KILL_AFTER:-30s}" "$limit" \
                "$RESTREAM_BOOTSTRAP_SELF" --apt-attempt "${argv[@]}" >"$log" 2>&1 || status=$?
            if ((status == 0)); then success=1; break; fi
            echo "apt-get $action failed (exit $status); last output:" >&2
            tail -n 40 "$log" >&2
            restream_demote_mirror
            echo "apt-get $action attempt $attempt/$attempts failed" >&2
            if ((attempt < attempts)); then sleep "$delay"; fi
        done
        ((success)) || exit 1
    done
)

restream_debian_install_groups() {
    local text packages=()
    text="$(restream_debian_packages_for_groups "$@")" || return
    [[ -z "$text" ]] || mapfile -t packages <<< "$text"
    if ((${#packages[@]} == 0)); then echo 'debian-packages: no packages requested'; return; fi
    restream_debian_install_packages "${packages[@]}"
}

restream_harness_host_prereqs() (
    local configure="${1:-0}"
    if [[ "$(uname -s)" != Linux ]]; then echo 'harness-host-prereqs: Linux host settings are not applicable' >&2; exit 0; fi
    if ((configure)); then
        local conf
        conf="$(mktemp)"
        trap 'rm -f "$conf"' EXIT
        printf '%s\n' '# Required for Restream private-loopback live harness and 8 MiB SRT UDP buffers.' \
            kernel.unprivileged_userns_clone=1 user.max_user_namespaces=28633 \
            net.core.rmem_max=26214400 net.core.wmem_max=8388608 > "$conf"
        restream_run_as_root install -m 0644 "$conf" /etc/sysctl.d/99-restream-harness.conf
        restream_run_as_root sysctl --system >/dev/null
        echo 'harness-host-prereqs: persisted live-harness sysctls'
    fi
    local rmem_max='' wmem_max='' somaxconn='' netdev_max_backlog='' port_range='' udp_mem='' file_max=''
    [[ ! -r /proc/sys/net/core/rmem_max ]] || rmem_max="$(</proc/sys/net/core/rmem_max)"
    [[ ! -r /proc/sys/net/core/wmem_max ]] || wmem_max="$(</proc/sys/net/core/wmem_max)"
    [[ ! -r /proc/sys/net/core/somaxconn ]] || somaxconn="$(</proc/sys/net/core/somaxconn)"
    [[ ! -r /proc/sys/net/core/netdev_max_backlog ]] || netdev_max_backlog="$(</proc/sys/net/core/netdev_max_backlog)"
    [[ ! -r /proc/sys/net/ipv4/ip_local_port_range ]] || port_range="$(</proc/sys/net/ipv4/ip_local_port_range)"
    [[ ! -r /proc/sys/net/ipv4/udp_mem ]] || udp_mem="$(</proc/sys/net/ipv4/udp_mem)"
    [[ ! -r /proc/sys/fs/file-max ]] || file_max="$(</proc/sys/fs/file-max)"
    if command -v unshare >/dev/null 2>&1 && unshare --user --map-root-user --net true >/dev/null 2>&1; then
        echo 'harness-host-prereqs: private live-harness network namespaces are available'
    else
        cat >&2 <<'EOF'
harness-host-prereqs: private live-harness network namespaces are unavailable.
Use --no-netns only as a temporary fallback, or configure this host explicitly:
  scripts/dev/bootstrap-runtime.sh --configure-harness-host
If AppArmor still blocks unshare afterwards, ask the host administrator to
approve that policy change; this helper never disables AppArmor.
EOF
    fi
    local nofile_soft nofile_hard
    nofile_soft="$(ulimit -Sn 2>/dev/null || true)"; nofile_hard="$(ulimit -Hn 2>/dev/null || true)"
    echo "harness-host-prereqs: nofile soft=${nofile_soft:-unavailable} hard=${nofile_hard:-unavailable}"
    if [[ "$nofile_hard" =~ ^[0-9]+$ && "$nofile_hard" -ge 65536 ]]; then
        echo 'harness-host-prereqs: RLIMIT_NOFILE hard limit supports Restream and 1,200-output MSR'
    else
        cat >&2 <<'EOF'
harness-host-prereqs: RLIMIT_NOFILE hard limit is below the 65,536 production/MSR policy.
The harness raises its soft limit automatically, but cannot raise the hard limit.
For systemd, configure LimitNOFILE=65536 (or higher); for an interactive shell,
start it from a session whose `ulimit -Hn` is at least 65536.
EOF
    fi
    echo "harness-host-prereqs: net.core.somaxconn=${somaxconn:-unavailable} net.core.netdev_max_backlog=${netdev_max_backlog:-unavailable} net.ipv4.udp_mem=${udp_mem:-unavailable} fs.file-max=${file_max:-unavailable}"
    if [[ "$somaxconn" =~ ^[0-9]+$ && "$somaxconn" -ge 4096 ]]; then
        echo 'harness-host-prereqs: TCP listener backlog supports the 1,200-output MSR target'
    else echo 'harness-host-prereqs: net.core.somaxconn should be at least 4096 for 1,200 concurrent RTMP peers' >&2; fi
    if [[ "$port_range" =~ ^([0-9]+)[[:space:]]+([0-9]+)$ ]]; then
        local count=$((${BASH_REMATCH[2]} - ${BASH_REMATCH[1]} + 1))
        echo "harness-host-prereqs: net.ipv4.ip_local_port_range=$port_range ($count ports)"
        if ((count < 4096)); then echo 'harness-host-prereqs: local ephemeral-port range should provide at least 4096 ports for MSR' >&2; fi
    else echo "harness-host-prereqs: net.ipv4.ip_local_port_range=${port_range:-unavailable}" >&2; fi
    if [[ "$rmem_max" =~ ^[0-9]+$ && "$rmem_max" -ge 26214400 && "$wmem_max" =~ ^[0-9]+$ && "$wmem_max" -ge 8388608 ]]; then
        echo 'harness-host-prereqs: SRT UDP buffer ceilings satisfy the 8 MiB policy'
    else
        echo "harness-host-prereqs: SRT UDP buffer ceilings are below the live-harness policy (rmem_max=${rmem_max:-unavailable}, wmem_max=${wmem_max:-unavailable}; need at least 26214400 and 8388608)." >&2
        echo 'Configure explicitly with scripts/dev/bootstrap-runtime.sh --configure-harness-host' >&2
    fi
)

restream_ci_main() {
    local print_only=0 profile key name media rest found=0 groups=() package_text
    if [[ "${1:-}" == --print ]]; then print_only=1; shift; fi
    profile="${1:-base}"
    while read -r key name media rest; do
        [[ "$key" == '@profile' && "$name" == "$profile" ]] || continue
        found=1
        read -r -a groups <<< "$rest"
        break
    done < "$RESTREAM_PACKAGE_DATA"
    if (( ! found )); then
        echo "ci-system-deps: unknown profile '$profile'" >&2
        echo 'expected one of: base, build, media, browser, browser-build, live, live-build, harness-runtime, live-runtime' >&2
        return 2
    fi
    package_text="$(restream_debian_packages_for_groups "${groups[@]}")" || return
    if ((print_only)); then
        printf 'profile=%s\ngroups=%s\npackages=%s\nbootstrap_mediamtx=%s\n' "$profile" "${groups[*]}" "${package_text//$'\n'/ }" "$media"
        return
    fi
    echo "ci-system-deps: profile=$profile groups=${groups[*]}"
    restream_debian_install_groups "${groups[@]}"
    if [[ "$media" == 1 ]]; then
        echo 'ci-system-deps: bootstrap runtime media peer (timeout 5m)'
        timeout --kill-after="${RESTREAM_TIMEOUT_KILL_AFTER:-30s}" 5m \
            "$ROOT/scripts/dev/bootstrap-runtime.sh" --mediamtx-only --skip-harness-host-check
    fi
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    case "${1:-}" in
        --apt-attempt) shift; restream_apt_attempt "$@" ;;
        --demote-mirror)
            list=/etc/apt/apt-mirrors.txt
            [[ -f "$list" ]] || exit 0
            awk 'FNR == NR {if (NF && $1 !~ /^#/) entries++; next}
                NF && $1 !~ /^#/ && !dropped && entries > 1 {dropped=1; print "debian-packages: dropping apt mirror " $1 > "/dev/stderr"; next}
                {print}' "$list" "$list" > "$list.new" && mv "$list.new" "$list"
            ;;
        *) restream_ci_main "$@" ;;
    esac
fi
