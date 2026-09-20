#!/usr/bin/env bash
# Build and prove the supported container runtime without a mount. This is the
# canonical container-release smoke used locally and by release automation.
#
# It proves the deployment contract, not just process startup:
#   1. the image starts as the expected non-root user with no mounts and
#      answers /healthz under the SHIPPED seccomp profile;
#   2. the shipped image performs REAL SRT egress under that profile -- the
#      ordinary application lifecycle (live input -> pipeline -> output ->
#      SRT fabric -> Compio Owner -> external SRT sink) via the existing live
#      harness, with observed byte progress;
#   3. a negative control under the engine's DEFAULT seccomp profile records
#      whether it blocks the io_uring syscalls Restream needs. That outcome is
#      reported, and it never fails the run when it is the expected io_uring
#      denial (or when a future default profile allows io_uring and works).
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"

IMAGE="${RESTREAM_CONTAINER_IMAGE:-restream:release-smoke}"
ARCHIVE=""
LOAD_ARCHIVE=""
SECCOMP_PROFILE="$ROOT/distribution/docker/restream-seccomp.json"
ENGINE="${CONTAINER_ENGINE:-docker}"
USE_EXISTING=0
DIAGNOSTIC_UNCONFINED=0

usage() {
    cat <<'EOF'
Usage: scripts/check/container-smoke.sh [--image NAME] [--archive PATH]
                                        [--load-archive PATH] [--seccomp-profile PATH]

Builds Docker's default `runtime` target, or loads an existing image archive,
proves it starts as the expected non-root user with no mounts, verifies the
HTTP health endpoint, and proves real SRT egress under the shipped seccomp
profile (default: distribution/docker/restream-seccomp.json; a release passes
the profile shipped next to the archive). When --archive is supplied, writes a
reproducible gzip-compressed Docker image archive for a GitHub Release asset.

Options:
  --use-existing-image   test IMAGE as-is (no build, no load)
  --diagnostic-unconfined  ALSO run the seccomp=unconfined control (troubleshooting
                         only: never a deployment recommendation)

Environment:
  CONTAINER_ENGINE                  docker (default) or a Docker-compatible engine
  CONTAINER_EXTRA_ARGS              extra engine run arguments (engine workarounds)
  RESTREAM_HARNESS_BIN              live harness (default target/bench/test_harness)
  RESTREAM_CONTAINER_SMOKE_REPORT   write a JSON report of the three outcomes here
  RESTREAM_CONTAINER_SMOKE_SKIP_SRT=1  skip the SRT capability proof (loudly; never in CI)
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --image)
            IMAGE=${2:?--image requires a value}
            shift 2
            ;;
        --archive)
            ARCHIVE=${2:?--archive requires a value}
            shift 2
            ;;
        --load-archive)
            LOAD_ARCHIVE=${2:?--load-archive requires a value}
            shift 2
            ;;
        --seccomp-profile)
            SECCOMP_PROFILE=${2:?--seccomp-profile requires a value}
            shift 2
            ;;
        --use-existing-image)
            USE_EXISTING=1
            shift
            ;;
        --diagnostic-unconfined)
            DIAGNOSTIC_UNCONFINED=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "container-smoke: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

for command in "$ENGINE" curl; do
    command -v "$command" >/dev/null || {
        echo "container-smoke: required command not found: $command" >&2
        exit 1
    }
done
[[ -s "$SECCOMP_PROFILE" ]] || {
    echo "container-smoke: seccomp profile not found: $SECCOMP_PROFILE" >&2
    exit 1
}

name="restream-release-smoke-$$"
label="restream.container-smoke=$$"
workdirs=()

