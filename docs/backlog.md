# Backlog

The one list of open engineering work. Each item states its goal, the files it
touches, the gates that prove it, and the evidence so far. Close an item by
deleting it in the change that does the work; the commit message keeps the
evidence. Larger directions live in [assurance roadmap](assurance-roadmap.md)
(verification) and [runtime crossings](runtime-crossings.md) (open measurement
items).

## Contents

- [Q-027 harness SRT sink drops at SRT×100](#q-027-performance-opus-stop-the-harness-srt-sink-dropping-datagrams-at-srt100-kernel-700-38)
- [Q-025 SRT shard law cross-host](#q-025-performance-opus-re-qualify-the-srt-shard-law-after-transport-convergence)
- [Q-026 frozen-SRT-destination RSS gate](#q-026-resilience-opus-attribute-and-recalibrate-the-frozen-srt-destination-rss-gate)
- [Q-031 one capacity allocator](#q-031-architecture-opus-derive-every-thread-count-from-one-capacity-allocator)
- [Q-033 host-level shard pool](#q-033-architecture-opus-run-egress-shards-from-one-host-level-pool-instead-of-per-feed-groups)
- [Q-034 overload policy](#q-034-resilience-opus-define-one-overload-policy-for-shared-service-centers)
- [Q-047 MoQ egress: IETF MOQT profiles](#q-047-protocol-opus-moq-egress-ietf-moqt-profiles)
- [Q-049 silent SRT leaves](#q-049-resilience-opus-find-why-connected-srt-leaves-sent-nothing-under-host-overload)
- [Q-066 SRT ingress test port race](#q-066-testing-sonnet-srt-multi-owner-test-port-can-join-a-foreign-reuseport-group)
- [Q-067 optional Basic auth for HLS push](#q-067-protocol-opus-optional-basic-auth-for-hls-push)
- [Q-070 MoQ ingest: IETF MOQT profiles](#q-070-protocol-opus-moq-ingest-ietf-moqt-profiles)
- [Q-071 HLS-to-HLS passthrough](#q-071-architecture-sonnet-hls-to-hls-passthrough)
- [Q-074 bounded HLS ingest poll](#q-074-performance-opus-a-rare-multi-millisecond-hls-ingest-poll-is-bounded-whatever-its-cause)
- [Q-087 separate-rendition timeline breaks](#q-087-protocol-sonnet-align-separate-renditions-on-timeline-breaks-not-on-raw-proximity)
- [Q-088 MoQ egress cost](#q-088-performance-sonnet-price-moq-egress-from-a-measured-cost)
- [Q-089 MoQ origin track-set changes](#q-089-protocol-sonnet-the-moq-origin-follows-a-track-set-change-while-served)
- [Q-090 MoQ `ca=` path confinement](#q-090-security-sonnet-confine-moq-egress-ca-files-to-a-configured-directory)
- [Q-091 SRT Backup replays the lost prefix](#q-091-protocol-opus-srt-backup-failover-replays-the-lost-prefix)
- [Q-092 no descendants of a dropped dependency](#q-092-resilience-opus-never-emit-descendants-of-a-dropped-dependency-bearing-unit)
- [Q-093 headless MoQ subscriber](#q-093-testing-sonnet-headless-moq-subscriber-for-impairment-and-capacity-gates)
- [Q-094 sideband media events](#q-094-protocol-opus-first-class-sideband-media-events)
- [Q-095 stream inspector](#q-095-observability-sonnet-stream-inspector-gop-health-skew-and-scte)
- [Q-096 GRO and arrival timestamps](#q-096-performance-sonnet-gro-and-arrival-timestamps-in-the-reference-srt-path)
- [Q-097 MediaMTX MoQ dialect interoperability](#q-097-protocol-opus-align-mediamtx-moq-dialect-with-ietf-interop)

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
  predates the cutover. New runs in #374 passed at +7.6 MiB (bench) and
  +4.7 MiB (debug), and the scenario now gates CI's concurrency contract and
  the nightly release shards. These passes do not attribute the previous
  ~70 MB growth; attribution remains open and the threshold is unchanged.
  Earlier evidence is in git history
  (`test/harness/baselines/srt-compio-owner-final/`, removed with the ledgers). Filed: 2026-09-20 by claude.

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

### Q-047 [protocol] [opus] MoQ egress: IETF MOQT profiles
- Goal: the MoQ egress and origin also speak IETF MOQT draft-18 with the MSF
  catalog and LOC objects, Cloudflare's draft-16 (token in the URL path) and
  the MediaMTX v1.21.2 subset, beside the moq-lite-06 + hang profile.
- Gates: interop with moxygen, MediaMTX and Cloudflare's relay as
  subscribers (one harness mode); fuzz targets for the control and object
  parsers.
- Status: open; phase 1 (moq-lite-06 + hang, loom model for the origin
  hand-off) landed in #371 with no regression on the unchanged shared
  egress and ring benches (A/A); the MoQ egress cost itself is Q-088
  (Filed: 2026-10-05 by claude, from the architecture reviews).

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

### Q-067 [protocol] [opus] Optional Basic auth for HLS push
- Goal: an encoder that must authenticate with a username and password (as
  `http://user:pass@host/ingest/hls/KEY/index.m3u8`) can push, on top of the
  stream key.
- Why: HLS push authorizes by the stream key in the path alone
  (`src/api/hls_ingest.rs`). A per-input push credential is new stored state:
  a write-only field on the input (as pull `source_auth` is), its API
  validation and redaction, and the input form.
- Gates: an upload with the configured credentials is accepted and one
  without (or with wrong ones) is refused with `401` before any body byte is
  read; an input with no credential behaves as today.
- Status: open (Filed: 2026-10-16 by claude, user request; TS and HEVC/H.264
  fMP4 push, `ingestUrls.hls`, panic isolation and the PUT/POST harness gate
  are done).

### Q-070 [protocol] [opus] MoQ ingest: IETF MOQT profiles
- Goal: an IETF MOQT draft-18 publisher (MSF catalog, LOC objects) feeds a
  pipeline input through the existing MoQ listener, as moq-lite + hang
  publishers do since #371.
- Contract: the stream key comes from the connection path, else the track
  namespace; objects map to `MediaPacket`s and the catalog's codec
  configuration to the ring's track configuration; the input registers as
  protocol `"moq"`.
- Gates: interop with moxygen and MediaMTX as publishers (one harness mode);
  a fuzz target for the object parser.
- Status: open; phase 1 landed in #371 (Filed: 2026-10-16 by claude, user
  request; with Q-047).

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
- Steps 1–3 are done (2026-10-18): executor rows carry `cpuUs`, `maxCpuUs`
  and `pollsOverCpu{1,4,16}Ms`, with `RESTREAM_MEDIA_POLL_USAGE=1` the
  per-poll `getrusage` split (`docs/observability.md`), and `hls-push-duty`
  reports the window per class and session plus `/proc/vmstat`, cgroup
  `memory.events` and THP (`docs/testing.md`).
- **Measured (release, M=8, 60 s, `--poll-usage`, 6-vCPU KVM VPS):** five
  control runs at load-1m 1.38–1.46: class `maxCpuUs` 247781, 33407, 56333,
  35231, 22511 µs; polls above 4 ms 2.38, 0.63, 1.37, 0.75, 0.13 per minute
  per session; CPU per poll 67–119 µs. The gate fails in every run on
  `maxCpuUs`, and in two of five on the rate.
- **Not the kernel's memory management:** `compact_stall`, `allocstall_*`,
  `thp_fault_alloc` and every `memory.events` counter moved 0 in all runs;
  THP is `madvise`/`madvise` and glibc does not madvise its heap; polls above
  1 ms took 0.2–4.0 minor faults each. **Not the allocator:** five strictly
  interleaved A/B pairs (load-1m 1.23–1.49) with
  `MALLOC_MMAP_THRESHOLD_=32 MiB` and `MALLOC_TRIM_THRESHOLD_=256 MiB`
  (no trim, no per-chunk mmap): `maxCpuUs` 6172–109490 µs against
  21764–91604, above-4 ms rate 0.88–9.75 against 0.88–6.38, peak RSS
  297–306 against 296–311 MiB.
- **The host:** 38–59 % of the polls' CPU is system time (48–65 % in the
  polls above 1 ms), and this guest accounts no steal (`/proc/stat` steal
  0 for 15 days): a thread that only spins on the two clocks saw 29 gaps
  over 1 ms in an idle minute (worst 9.3 ms), each charged to it as thread
  CPU. Thread CPU on this VPS therefore includes hypervisor descheduling,
  so a poll's multi-millisecond CPU is not by itself evidence of work
  [inference: the system share can also be real kernel work such as socket
  I/O or futex wakes, which these runs do not exclude].
- Decision: no allocator or kernel setting; the tail is left to host
  qualification. What would close the item is the same five runs on a host
  that reports steal (or bare metal), or a poll cost measured in guest
  cycles (vPMU) rather than thread CPU — the gate as written cannot be met
  on this VPS by any code change.
- Gates: at M=8 over five runs on an otherwise idle 6-vCPU host, `hlsIngest`
  polls above 4 ms of thread CPU stay under 1 per minute per session, and
  the class `maxCpuUs` stays under 4 ms in every run. A wall-clock maximum
  on a shared host is not a gate any code change can meet.
- Status: open, blocked on a qualifying host (Filed: 2026-10-16 by claude,
  from the HLS ingest isolation measurement; rewritten 2026-10-17 from the
  attribution that measurement asked for; steps 1–3 measured 2026-10-18,
  gate not met on the reference VPS).

### Q-087 [protocol] [sonnet] Align separate renditions on timeline breaks, not on raw proximity

- Goal: a discontinuity crossed by a variant and its separate audio rendition
  re-anchors both on the same new timeline, whatever the raw jump, and an
  abandoned pulled audio rendition is tried again.
- Why: since #364 each rendition keeps its own continuity, and a rendition
  that re-anchors adopts the offset of another whose last raw time is within
  the merge window (three target durations). A forward jump smaller than the
  window at a discontinuity (an SSAI ad whose PTS starts a few seconds past
  the content) is therefore adopted from the other rendition's old timeline
  and stays in the output as a gap of up to the window; A and V stay aligned.
  The proximity rule is what keeps a variant switch (only rendition 0 marks a
  discontinuity, on the same clock) aligned, so it cannot simply go. Separately,
  a pull that failed its audio rendition `MAX_ATTEMPTS` cycles in a row stays
  video-only until it restarts.
- Change: ordering tells the publisher which marks are timeline breaks
  (`EXT-X-DISCONTINUITY`, a new epoch: shared by every rendition, counted as a
  generation) and which keep the clock (a variant switch, an init change);
  a rendition adopts an offset only from one that crossed the same generation,
  and keeps the proximity rule for clock-keeping marks. The pull re-arms an
  abandoned audio rendition at the next master resolution the video itself
  triggers, or after a quiet period.
- Gates: a publisher test with a sub-window forward jump at a discontinuity on
  both renditions yields no gap; the variant-switch alignment test still
  passes; a pull test whose audio playlist recovers after abandonment pulls
  audio again.
- Status: open (Filed: 2026-10-18 by claude, from the #364 review NITs).

### Q-088 [performance] [sonnet] Price MoQ egress from a measured cost
- Goal: capacity planning prices a `moqt://` output from its measured cost
  per output instead of the RTMP prior (`OutputProtocol::Moq` in
  `src/capacity.rs`).
- Gates: a capacity ramp of MoQ outputs against moq-relay on an idle host
  (`cargo xtask build-release`), CPU and memory per output beside RTMP's.
- Status: open (Filed: 2026-10-18 by claude, from the #371 review).

### Q-089 [protocol] [sonnet] The MoQ origin follows a track-set change while served
- Goal: a pipeline whose media gains or loses video or audio while viewers
  watch it over MoQ republishes its broadcast with the new track set, so
  current viewers do not keep the stale one. A served broadcast's track set
  is fixed when its first viewer starts it (`origin::start`); a pipeline
  with no media is refused (#371).
- Contract: a `Served` generation per pipeline, so viewers of a replaced
  broadcast detach from their own generation, and the replacement closes the
  old broadcast.
- Gates: an origin test that switches a ring from audio-only to audio and
  video while a viewer is attached, and the viewer then receives video.
- Status: open (Filed: 2026-10-18 by claude, from the #371 review).

### Q-090 [security] [sonnet] Confine MoQ egress CA files to a configured directory
- Goal: `moqt://…?ca=<file>` reads only PEM files under an operator-set
  directory, so an authenticated dashboard user cannot make the engine read
  any path it can read.
- Sources: `src/media/moq/egress/url.rs`, the egress TLS trust-root setting.
- Gates: a URL test refusing a path outside the directory, including `..`
  and symlink escapes.
- Status: open (Filed: 2026-10-18 by claude, from the #371 review NIT).

### Q-091 [protocol] [opus] SRT Backup failover replays the lost prefix

- Goal: when the active sender in an SRT Backup group disappears after the
  group advanced its sequence, the promoted standby replays the group's
  recent send history before new payload, so the packets the receiver
  missed are not skipped forever.
- Why: srt-rs marks the active unstable/broken, promotes the
  highest-weight standby and aligns it to `group.next_send_sequence`; it
  keeps no group-level recent-send history, so the interval the active
  issued but the receiver never got has no physical sender. srtgo models
  failover with `ActiveFresh`/`ActiveStable` states, a stability threshold
  of `max(T_min, 2*SRTT + 4*RTTVar)` and a replay of retained history on
  promotion; Robotweax independently treats failover as more than
  switching the next sequence.
- Change (srt-rs, then bump the pinned rev in `Cargo.toml`): retain a
  bounded recent-send window per group, qualify standby stability with
  the RTT-relative threshold, and replay the prefix on promotion; refuse
  promotion when the window cannot cover the gap instead of skipping it.
- Gates: differential cases against srtgo's state model for prefix loss,
  RTT-relative stability and repeated failures; a harness mode that kills
  the active SRT output mid-stream and checks receiver continuity.
- Status: open (Filed: 2026-10-20 by claude, from the srt-rs/srtgo/srtrust
  comparison).

### Q-092 [resilience] [opus] Never emit descendants of a dropped dependency-bearing unit

- Goal: every fanout boundary that drops a video unit under pressure also
  suppresses later units that depend on it, until the next keyframe.
- Why: Prism's MoQ viewer queues mark the current GOP damaged and drop
  later delta frames; Restream's boundaries (ring overrun, MoQ group
  eviction, WebTransport pressure, HLS discontinuity, recirculation, RTMP
  congestion) each handle this differently or not at all. An undecodable
  descendant costs bandwidth and shows artifacts.
- Change: one damaged-until-keyframe rule per boundary that can see
  keyframe boundaries; where it cannot, document the fallback and test it.
- Gates: a test per boundary that drops one unit and asserts no later
  dependent unit is emitted before the keyframe; a mutation that removes
  the suppression fails it.
- Status: open (Filed: 2026-10-20 by claude, from the Prism comparison).

### Q-093 [testing] [sonnet] Headless MoQ subscriber for impairment and capacity gates

- Goal: a non-browser subscriber reports startup delay, frame and keyframe
  delivery, audio delivery and A/V skew through an impaired network, beside
  the browser E2E.
- Why: Chromium E2E is slow and nondeterministic for impairment;
  `moq-dev-interop --browsers` proves the product, not the protocol under
  loss.
- Change: extend the `moq-dev-interop` subscriber entry (or a small binary
  over moq-net/hang) to print those counts as JSON and drive one harness
  mode under netem.
- Gates: a harness mode with 2 % loss and 200 ms delay that still reports
  keyframes and audio; the browser E2E stays unchanged.
- Status: open (Filed: 2026-10-20 by claude, from the Prism comparison).

### Q-094 [protocol] [opus] First-class sideband media events

- Goal: captions (CEA-608/708), SCTE-35 and SMPTE timecode survive ingest,
  stages and egress as typed events with the same retention and timestamps
  as media.
- Why: Prism carries these from MPEG-TS to the browser; Restream smuggles
  metadata or drops it, so a spliced or captioned source loses meaning.
- Change: a media-event model shared by ring retention and each egress
  writer, rather than per-path metadata.
- Gates: an ingest fixture with captions and an SCTE-35 splice reaches the
  HLS output and the MoQ catalog; a ring-overrun test keeps events aligned
  with the retained media.
- Status: open (Filed: 2026-10-20 by claude, from the Prism comparison).

### Q-095 [observability] [sonnet] Stream inspector: GOP health, skew and SCTE

- Goal: one view per live pipeline shows GOP health, A/V skew, source
  timecode, SCTE timeline, decoder state and transport state.
- Why: current telemetry is resource-focused; Prism demonstrates the
  media-facing half of observability. This is the human-facing side of the
  telemetry work behind Q-071.
- Gates: a frontend test for a pipeline that loses keyframes shows the
  damaged state; every value comes from the API, not client guessing.
- Status: open (Filed: 2026-10-20 by claude, from the Prism comparison).

### Q-096 [performance] [sonnet] GRO and arrival timestamps in the reference SRT path

- Goal: the non-Compio reference path accepts GRO-coalesced datagrams and
  records socket arrival time before queueing, so scheduler backlog does
  not distort inter-arrival facts.
- Why: srtrust exposes `poll_recv_gro` and stamps arrival at the socket;
  Restream's GSO send exists, but the receive reference path and the
  arrival-time discipline are neither documented nor proven.
- Change (srt-rs): a portable UDP-offload abstraction for the reference
  path; split coalesced receives into logical datagrams with the original
  arrival stamp before protocol processing.
- Gates: a unit test that a coalesced receive splits into logical
  datagrams with identical arrival stamps; a bench of per-datagram cost
  with and without GRO.
- Status: open (Filed: 2026-10-20 by claude, from the srtrust comparison).

### Q-097 [protocol] [opus] Align MediaMTX MoQ dialect with IETF interop
- Goal: unmodified upstream MediaMTX interoperates with Restream's IETF
  MSF/LOC profile in both directions; this is separate from Q-047/Q-070.
- Files: `src/bin/test_harness/moq_relay_interop/`, the pinned upstream
  MediaMTX release and its protocol implementation; no proprietary fork.
- Gates: real egress and forwarded-ingest read-back with H.264/AAC,
  monotonic timestamps and frame counts; retain lite/browser and
  `moq-dev-interop --ietf` coverage, then enable the MediaMTX CI gate.
- Status: open (Filed: 2026-10-18, recovered phase-2 source/spec audit).
- MediaMTX v1.21.2 is a separate dialect blocker, not evidence that IETF
  support is missing from moq-net. Keep `moq-relay-interop` diagnostic-only;
  `moq-dev-interop --ietf` is the moq-relay 0.17.7 draft-19 gate. Do not
  introduce a proprietary MediaMTX profile/fork to make the gate green.
  Pinned source/spec evidence:
  - Namespace discovery: MediaMTX's
    [`controlmessage.Read`](https://github.com/bluenviron/mediamtx/blob/v1.21.2/internal/protocols/moq/controlmessage/message.go#L40-L63)
    has no PUBLISH_NAMESPACE dispatch. MOQT-19
    [§3.3 and §10.15](https://www.ietf.org/archive/id/draft-ietf-moq-transport-19.html#section-10.15)
    define it as a request-stream opener, type `0x6`; the recovered
    Restream→MediaMTX attempt failed with `unknown message type: 0x6`.
  - Setup progress: MediaMTX's
    [`performSetup`](https://github.com/bluenviron/mediamtx/blob/v1.21.2/internal/protocols/moq/client.go#L75-L126)
    waits for the server's SETUP before sending its own for drafts 17–19.
    The recovered MediaMTX→Restream attempt stalled because moq-net's
    server waits for the client's SETUP. MOQT-19
    [§3.3](https://www.ietf.org/archive/id/draft-ietf-moq-transport-19.html#section-3.3)
    permits either peer to send first; do not misreport a client-first
    requirement. Closing the control stream after SETUP also conflicts
    with that section's control-stream lifetime requirement.
  - Media representation: the pinned
    [`session.go`](https://github.com/bluenviron/mediamtx/blob/v1.21.2/internal/servers/moq/session.go#L538-L564)
    sends one codec frame per group, raw codec payload and a public
    timestamp `0x06` in source codec ticks without a TIMESCALE.
    [LOC-04 §2.2–2.3.1.2](https://www.ietf.org/archive/id/draft-ietf-moq-loc-04.html#section-2.3.1.1)
    uses timestamp `0x10` and timescale `0x08` (absent timescale means
    microseconds since Unix epoch). This is not the LOC framing consumed
    by moq-loc 0.2.24. [MSF-00 §4.1](https://www.ietf.org/archive/id/draft-ietf-moq-msf-00.html#section-4.1)
    requires a GOP's samples in one group, not a new group per frame.
  - Catalog/codec subset: the pinned
    [`catalog.Track`](https://github.com/bluenviron/mediamtx/blob/v1.21.2/internal/protocols/moq/catalog/catalog.go)
    emits `channels`/`clockrate`; MSF-00
    [§5.1.27, §5.1.31–32](https://www.ietf.org/archive/id/draft-ietf-moq-msf-00.html#section-5.1.32)
    specifies `timescale`, `samplerate` and `channelConfig` (`samplerate`
    itself is not a mismatch). Its
    [`ToStream`](https://github.com/bluenviron/mediamtx/blob/v1.21.2/internal/protocols/moq/to_stream.go#L103-L125)
    accepts `avc3`/`hev1`, not Restream's `avc1`/`hvc1`; LOC-04
    [§2.1.1–2](https://www.ietf.org/archive/id/draft-ietf-moq-loc-04.html#section-2.1.2)
    describes both forms. Revisit against an unmodified upstream release
    with these contracts aligned; then require both directions and media
    read-back before re-enabling the MediaMTX gate.
