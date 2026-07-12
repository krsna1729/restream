#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/deploy/install-systemd-service.sh [--now]

Installs /etc/systemd/system/restream.service.

Environment overrides:
  RESTREAM_SERVICE_NAME      restream
  RESTREAM_BIN               /usr/local/bin/restream
  RESTREAM_USER              restream
  RESTREAM_GROUP             restream
  RESTREAM_WORKDIR           /var/lib/restream
  RESTREAM_ENV_FILE          /etc/restream/restream.env
  RESTREAM_CPU_AFFINITY      optional systemd CPUAffinity value, e.g. "0-5"
  RESTREAM_NUMA_POLICY       optional systemd NUMAPolicy, e.g. "local"
  RESTREAM_NUMA_MASK         optional systemd NUMAMask, e.g. "0"

Use --now to enable and restart the unit after installing it.
EOF
}

start_now=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --now)
      start_now=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
  shift
done

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  echo "must run as root to write systemd unit files" >&2
  exit 1
fi

service_name=${RESTREAM_SERVICE_NAME:-restream}
bin=${RESTREAM_BIN:-/usr/local/bin/restream}
user=${RESTREAM_USER:-restream}
group=${RESTREAM_GROUP:-restream}
workdir=${RESTREAM_WORKDIR:-/var/lib/restream}
env_file=${RESTREAM_ENV_FILE:-/etc/restream/restream.env}
cpu_affinity=${RESTREAM_CPU_AFFINITY:-}
numa_policy=${RESTREAM_NUMA_POLICY:-}
numa_mask=${RESTREAM_NUMA_MASK:-}

unit="/etc/systemd/system/${service_name}.service"
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT

mkdir -p "$(dirname "$env_file")"
install -d -m 0750 -o "$user" -g "$group" "$workdir"
if [[ ! -f "$env_file" ]]; then
  install -m 0640 -o root -g root /dev/null "$env_file"
fi

cat > "$tmp" <<EOF
[Unit]
Description=Restream media runtime
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${user}
Group=${group}
WorkingDirectory=${workdir}
EnvironmentFile=-${env_file}
ExecStart=${bin}
Restart=on-failure
RestartSec=2s
LimitNOFILE=65536
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=true
ReadWritePaths=${workdir}
EOF

if [[ -n "$cpu_affinity" ]]; then
  printf 'CPUAffinity=%s\n' "$cpu_affinity" >> "$tmp"
fi
if [[ -n "$numa_policy" ]]; then
  printf 'NUMAPolicy=%s\n' "$numa_policy" >> "$tmp"
fi
if [[ -n "$numa_mask" ]]; then
  printf 'NUMAMask=%s\n' "$numa_mask" >> "$tmp"
fi

cat >> "$tmp" <<'EOF'

[Install]
WantedBy=multi-user.target
EOF

install -m 0644 -o root -g root "$tmp" "$unit"
systemctl daemon-reload

echo "installed $unit"
if [[ -n "$cpu_affinity" || -n "$numa_policy" || -n "$numa_mask" ]]; then
  echo "placement:"
  [[ -n "$cpu_affinity" ]] && echo "  CPUAffinity=$cpu_affinity"
  [[ -n "$numa_policy" ]] && echo "  NUMAPolicy=$numa_policy"
  [[ -n "$numa_mask" ]] && echo "  NUMAMask=$numa_mask"
fi

if [[ "$start_now" -eq 1 ]]; then
  systemctl enable "$service_name.service"
  systemctl restart "$service_name.service"
  systemctl --no-pager --full status "$service_name.service"
else
  echo "run 'systemctl enable --now $service_name.service' when ready"
fi