# Outcomes for the durable report; written by the EXIT trap so it exists whether
# the smoke passes or fails (and even when the image cannot be built).
default_health="not-run"; default_srt="not-run"; default_note=""
shipped_health="not-run"; shipped_srt="not-run"; unconfined_srt="not-run"
stage="start"; smoke_passed=0; probe_logs=""; default_probe_logs=""; shipped_probe_logs=""; srt_log=""
write_report() {
    [[ -n "${RESTREAM_CONTAINER_SMOKE_REPORT:-}" ]] || return 0
    local dir
    dir="$(dirname "$RESTREAM_CONTAINER_SMOKE_REPORT")"
    mkdir -p "$dir"
    # Logs that back the report: the default-profile startup, and the shipped
    # profile's Restream log (io_uring / RX substrate / mode lines live here).
    [[ -z "$default_probe_logs" ]] || printf '%s\n' "$default_probe_logs" | sed 's/\x1b\[[0-9;]*m//g' >"$dir/default-profile-startup.log"
    [[ -z "$shipped_probe_logs" ]] || printf '%s\n' "$shipped_probe_logs" | sed 's/\x1b\[[0-9;]*m//g' >"$dir/shipped-profile-startup.log"
    [[ -z "$srt_log" ]] || printf '%s\n' "$srt_log" | sed 's/\x1b\[[0-9;]*m//g' >"$dir/last-srt-probe-restream.log"
    printf '{"result":"%s","failed_stage":"%s","image":"%s","engine":"%s","engine_version":"%s","kernel":"%s","default_seccomp":{"health":"%s","srt":"%s","note":"%s"},"shipped_profile":{"path":"%s","health":"%s","srt":"%s"},"unconfined_control":"%s"}\n' \
        "$([[ $smoke_passed == 1 ]] && echo pass || echo fail)" \
        "$([[ $smoke_passed == 1 ]] && echo "" || echo "$stage")" \
        "$IMAGE" "$ENGINE" \
        "$("$ENGINE" version --format '{{.Server.Version}}' 2>/dev/null || echo unknown)" \
        "$(uname -sr)" \
        "$default_health" "$default_srt" "$default_note" \
        "$SECCOMP_PROFILE" "$shipped_health" "$shipped_srt" "$unconfined_srt" \
        >"$RESTREAM_CONTAINER_SMOKE_REPORT" || true
}
cleanup() {
    write_report
    "$ENGINE" rm -f "$name" >/dev/null 2>&1 || true
    while read -r stray; do
        [[ -n "$stray" ]] && "$ENGINE" rm -f "$stray" >/dev/null 2>&1 || true
    done < <("$ENGINE" ps -aq --filter "label=$label" 2>/dev/null || true)
    for dir in "${workdirs[@]}"; do
        rm -rf "$dir"
    done
}
trap cleanup EXIT

extra_args=()
if [[ -n "${CONTAINER_EXTRA_ARGS:-}" ]]; then
    read -r -a extra_args <<<"$CONTAINER_EXTRA_ARGS"
fi

stage="image-build-or-load"
if [[ "$USE_EXISTING" == "1" ]]; then
    :
elif [[ -n "$LOAD_ARCHIVE" ]]; then
    [[ -s "$LOAD_ARCHIVE" ]] || {
        echo "container-smoke: archive not found: $LOAD_ARCHIVE" >&2
        exit 1
    }
    "$ENGINE" load -i "$LOAD_ARCHIVE"
else
    build_commit="$(git rev-parse HEAD)"
    build_timestamp="$(git show -s --format=%cI HEAD)"
    build_args=(
        --build-arg "RESTREAM_BUILD_GIT_COMMIT=$build_commit" \
        --build-arg "RESTREAM_BUILD_TIMESTAMP=$build_timestamp" \
        --target runtime \
        -t "$IMAGE"
    )
    if [[ "${RESTREAM_DOCKER_GHA_CACHE:-0}" == "1" ]]; then
        "$ENGINE" buildx build \
            "${build_args[@]}" \
            --cache-from "type=gha,scope=runtime-release" \
            --cache-to "type=gha,mode=max,scope=runtime-release" \
            --load \
            .
    else
        "$ENGINE" build "${build_args[@]}" .
    fi
fi

stage="image-inspection"
user="$("$ENGINE" image inspect --format '{{.Config.User}}' "$IMAGE")"
if [[ "$user" != "1000:1000" ]]; then
    echo "container-smoke: expected non-root runtime user 1000:1000, got ${user:-empty}" >&2
    exit 1
fi

workdir="$("$ENGINE" image inspect --format '{{.Config.WorkingDir}}' "$IMAGE")"
if [[ "$workdir" != "/" ]]; then
    echo "container-smoke: expected runtime workdir /, got ${workdir:-empty}" >&2
    exit 1
