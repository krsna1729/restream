#!/usr/bin/env bash
# Fetch the MediaMTX release the MoQ interop harness is pinned to.
#
# The `moq-relay-interop` harness mode and the "MoQ interop" CI job need a
# MediaMTX that speaks native MoQ over QUIC (ALPN moqt-19) and forwards to a
# moqt:// destination. That is v1.21.2, which is newer than the MediaMTX the
# other live modes and the demo use (/usr/local/bin/mediamtx, installed by
# scripts/dev/bootstrap-runtime.sh). This script never touches that binary:
# it installs only `.local/bin/mediamtx-1.21.2` under the repository root.
#
# Pin: MediaMTX v1.21.2, commit 914e39535ece709919ac325aaeed98d394ed9093.
# SHA256 of mediamtx_v1.21.2_linux_amd64.tar.gz, copied from the release's
# published checksums file
# (https://github.com/bluenviron/mediamtx/releases/download/v1.21.2/checksums.sha256).
set -euo pipefail

MEDIAMTX_VERSION="v1.21.2"
MEDIAMTX_COMMIT="914e39535ece709919ac325aaeed98d394ed9093"
ARCHIVE="mediamtx_${MEDIAMTX_VERSION}_linux_amd64.tar.gz"
ARCHIVE_SHA256="121be6e00397e473a95d83c75f4fcd3819a6af3b0e1a50889cba43ab4fab6a27"
URL="https://github.com/bluenviron/mediamtx/releases/download/${MEDIAMTX_VERSION}/${ARCHIVE}"

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
BIN_DIR="$ROOT/.local/bin"
TARGET="$BIN_DIR/mediamtx-${MEDIAMTX_VERSION#v}"

usage() {
    cat <<EOF
Usage: scripts/dev/moq/fetch-mediamtx.sh [--print-path]

Downloads MediaMTX ${MEDIAMTX_VERSION} (commit ${MEDIAMTX_COMMIT}) for
linux/amd64, verifies the archive against the pinned SHA256 and installs the
binary at ${TARGET#"$ROOT"/}. An existing binary that reports
${MEDIAMTX_VERSION} is kept. --print-path prints the installed path only.
EOF
}

print_path=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --print-path)
            print_path=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "fetch-mediamtx: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ "$(uname -s)" != "Linux" || "$(uname -m)" != "x86_64" ]]; then
    echo "fetch-mediamtx: only linux/amd64 is pinned (host: $(uname -s)/$(uname -m))" >&2
    exit 1
fi

installed_version() {
    "$TARGET" --version 2>/dev/null | grep -o -m1 'v[0-9][0-9.]*' | head -n1
}

if [[ -x "$TARGET" && "$(installed_version)" == "$MEDIAMTX_VERSION" ]]; then
    if (( print_path )); then
        echo "$TARGET"
    else
        echo "fetch-mediamtx: $TARGET already reports $MEDIAMTX_VERSION"
    fi
    exit 0
fi

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

echo "fetch-mediamtx: downloading $URL" >&2
curl --fail --show-error --silent --location \
    --connect-timeout 20 \
    --max-time "${RESTREAM_BOOTSTRAP_DOWNLOAD_TIMEOUT_SECS:-120}" \
    --retry 3 \
    --retry-delay 2 \
    --retry-all-errors \
    "$URL" -o "$tmpdir/$ARCHIVE"

actual="$(sha256sum "$tmpdir/$ARCHIVE" | awk '{print $1}')"
if [[ "$actual" != "$ARCHIVE_SHA256" ]]; then
    echo "fetch-mediamtx: SHA256 mismatch for $ARCHIVE" >&2
    echo "  expected $ARCHIVE_SHA256" >&2
    echo "  actual   $actual" >&2
    exit 1
fi

tar -xzf "$tmpdir/$ARCHIVE" -C "$tmpdir" mediamtx
if [[ ! -x "$tmpdir/mediamtx" ]]; then
    echo "fetch-mediamtx: $ARCHIVE did not contain an executable mediamtx" >&2
    exit 1
fi

mkdir -p "$BIN_DIR"
install -m 0755 "$tmpdir/mediamtx" "$TARGET.partial"
mv -f "$TARGET.partial" "$TARGET"

reported="$(installed_version)"
if [[ "$reported" != "$MEDIAMTX_VERSION" ]]; then
    echo "fetch-mediamtx: installed binary reports '$reported', expected $MEDIAMTX_VERSION" >&2
    exit 1
fi

if (( print_path )); then
    echo "$TARGET"
else
    echo "fetch-mediamtx: installed $TARGET ($MEDIAMTX_VERSION, sha256 verified)"
fi
