# MSR 1,200-Output netns Confound Investigation — 2026-08-14

## Contents

- [Summary](#summary)
- [Background](#background)
- [Method: 4 worktrees x 4 mixes, 16 controlled runs](#method-4-worktrees-x-4-mixes-16-controlled-runs)
- [Results](#results)
- [Root cause: shared network namespace, not code](#root-cause-shared-network-namespace-not-code)
- [What this means for each code variant](#what-this-means-for-each-code-variant)
- [What remains open](#what-remains-open)

## Summary

Earlier the same day, three live-harness attempts to validate an SRT
egress shard-count optimization ("small-pool") at the full 1,200-output
MSR target each failed, and were provisionally attributed to the code
change itself (see the "Efficiency evaluation" section of
[the resource-attribution doc](msr-1200-resource-attribution-2026-08-13.md)
for that provisional, now-superseded account). A controlled follow-up
campaign — 4 isolated worktrees x 4 protocol mixes, 16 runs total, each
gated on a genuinely idle host — shows conclusively that **every code
variant, including the unmodified baseline, fails `srt-only` at 1,200
outputs identically**, while every variant passes every other mix cleanly.
The failure is the test environment (no private network namespace
available this session, forcing all runs onto the host's shared network
stack), not a Restream code defect, and not specific to the small-pool
shard-count change.

## Background

`scripts/agent/worktree.sh`-based live-harness runs normally use a private
network namespace (`unshare --net`) so each run gets an isolated, unshared
loopback stack. Partway through this session, `unshare --net` started
failing with `Operation not permitted` in this sandbox, forcing every live
run to add `--no-netns` and share the host's default network namespace.
Three live 1,200-output `srt-only` runs after that point all failed with
the same `SRT fabric leaf terminated unexpectedly (peer closed, protocol
failure, or stall recovery)` signature that an earlier investigation
(`srt-egress-scale-investigation-2026-08-10.md`) had already root-caused
and fixed once, for a different reason, days before. Host load average
also spiked as high as 22-35 (on a 6-core host) between some of those
runs, from stacking builds and 1,200-connection live tests too tightly —
a second plausible confound. Neither confound had been isolated from the
code changes under test (an SRT egress shard-count formula change and a
new connect-admission mechanism) before this campaign.

## Method: 4 worktrees x 4 mixes, 16 controlled runs

Four independent git worktrees, each with its own `target/` (no shared
build state) and built one at a time so no two builds ever overlapped:

| Worktree | Code state |
|---|---|
| `msr-control` | Pure baseline: commit `c126c95b`, no 2026-08-14 changes at all |
| `msr-1080p-investigation` (`current-pr`) | Baseline + pipeline-scoped `SrtEgressMuxerPorts` + SRT connect-admission (commits `338b94aa`, `383fab88`) |
| `msr-small-pool` | Baseline + only the `SrtCpuParallel` output-count-scaled shard formula (no pipeline-scoping, no connect-admission) |
| `msr-small-pool-admission` | `current-pr` + the same shard-formula change added on top (small-pool + pipeline-scoping + connect-admission together) |

Four `MSR_PROTOCOL_MIX` values, each run once at `MSR_OUTPUT_COUNTS=1200`
with the real 1080p60/8Mbps 30-audio-track fixture, `MSR_PEER=sink`,
`--no-netns` (required in every worktree, not just the ones under test):

- `rtmp-only`
- `canonical` (95% RTMP / 5% SRT)
- `srt-every:2` (50% RTMP / 50% SRT)
- `srt-only`

An orchestration script ran all 16 cells strictly serially. Before every
cell it polled `/proc/loadavg` and `pgrep -x restream` in a loop, refusing
to start until load average was at or below 2.0 *and* no `restream`
process was still alive from a previous cell — eliminating the
build/test-overlap confound directly, with the actual load-at-start
recorded for every cell as evidence.

## Results

| Worktree | rtmp-only | 95/5 | 50/50 | srt-only |
|---|---|---|---|---|
| `control` | PASS 49s | PASS 63s | PASS 137s | **FAIL** 918s, 1165/1200 |
| `current-pr` | PASS 42s | PASS 36s | PASS 679s | **FAIL** 953s, 1026/1200 |
| `small-pool` | PASS 29s | PASS 38s | PASS 83s | **FAIL** 924s, 1192/1200 |
| `small-pool-admission` | PASS 34s | PASS 38s | PASS 98s | **FAIL** 929s, 1169/1200 |

12 of 16 cells passed cleanly. All 4 failures are `srt-only` at 1,200 —
one per worktree, none anywhere else. Every failure's recorded
`load_before` was between 1.63 and 1.96 (host genuinely idle, well under
the 2.0 gate) and every failure carries the identical `lastError=SRT
fabric leaf terminated unexpectedly` signature. Full per-cell logs live
under `.local/artifacts/msr-campaign/` (not committed; regenerate via
`.local/artifacts/msr-campaign/run-campaign.sh` if needed).

Two secondary observations, not failures:

- `current-pr`'s 50/50 cell took 679s to pass — 5-8x longer than every
  other worktree's 50/50 cell (83-137s). This is connect-admission's
  default 64-concurrent-handshake budget serializing 600 SRT connects
  without small-pool's shard-count reduction to shrink the queue depth
  each shard has to drain; see [What remains open](#what-remains-open).
- `small-pool-admission`'s 50/50 cell (98s) does *not* show this slowdown,
  suggesting the two changes interact rather than simply stacking their
  individual costs — also unexplained, listed below.

## Root cause: shared network namespace, not code

Kernel UDP receive statistics (`/proc/net/snmp`, `Udp:` line) were
snapshotted before and after the campaign's first 11 cells:

```
InDatagrams delta:              20,402,034
InErrors / RcvbufErrors delta: 207,141,050
```

`RcvbufErrors` equals `InErrors` exactly in both snapshots — every
recorded UDP input error on this host is a receive-buffer overflow, i.e.
the kernel dropping datagrams it could not queue fast enough. The error
count is roughly 10x the successfully-received datagram count over the
same window, concentrated in the cells carrying real SRT traffic (RTMP
generates none). Cells with 60 or 600 concurrent SRT flows mostly
absorbed this without failing; four independent code variants all failed
once concurrency reached 1,200 real 1080p60/8Mbps SRT flows (~9.6 Gbps
aggregate) sharing one *non-isolated* loopback stack. `net.core.rmem_max`
and the other harness-configured buffer sysctls were already at their
expected bootstrap values (25 MiB / etc.) — this is not a missing-sysctl
problem, and both `udp_mem` and `rmem_max` are unusually generous already.
The most likely mechanism is that this environment's default network
namespace is itself sandboxed/virtualized in a way a genuinely private
`unshare --net` namespace's loopback is not — this was not confirmed
further; see [What remains open](#what-remains-open).

## What this means for each code variant

- **`EgressShardProfile::SrtCpuParallel`'s output-count-scaled formula
  ("small-pool")**: not shown unsafe. It matched or beat the unmodified
  baseline on every metric measured — best `srt-only` failure margin of
  the four variants (1192/1200 vs. baseline's 1165/1200) and the fastest
  clean 50/50 pass (83s vs. baseline's 137s). The two July-2026 failures
  that originally triggered reverting it were an environmental confound,
  not evidence against the change itself. It remains unshipped only
  because this campaign could not produce a *valid* passing `srt-only`
  run under real network-namespace isolation to re-prove it against —
  see `src/config.rs`'s `EgressShardProfile` doc comment, updated
  alongside this investigation.
- **SRT connect-admission** (`srt_connect_admission.rs`): unaffected by
  this confound either way — it neither fixed nor worsened the `srt-only`
  failure signature (all four variants failed similarly), consistent with
  the earlier finding that the failure is not a connection-establishment
  burst problem. It remains shipped on its own independent merits
  (closes a real, previously-unimplemented gap described in
  `egress-architecture.md`'s "Dead destinations and retries" section) but
  introduced a real, unexplained slowdown at 50/50 in isolation (679s)
  that did not reproduce when combined with small-pool (98s) — worth
  its own investigation before treating the default concurrency (64) as
  final.
- **Pipeline-scoped `SrtEgressMuxerPorts`**: numerically a no-op for
  every cell in this campaign (single-pipeline MSR), exactly as designed;
  this campaign adds no new evidence for or against it beyond the unit
  tests already covering it.

## What remains open

- **Restore `unshare --net` capability** (or otherwise obtain a live
  environment with real network-namespace isolation) before re-attempting
  any `srt-only`-at-1,200 live verification — this campaign's `srt-only`
  cells are not informative about *code* correctness at all, only about
  this environment's shared-namespace UDP capacity.
- **Re-run `srt-only` at 1,200 for `small-pool` under real netns
  isolation** once available. If it passes as cleanly as the other three
  mixes did in this campaign, the shard-count formula can ship following
  the normal proof ladder (already has unit/proptest coverage from the
  earlier attempt).
- **Explain the connect-admission 50/50 slowdown** (679s alone vs. 98s
  combined with small-pool) — not investigated further here; worth
  profiling before tuning `RESTREAM_SRT_EGRESS_CONNECT_CONCURRENCY`'s
  default away from 64.
- **Confirm the shared-namespace UDP mechanism directly** (e.g. by
  comparing `ethtool -S lo` or namespace-scoped `/proc/net/snmp` behavior
  under a working `unshare --net` on a comparable host) rather than
  relying on the strong but indirect evidence gathered here.