fi

# Signatures of "the container's syscall policy denied io_uring", as opposed to
# an unrelated startup failure: the native RTMP/SRT ingress rings and the SRT
# egress Compio runtime all need io_uring_setup/enter/register.
IO_URING_DENIAL='native RTMP acceptor|Compio runtime failed to build|production runtime builds|io_uring.*(Operation not permitted|Function not implemented)|Operation not permitted \(os error 1\)|Function not implemented \(os error 38\)'

# (probe_logs / srt_log are initialised with the report state above.)
# health_probe <label> [engine-run arguments...]: start the image detached with
# an ephemeral published HTTP port, require no mounts, wait for /healthz.
# Sets probe_logs; returns 0 only when the process became healthy.
health_probe() {
    local label_text=$1
    shift
    "$ENGINE" rm -f "$name" >/dev/null 2>&1 || true
    "$ENGINE" run -d --name "$name" --label "$label" "${extra_args[@]}" "$@" \
        -e RESTREAM_INITIAL_ADMIN_PASSWORD=release-smoke-password \
        -p 127.0.0.1::3030 \
        "$IMAGE" >/dev/null
    if [[ "$("$ENGINE" inspect --format '{{json .Mounts}}' "$name")" != "[]" ]]; then
        echo "container-smoke: runtime unexpectedly requires a mount ($label_text)" >&2
        exit 1
    fi
    local port
    port="$("$ENGINE" port "$name" 3030/tcp | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -n1)"
    local healthy=1
    if [[ -n "$port" ]]; then
        for _ in $(seq 1 20); do
            if curl --fail --silent --show-error "http://127.0.0.1:${port}/healthz" >/dev/null 2>&1; then
                healthy=0
                break
            fi
            sleep 1
        done
    fi
    # /healthz can answer in the first moments of a process whose critical
    # listener task is about to bring it down (an io_uring-dependent one under
    # a denying seccomp profile). Healthy means still serving after a settle
    # window, so startup that only LOOKS healthy is not counted.
    if [[ "$healthy" == "0" ]]; then
        sleep 4
        if [[ "$("$ENGINE" inspect --format '{{.State.Running}}' "$name" 2>/dev/null)" != "true" ]]             || ! curl --fail --silent "http://127.0.0.1:${port}/healthz" >/dev/null 2>&1; then
            healthy=1
        fi
    fi
    probe_logs="$("$ENGINE" logs "$name" 2>&1 || true)"
    "$ENGINE" rm -f "$name" >/dev/null 2>&1 || true
    return "$healthy"
}

# srt_probe <label> <CONTAINER_SECCOMP value>: real SRT egress through the
# existing live harness driving THIS image as its restream (see
# container-restream-shim.sh). Returns 0 only with observed egress progress.
srt_probe() {
    local label_text=$1 seccomp=$2
    local harness="${RESTREAM_HARNESS_BIN:-target/bench/test_harness}"
    [[ -x "$harness" ]] || {
        echo "container-smoke: the SRT capability proof needs the live harness ($harness)." >&2
        echo "container-smoke: build it with scripts/build/bench-harness.sh" >&2
        return 3
    }
    local dir
    dir="$(mktemp -d)"
    workdirs+=("$dir")
    local status=0
    CONTAINER_ENGINE="$ENGINE" CONTAINER_IMAGE="$IMAGE" CONTAINER_SECCOMP="$seccomp" \
        CONTAINER_EXTRA_ARGS="${CONTAINER_EXTRA_ARGS:-}" CONTAINER_LABEL="$label" \
        RESTREAM_BIN="$ROOT/scripts/check/container-restream-shim.sh" \
        HARNESS_BIN="$harness" BENCH_BUILD=never \
        MSR_PEER=sink RESOURCE_SWEEP_SCENARIOS=egress-growth-source-srt \
        RESOURCE_SWEEP_EGRESS_COUNTS=1 RESOURCE_SWEEP_SAMPLE_SECS=3 \
        RESOURCE_SWEEP_SETTLE_SECS=2 RESOURCE_SWEEP_PROGRESS_TIMEOUT_BASE_SECS=30 \
        WORK_DIR="$dir" \
        timeout 300 scripts/harness/run.sh resource-sweep -- --no-netns \
        >"$dir/harness.log" 2>&1 || status=$?
    srt_log="$(cat "$dir/restream.log" 2>/dev/null || true)"
    echo "container-smoke: [$label_text] harness exit=$status"
    grep -E 'outputs-progress (start|pass)|harness failed' "$dir/harness.log" | sed 's/^/  /' || true
    return "$status"
}

