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
- [Q-031 one capacity allocator](#q-031-architecture-opus-derive-every-thread-count-from-one-capacity-allocator)
- [Q-033 host-level shard pool](#q-033-architecture-opus-run-egress-shards-from-one-host-level-pool-instead-of-per-feed-groups)
- [Q-034 overload policy](#q-034-resilience-opus-define-one-overload-policy-for-shared-service-centers)
- [Q-036 enforced runtime truth table](#q-036-docs-sonnet-make-runtime-crossings-an-enforced-truth-table)
- [Q-047 MoQ egress](#q-047-protocol-opus-moq-egress)
- [Q-049 silent SRT leaves](#q-049-resilience-opus-find-why-connected-srt-leaves-sent-nothing-under-host-overload)
- [Q-050 inputs and stages in runtime state](#q-050-architecture-sonnet-project-inputs-and-stages-into-the-runtime-state)
- [Q-053 external FFmpeg process topology](#q-053-architecture-sonnet-external-ffmpeg-process-topology-per-rendition)
- [Q-063 control-room nested-path boot](#q-063-frontend-sonnet-control-room-boots-on-nested-unknown-paths)
- [Q-064 incident signals before the page opened](#q-064-observability-sonnet-incident-correlation-before-the-page-opened)
- [Q-065 capacity cost of HLS, sink and pipeline outputs](#q-065-performance-sonnet-price-hls-sink-and-pipeline-outputs-from-a-measured-cost)
- [Q-066 SRT ingress test port race](#q-066-testing-sonnet-srt-multi-owner-test-port-can-join-a-foreign-reuseport-group)
- [Q-067 HLS push ingest: remaining gates](#q-067-protocol-opus-hls-push-ingest-remaining-gates)
- [Q-069 frame-level media units](#q-069-architecture-opus-frame-level-media-units-for-moq-and-fmp4-ingest)
- [Q-070 MoQ ingest](#q-070-protocol-opus-moq-ingest)
- [Q-071 HLS-to-HLS passthrough](#q-071-architecture-sonnet-hls-to-hls-passthrough)
- [Q-072 dedicated HLS ingest listener](#q-072-architecture-sonnet-dedicated-hls-ingest-listener-with-half-close)
- [Q-073 HEVC fMP4 HLS push ingest](#q-073-protocol-opus-hevc-fmp4-hls-push-ingest)
- [Q-074 bounded HLS ingest poll](#q-074-performance-opus-a-rare-multi-millisecond-hls-ingest-poll-is-bounded-whatever-its-cause)
- [Q-077 follow every rendition into one ring](#q-077-architecture-opus-follow-every-rendition-of-a-master-playlist-into-one-ring)
- [Q-078 choose versions the ingest can decode](#q-078-protocol-opus-choose-the-versions-and-codecs-the-ingest-can-decode)
- [Q-083 measure RTMP and SRT under pull load](#q-083-performance-sonnet-measure-rtmp-and-srt-under-pull-load)
- [Q-086 reassemble ADTS frames across PES](#q-086-protocol-sonnet-reassemble-adts-frames-that-span-pes-boundaries)

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
- Status: step 1 done (2026-10-13): one `HostCapacity` (`src/capacity.rs`),
  read once at boot, holds the CPU count and the CONTROL, MEDIA and
  per-pool egress defaults (values unchanged); every sizing law and the
  capacity priors read it (before, three priors read `available_parallelism`,
  the process's own cgroup, rather than the root `cpu.max` + affinity count
  of the thread laws, and each fabric re-read `/proc` and `/sys` on every
  rescale); `/metrics/system` `hostCapacity`
  reports CPUs and threads per class. Measured (C = 6, 4 mixed scenarios up
  to 100 outputs): CONTROL + MEDIA use less than 0.02 core and egress shards
  carry over 90 % of engine CPU, so resizing resident classes does not move
  delivery.
  The first shortfall (SRT, transcoded feed, 50 outputs, host at 1.7 of 6
  cores) sits in one SRT pool that grew to 4 shards at Add time and never
  again: `Observe` only shrinks, and a grown shard takes only new outputs.
  That is a placement/admission question and moves to Q-034 and Q-049.

### Q-033 [architecture] [opus] Run egress shards from one host-level pool instead of per-feed groups
- Goal: the unit of egress execution matches the resource it spends (host
  CPU): shards are a host pool that serves every feed, not one group per
  (protocol, feed).
- Files: `src/media/egress/runtime.rs`, `src/media/engine_*_egress_fabric.rs`,
  `src/media/egress/sizing.rs`, `src/media/egress/backends/sink_shard.rs` and
  the pipeline-recirculation backend.
- Gates: in the many-feed matrix (`runtime-crossings.md` M8), shard threads
  stop growing with feeds and the shard CPU each extra feed adds (about 5%
  per feed of 2 × 1.5 Mbit/s SRT today) falls toward the per-output cost
  (about 0.7%), every output delivered; record the per-feed floor a pool
  cannot remove (ingest, feed ring, publish wake). Per-feed isolation (Q-034
  admission, a frozen destination, feed teardown) keeps its harness cases;
  structural Owner and shard failures must keep the required failure domains,
  not merely leave memory finite; loom for the shared publish wake (`WakeGate`).
- Context: measured 2026-10-14 (M8). The same 48 Mbit/s of SRT egress over 32
  connections costs 84–87% shard CPU as 16 feeds × 2 and 30–35% as 1 feed
  × 32; process threads 51–52 against 21–22, RSS 291 MB against 62–73 MB.
  Part of that is per-feed work a pool keeps (16 ingests, rings, wakes).
  Every feed holds at least one shard thread per protocol, including sink
  and pipeline outputs that have no socket poller and could run on MEDIA.
  Feed publish wake cost scales with subscribed shards (`WakeGate`, W1).
- Status: experiment complete, cutover rejected (2026-10-14, M9); implementation
  remains open. At 16 × 2, pooling reduced shard CPU from 89.9/96.4% to
  37.2/29.5%, with 32/32 delivered. Frozen-destination and feed-teardown
  isolation passed, but an injected production Owner-fault handler failed
  both feeds in a shared pool where the per-feed control spared the sibling.
  Keep production per-feed groups until a design preserves the required
  failure domains or that contract is explicitly changed. The rejected
  prototype is preserved on its local experiment branch (`d8c39b52`).

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
- Status: admission done (2026-10-13). SRT and RTMP pools place new outputs
  on their least-loaded shard; before an Add, after the pool grows for it,
  the shard it would land on projects its busy with it (`ShardSizer::admit`,
  ceiling 60%: one shard degraded at 62%). Over the ceiling, the Add is not
  dispatched: the output fails with phase `egress_capacity`, the diagnosis
  is `capacityRefused` at the shard layer, an output alert names it, and the
  restart backoff retries it until the output's retry budget is spent
  (`retriesExhausted` then). The price uses the feed's rate: a runtime's
  first look at a ring takes the rate of its retained window (DTS), and a
  new SRT runtime looks at the ring its TS muxer reads, because the muxer
  may start with the runtime and every Add of a burst lands within the
  first 0.5 s (observed: 80 Adds in 170 ms were priced at the 4.8 Mbit/s
  reference rate before this). Live gate `srt.admission` (one 8 Mbit/s
  feed, one sink thread per 32 outputs): on one shard, 39 of 96 admitted,
  peak busy 42%; on two shards, 80 of 192, peak busy 46% and 47%; no
  stalls, and refused outputs sent nothing. Priced at the reference rate,
  76 per shard were admitted at 61–65% peak busy. With one sink thread for
  all 192 outputs the receiver, not the shards, was the limit: shard 0
  reached 70% and 27 outputs stalled. Admitted outputs are never shed; if
  load rises after admission (feed bitrate), only new Adds are refused.
  Still open: HLS PUT, sink and pipeline fabrics have no admission; the
  cold prior prices this host at 1.4–1.9× its measured per-output cost
  until a healthy window measures it (Q-025).

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

### Q-047 [protocol] [opus] MoQ egress
- Goal: subscribers receive a live pipeline over MoQ: IETF MOQT with the
  MSF catalog and LOC objects, one object per frame, one group per GOP,
  read from the pipeline ring (one retention layer, no ring to MoQ-cache
  copy), on Owner threads behind a Compio QUIC adapter.
- Sources: a pinned-SHA `moq-dev/moq` (or moxygen) wire profile; a new
  `EgressProtocol` variant, planner capability and fabric backend
  (`src/domain/output_spec/protocol.rs`, `src/media/egress/`). Needs
  Q-069 first.
- Gates: interop with moxygen and MediaMTX as subscribers (one harness
  mode); a loom model for the subscriber hand-off; bench before and after
  on the egress shard.
- Status: open; unparked by the user 2026-10-16. Q-025 (cross-host shard law)
  is still open, so the shard sizing for MoQ starts from the current-host
  priors (Filed: 2026-10-05 by claude, from the architecture reviews).

### Q-049 [resilience] [opus] Find why connected SRT leaves sent nothing under host overload
- Evidence: while `mixed.signal` ran an H.265 case beside another case on a
  6-core host (the wave pairing fixed in Q-048), every SRT 1080p leaf of one
  pipeline completed the SRT handshake (pcap: the FFmpeg listener accepted
  the conclusion) and then sent no payload for 30 s. `/api/v1/engine/health`
  showed `bytesOut` 0, feed lag growing, `backpressureReason`
  `backpressured` and no caller quality stats. SRT 720p leaves and RTMP
  1080p leaves on the same stage delivered. It did not reproduce
  standalone.
- Question: can a leaf be starved by its shard or Owner under host CPU
  pressure while its siblings progress? That would break output isolation.
- Gates: a reproduction (load generator plus one feed) or a proof that the
  leaf cannot stall while connected; the fix keeps sibling leaves unaffected.
- Not reproduced (2026-10-13, master `f040cbdc` plus #320, 6 cores):
  `mixed.live.srt.h265.a2.bf2` with five CPU-bound spinners passed, and the
  whole `mixed.signal` mode with four spinners passed (6 cases).
- The stall bound does not explain the evidence; it contradicts it.
  `classify_stall` (`egress/policy.rs`) marks a leaf with pending bytes as
  `stalled` when its feed lag exceeds `max_lag_units` (300 for SRT) or when
  it has made no progress for `max_backpressure_duration`. SRT leaves take
  their limits from `LeafLimits::from_policy`, so that duration is
  `LeafPolicy::no_progress_timeout`, 15 s (the 30 s `LeafLimits` default
  does not apply to them). The SRT stall sweep (`srt_drain.rs`) closes a
  stalled leaf and the output retries. A leaf that reported `backpressured`
  for 30 s with growing lag was therefore either recording progress without
  payload, or never reaching `stalled`. Progress without payload can come
  from:
  - `record_send` with units and no bytes (`leaf.rs`);
  - the backlog-shrink rule in `SrtLeaf::observe_stall`.

  A lag of 0 is reported while the cursor is unprimed. The leaf also
  escapes the bound in these cases:
  - With zero pending bytes the leaf is `idle`. When there are no caller
    stats the backlog counts as 0.
  - While the shard reports `saturated`, a stalled leaf is kept as
    `shard_saturated` for as long as the overload lasts.

  "No caller quality stats" means `owners.stats(&leaf.caller)` returned
  nothing, or `sample_quality` had no first sample yet. The next probes:
  - the Owner's view of that caller: still connecting, or missing from its
    peer table?
  - which of the progress paths above fired.
- Status: open (Filed: 2026-10-12 by claude, found while fixing Q-048).

### Q-050 [architecture] [sonnet] Project inputs and stages into the runtime state
- Goal: the control room reads pipeline inputs and stages from
  `/api/v1/state/stream` (Q-042's projection: pipelines and outputs today)
  instead of polling them; nothing else changes for operators.
- Evidence: after the FE7 cut, Operate polls `/pipelines/{id}/inputs` and
  the graph every 2 s; Inspect and Monitor poll the resource map and the
  graph (`usePolled` in `operate.tsx`, `inspect.tsx`, `monitor.tsx`).
- Gates: the drift test of the generated TypeScript contract; resync tests
  for the new entities; a Playwright case that sees an input change without
  a poll.
- Status: open (Filed: 2026-10-13 by claude, the follow-up Q-042 named when
  it closed at the FE7 cut).

### Q-053 [architecture] [sonnet] External FFmpeg process topology per rendition
- Goal: one recorded decision for how many external FFmpeg children a pipeline
  runs, so nobody builds a merged multi-output child without an isolation gate.
- Decided and recorded: one child per live unique FFmpeg-backed `StageKey`,
  shared by every destination, so the child count follows a pipeline's
  FFmpeg-backed stages rather than its destinations
  (`high-performance-data-path.md`, "Native and child-process boundaries"). A
  merged multi-output child would put every merged destination in one codec
  failure domain, which "Sharing and isolation" forbids, so it needs an explicit
  failure-isolation gate and an owner decision.
- Measured (2026-10-15, release binaries at `1f4e0a34`, 1/5/10 egress counts):
  `docs/resource-sweep.md`, "Per-rendition FFmpeg child cost". One child carried
  1 to 20 outputs, two carried 4 to 60, and total child RSS stayed flat (186.6 to
  186.7 MB across 2/10/20 outputs; 422.3 to 422.9 MB across 4/20/40).
- Resolved: `egress-growth-hevc-bridge` runs `configIndex: 2` (HEVC SRT ingest)
  with `outputKinds: ["rtmpSource"]`, and `SweepOutputKind::RtmpSource` is
  `RtmpOutputMode::Enhanced` (`resource_sweep/catalog.rs:102`). Enhanced RTMP
  carries HEVC, so the output resolves to HEVC, `needs_h264_codec_edge` is false
  (`output_path.rs:170-176`), and 0 children is what the code predicts. The
  earlier claim that "a `source` passthrough output on HEVC ingest costs 1, not
  0" holds only when the destination resolves to H.264.
- Open: two cases no scenario exercises. Both are one JSON line, because the
  catalog kinds already exist:
  - the non-lightweight audio route: `RtmpSourceDownmix`/`SrtSourceDownmix`
    (`source+downmix:0`, `resource_sweep/catalog.rs:89`) exist and no scenario
    uses them. The route chains after `Source`, so this is one child, not two in
    series; two in series would need a preset plus a route, which no catalog
    kind produces.
  - the HEVC-to-H.264 codec edge (`output_path.rs:131,170-176`): needs HEVC
    ingest (`configIndex: 2`) with an H.264-only kind - `rtmpsSource` or
    `hlsPut`, the `Legacy` modes (`catalog.rs:103-105`).
- Gates (measurement only): release binaries (`cargo xtask build-release` ->
  `target/qual-release/`, provenance `COMMIT` matching HEAD), `HARNESS_BIN` and
  `RESTREAM_BIN` pointing at them with `BENCH_BUILD=never` so
  `scripts/harness/run.sh` cannot trigger a `cargo xtask build-bench`
  immediately before measuring, and `--no-netns` on this host (no `unshare`).
- Status: open (Filed: 2026-10-15 by claude; the cost measurement closed
  2026-10-15. The former board item "FFmpeg A/B: external per-stage vs external
  multi-output" cannot be run as written: the second arm does not exist).

### Q-063 [frontend] [sonnet] Control room boots on nested unknown paths
- Goal: any served path boots the control room.
- Evidence: a nested unknown path (`/a/b`) serves `index.html`, whose
  relative asset URLs (`js/control-room/main.js`, `base-path.js`,
  `output.css`) then resolve under `/a/`, so the page stays blank. `/`,
  `/index.html` and single-segment paths work (#333 test "a stale deep path
  still opens the workspace in the query"). The app never links to such a
  path; pre-existing.
- Fix: asset URLs from `window.__RESTREAM_BASE_PATH__`, or a redirect to the
  base path.
- Gates: a Playwright case on `/a/b?w=inspect`.
- Status: open (Filed: 2026-10-15 by claude, found while testing #333).

### Q-064 [observability] [sonnet] Incident correlation before the page opened
- Goal: the Incidents correlation chart covers the incident's whole span,
  not only the page's last 15 minutes.
- Sources: the chart reads the store's fleet samples
  (`LiveSnapshot.history`, `WINDOW_MS` 15 minutes, from when the page
  opened). The engine keeps no fleet signal history, so an incident older
  than the page, or a 1h/6h/24h resolved window, has no chart before it.
  Needs a bounded server-side history of the fleet signals (delivering,
  headroom, degraded by layer) with `since`, and the chart reading it.
- Gates: Rust tests for the history's bound and `since`; Playwright for an
  incident older than the page.
- Status: open (Filed: 2026-10-15 by claude, the rest of Q-061).

### Q-065 [performance] [sonnet] Price HLS, sink and pipeline outputs from a measured cost
- Goal: the capacity model prices each output kind by what it costs.
- Sources: `OutputProtocol::of_egress` (`src/capacity.rs`) prices every
  output that is not SRT or RTMPS as RTMP egress. Sink and pipeline outputs
  send nothing over the network, so they are overcharged on egress and NIC.
  HLS PUT uploads segments, not a per-packet stream. Needs a measured cost
  per kind (a capacity-ramp run or bench), not an assumed weight.
- Gates: the measurement in the commit message; a Rust test per kind.
- Status: open (Filed: 2026-10-16 by claude, from the #345 review).

### Q-066 [testing] [sonnet] SRT multi-Owner test port can join a foreign reuseport group
- Goal: `two_owners_share_the_port_and_each_session_command_reaches_its_owner`
  cannot share its port with another process.
- Sources: a multi-Owner listener needs an explicit port (srt-rs
  `owner_layout.rs`; production ingest uses a configured, well-known
  one), so the test probes a free port and releases it before binding. A
  bind error now retries on a fresh port. A foreign `SO_REUSEPORT` socket of
  the same uid (another `cargo test` on this host) that takes the port in
  the gap would instead join the group and take a share of the datagrams;
  the test's byte counts fail on that, so it is loud, not silent. Removing
  it needs a per-test network namespace for this unit test, or srt-rs
  binding Owner 0 first and the rest to its port.
- Gates: none until it flakes.
- Status: open (Filed: 2026-10-16 by claude).

### Q-067 [protocol] [opus] HLS push ingest: remaining gates
- Goal: what HLS push ingest (`src/media/hls_ingest/`, TS and H.264/AAC
  fMP4 segments over PUT and POST) still lacks for encoders that need more
  than the stream key, and the gates it has not yet passed.
- Contract:
  - Optional basic auth over the stream key.
  - Generated `ingestUrls.hls`: `AppState` does not know the HTTP port yet.
- Gates: a test that a panic inside one session ends only that session and
  releases its input; one harness mode with FFmpeg pushing TS and fMP4
  over PUT and POST.
- Status: open (Filed: 2026-10-16 by claude, user request; TS and
  H.264/AAC fMP4 push done).

### Q-069 [architecture] [opus] Frame-level media units for MoQ and fMP4 ingest
- Goal: the ring can feed per-frame consumers (MoQ objects, LL-HLS parts).
- Sources: `MediaPacket` (`src/media/packet.rs`) has no codec
  configuration, configuration generation, duration or group id. The TS
  demuxer already emits one AAC frame per packet. Timestamps are
  milliseconds: per-frame jitter is at most 0.5 ms, and the fMP4 path
  derives durations from DTS deltas (`build_mux_samples` in
  `src/media/hls/fmp4/codec.rs`),
  so the total does not drift.
- Change: a per-track
  configuration with a generation number that readers can follow; a GOP
  sequence number from the keyframe gate. `MediaPacket` is pinned at 56
  bytes by a layout test, so the configuration generation and group id go
  in a side table keyed by ring sequence unless the ring bench shows that
  a larger packet costs nothing.
- Gates: the existing fMP4 and TS output tests unchanged; bench of the ring
  hot path before and after.
- Status: open (Filed: 2026-10-16 by claude; blocks Q-047 and Q-070).

### Q-070 [protocol] [opus] MoQ ingest
- Goal: a MoQ publisher (MSF catalog, LOC objects) feeds a pipeline input.
- Contract: the track namespace carries the stream key. Objects map to
  `MediaPacket`s through Q-069, and the input registers as protocol
  `"moq"`. The QUIC listener runs on Owner threads.
- Gates: interop with moxygen and MediaMTX as publishers (one harness
  mode); a fuzz target for the object parser.
- Status: open (Filed: 2026-10-16 by claude, user request; after Q-047).

### Q-071 [architecture] [sonnet] HLS-to-HLS passthrough
- Goal: an HLS output relays the selected HLS input's segments instead of
  demuxing and repackaging them.
- Why it is possible: the HLS PUT shard already works at segment level
  (`HlsStore`, `egress/backends/hls_put_shard.rs`), so a passthrough
  skips the ring, the demuxer and the segmenter, and feeds the store the
  shard already reads.
- Conditions: HLS PUT output only; no stage between input and output;
  matching container (TS to TS, fMP4 to fMP4), codecs and segment
  boundaries (our output cuts at keyframes, 1 s minimum,
  `hls/mod.rs:52`). The input must be self-initializing. Validated at
  apply time, with a fallback to the normal path and a stated reason.
- Costs:
  - The output stops reading the ring, so capacity accounting, preview and
    pipeline health need a stated answer for passthrough outputs.
  - `HlsStore` gains a second source of truth, or gains a per-output
    store.
  - Failover between primary and backup inputs runs through the ring's
    input gate and promotion timestamp mapper (`src/media/input_gate.rs`).
    A relay loses failover unless it rebuilds it at segment level:
    switch inputs on a segment boundary, add `EXT-X-DISCONTINUITY`, and
    keep the output's media sequence monotonic. This must not ship
    silently broken.
- Trigger: telemetry showing meaningful CPU in HLS demux plus segmenting
  for HLS-to-HLS pipelines. Until then the ring path stands.
- Gates: one harness mode pushing HLS and relaying it, checked against a
  normal-path run (bytes and timing); a test that an ineligible pipeline
  (a stage, or a non-HLS output) falls back with the reason recorded.
- Status: open (Filed: 2026-10-16 by claude, user question on passthrough;
  after Q-067/Q-068).

### Q-072 [architecture] [sonnet] Dedicated HLS ingest listener with half-close
- Goal: only HLS push uploads get HTTP/1.1 half-close; dashboard SSE
  streams notice a departed client at once again.
- Sources: `src/api/http_server.rs` serves every dashboard route with
  `half_close(true)` because FFmpeg (default `-http_persistent 0`) closes
  its side after the body and hyper otherwise drops the upload. With it,
  a client that leaves an SSE stream (`/api/v1/state/stream`, logs) is
  noticed only when a write fails, at the second write after the reset
  (20 to 40 s with the 20 s heartbeat), not at `tx.closed()`.
- Change: serve `/ingest/hls/` on its own listener (or a per-route
  connection policy) that sets half-close, and return the dashboard to
  hyper's default.
- Gates: the FFmpeg POST push test still passes; a test that a dropped
  SSE client releases its receiver within one heartbeat.
- Status: open (Filed: 2026-10-16 by claude).

### Q-073 [protocol] [opus] HEVC fMP4 HLS push ingest
- Goal: HLS push ingest accepts fMP4 segments with HEVC video, as it does
  H.264.
- Sources: `src/media/ingest_publisher/fmp4.rs` rejects an HEVC init
  (`hvc1`/`hev1`) with `415` "HEVC fMP4 ingest is not implemented".
  Supporting it needs an `hvcC` parser: the VPS/SPS/PPS arrays and the
  NALU length size from the `hvcC` record, with the parameter sets
  injected at keyframes as the H.264 path does from `avcC`.
  `codec/enhanced_rtmp_hevc.rs` only builds an `hvcC` and never parses one.
- Gates: an `ffmpeg -hls_segment_type fmp4` end-to-end test with an HEVC
  input in `tests/hls_push_ingest.rs`; a unit test for the `hvcC` parser.
- Status: open (Filed: 2026-10-16 by claude).

### Q-074 [performance] [opus] A rare multi-millisecond HLS ingest poll is bounded whatever its cause
- Goal: a `hlsIngest` poll on the MEDIA executor never spends multiple
  milliseconds of thread CPU, so one session's turn cannot hold a shared
  worker whatever the stall's cause is.
- Files: `src/media/executor.rs` (poll timing), `src/media/ingest_publisher.rs`
  (the 64 KiB chunk publish), `src/media/mpegts/demux.rs`,
  `src/media/ring_buffer.rs`, `src/malloc_tuning.rs`.
- Sources: the `hls-push-duty` harness mode (release binaries, 2 RTMP base
  publishers plus M HLS push sessions, 60 s, 6-vCPU shared host, one run per
  arm), with the poll wrapper extended to time the polling thread's own CPU
  (`CLOCK_THREAD_CPUTIME_ID`) beside the wall time and the session turn
  instrumented per phase.
- **A chunk-publish poll** costs ~170 µs of thread CPU (averaged over every
  poll, including those that only apply an object or wait, ~84 µs): demux/feed
  77 µs, ring publication 76 µs (2.8 packets per chunk; 16812 packets over
  6034 calls), preview push 2 µs, input gate 1 µs, parameter sets and
  keyframe bookkeeping 6 µs, continuity/timestamps 3 µs, and the probe's
  engine hop 673 µs once per session (`apply_init` never ran: the arm pushes
  MPEG-TS). The turn's other work is negligible: `session.poll` (the
  due-release burst) ≤402 µs and its outbox drain ≤9 µs per turn, at most 2
  releases in a turn (23 in 800 turns).
- **The multi-millisecond poll is not per-unit work.** The same 64 KiB feed
  costs 10 µs at best and 32069 µs at worst, and the same 2.8-packet ring
  push 1 µs at best and 12077 µs at worst (54 of 6034 calls above 1 ms). The
  polling thread's own CPU tracks the poll's wall time (maxCpuUs 51337 of
  maxPollUs 52507 µs), so the thread is executing, not waiting on a lock and
  not descheduled: host load was ~25 % of 6 vCPUs, steal 0 %,
  `cpu.stat nr_throttled` 0, no D-state.
  `MALLOC_TRIM_THRESHOLD_=-1` (with `MALLOC_MMAP_THRESHOLD_=16M`) cuts the
  tail: one run each on the fixed build, M=8, class `maxPollUs` 15882 → 6022
  µs and `maxCpuUs` 12443 → 8051 µs, with the per-service maxima 4.5–15.9 ms →
  2.8–6.0 ms while per-poll CPU stayed 84 µs in both arms — the tuning cannot
  change the work, only the stall. One run each, so a direction, not a gate.
  What it does not separate is which layer stalls: glibc's own work (heap
  trim, `munmap` of mmap-threshold chunks, arena locking) and the kernel's
  memory management behind it (page faults on fresh pages — up to 1762 minor
  faults in one demux call — THP compaction, memcg reclaim) both shrink
  when glibc stops returning memory.
- **The reported scaling does not reproduce.** Control re-runs of the
  unmodified code (executor poll CPU clock only) gave `maxPollUs` 21985 µs at
  M=4 and 23328 µs at M=8 with per-poll busy 110 → 139 µs, while the
  per-session stall varied 12–52 ms across runs of either build: the 5259 µs
  M=4 arm was a lucky run beside an unlucky M=8 one. `maxPollUs` alone is not
  a gate; per-poll CPU time is.
- Done in this change: the one path whose work per poll was unbounded by
  design — the object branch applied every object the HTTP handlers queued
  while the poll ran (2 in the measured run, 3 in the unit test with the
  bound removed) — now yields after one object, so a poll never joins two
  units (invariant in `docs/media-pipeline.md`, gate in
  `src/media/hls_ingest/session_turn_tests.rs`). Within an fMP4 unit, the
  top-level box walk stops at 4096 boxes, a `moof` is at most 256 KiB, a
  segment declares at most 16384 samples, and a publish converts only the
  samples its chunk released (gates in `boxes_tests.rs` and
  `fmp4_tests.rs` under `src/media/ingest_publisher/`). Still per fragment,
  not per chunk: `shiguredo_mp4`'s `handle_media_segment` decodes the whole
  `moof` and the session plans its samples in the poll that first reaches
  it, within those caps; a library-side incremental `trun` parse is the
  remaining work for fMP4.
- Steps:
  1. Land per-poll thread CPU in the executor metrics: beside `maxPollUs`, a
     `maxCpuUs` and a count of polls above fixed CPU thresholds (1, 4, 16 ms)
     per class and service, read with the `thread_cpu_ns` helper in
     `src/media/egress/shard.rs`; the investigation's patch did this and cost
     one `clock_gettime(CLOCK_THREAD_CPUTIME_ID)` pair per poll.
  2. Per arm, separate allocator from kernel: per-poll user/sys split and
     minor faults from `getrusage(RUSAGE_THREAD)` around the poll, and
     before/after deltas of `/proc/vmstat` `compact_stall` and
     `allocstall_*`, the cgroup's `memory.events`, and the THP settings in
     `/sys/kernel/mm/transparent_hugepage/{enabled,defrag}`.
  3. Repeat each arm several times on an idle host; then choose between
     allocator tuning (glibc `mallopt` at startup next to the arena policy in
     `src/malloc_tuning.rs`, or a different allocator), a kernel-side setting
     (THP `defrag`), and leaving the tail to host qualification, and record
     the result, negative included.
- Gates: at M=8 over five runs on an otherwise idle 6-vCPU host, `hlsIngest`
  polls above 4 ms of thread CPU stay under 1 per minute per session, and
  the class `maxCpuUs` stays under 4 ms in every run. A wall-clock maximum
  on a shared host is not a gate any code change can meet.
- Status: open (Filed: 2026-10-16 by claude, from the HLS ingest isolation
  measurement; rewritten 2026-10-17 from the attribution that measurement
  asked for).

### Q-077 [architecture] [opus] Follow every rendition of a master playlist into one ring
- Goal: an HLS ingest session whose master carries audio as a separate
  `EXT-X-MEDIA` rendition delivers both video and audio — for push and pull
  alike.
- Why it is not one ingest kind: the assembler is shared, and two independent
  places stop it today.
  - `ordering::Session` locks onto one playlist path (`Lock::Media(path)` /
    `Lock::Master(path)`); `Lock::follows()` answers `Some(false)` for any
    other path, so a second playlist and every segment of it take the
    `on_other_variant` path and are dropped (`ordering.rs:380-390,427-441`,
    `session.rs:384,431`). Two tests pin that behaviour:
    `without_a_master_the_first_media_playlist_is_followed` and
    `a_master_playlist_selects_the_highest_bandwidth_variant`.
  - `SegmentPublisher` holds one `Box<dyn SegmentDemux>`, and the fMP4 path
    replaces init and probe on `apply_init` (`ingest_publisher/fmp4.rs:411-416`)
    while `plan_fragment` skips samples outside the current init. Two
    renditions alternate inits: the audio init replaces the video track set,
    and `take_init` will not re-deliver the video init (its hash is already
    delivered), so the video segments after it are demuxed against the wrong
    init.
  This is why a pull of Restream's own preview is video-only: its master has
  `video/index.m3u8` and `audio/{track}/index.m3u8` as separate fMP4
  renditions, each with its own `EXT-X-MAP`.
- Change: a second followed path with its own sequence space in
  `ordering::Session`; per-rendition demuxers in `SegmentPublisher` feeding
  one ring, with pacing and continuity answering the rendition set (which
  timeline decides `media_time_ms`, and how two renditions' DTS align). The
  puller then follows the chosen variant's default audio rendition
  (`DEFAULT=YES`, else `AUTOSELECT=YES`, else the first), as FFmpeg's HLS
  demuxer does.
- Gates: a session whose master has separate audio and video renditions
  publishes both to the ring, for push and for pull; `hls-pull-duty` and
  `hls-push-duty` both report non-zero audio and video counts; the
  single-rendition and pacing tests still hold.
- Status: open (Filed: 2026-10-17 by claude, from the pull audio
  investigation; the pull feature reports the gap with a warning instead of
  losing audio silently).

### Q-078 [protocol] [opus] Choose the versions and codecs the ingest can decode
- Goal: a pull that follows a master playlist picks a variant whose media
  this ingest can publish, so a ladder is not refused from the top down.
- Why: the pull's rule is `BANDWIDTH` (`highest`, the default, or `lowest`)
  and nothing else. `EXT-X-STREAM-INF` carries `CODECS`, `RESOLUTION` and
  `FRAME-RATE`, and the parser already keeps the variant's `path`,
  `BANDWIDTH` and `AUDIO` group; a ladder whose highest variant is HEVC
  (`hvc1`) is pulled, its frames are refused one segment at a time
  (`ObjectOutcome::Unsupported`), and the pull keeps refusing until the
  operator changes the rule. `hls-push-duty` has the same shape from the
  encoder side: a single ingest protocol cannot carry two dialects.
- Change: record `CODECS`' video and audio codecs on `Variant` (additive, the
  parser is shared) and have `choose_variant` prefer a variant whose codecs
  the assembler accepts (H.264 video, AAC audio, and whatever Q-073 lands),
  falling back to the current rule when every variant is unsupported or none
  declares its codecs. Where the ladder is the pull's, the variant rule
  becomes a preference rather than a promise; the refusal path stays as it is.
- Gates: a master whose highest variant is HEVC and whose lower one is H.264
  publishes media with the default rule; a master whose variants declare no
  codecs behaves exactly as today; the push parser's behaviour is unchanged.
- Status: open (Filed: 2026-10-17 by claude, from the HLS pull review: the
  ingest-unsupported top variant has no way out but a manual rule change).

### Q-083 [performance] [sonnet] Measure RTMP and SRT under pull load

- Goal: the isolation of the other protocols under M pull sessions is measured
  the way `hls-push-duty` measures it under M push sessions.
- Why: `hls-push-duty` has a baseline arm (`--hls-publishers 0`) and an HLS arm
  (base RTMP publishers plus M HLS push sessions), which is how Q-074 priced the
  push ingest. `hls-pull-duty` has no base arm: it runs only its own pull
  pipeline, so "RTMP under pull load" cannot be measured from the catalog. The
  pull's incremental cost is bounded by construction — a fetch on a Tokio
  worker (not the MEDIA executor), a playlist parse of at most
  `max_playlist_bytes` per refresh no shorter than `MIN_REFRESH_INTERVAL`
  (500 ms), and one read of pull rows per reconcile pass — but it is unmeasured
  beside a live RTMP or SRT pipeline.
- Change: give `hls-pull-duty` a `--base-publishers N` arm, or add M pull
  inputs to `hls-push-duty`, so one release-binary run compares the base
  publishers' delivery and per-poll CPU with and without pulls.
- Gates: both arms pass, and the base publishers' delivery and per-poll CPU are
  reported side by side.
- Status: open (Filed: 2026-10-17 by claude, from the #354 non-HLS audit: the
  push ingest has this arm, the pull ingest does not).

### Q-086 [protocol] [sonnet] Reassemble ADTS frames that span PES boundaries

- Goal: a TS source whose audio PES packets do not start and end on ADTS frame
  boundaries (`data_alignment_indicator = 0`) still yields one complete AAC
  frame per packet.
- Why: `TsDemuxer::flush_pes` cuts an AAC PES into its complete ADTS frames
  and drops bytes after the last one; the next PES then starts mid-frame, finds
  no sync at offset 0, and is forwarded whole. Such a stream loses the tail
  frame of every PES and hands consumers a misaligned unit. Muxers that
  align audio PES (FFmpeg, MediaMTX, most encoders) never hit this; the
  behaviour predates the split.
- Change: carry an audio stream's trailing partial frame into its next PES
  and resync on the next ADTS header, with the carried bytes bounded by one
  maximum ADTS frame (8191 bytes).
- Gates: a demuxer test with frames split across two PES packets yields every
  frame once, in order, with stepped timestamps; a fuzz case of random
  PES cuts never yields a unit without a leading sync word.
- Status: open (Filed: 2026-10-17 by claude, from the #358 review: the
  frame split drops the cut tail rather than carrying it).
