#!/usr/bin/env bash
# `RESTREAM_BIN` shim for qualification runs that need Restream on one CPU.
# NOTE: one effective CPU is NOT one SRT shard: `default_egress_fabric_shards`
# clamps the CPU-derived shard count to 2..=8, so the SRT profile runs TWO shards
# (two Compio runtimes, up to two IPv4 Owners). The harness and its peers stay
# unpinned. `REAL_RESTREAM` is the binary to run;
# `PIN_CPUS` (default 5) the cpuset.
set -euo pipefail
exec taskset -c "${PIN_CPUS:-5}" "${REAL_RESTREAM:?REAL_RESTREAM is required}" "$@"
