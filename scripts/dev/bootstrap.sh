#!/usr/bin/env bash
# The retained entrypoint installs prerequisites before it ever invokes Cargo.
set -euo pipefail
ROOT="${RESTREAM_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
source "$ROOT/scripts/ci/install-system-deps.sh"
RUST_TOOLCHAIN=''
if [[ -f "$ROOT/rust-toolchain.toml" ]]; then
    while IFS= read -r line; do
        if [[ "$line" =~ ^channel[[:space:]]*=[[:space:]]*\"([^\"]+)\" ]]; then
            RUST_TOOLCHAIN="${BASH_REMATCH[1]}"
            break
        fi
    done < "$ROOT/rust-toolchain.toml"
fi
FRONTEND_NODE_MAJOR="${RESTREAM_FRONTEND_NODE_MAJOR:-22}"
FRONTEND_NODE_MIN_MAJOR=20
WITH_FRONTEND=1
INSTALL_MEDIAMTX=1
CONFIGURE_HARNESS_HOST=0
CHECK_HARNESS_HOST=1
usage() {
    cat <<'EOF'
Usage: scripts/dev/bootstrap.sh [options]
Bootstraps a Debian/Ubuntu development environment: apt prerequisites, rustup
and the pinned Rust toolchain, frontend npm dependencies, repo-managed Git hooks,
and a pinned MediaMTX binary for live-harness interoperability checks.
  --skip-frontend             skip Node.js/npm installation and npm ci
  --skip-mediamtx             skip the live-harness MediaMTX binary
  --configure-harness-host   explicitly persist namespace/SRT-buffer sysctls
  --skip-harness-host-check  skip host readiness checks (Docker only)
  -h, --help                 show help
EOF
}
while (($#)); do
    case "$1" in
        --skip-frontend) WITH_FRONTEND=0 ;;
        --skip-mediamtx) INSTALL_MEDIAMTX=0 ;;
        --configure-harness-host) CONFIGURE_HARNESS_HOST=1 ;;
        --skip-harness-host-check) CHECK_HARNESS_HOST=0 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "bootstrap-dev: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done
if [[ "$(uname -s)" != Linux ]]; then
    echo 'bootstrap-dev: this script currently supports Linux hosts only' >&2
    exit 1
fi
if ! command -v apt-get >/dev/null; then
    echo 'bootstrap-dev: apt-get is required; install dependencies manually on this distro' >&2
    exit 1
fi

ensure_frontend_node_toolchain() (
    local current_major='' version tmp
    if command -v node >/dev/null 2>&1 && version="$(node --version)"; then
        version="${version#v}"
        current_major="${version%%.*}"
    fi
    if command -v npm >/dev/null 2>&1 && [[ "$current_major" =~ ^[0-9]+$ ]] && ((current_major >= FRONTEND_NODE_MIN_MAJOR)); then
        echo "bootstrap-dev: Node.js $(node --version) already satisfies frontend toolchain"
        exit 0
    fi
    echo "bootstrap-dev: installing Node.js ${FRONTEND_NODE_MAJOR}.x frontend toolchain"
    tmp="$(mktemp)"
    trap 'rm -f "$tmp"' EXIT
    # Download/install only the upstream vendor's bootstrap; no removed repo helpers.
    curl -fsSL "https://deb.nodesource.com/setup_${FRONTEND_NODE_MAJOR}.x" -o "$tmp"
    restream_run_as_root bash "$tmp"
    restream_run_as_root apt-get install -y nodejs
)

restream_debian_install_groups dev-workstation
export PATH="$HOME/.cargo/bin:$PATH"
if ! command -v rustup >/dev/null; then
    echo 'bootstrap-dev: installing rustup'
    (
        tmp="$(mktemp)"
        trap 'rm -f "$tmp"' EXIT
        curl https://sh.rustup.rs -sSf -o "$tmp"
        sh "$tmp" -y --default-toolchain none
    )
fi
export PATH="$HOME/.cargo/bin:$PATH"
if [[ -z "$RUST_TOOLCHAIN" ]]; then echo 'bootstrap-dev: failed to read rust-toolchain.toml' >&2; exit 1; fi
echo "bootstrap-dev: installing Rust toolchain $RUST_TOOLCHAIN"
rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal --component rustfmt --component clippy
(cd "$ROOT" && rustup override set "$RUST_TOOLCHAIN" >/dev/null)
if ((WITH_FRONTEND)); then
    ensure_frontend_node_toolchain
    echo 'bootstrap-dev: installing frontend npm dependencies'
    npm ci --include=optional --prefix "$ROOT"
    if ! (cd "$ROOT" && npx tailwindcss --help >/dev/null); then
        echo 'bootstrap-dev: frontend toolchain check failed after npm ci' >&2
        exit 1
    fi
fi
if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo 'bootstrap-dev: pointing core.hooksPath at the repo-managed .githooks'
    git -C "$ROOT" config --local core.hooksPath .githooks
else echo 'bootstrap-dev: skipping Git hooks outside a worktree'; fi
if ((INSTALL_MEDIAMTX)); then "$ROOT/scripts/dev/bootstrap-runtime.sh" --mediamtx-only; fi
if ((CONFIGURE_HARNESS_HOST)); then restream_harness_host_prereqs 1
elif ((CHECK_HARNESS_HOST)); then restream_harness_host_prereqs 0; fi
cat <<'EOF'
bootstrap-dev: done

Next steps:
  cargo xtask release app-native
  cargo run
EOF
