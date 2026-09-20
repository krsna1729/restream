#!/usr/bin/env bash
# `RESTREAM_BIN` shim for qualification runs that need Restream on one CPU
# (one effective CPU => one SRT shard => one Owner/caller socket). The harness
# and its peers stay unpinned. `REAL_RESTREAM` is the binary to run;
# `PIN_CPUS` (default 5) the cpuset.
set -euo pipefail
exec taskset -c "${PIN_CPUS:-5}" "${REAL_RESTREAM:?REAL_RESTREAM is required}" "$@"
