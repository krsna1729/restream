# Backlog

The one list of open engineering work. Each item states its goal, the files it
touches, the gates that prove it, and the evidence so far. Close an item by
deleting it in the change that does the work; the commit message keeps the
evidence. Larger directions live in [assurance roadmap](assurance-roadmap.md)
(verification) and [runtime crossings](runtime-crossings.md) (open measurement
items).

## Contents

- [Q-027 harness SRT sink drops at SRT×100](#q-027-performance-opus-stop-the-harness-srt-sink-dropping-datagrams-at-srt100-kernel-700-38)
- [Q-028 local live-harness flakes](#q-028-testing-sonnet-local-live-harness-flakes-on-a-loaded-6-vcpu-vps)
- [Q-025 SRT shard law cross-host](#q-025-performance-opus-re-qualify-the-srt-shard-law-after-transport-convergence)
- [Q-026 frozen-SRT-destination RSS gate](#q-026-resilience-opus-attribute-and-recalibrate-the-frozen-srt-destination-rss-gate)
- [Q-029 payload-cache delivery watch](#q-029-performance-sonnet-close-the-payload-cache-delivery-watch-item)
- [Q-030 HLS upload is polled on CONTROL](#q-030-performance-opus-make-hls-upload-publication-driven-and-move-it-off-control)
- [Q-031 one capacity allocator](#q-031-architecture-opus-derive-every-thread-count-from-one-capacity-allocator)
- [Q-032 CPU placement](#q-032-performance-opus-measure-core-segregation-of-control-media-and-hot-threads)
- [Q-033 host-level shard pool](#q-033-architecture-opus-decide-per-feed-shard-groups-versus-a-host-level-shard-pool)
- [Q-034 overload policy](#q-034-resilience-opus-define-one-overload-policy-for-shared-service-centers)
- [Q-035 MEDIA attribution](#q-035-observability-sonnet-attribute-media-pool-time-to-feed-and-stage)
- [Q-036 enforced runtime truth table](#q-036-docs-sonnet-make-runtime-crossings-an-enforced-truth-table)

### Q-027 [performance] [opus] Stop the harness SRT sink dropping datagrams at SRT×100 (kernel 7.0.0-38)
- Goal: SRT capacity rungs measure Restream, not the receiver: the harness SRT
  sink keeps up with SRT×100 on this host, and receiver loss is reported per
  socket so it can never again be mistaken for Restream loss.
- Files: `src/bin/test_harness/harness_srt_sink.rs`, `resource_sweep.rs`,
  `srt_sink.rs`; production reference `src/media/srt/ingress_owner.rs`.
- Gates: `CAPACITY_PROTOCOLS=srt CAPACITY_SRT_OUTPUTS=100` (Restream on 4 CPUs,
  harness on 2) passes at least 6 of 7 repeats with the default 8 MiB sink
  buffer; per-socket receive drops and managed-RX exhaustion appear in the
  resource-sweep results.
- Context: after the reboot from kernel 7.0.0-34 to 7.0.0-38 the same release
  binary went from 8/8 SRT×100 passes to intermittent failure. Evidence
  (2026-10-03, `6f1bcfbf`, 21 probed runs):
  - Restream's send side is healthy in failing runs: 69–107k TX datagrams/s,
    all completed, about 850 per output per second against ~760 needed.
  - Pass or fail follows the harness sink's UDP receive-buffer drops: passes at
    500–2,400 drops/s, failures at 19,800–39,200/s (7 runs, 2 passed).
  - A 24 MiB sink buffer cut drops to 0–4,300/s and passed 2 of 4.
  - GSO off in Restream (one `sendmsg` per datagram) passed 1 of 4 with drops of
    1.4k–29k/s, against 0 of 4 with GSO on: not the cause.
  - The sink drains at most 512 datagrams per wake, then sends every control
    packet with its own `send_to().await` before reading again; at ~64k
    datagrams/s per socket an 8 MiB buffer covers ~56 ms of not reading, while
    the harness CPUs are shared with the publisher and the VPS shows vCPU gaps
    up to 128 ms.
  Plan: run the sink on the srt-rs Compio Owner listener exactly as SRT ingress
  does (managed multishot RX fills a kernel-side buffer ring, batched TX), with
  per-socket drop counters. Proving that 7.0.0-38 itself made the receive path
  burstier needs a boot back into 7.0.0-34 (owner decision).
  Owner sink result (2026-10-03, #217): the plan did not fix it. On 2 harness
  vCPUs the Owner sink passed 0 of 7 with 17.5–25.5k managed-RX buffer
  exhaustions and 23k–361k kernel socket drops per 30 s window. A 4096-entry
  RX ring removed the exhaustions but delivered less (2–21 of 100), so
  exhaustion is a symptom. With 3 harness vCPUs (the script's default split)
  it delivered 97, 100 and 99 of 100: the sink is CPU-bound at one Owner per
  port. Production SRT ingest has the same one-Owner-per-port ceiling.
  Next: multiple Owners per port (`SO_REUSEPORT` in `Owner::listen`), added
  when kernel socket drops persist with the Owner busy; then rerun this gate.
- Status: open (Filed: 2026-10-02 by claude; investigated 2026-10-03). Blocks
  SRT capacity evidence on this host at the 4/2 CPU split.

### Q-028 [testing] [sonnet] Local live-harness flakes on a loaded 6-vCPU VPS
- Goal: decide whether the local failures below are real defects or host
  load, so a local live run is either trustworthy or explicitly ignored.
- Files: `test/harness/` scenarios `mixed.live.srt.*` and
  `srt-crypto-matrix`; `src/bin/test_harness/` signal validation.
- Gates: the same modes pass in CI's master push transport matrix (they do
  today); a local rerun on an idle host either passes or reproduces with a
  named cause.
- Context (2026-10-03, base `b118d1e` and the srt-rs repin alike, load
  average 13–20 on 6 vCPUs): `mixed.live.srt.h264.a2.bf2` failed 3 of 3 on
  base with too few video/audio markers, audio PTS gaps of 85–128 ms (vCPU
  stalls on this VPS reach 128 ms) and one missing audio rendition;
  `srt-crypto-matrix` failed 1 of 3 on base with `error sending request` to
  the API (`/auth/login`, `/engine/telemetry`). Correctness gates now run in
  PR CI (AGENTS.md); this item only covers whether local runs can be trusted.
- Status: open (Filed: 2026-10-03 by claude).

### Q-025 [performance] [opus] Re-qualify the SRT shard law after transport convergence
- Goal: settle `EgressShardProfile::SrtCpuParallel` and
  `srt_egress_connect_concurrency` against the final Compio Owner and transport
  architecture; the previous libsrt `CSndQueue` saturation rationale no longer
  applies.
- Files: `src/config.rs` (`target_egress_fabric_shards`,
  `EgressShardProfile`, `srt_egress_connect_concurrency`).
- Gates: one-core/shard-law matrix at the final transport topology, followed by
  cross-host qualification; no production policy change without valid measured
  evidence.
- Context: WI3.7's provenance-clean current-host result is provisional
  evidence, not a frozen coefficient or runtime-law decision. Defer new
  measurement until WI4A–WI6 transport convergence, WI7/WI9 cleanup, and WI8
  runtime/host calibration are complete. No production shard policy or
  performance constants change from WI3.7.
- Status: current-host part done (2026-10-02). The CPU-ceiling SRT profile is
  gone: SRT and RTMP share one service-demand law (`src/media/egress/sizing.rs`)
  with a delivery-checked cold prior (64 SRT outputs per shard at 4.8 Mbit/s),
  and resizing never moves a live output (`docs/runtime-crossings.md` M6).
  Still open: cross-host qualification of the cold prior and of
  `srt_egress_connect_concurrency` (Filed: 2026-09-05 by claude, from PR #141
  review).

### Q-026 [resilience] [opus] Attribute and recalibrate the frozen-SRT-destination RSS gate
- Goal: explain the ~70 MB RSS growth of `fault.srt-output-stall`'s frozen
  destination case (114 -> ~185 MB, then a plateau) and either remove the cause
  or recalibrate the fixed 64 MiB `MAX_ACCEPTABLE_RSS_GROWTH_KB` from an
  attribution, not from the observed result.
- Files: `src/bin/test_harness/fault_recovery/srt_stall.rs`, retry/cleanup paths
  in `src/media/egress/backends/srt*.rs`.
- Gates: `scripts/harness/run.sh fault.srt-output-stall -- --no-netns`;
  `scripts/harness/srt_final_qual.py frozen` for the RSS time series.
- Context: WI2.5 measured the gate failing on BOTH `c323e5f5` (67.6 and 72.5 MB)
  and the Compio Owner (69.7-74.6 MB over four runs), with a late-window plateau
  in each (candidate ~197 MB, baseline ~184 MB). No Compio-specific retry-memory
  regression, so the threshold was deliberately left unchanged; this debt
  predates the cutover. Evidence is in git history
  (`test/harness/baselines/srt-compio-owner-final/`, removed with the ledgers). Filed: 2026-09-20 by claude.

### Q-029 [performance] [sonnet] Close the payload-cache delivery watch item
- Goal: show whether the egress payload cache (E0) widens the worst-destination
  delivery ratio, or close the watch item.
- Files: `src/media/rtmp/egress_payload_cache.rs`.
- Gates: `cargo xtask capacity-ramp` RTMP at 100 outputs, cache build against
  `702bc3df`, interleaved, at least 10 repeats; record delivery (worst
  destination ratio, Jain) next to CPU.
- Context: SRT H.264 → 100 RTMP outputs, 10 interleaved repeats: CPU median
  48.2 % → 44.0 %, RSS 209 → 189 MiB. 9/10 runs had every destination ≥ 0.95
  against 10/10 for the baseline; the one dip (86/100) was uniform (Jain
  0.99978) and coincided with a host preemption burst. The worst-destination
  ratio spread was wider with the cache (0.892–0.994 vs 0.970–0.980).
- Status: open (filed from the media copy audit, removed 2026-10-04).

### Q-030 [performance] [opus] Make HLS upload publication-driven and move it off CONTROL
- Goal: HLS PUT reacts to segment/playlist publication, as RTMP/SRT egress
  reacts to feed wakes, and its network I/O leaves the CONTROL runtime.
- Files: `src/media/hls/upload.rs` (`UPLOAD_INTERVAL` 500 ms poll per output,
  Reqwest on Tokio CONTROL), `src/media/egress/` (fabric backend candidate).
- Gates: fake-server unit tests and proptest for request framing and retry
  state; a fuzz target for the HTTP response parser; a loom model if a thread
  hop is added; HLS-output benches before and after against Reqwest; a live
  harness fault case (destination stall and restart).
- Context: poll cost scales with outputs even when nothing changes, and the
  PUTs load the scheduler meant for low-rate control work. Two separable
  wins: publication-driven wake (independent of runtime) and an
  egress-fabric shard backend (Compio HTTP client). Measure each alone.
- Status: open (Filed: 2026-10-04 by claude, from the runtime-consistency
  review).

### Q-031 [architecture] [opus] Derive every thread count from one capacity allocator
- Goal: thread counts follow measured service demand (λ × D at a target
  utilization, bounded by real CPUs) instead of independent constants that
  together can claim more CPUs than the host has.
- Files: `src/config.rs` (`default_tokio_worker_threads` = `ceil(C/3)` in
  `[2, 8]`, `default_egress_fabric_shards` = `clamp(C, 2, 8)`,
  `RESTREAM_RTMP_INGRESS_OWNERS`, `RESTREAM_SRT_INGRESS_OWNERS`),
  `src/media/executor.rs` (MEDIA `clamp(C, 1, 4)`, fixed after start),
  `src/media/egress/sizing.rs` (the one demand-driven law today).
- Gates: mixed-load matrix (RTMP + SRT + HLS + recording) showing no
  regression in delivery against the current constants; Kani/Lean obligations
  in the [assurance roadmap](assurance-roadmap.md) once the model gates
  admission.
- Context: egress shards already use the demand law (Q-025). MEDIA has
  per-class busy/poll telemetry but no feedback loop; no auto-resize before
  mixed-load data. Ingress RX mode (managed multishot vs `RawReadiness`) is a
  host fact with its own coefficients: model it, do not unify it.
- Status: open (Filed: 2026-10-04 by claude, from the runtime-consistency
  review).

### Q-032 [performance] [opus] Measure core segregation of CONTROL, MEDIA and HOT threads
- Goal: decide, with evidence, whether CONTROL, MEDIA and Owner/shard threads
  get disjoint CPU sets.
- Files: thread spawn sites in `src/media/executor.rs`,
  `src/media/egress/shard.rs`, `src/media/srt/ingress_owner.rs`,
  `src/media/rtmp/` ingress owners.
- Gates: interleaved A/B (shared vs segregated affinity) at RTMP×100 and
  SRT×64 with delivery checked per output; `perf sched` run-queue latency.
- Context: the schedulers are logically separate but Linux can stack them on
  one run queue while another core idles; RTMP-owner experiments
  (`runtime-crossings.md` M4/M5) suggest placement matters.
- Status: open (Filed: 2026-10-04 by claude, from the runtime-consistency
  review).

### Q-033 [architecture] [opus] Decide per-feed shard groups versus a host-level shard pool
- Goal: the unit of egress execution matches the resource it spends (host
  CPU), or the per-feed shape is kept with a measured reason.
- Files: `src/media/egress/runtime.rs`, `src/media/engine_*_egress_fabric.rs`,
  `src/media/egress/backends/sink_shard.rs` and the pipeline-recirculation
  backend.
- Gates: many-feed matrix (1/4/8/16 feeds × few outputs) for threads, RSS,
  context switches and process CPU, delivery checked per output.
- Context: every fabric now starts at one shard and grows by demand, which
  removed most of the waste measured in M6 (8 × 1 SRT feeds: 51 → 35 threads
  with one shard per feed). Still per feed: one OS thread minimum for each
  (protocol, feed), including sink and pipeline outputs that have no socket
  poller and could run on MEDIA. Feed publish wake cost scales with
  subscribed shards (`WakeGate`, W1); an arm-before-park refinement is only
  worth it if the many-feed matrix shows it.
- Status: open (Filed: 2026-10-04 by claude, from the runtime-consistency
  review).

### Q-034 [resilience] [opus] Define one overload policy for shared service centers
- Goal: when a shared Owner or shard saturates, viable destinations keep
  delivering and the excess is shed or refused explicitly, with an
  operator-visible reason.
- Files: `src/media/egress/sizing.rs` (admission point before Add),
  `src/media/egress/backends/srt*.rs`, `src/media/rtmp/` egress.
- Gates: live harness case that drives one shard past saturation (M6: one SRT
  shard collapses from 92/96 to 0/128 delivered) and asserts the surviving set
  and the status contract.
- Context: RTMP connections mostly fail and retry one at a time; SRT outputs
  on one shard deteriorate together after saturation.
- Status: open (Filed: 2026-10-04 by claude, from the runtime-consistency
  review).

### Q-035 [observability] [sonnet] Attribute MEDIA pool time to feed and stage
- Goal: MEDIA telemetry answers "which feed/stage used the pool", not only
  "is the pool busy", at per-task (not per-packet) cost.
- Files: `src/media/executor.rs` (`MediaServiceClass` counters).
- Gates: unit test for the attribution counters; bench showing no hot-path
  regression.
- Context: input to Q-031's service-demand model.
- Status: open (Filed: 2026-10-04 by claude, from the runtime-consistency
  review).

### Q-036 [docs] [sonnet] Make runtime-crossings an enforced truth table
- Goal: `docs/runtime-crossings.md` is the one statement of which thread
  owns each crossing, and CI rejects docs that contradict it.
- Files: `docs/runtime-crossings.md`, `scripts/check/docs.mjs`.
- Gates: `node scripts/check/docs.mjs` fails on a seeded contradiction (for
  example the removed RTMP owner → Tokio media handoff with a 64 MiB permit).
- Context: a stale WI8 table (since deleted with `srt-compio-roadmap.md`)
  once led design work to a wrong runtime model.
- Status: open (Filed: 2026-10-04 by claude, from the runtime-consistency
  review).
