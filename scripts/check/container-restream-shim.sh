#!/usr/bin/env bash
# Launch the production runtime image where the live harness expects a
# `restream` executable. `RESTREAM_BIN=scripts/check/container-restream-shim.sh`
# lets the existing harness (`resource-sweep`, ...) drive the SHIPPED image
# through the normal application lifecycle -- no second protocol harness.
#
# The container runs on the host network (harness peers live on loopback), as
# the image's own non-root user, with no mounts. Host-path settings the harness
# passes for its own process (`RESTREAM_DB_PATH`, `RESTREAM_LOG_DIR`, ...) are
# dropped so the image uses its own writable state; the container's stdout and
# stderr become the harness's restream log.
#
# Environment:
#   CONTAINER_ENGINE     docker (default) or a Docker-compatible engine
#   CONTAINER_IMAGE      image to run (required)
#   CONTAINER_SECCOMP    empty = the engine's default profile; a path = that
#                        profile; "unconfined" is a DIAGNOSTIC control only
#   CONTAINER_EXTRA_ARGS extra engine arguments (word-split), e.g. an engine
#                        specific workaround; empty for Docker
set -euo pipefail

engine="${CONTAINER_ENGINE:-docker}"
image="${CONTAINER_IMAGE:?CONTAINER_IMAGE is required}"
name="restream-shim-$$"

# Every container this shim starts carries the smoke's label so the caller can
# remove strays even if the harness kills this process without a chance to
# clean up.
args=(run --rm --name "$name" --network host)
if [[ -n "${CONTAINER_LABEL:-}" ]]; then
    args+=(--label "$CONTAINER_LABEL")
fi
if [[ -n "${CONTAINER_SECCOMP:-}" ]]; then
    args+=(--security-opt "seccomp=${CONTAINER_SECCOMP}")
fi
if [[ -n "${CONTAINER_EXTRA_ARGS:-}" ]]; then
    # shellcheck disable=SC2206
    args+=(${CONTAINER_EXTRA_ARGS})
fi
while IFS='=' read -r key _; do
    case "$key" in
        RESTREAM_DB_PATH|RESTREAM_LOG_DIR|RESTREAM_MEDIA_DIR|RESTREAM_BIN) ;;
        RESTREAM_*) args+=(-e "$key") ;;
    esac
done < <(env)

# Not `exec`: forward termination to the container so an ordinary harness stop
# (SIGTERM) tears it down, leaving the label as the backstop for SIGKILL.
"$engine" "${args[@]}" "$image" "$@" &
client=$!
stop_container() {
    "$engine" stop --time 5 "$name" >/dev/null 2>&1 || true
}
trap 'stop_container' TERM INT
trap 'stop_container; kill "$client" 2>/dev/null || true' EXIT
wait "$client"
