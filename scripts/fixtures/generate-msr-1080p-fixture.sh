#!/usr/bin/env bash
# Generate the high-bitrate MSR envelope fixture: Big Buck Bunny 1080p60
# (h264, ~4 Mbps, CC-BY) with its audio replicated as 30 AAC tracks —
# the real-MSR shape (1 video + 30 languages) at the upper half of the
# 1.5-8 Mbps envelope. Output is ~605 MB and is NOT committed; harness
# runs point at it via RESTREAM_MSR_FIXTURE_OVERRIDE.
#
# Usage: scripts/fixtures/generate-msr-1080p-fixture.sh [outdir]
set -euo pipefail

outdir="${1:-.local/fixtures}"
mkdir -p "$outdir"
cd "$outdir"

src=bbb_sunflower_1080p_60fps_normal.mp4
out=bbb-1080p60-30a.mp4

if [ ! -f "$src" ]; then
  # download.blender.org no longer serves this path; the Blender mirror
  # network does. The mirror stores the mp4 inside a zip.
  curl -fL -o bbb.zip \
    https://ftp.halifax.rwth-aachen.de/blender/demo/movies/BBB/bbb_sunflower_1080p_60fps_normal.mp4
  unzip -o bbb.zip "$src"
  rm bbb.zip
fi

# One AAC encode, then 30 stream-copies of it muxed alongside the copied
# video — no per-track encode cost.
ffmpeg -v error -y -i "$src" -map 0:a:0 -c:a aac -b:a 128k -ac 2 bbb-audio.m4a
maps=$(printf ' -map 1:a:0%.0s' $(seq 30))
# shellcheck disable=SC2086
ffmpeg -v error -y -i "$src" -i bbb-audio.m4a -map 0:v:0 $maps \
  -c copy -movflags +faststart "$out"
rm bbb-audio.m4a

echo "fixture ready: $outdir/$out"
ffprobe -v error -show_entries format=nb_streams,duration,bit_rate -of csv=p=0 "$out"
