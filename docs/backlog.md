# Backlog

The one list of open engineering work. Each item states what, why and the
gates that close it. Close an item by deleting it in the change that does the
work; the commit message keeps the evidence. Verification direction lives in
the [assurance roadmap](assurance-roadmap.md).

## Contents

- [Q-025 SRT shard law cross-host](#q-025-performance-opus-re-qualify-the-srt-shard-law-after-transport-convergence)
- [Q-120 split the last complexity baselines](#q-120-performance-opus-split-the-last-two-complexity-baselines)
- [Q-121 converge EgressTask fabric paths](#q-121-lifecycle-opus-converge-the-fabric-run-paths-in-egresstask)
- [Not planned](#not-planned)
  - [Q-033 host-level shard pool](#q-033-host-level-shard-pool)
  - [Q-071 HLS-to-HLS passthrough](#q-071-hls-to-hls-passthrough)
  - [Q-097 MediaMTX MoQ dialect interoperability](#q-097-mediamtx-moq-dialect-interoperability)

### Q-025 [performance] [opus] Re-qualify the SRT shard law after transport convergence
- Goal: qualify the SRT cold prior (64 outputs per shard at 4.8 Mbit/s,
  `crates/restream-engine/src/egress/sizing.rs`, [SRT shard capacity](runtime-crossings.md#srt-shard-capacity))
  and `srt_egress_connect_concurrency` (`crates/restream-engine/src/config.rs`) on a second host.
- Gates: the single-shard SRT matrix on another host; no production policy
  change without measured evidence.

### Q-120 [performance] [opus] Split the last two complexity baselines
- Goal: `SrtOwners::drain_events` (22) and `KtlsConnection::read_records` (21)
  leave `crates/xtask/reportcard-baseline.json`, so the reportcard ratchet has
  no engine entries left.
- Why still open: both are hot paths, so a split needs before/after numbers
  from a benchmark that drives the real path. `drain_events` needs a live SRT
  owner set; `read_records` needs a kernel-TLS socket and the engine's private
  connection type. No existing Criterion suite covers either.
- Gates: a production-path benchmark for each, then the split with numbers
  showing no regression on a named host, then delete the entry with
  `cargo xtask reportcard --update-baseline`.

### Q-121 [lifecycle] [opus] Converge the fabric run paths in EgressTask
- Goal: `run_srt_fabric`, `run_rtmp_fabric`, and `run_hls_put_fabric` share
  one sequence for retain, dispatch placement, stop or leaf wait, Remove or
  Abandon, runtime release, and status forwarding. A protocol supplies only
  its hooks and phase.
- Why open: #697 fenced retry settlement by attempt and
  `EgressManager::dispatch_remove` routes Remove and Abandon through one
  path, but the three run methods still repeat the sequence and can drift.
  HLS uses Abandon when a stage ends under the output; SRT and RTMP always
  Remove. No known correctness gap; this is duplication risk.
- Gates: RTMP, SRT, and HLS fabric tests cover stop, leaf failure, dispatch
  failure, and stage end; every status and retry write stays `_if_current`;
  shutdown ordering does not change.

## Not planned

### Q-033 host-level shard pool
A pooled prototype cut shard CPU at 16 feeds × 2 outputs by more than half,
but an injected Owner fault failed both feeds in a shared pool. Production
keeps per-feed shard groups
([per-feed shard isolation](runtime-crossings.md#per-feed-shard-isolation));
reopen only if that failure-domain contract changes.

### Q-071 HLS-to-HLS passthrough
Relaying pushed HLS segments past the ring would need separate capacity
accounting, preview and health for ring-less outputs, and segment-level
failover. No telemetry shows HLS demux plus segmenting as a meaningful cost.

### Q-097 MediaMTX MoQ dialect interoperability
Blocked upstream: MediaMTX v1.21.2 has no PUBLISH_NAMESPACE dispatch, waits
for the server's SETUP, puts one frame per group with codec-tick timestamps
and no timescale, and emits a non-MSF catalog. Restream carries no MediaMTX
profile; `moq-relay-interop` stays diagnostic-only.
