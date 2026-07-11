#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${RESTREAM_REPO_ROOT:-}" ]]; then
    ROOT="$RESTREAM_REPO_ROOT"
elif command -v git >/dev/null 2>&1 && git rev-parse --show-toplevel >/dev/null 2>&1; then
    ROOT="$(git rev-parse --show-toplevel)"
else
    # Container/tarball onboarding can run before Git is installed. Resolve
    # the repository from this script's canonical location instead of making
    # the bootstrapper depend on the tool it is about to provision.
    ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fi
RUST_TOOLCHAIN=""
if [[ -f "$ROOT/rust-toolchain.toml" ]]; then
    RUST_TOOLCHAIN="$(sed -n 's/^channel = "\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml")"
fi
FRONTEND_NODE_MAJOR="${RESTREAM_FRONTEND_NODE_MAJOR:-22}"
FRONTEND_NODE_MIN_MAJOR=20
WITH_FRONTEND=1
RUN_NATIVE_SETUP=1
INSTALL_MEDIAMTX=1
CONFIGURE_HARNESS_HOST=0

usage() {
    cat <<'EOF'
Usage: scripts/dev/bootstrap.sh [options]

Bootstraps a Debian/Ubuntu development environment for this repo:
  - installs required apt packages
  - installs rustup if needed and the pinned Rust toolchain
  - installs frontend npm dependencies
  - installs a pinned mediamtx binary for live harness interoperability checks
  - builds the pinned native dependency prefix via scripts/build/native-deps.sh

Options:
  --skip-frontend      skip nodejs/npm install and npm ci
  --skip-native-setup  skip scripts/build/native-deps.sh
  --skip-mediamtx      skip the live-harness Mediamtx binary
  --configure-harness-host
                      explicitly persist Linux user-namespace and SRT UDP-buffer
                      prerequisites for the private-network live harness
  -h, --help           show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-frontend)
            WITH_FRONTEND=0
            shift
            ;;
        --skip-native-setup)
            RUN_NATIVE_SETUP=0
            shift
            ;;
        --skip-mediamtx)
            INSTALL_MEDIAMTX=0
            shift
            ;;
        --configure-harness-host)
            CONFIGURE_HARNESS_HOST=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "bootstrap-dev: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ "$(uname -s)" != "Linux" ]]; then
    echo "bootstrap-dev: this script currently supports Linux hosts only" >&2
    exit 1
fi

if ! command -v apt-get >/dev/null; then
    echo "bootstrap-dev: apt-get is required; install dependencies manually on this distro" >&2
    exit 1
fi

APT_PACKAGES=(
    build-essential
    bzip2
    ca-certificates
    clang
    cmake
    curl
    ffmpeg
    file
    git
    jq
    libavcodec-dev
    libavdevice-dev
    libavfilter-dev
    libavformat-dev
    libavutil-dev
    libswresample-dev
    libswscale-dev
    mold
    nasm
    ninja-build
    perl
    pkg-config
    iproute2
    sqlite3
    tzdata
    util-linux
)

run_as_root() {
    if [[ "$(id -u)" -eq 0 ]]; then
        "$@"
    elif command -v sudo >/dev/null; then
        sudo "$@"
    else
        echo "bootstrap-dev: need sudo to install: $*" >&2
        exit 1
    fi
}

