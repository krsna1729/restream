#!/usr/bin/env bash
# Install only the host tools required to run Restream's live harness. This is
# separate from bootstrap.sh: it neither installs a compiler nor mutates host
# namespace/sysctl policy, so Docker and runtime-only users do not inherit
# development-machine setup.
set -euo pipefail

if [[ -n "${RESTREAM_REPO_ROOT:-}" ]]; then
    ROOT="$RESTREAM_REPO_ROOT"
elif command -v git >/dev/null 2>&1 && git rev-parse --show-toplevel >/dev/null 2>&1; then
    ROOT="$(git rev-parse --show-toplevel)"
else
    ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fi

MEDIAMTX_VERSION="${RESTREAM_MEDIAMTX_VERSION:-v1.19.1}"
INSTALL_PACKAGES=1
INSTALL_MEDIAMTX=1

usage() {
    cat <<'EOF'
Usage: scripts/dev/bootstrap-runtime.sh [options]

Installs the minimal Debian/Ubuntu runtime tools required by the live harness:
FFmpeg/ffprobe, MediaMTX, networking utilities, SQLite, and certificates.

Options:
  --mediamtx-only     install or update only the pinned MediaMTX binary
  --skip-mediamtx     install runtime packages without MediaMTX
  -h, --help          show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --mediamtx-only)
            INSTALL_PACKAGES=0
            shift
            ;;
        --skip-mediamtx)
            INSTALL_MEDIAMTX=0
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "bootstrap-runtime: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ "$(uname -s)" != "Linux" ]] || ! command -v apt-get >/dev/null; then
    echo "bootstrap-runtime: a Debian/Ubuntu Linux host is required" >&2
    exit 1
fi

run_as_root() {
    if [[ "$(id -u)" -eq 0 ]]; then
        "$@"
    elif command -v sudo >/dev/null; then
        sudo "$@"
    else
        echo "bootstrap-runtime: need sudo to install: $*" >&2
        exit 1
    fi
}

mediamtx_archive_name() {
    case "$(uname -m)" in
        x86_64) echo "mediamtx_${MEDIAMTX_VERSION}_linux_amd64.tar.gz" ;;
        aarch64|arm64) echo "mediamtx_${MEDIAMTX_VERSION}_linux_arm64v8.tar.gz" ;;
        armv7l) echo "mediamtx_${MEDIAMTX_VERSION}_linux_armv7.tar.gz" ;;
        *)
            echo "bootstrap-runtime: unsupported architecture for mediamtx: $(uname -m)" >&2
            exit 1
            ;;
    esac
}

install_mediamtx() {
    local archive_name archive_url tmpdir extracted_bin target_bin current_version
    target_bin="/usr/local/bin/mediamtx"
    if command -v mediamtx >/dev/null 2>&1; then
        current_version="$(mediamtx --version 2>/dev/null | awk 'NR==1 {print $2}')"
        if [[ "$current_version" == "$MEDIAMTX_VERSION" ]]; then
            echo "bootstrap-runtime: mediamtx $MEDIAMTX_VERSION already present"
            return
        fi
    fi

    archive_name="$(mediamtx_archive_name)"
    archive_url="https://github.com/bluenviron/mediamtx/releases/download/${MEDIAMTX_VERSION}/${archive_name}"
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' RETURN
    echo "bootstrap-runtime: installing mediamtx ${MEDIAMTX_VERSION}"
    curl -fsSL "$archive_url" -o "$tmpdir/$archive_name"
    tar -xzf "$tmpdir/$archive_name" -C "$tmpdir"
    extracted_bin="$tmpdir/mediamtx"
    if [[ ! -x "$extracted_bin" ]]; then
        echo "bootstrap-runtime: mediamtx archive did not contain an executable binary" >&2
        exit 1
    fi
    run_as_root install -m 0755 "$extracted_bin" "$target_bin"
}

if (( INSTALL_PACKAGES )); then
    packages=(ca-certificates curl ffmpeg iproute2 jq sqlite3 util-linux)
    missing=()
    for package in "${packages[@]}"; do
        dpkg-query -W "$package" >/dev/null 2>&1 || missing+=("$package")
    done
    if ((${#missing[@]})); then
        echo "bootstrap-runtime: installing apt packages: ${missing[*]}"
        run_as_root apt-get update
        run_as_root apt-get install -y "${missing[@]}"
    else
        echo "bootstrap-runtime: runtime packages already present"
    fi
fi

if (( INSTALL_MEDIAMTX )); then
    install_mediamtx
fi

echo "bootstrap-runtime: ready"
