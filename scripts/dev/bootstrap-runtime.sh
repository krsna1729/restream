#!/usr/bin/env bash
# Compilerless runtime path: never builds xtask or installs Rust/Node.
set -euo pipefail
ROOT="${RESTREAM_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
source "$ROOT/scripts/ci/install-system-deps.sh"
MEDIAMTX_VERSION="${RESTREAM_MEDIAMTX_VERSION:-v1.20.0}"
INSTALL_PACKAGES=1
INSTALL_MEDIAMTX=1
CHECK_HARNESS_HOST=1
CONFIGURE_HARNESS_HOST=0

usage() {
    cat <<'EOF'
Usage: scripts/dev/bootstrap-runtime.sh [options]
Installs minimal Debian/Ubuntu live-harness tools: FFmpeg/ffprobe, MediaMTX,
networking utilities, SQLite and certificates, without a compiler.
  --mediamtx-only              install/update only the pinned MediaMTX binary
  --skip-mediamtx              install runtime packages without MediaMTX
  --configure-harness-host    explicitly persist namespace/SRT-buffer sysctls
  --skip-harness-host-check   skip host readiness checks (Docker only)
  -h, --help                  show help
EOF
}
while (($#)); do
    case "$1" in
        --mediamtx-only) INSTALL_PACKAGES=0 ;;
        --skip-mediamtx) INSTALL_MEDIAMTX=0 ;;
        --configure-harness-host) CONFIGURE_HARNESS_HOST=1 ;;
        --skip-harness-host-check) CHECK_HARNESS_HOST=0 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "bootstrap-runtime: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done
if [[ "$(uname -s)" != Linux ]] || ! command -v apt-get >/dev/null; then
    echo 'bootstrap-runtime: a Debian/Ubuntu Linux host is required' >&2
    exit 1
fi

install_mediamtx() (
    local current_version='' archive_name archive_url tmpdir
    if command -v mediamtx >/dev/null 2>&1; then
        current_version="$(mediamtx --version 2>/dev/null | awk 'NR==1 {print $2}')"
        if [[ "$current_version" == "$MEDIAMTX_VERSION" ]]; then
            echo "bootstrap-runtime: mediamtx $MEDIAMTX_VERSION already present"
            exit 0
        fi
    fi
    case "$(uname -m)" in
        x86_64) archive_name="mediamtx_${MEDIAMTX_VERSION}_linux_amd64.tar.gz" ;;
        aarch64|arm64) archive_name="mediamtx_${MEDIAMTX_VERSION}_linux_arm64v8.tar.gz" ;;
        armv7l) archive_name="mediamtx_${MEDIAMTX_VERSION}_linux_armv7.tar.gz" ;;
        *) echo "bootstrap-runtime: unsupported architecture for mediamtx: $(uname -m)" >&2; exit 1 ;;
    esac
    archive_url="https://github.com/bluenviron/mediamtx/releases/download/${MEDIAMTX_VERSION}/${archive_name}"
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT
    echo "bootstrap-runtime: installing mediamtx $MEDIAMTX_VERSION"
    curl --fail --show-error --location --connect-timeout 20 \
        --max-time "${RESTREAM_BOOTSTRAP_DOWNLOAD_TIMEOUT_SECS:-120}" \
        --retry 3 --retry-delay 2 --retry-all-errors "$archive_url" -o "$tmpdir/$archive_name"
    tar -xzf "$tmpdir/$archive_name" -C "$tmpdir"
    if [[ ! -x "$tmpdir/mediamtx" ]]; then
        echo 'bootstrap-runtime: mediamtx archive did not contain an executable binary' >&2
        exit 1
    fi
    restream_run_as_root install -m 0755 "$tmpdir/mediamtx" /usr/local/bin/mediamtx
)

if ((INSTALL_PACKAGES)); then restream_debian_install_groups harness-runtime; fi
if ((INSTALL_MEDIAMTX)); then install_mediamtx; fi
if ((CONFIGURE_HARNESS_HOST)); then restream_harness_host_prereqs 1
elif ((CHECK_HARNESS_HOST)); then restream_harness_host_prereqs 0; fi
echo 'bootstrap-runtime: ready'
