#!/usr/bin/env bash
# Release harness shard catalog and timeout policy. Keep this as the single
# owner for shard names, shard-to-mode mapping, measurement selector env, and
# expected-duration timeout buckets. CI should select shard names; this file
# defines what those names mean.

RESTREAM_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/common.sh
source "$RESTREAM_LIB_DIR/common.sh"

restream_release_shard_list() {
    # Keep the longer successful-duration shards first so GitHub-hosted runner
    # slots fill with the critical path early. This order is based on passed
    # historical runs, not on one-off stuck/cancelled outliers.
    cat <<'EOF'
mixed.file.h265.a2.bf2
mixed.file.h265.a2.bf0
mixed.live.srt.h265.a2.bf0
mixed.live.srt.h265.a2.bf2
mixed.live.srt.h264.a2.bf2
mixed.live.srt.h264.a2.bf0
mixed.file.h264.a2.bf0
mixed.file.h264.a2.bf2
srt-crypto-matrix
ramp-family
mixed.file.h265.a1.bf0
mixed.file.h265.a1.bf2
mixed.live.srt.h265.a1.bf0
mixed.live.srt.h265.a1.bf2
mixed.file.h264.a1.bf2
mixed.file.h264.a1.bf0
mixed.live.rtmp.h264.a1.bf0
mixed.live.rtmp.h264.a1.bf2
bitrate-sweep.h264-rtmp
bitrate-sweep.h265-srt
bitrate-sweep.h264-srt
bitrate-sweep.mixed-h264-a2
bitrate-sweep.mixed-h265-a2
branch-matrix
fault.resilience
resource-sweep.source
resource-sweep.transcode
resource-sweep.hevc
smoke
mixed.live.srt.h264.a1.bf0
mixed.live.srt.h264.a1.bf2
EOF
}

restream_release_shard_exists() {
    local shard=$1
    restream_release_shard_list | grep -Fxq -- "$shard"
}

restream_release_shard_timeout() {
    local shard=$1
    # Keep these grouped by observed local shard cost, not by intuition about
    # scenario names. Hosted-runner contention can consume setup/cleanup time
    # before useful evidence is visible, so use stable timeout buckets with
    # enough room above the latest local release timings:
    #   - smoke/correctness: <= ~1.5m locally -> 10m
    #   - small mixed shards: <= ~6m locally -> 30m
    #   - medium mixed/measurement shards: <= ~12.5m locally -> 50m
    #     srt-crypto-matrix reached full output progress on hosted runners but
    #     still needed cleanup/completion time beyond the 15m bucket.
    #   - full bitrate measurement family: <= ~15m locally -> 60m
    case "$shard" in
        smoke|branch-matrix)
            echo 10m
            ;;
        mixed.*.bf0|mixed.*.bf2|fault.resilience|ramp-family)
            echo 30m
            ;;
        srt-crypto-matrix|resource-sweep.*)
            echo 50m
            ;;
        bitrate-sweep.*)
            echo 60m
            ;;
        *)
            echo 40m
            ;;
    esac
}

restream_release_shard_plan() {
    local shard=$1
    case "$shard" in
        smoke)
            printf 'mode\tapi-smoke\n'
            printf 'mode\tfile.live-edge\n'
            printf 'mode\tsrt.policy\n'
            ;;
        mixed.live.*.bf0|mixed.live.*.bf2)
            printf 'mode\t%s\n' "$shard"
            ;;
        mixed.file.*.bf0|mixed.file.*.bf2)
            printf 'mode\tmixed.asset.file.%s\n' "${shard#mixed.file.}"
            ;;
        bitrate-sweep.h264-rtmp)
            printf 'bitrate\th264-rtmp\n'
            ;;
        bitrate-sweep.h264-srt)
            printf 'bitrate\th264-srt\n'
            ;;
        bitrate-sweep.h265-srt)
            printf 'bitrate\th265-srt\n'
            ;;
        bitrate-sweep.mixed-h264-a2)
            printf 'bitrate\tmixed.live.srt.h264.a2.bf2\n'
            ;;
        bitrate-sweep.mixed-h265-a2)
            printf 'bitrate\tmixed.live.srt.h265.a2.bf2\n'
            ;;
        resource-sweep.source)
            printf 'resource\tresource.egress-growth-source-same,resource.egress-growth-source-mixed\n'
            ;;
        resource-sweep.transcode)
            printf 'resource\tresource.egress-growth-transcode-same,resource.egress-growth-transcode-mixed,resource.egress-growth-source-plus-transcode-mixed,resource.egress-growth-transcode-dual-mixed,resource.egress-growth-source-plus-transcode-dual-mixed\n'
            ;;
        resource-sweep.hevc)
            printf 'resource\tresource.egress-growth-hevc-bridge\n'
            ;;
        ramp-family|srt-crypto-matrix|branch-matrix|fault.resilience)
            printf 'mode\t%s\n' "$shard"
            ;;
        *)
            echo "unknown release shard: $shard" >&2
            return 2
            ;;
    esac
}

restream_release_shard_explain() {
    local shard=$1
    restream_release_shard_exists "$shard" || {
        echo "unknown release shard: $shard" >&2
        return 2
    }
    printf 'shard=%s\n' "$shard"
    printf 'timeout=%s\n' "$(restream_release_shard_timeout "$shard")"
    restream_release_shard_plan "$shard"
}
