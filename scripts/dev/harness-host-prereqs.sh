#!/usr/bin/env bash
# Inspect or deliberately configure the host prerequisites for Restream's
# private-loopback live harness. Kept separate from both bootstrappers so the
# developer and runtime entry points cannot drift.
set -euo pipefail

CONFIGURE=0

usage() {
    cat <<'EOF'
Usage: scripts/dev/harness-host-prereqs.sh [--configure]

Checks that unprivileged user/network namespaces and the Linux UDP buffer
ceilings required by the live harness are available. With --configure, writes
the approved sysctls to /etc/sysctl.d/99-restream-harness.conf and applies them.

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