ensure_frontend_node_toolchain() {
    local current_major=""
    if command -v node >/dev/null 2>&1; then
        current_major="$(node -p 'process.versions.node.split(".")[0]')"
    fi

    if command -v npm >/dev/null 2>&1 &&
        [[ -n "$current_major" ]] &&
        (( current_major >= FRONTEND_NODE_MIN_MAJOR )); then
        echo "bootstrap-dev: Node.js $(node --version) already satisfies frontend toolchain"
        return
    fi

    echo "bootstrap-dev: installing Node.js ${FRONTEND_NODE_MAJOR}.x frontend toolchain"
    run_as_root bash -lc "curl -fsSL https://deb.nodesource.com/setup_${FRONTEND_NODE_MAJOR}.x | bash -"
    run_as_root apt-get install -y nodejs
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

configure_harness_host() {
    local conf
    conf="$(mktemp)"
    cat >"$conf" <<'EOF'
# Required for Restream's private-loopback live harness and 8 MiB SRT UDP buffers.
kernel.unprivileged_userns_clone=1
user.max_user_namespaces=28633
net.core.rmem_max=26214400
net.core.wmem_max=8388608
EOF
    run_as_root install -m 0644 "$conf" /etc/sysctl.d/99-restream-harness.conf
    rm -f "$conf"
    run_as_root sysctl --system >/dev/null
}

report_harness_host_prerequisites() {
    local rmem_max wmem_max
    rmem_max="$(sysctl_value net.core.rmem_max || true)"
    wmem_max="$(sysctl_value net.core.wmem_max || true)"

    if unprivileged_netns_available; then
        echo "bootstrap-dev: private live-harness network namespaces are available"
    else
        cat >&2 <<'EOF'
bootstrap-dev: private live-harness network namespaces are unavailable.
Use --no-netns only as a temporary fallback, or explicitly configure this host:
  scripts/dev/bootstrap.sh --configure-harness-host

If an AppArmor policy still blocks unshare after configuration, ask the host
administrator to approve that policy change; bootstrap never disables AppArmor.
EOF
    fi

    if [[ "$rmem_max" =~ ^[0-9]+$ && "$rmem_max" -ge 26214400 \
        && "$wmem_max" =~ ^[0-9]+$ && "$wmem_max" -ge 8388608 ]]; then
        echo "bootstrap-dev: SRT UDP buffer ceilings satisfy the 8 MiB harness policy"
    else
        cat >&2 <<EOF
bootstrap-dev: SRT UDP buffer ceilings are below the live-harness policy
(rmem_max=${rmem_max:-unavailable}, wmem_max=${wmem_max:-unavailable}; need at least 26214400 and 8388608).
Configure them explicitly with:
  scripts/dev/bootstrap.sh --configure-harness-host
EOF
    fi
}

missing_packages=()
for package in "${APT_PACKAGES[@]}"; do
    if ! dpkg-query -W "$package" >/dev/null 2>&1; then
        missing_packages+=("$package")
    fi
done

if ((${#missing_packages[@]})); then
    echo "bootstrap-dev: installing apt packages: ${missing_packages[*]}"
    run_as_root apt-get update
    run_as_root apt-get install -y "${missing_packages[@]}"
else
    echo "bootstrap-dev: apt packages already present"
fi

export PATH="$HOME/.cargo/bin:$PATH"

if ! command -v rustup >/dev/null; then
    echo "bootstrap-dev: installing rustup"
    curl https://sh.rustup.rs -sSf | sh -s -- -y --default-toolchain none
fi

export PATH="$HOME/.cargo/bin:$PATH"

if [[ -z "$RUST_TOOLCHAIN" ]]; then
    echo "bootstrap-dev: failed to read rust-toolchain.toml" >&2
    exit 1
fi

echo "bootstrap-dev: installing Rust toolchain $RUST_TOOLCHAIN"
rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal --component rustfmt --component clippy
(cd "$ROOT" && rustup override set "$RUST_TOOLCHAIN" >/dev/null)

if (( WITH_FRONTEND )); then
    ensure_frontend_node_toolchain
    echo "bootstrap-dev: installing frontend npm dependencies"
    npm ci --include=optional --prefix "$ROOT"
    if ! (cd "$ROOT" && npx tailwindcss --help >/dev/null); then
        echo "bootstrap-dev: frontend toolchain check failed after npm ci" >&2
        exit 1
    fi
fi

if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "bootstrap-dev: installing repo-managed Git hooks"
    "$ROOT/scripts/dev/install-git-hooks.sh"
else
    echo "bootstrap-dev: skipping Git hooks outside a worktree"
fi

if (( INSTALL_MEDIAMTX )); then
    "$ROOT/scripts/dev/bootstrap-runtime.sh" --mediamtx-only
fi

if (( CONFIGURE_HARNESS_HOST )); then
    echo "bootstrap-dev: persisting live-harness host prerequisites"
    configure_harness_host
fi

report_harness_host_prerequisites

if (( RUN_NATIVE_SETUP )); then
    echo "bootstrap-dev: building pinned native dependency prefix"
    "$ROOT/scripts/build/resource-limit.sh" "$ROOT/scripts/build/native-deps.sh"
fi

cat <<EOF
bootstrap-dev: done

Next steps:
  scripts/build/resource-limit.sh ./scripts/build/app-native.sh
  cargo run
EOF
