# RTMP fabric A/B baseline

This directory records captured artifacts from the `rtmp-fabric-matrix`
harness command (`src/bin/test_harness/resource_sweep/branch_matrix.rs`),
which runs the same RTMP-source workload twice — once with the legacy
per-connection sender and once with the RTMP fabric routed
(`RESTREAM_EGRESS_FABRIC=rtmp`) — each in its own isolated mediamtx+restream
stack, and compares CPU/RSS.

See [egress-implementation](../../../../docs/egress-implementation.md)
Phase 5 status for how this fits the migration plan. This is a bounded,
default-N=10 smoke-scale A/B, not the exhaustive 1,000+-output parity proof
Phase 5's exit gate ultimately requires before a default-mode flip; see
`RTMP_FABRIC_MATRIX_EGRESS_COUNT` to run at larger scale.

## Contents

- [Reproduction](#reproduction)
- [Recorded artifacts](#recorded-artifacts)

## Reproduction

```sh
pkill -x restream; pkill -x mediamtx; pkill -x ffmpeg
export RESTREAM_BUILD_LOCK_FILE=/tmp/restream-build.lock
scripts/build/resource-limit.sh cargo build --bin test_harness
WORK_DIR=.local/artifacts/rtmp-fabric-matrix \
  target/debug/test_harness rtmp-fabric-matrix
```

Env knobs:

- `RTMP_FABRIC_MATRIX_SCENARIO` (default `egress-growth-source-same`) — any
  name from `src/bin/test_harness/resource_egress_scenarios.json`.
- `RTMP_FABRIC_MATRIX_EGRESS_COUNT` (default `10`) — output count per
  variant.

## Recorded artifacts

Per-host-class subdirectories (e.g. `vps-6cpu-12gb/`) each store
`results.json` (the full harness output), `legacy.csv`/`fabric.csv` (the
per-variant resource-sweep rows), and `capture.json` (commit, date, scale,
comparison, and deviations — same convention as
`test/harness/baselines/egress-phase0/`).
