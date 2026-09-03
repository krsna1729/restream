#!/usr/bin/env bash
# Inspect or deliberately configure the host prerequisites for Restream's
# private-loopback live harness. Kept separate from both bootstrappers so the
# developer and runtime entry points cannot drift.
set -euo pipefail

CONFIGURE=0

usage() {
    cat <<'EOF'
Usage: scripts/dev/harness-host-prereqs.sh [--configure]

Checks the process limits and Linux kernel settings that bound the live
harness: private user/network namespaces, SRT UDP buffers, listener backlog,
and local ephemeral-port capacity. With --configure, writes the approved SRT
buffer and namespace sysctls to /etc/sysctl.d/99-restream-harness.conf and
applies them.

The helper never disables AppArmor or another host security policy.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --configure)
            CONFIGURE=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "harness-host-prereqs: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ "$(uname -s)" != "Linux" ]]; then
    echo "harness-host-prereqs: Linux host settings are not applicable" >&2
    exit 0
fi

run_as_root() {
    if [[ "$(id -u)" -eq 0 ]]; then
        "$@"
    elif command -v sudo >/dev/null; then
        sudo "$@"
    else
        echo "harness-host-prereqs: need sudo to configure host settings" >&2
        exit 1
    fi
}

unprivileged_netns_available() {
    command -v unshare >/dev/null 2>&1 \
        && unshare --user --map-root-user --net true >/dev/null 2>&1
}

sysctl_value() {
    local key=$1
    local path="/proc/sys/${key//./\/}"
    [[ -r "$path" ]] && <"$path"
}

if (( CONFIGURE )); then
    conf="$(mktemp)"
    trap 'rm -f "$conf"' EXIT
    cat >"$conf" <<'EOF'
# Required for Restream's private-loopback live harness and 8 MiB SRT UDP buffers.
kernel.unprivileged_userns_clone=1
user.max_user_namespaces=28633
net.core.rmem_max=26214400
net.core.wmem_max=8388608
EOF
    run_as_root install -m 0644 "$conf" /etc/sysctl.d/99-restream-harness.conf
    run_as_root sysctl --system >/dev/null
    echo "harness-host-prereqs: persisted live-harness sysctls"
fi

rmem_max="$(sysctl_value net.core.rmem_max || true)"
wmem_max="$(sysctl_value net.core.wmem_max || true)"
somaxconn="$(sysctl_value net.core.somaxconn || true)"
netdev_max_backlog="$(sysctl_value net.core.netdev_max_backlog || true)"
port_range="$(sysctl_value net.ipv4.ip_local_port_range || true)"
udp_mem="$(sysctl_value net.ipv4.udp_mem || true)"
file_max="$(sysctl_value fs.file-max || true)"
nofile_soft="$(ulimit -Sn 2>/dev/null || true)"
nofile_hard="$(ulimit -Hn 2>/dev/null || true)"

if unprivileged_netns_available; then
    echo "harness-host-prereqs: private live-harness network namespaces are available"
else
    cat >&2 <<'EOF'
harness-host-prereqs: private live-harness network namespaces are unavailable.
Use --no-netns only as a temporary fallback, or configure this host explicitly:
  scripts/dev/harness-host-prereqs.sh --configure

If AppArmor still blocks unshare afterwards, ask the host administrator to
approve that policy change; this helper never disables AppArmor.
EOF
fi

echo "harness-host-prereqs: nofile soft=${nofile_soft:-unavailable} hard=${nofile_hard:-unavailable}"
if [[ "$nofile_hard" =~ ^[0-9]+$ && "$nofile_hard" -ge 65536 ]]; then
    echo "harness-host-prereqs: RLIMIT_NOFILE hard limit supports Restream and 1,200-output MSR"
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
    echo "harness-host-prereqs: TCP listener backlog supports the 1,200-output MSR target"
else
    echo "harness-host-prereqs: net.core.somaxconn should be at least 4096 for 1,200 concurrent RTMP peers" >&2
fi

if [[ "$port_range" =~ ^([0-9]+)[[:space:]]+([0-9]+)$ ]]; then
    port_low="${BASH_REMATCH[1]}"
    port_high="${BASH_REMATCH[2]}"
    port_count=$((port_high - port_low + 1))
    echo "harness-host-prereqs: net.ipv4.ip_local_port_range=${port_range} (${port_count} ports)"
    if (( port_count < 4096 )); then
        echo "harness-host-prereqs: local ephemeral-port range should provide at least 4096 ports for MSR" >&2
    fi
else
    echo "harness-host-prereqs: net.ipv4.ip_local_port_range=${port_range:-unavailable}" >&2
fi

if [[ "$rmem_max" =~ ^[0-9]+$ && "$rmem_max" -ge 26214400 \
    && "$wmem_max" =~ ^[0-9]+$ && "$wmem_max" -ge 8388608 ]]; then
    echo "harness-host-prereqs: SRT UDP buffer ceilings satisfy the 8 MiB policy"
else
    cat >&2 <<EOF
harness-host-prereqs: SRT UDP buffer ceilings are below the live-harness policy
(rmem_max=${rmem_max:-unavailable}, wmem_max=${wmem_max:-unavailable}; need at least 26214400 and 8388608).
Configure them explicitly with:
  scripts/dev/harness-host-prereqs.sh --configure
EOF
fi
