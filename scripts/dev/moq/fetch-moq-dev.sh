#!/usr/bin/env bash
# Fetch the moq.dev tools the `moq-dev-interop` harness mode is pinned to.
#
# Restream's phase-1 MoQ profile is moq-lite-06 + hang, built on moq-net
# 0.3.15. The interop peers are the binaries released from the same commit:
#
#   moq-net-v0.3.15 / moq-cli-v0.14.7 / moq-relay-v0.17.7
#   commit e2c5b50572cc305afcfffb4f40184200e9965204 (moq-dev/moq)
#
# Each archive is checked against the SHA256 the release publishes in its
# SHA256SUMS asset (recorded below; GitHub's own asset digests agree). The
# binaries go to `.local/bin/moq-0.14.7` and `.local/bin/moq-relay-0.17.7`
# under the repository root and nowhere else.
set -euo pipefail

MOQ_DEV_COMMIT="e2c5b50572cc305afcfffb4f40184200e9965204"
CLI_TAG="moq-cli-v0.14.7"
CLI_ARCHIVE="moq-cli-v0.14.7-x86_64-unknown-linux-gnu.tar.gz"
CLI_SHA256="19b174b98274f6f2e20403469a4472c9d91b91be0a1ae041ceecd5beeb0444ec"
RELAY_TAG="moq-relay-v0.17.7"
RELAY_ARCHIVE="moq-relay-v0.17.7-x86_64-unknown-linux-gnu.tar.gz"
RELAY_SHA256="2a644213cc9dec7805658f91725e3cfc3624577bc761f8a4171cc47ddd59062d"
RELEASES="https://github.com/moq-dev/moq/releases/download"

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
BIN_DIR="$ROOT/.local/bin"
CLI_TARGET="$BIN_DIR/moq-0.14.7"
RELAY_TARGET="$BIN_DIR/moq-relay-0.17.7"

usage() {
    cat <<EOF
Usage: scripts/dev/moq/fetch-moq-dev.sh

Downloads the moq.dev \`moq\` CLI (${CLI_TAG}) and \`moq-relay\` (${RELAY_TAG}),
both built from moq-dev/moq ${MOQ_DEV_COMMIT}, for linux/amd64, verifies each
archive against its pinned SHA256 and installs:

  ${CLI_TARGET#"$ROOT"/}
  ${RELAY_TARGET#"$ROOT"/}

A binary already installed from an archive with the pinned hash is kept.
EOF
}

case "${1:-}" in
    "") ;;
    -h|--help) usage; exit 0 ;;
    *) echo "fetch-moq-dev: unknown argument: $1" >&2; usage >&2; exit 2 ;;
esac

if [[ "$(uname -s)" != "Linux" || "$(uname -m)" != "x86_64" ]]; then
    echo "fetch-moq-dev: only linux/amd64 is pinned (host: $(uname -s)/$(uname -m))" >&2
    exit 1
fi

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT
mkdir -p "$BIN_DIR"

# install_pinned TAG ARCHIVE SHA256 BINARY TARGET
install_pinned() {
    local tag=$1 archive=$2 sha=$3 binary=$4 target=$5
    # The archive hash is the identity: the stamp records the hash the
    # installed binary came from, so a stale or foreign binary is replaced.
    if [[ -x "$target" && "$(cat "$target.sha256" 2>/dev/null)" == "$sha" ]]; then
        echo "fetch-moq-dev: $target already installed from $archive"
        return
    fi
    echo "fetch-moq-dev: downloading $tag/$archive" >&2
    curl --fail --show-error --silent --location \
        --connect-timeout 20 \
        --max-time "${RESTREAM_BOOTSTRAP_DOWNLOAD_TIMEOUT_SECS:-120}" \
        --retry 3 \
        --retry-delay 2 \
        --retry-all-errors \
        "$RELEASES/$tag/$archive" -o "$tmpdir/$archive"
    local actual
    actual="$(sha256sum "$tmpdir/$archive" | awk '{print $1}')"
    if [[ "$actual" != "$sha" ]]; then
        echo "fetch-moq-dev: SHA256 mismatch for $archive" >&2
        echo "  expected $sha" >&2
        echo "  actual   $actual" >&2
        exit 1
    fi
    local inner="${archive%.tar.gz}/bin/$binary"
    tar -xzf "$tmpdir/$archive" -C "$tmpdir" "$inner"
    if [[ ! -x "$tmpdir/$inner" ]]; then
        echo "fetch-moq-dev: $archive did not contain an executable $binary" >&2
        exit 1
    fi
    install -m 0755 "$tmpdir/$inner" "$target.partial"
    mv -f "$target.partial" "$target"
    printf '%s\n' "$sha" >"$target.sha256"
    echo "fetch-moq-dev: installed $target (sha256 verified)"
}

install_pinned "$CLI_TAG" "$CLI_ARCHIVE" "$CLI_SHA256" moq "$CLI_TARGET"
install_pinned "$RELAY_TAG" "$RELAY_ARCHIVE" "$RELAY_SHA256" moq-relay "$RELAY_TARGET"
