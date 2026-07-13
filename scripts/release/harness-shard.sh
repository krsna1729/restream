#!/usr/bin/env bash
# Run one release harness shard. GitHub Actions Free allows 20 standard hosted
# jobs at once; release.yml caps the matrix below that and uses these stable
# shard names so the coverage split stays in repo code instead of YAML.
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT"

usage() {
    cat <<'EOF' >&2
usage: scripts/release/harness-shard.sh <shard>

Full-release shards:
  smoke
  mixed.live.rtmp.h264.a1
  mixed.live.srt.h264.a1
  mixed.live.srt.h264.a2
  mixed.live.srt.h265.a1
  mixed.live.srt.h265.a2
  mixed.file.h264.a1
  mixed.file.h264.a2
  mixed.file.h265.a1
  mixed.file.h265.a2
  bitrate-sweep.h264-rtmp
  bitrate-sweep.h264-srt
  bitrate-sweep.h265-srt
  bitrate-sweep.mixed-h264-a2
  bitrate-sweep.mixed-h265-a2
  resource-sweep.source
  resource-sweep.transcode
  resource-sweep.hevc
  ramp-family
  srt-crypto-matrix
  branch-matrix
  fault.resilience
EOF
}

shard="${1:-}"
if [[ -z "$shard" || "$shard" == "--help" || "$shard" == "-h" ]]; then
    usage
    [[ -n "$shard" ]] && exit 0 || exit 2
fi

safe_shard="${shard//[^A-Za-z0-9_.-]/-}"
run_id="${RELEASE_HARNESS_RUN_ID:-release-${GITHUB_SHA:-local}-$safe_shard}"
common_args=(--no-netns --run-id "$run_id")

run_mode() {
    local mode=$1
    shift || true
    echo "[release-shard] run $shard: $mode"
    scripts/harness/run.sh "$mode" -- "${common_args[@]}" "$@"
}

run_many_modes() {
    local mode
    for mode in "$@"; do
        run_mode "$mode"
    done
}

run_bitrate_config() {
    local config=$1
    BITRATE_SWEEP_CONFIGS="$config" run_mode bitrate-sweep
}

run_resource_scenarios() {
    local scenarios=$1
    RESOURCE_SWEEP_SCENARIOS="$scenarios" run_mode resource-sweep
}

case "$shard" in
    smoke)
        run_many_modes api-smoke file.live-edge srt.policy
        ;;

    mixed.live.rtmp.h264.a1)
        run_many_modes \
            mixed.live.rtmp.h264.a1.bf0 \
            mixed.live.rtmp.h264.a1.bf2
        ;;
    mixed.live.srt.h264.a1)
        run_many_modes \
            mixed.live.srt.h264.a1.bf0 \
            mixed.live.srt.h264.a1.bf2
        ;;
    mixed.live.srt.h264.a2)
        run_many_modes \
            mixed.live.srt.h264.a2.bf0 \
            mixed.live.srt.h264.a2.bf2
        ;;
    mixed.live.srt.h265.a1)
        run_many_modes \
            mixed.live.srt.h265.a1.bf0 \
            mixed.live.srt.h265.a1.bf2
        ;;
    mixed.live.srt.h265.a2)
        run_many_modes \
            mixed.live.srt.h265.a2.bf0 \
            mixed.live.srt.h265.a2.bf2
        ;;

    mixed.file.h264.a1)
        run_many_modes \
            mixed.asset.file.h264.a1.bf0 \
            mixed.asset.file.h264.a1.bf2
        ;;
    mixed.file.h264.a2)
        run_many_modes \
            mixed.asset.file.h264.a2.bf0 \
            mixed.asset.file.h264.a2.bf2
        ;;
    mixed.file.h265.a1)
        run_many_modes \
            mixed.asset.file.h265.a1.bf0 \
            mixed.asset.file.h265.a1.bf2
        ;;
    mixed.file.h265.a2)
        run_many_modes \
            mixed.asset.file.h265.a2.bf0 \
            mixed.asset.file.h265.a2.bf2
        ;;

    bitrate-sweep.h264-rtmp)
        run_bitrate_config h264-rtmp
        ;;
    bitrate-sweep.h264-srt)
        run_bitrate_config h264-srt
        ;;
    bitrate-sweep.h265-srt)
        run_bitrate_config h265-srt
        ;;
    bitrate-sweep.mixed-h264-a2)
        run_bitrate_config mixed.live.srt.h264.a2.bf2
        ;;
    bitrate-sweep.mixed-h265-a2)
        run_bitrate_config mixed.live.srt.h265.a2.bf2
        ;;

    resource-sweep.source)
        run_resource_scenarios \
            resource.egress-growth-source-same,resource.egress-growth-source-mixed
        ;;
    resource-sweep.transcode)
        run_resource_scenarios \
            resource.egress-growth-transcode-same,resource.egress-growth-transcode-mixed,resource.egress-growth-source-plus-transcode-mixed,resource.egress-growth-transcode-dual-mixed,resource.egress-growth-source-plus-transcode-dual-mixed
        ;;
    resource-sweep.hevc)
        run_resource_scenarios resource.egress-growth-hevc-bridge
        ;;

    ramp-family|srt-crypto-matrix|branch-matrix|fault.resilience)
        run_mode "$shard"
        ;;

    *)
        echo "harness-shard: unknown shard '$shard'" >&2
        usage
        exit 2
        ;;
esac

echo "[release-shard] PASS $shard"