stage="default-seccomp-control"
echo "container-smoke: === default engine seccomp profile (negative control) ==="
if health_probe "default seccomp"; then
    default_health="ok"
    if [[ "${RESTREAM_CONTAINER_SMOKE_SKIP_SRT:-0}" == "1" ]]; then
        default_srt="skipped"
    elif srt_probe "default seccomp" ""; then
        default_srt="ok"
        default_note="the engine's default profile already allows the required io_uring syscalls"
    else
        default_srt="blocked"
        if grep -Eq "$IO_URING_DENIAL" <<<"$srt_log"; then
            default_note="SRT egress unavailable under the default profile: io_uring denied (expected)"
        else
            echo "container-smoke: SRT egress failed under the default profile for a reason other than io_uring denial:" >&2
            printf '%s\n' "$srt_log" | tail -n 20 >&2
            exit 1
        fi
    fi
else
    default_health="blocked"
    if grep -Eq "$IO_URING_DENIAL" <<<"$probe_logs"; then
        default_note="startup failed under the default profile: io_uring denied (expected; Restream's ingress and egress all need io_uring)"
    else
        echo "container-smoke: the image failed to start under the default profile for a reason other than io_uring denial:" >&2
        printf '%s\n' "$probe_logs" | tail -n 20 >&2
        exit 1
    fi
fi
default_probe_logs="$probe_logs"
echo "container-smoke: default seccomp: health=$default_health srt=$default_srt ${default_note:+($default_note)}"

stage="shipped-profile"
echo "container-smoke: === shipped Restream seccomp profile ($SECCOMP_PROFILE) ==="
shipped_health="fail"
if ! health_probe "shipped profile" --security-opt "seccomp=$SECCOMP_PROFILE"; then
    echo "container-smoke: image did not become healthy under the shipped profile" >&2
    printf '%s\n' "$probe_logs" | tail -n 30 >&2
    exit 1
fi
shipped_health="ok"
shipped_probe_logs="$probe_logs"
if [[ "${RESTREAM_CONTAINER_SMOKE_SKIP_SRT:-0}" == "1" ]]; then
    echo "container-smoke: WARNING: SRT capability proof SKIPPED by request; this does NOT prove SRT egress" >&2
else
    shipped_srt="fail"
    if ! srt_probe "shipped profile" "$SECCOMP_PROFILE"; then
        echo "container-smoke: real SRT egress FAILED under the shipped seccomp profile" >&2
        printf '%s\n' "$srt_log" | tail -n 30 >&2
        exit 1
    fi
    grep -Eq 'srt egress shard runtime ready.*io_uring.*true' <<<"$srt_log" || {
        echo "container-smoke: no Compio io_uring SRT runtime was instantiated in the container" >&2
        exit 1
    }
    shipped_srt="ok"
    grep -E 'srt egress owner attached' <<<"$srt_log" | head -n 1 | sed 's/\x1b\[[0-9;]*m//g' | sed 's/^/  /' || true
fi
echo "container-smoke: shipped profile: health=$shipped_health srt=$shipped_srt"

if [[ "$DIAGNOSTIC_UNCONFINED" == "1" ]]; then
    echo "container-smoke: === DIAGNOSTIC control: seccomp=unconfined (never a deployment recommendation) ==="
    if srt_probe "unconfined control" "unconfined"; then
        unconfined_srt="ok"
    else
        unconfined_srt="failed"
    fi
    echo "container-smoke: unconfined control: srt=$unconfined_srt"
fi

if [[ -n "$ARCHIVE" ]]; then
    mkdir -p "$(dirname "$ARCHIVE")"
    "$ENGINE" save "$IMAGE" | gzip -n >"$ARCHIVE"
fi

smoke_passed=1
echo "container-smoke: PASS image=$IMAGE"
